//! Compares ways of building a message's tokens by replaying a corpus the way
//! the filter would have lived it: score each message, then learn it.

pub mod cli;
mod fetch;
mod load;
mod net;

use std::collections::HashMap;
use std::io::Write;

use thiserror::Error;

use crate::classify::{self, Label, Model};

pub use fetch::{Spec, fetch_folder, parse_spec};
pub use load::load;

pub const SPAM_CUT: f64 = 0.9;
pub const GOOD_CUT: f64 = 0.2;
pub const DEFAULT_WARMUP: usize = 200;
const SHUFFLE_STREAM: u64 = 99;

#[derive(Debug, Error)]
pub enum Error {
    #[error("need scored messages of both classes")]
    OneClass,
    #[error("no messages")]
    NoMessages,
    #[error("{0}")]
    Io(String),
}

#[derive(Clone, Debug)]
pub struct Message {
    pub raw: Vec<u8>,
    pub spam: bool,
}

/// How a variant turns a message into the tokens that are scored and
/// learnt. It sees the model so far, as the recipient status does.
pub trait Featurizer {
    fn tokens(&self, raw: &[u8], model: &dyn Model) -> Vec<String>;
}

pub struct Variant {
    pub name: String,
    pub new: Box<dyn Fn() -> Box<dyn Featurizer> + Send + Sync>,
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Leading messages that are learnt but not scored.
    pub warmup: usize,
}

/// How one variant did on one pass over the corpus.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Outcome {
    /// `(1 - AUC)` as a percentage: how often a good message outscores spam.
    pub loss_pct: f64,
    /// Percentage of spam scoring at least [`SPAM_CUT`].
    pub spam_caught: f64,
    /// Percentage of good mail scoring at least [`SPAM_CUT`].
    pub ham_flagged: f64,
    /// Percentage of good mail between [`GOOD_CUT`] and [`SPAM_CUT`].
    pub ham_unsure: f64,
    pub ham: usize,
    pub spam: usize,
}

struct Production {
    own: Vec<String>,
}

impl Featurizer for Production {
    fn tokens(&self, raw: &[u8], model: &dyn Model) -> Vec<String> {
        classify::with_recipient_status(&classify::tokenize(raw), model, &self.own)
    }
}

/// What tamis does today.
#[must_use]
pub fn baseline(own: &[String]) -> Variant {
    let own = own.to_vec();
    Variant {
        name: "baseline (tamis today)".to_owned(),
        new: Box::new(move || Box::new(Production { own: own.clone() })),
    }
}

struct WithoutRecipients;

impl Featurizer for WithoutRecipients {
    fn tokens(&self, raw: &[u8], _: &dyn Model) -> Vec<String> {
        let mut tokens = classify::tokenize(raw);
        tokens.retain(|token| !token.starts_with("to:") && !token.starts_with("to-domain:"));
        tokens
    }
}

/// Tokens without the recipient addresses and their status.
#[must_use]
pub fn without_recipients() -> Variant {
    Variant {
        name: "- recipient tokens".to_owned(),
        new: Box::new(|| Box::new(WithoutRecipients)),
    }
}

/// The variants `tamis-eval run` compares; the first is the baseline.
#[must_use]
pub fn standard(own: &[String]) -> Vec<Variant> {
    vec![baseline(own), without_recipients()]
}

#[derive(Default)]
struct Counts {
    by_token: HashMap<String, (u64, u64)>,
    spam: u64,
    good: u64,
}

impl Model for Counts {
    fn counts(&self, token: &str) -> (u64, u64) {
        self.by_token.get(token).copied().unwrap_or((0, 0))
    }

    fn totals(&self) -> (u64, u64) {
        (self.spam, self.good)
    }
}

impl Counts {
    fn train(&mut self, tokens: &[String], label: Label) {
        for token in tokens {
            let entry = self.by_token.entry(token.clone()).or_default();
            match label {
                Label::Spam => entry.0 += 1,
                Label::Good => entry.1 += 1,
            }
        }
        match label {
            Label::Spam => self.spam += 1,
            Label::Good => self.good += 1,
        }
    }
}

fn share(scores: &[f64], keep: impl Fn(f64) -> bool) -> f64 {
    #[allow(
        clippy::cast_precision_loss,
        reason = "corpus sizes are far below 2^52"
    )]
    let ratio = scores.iter().filter(|s| keep(**s)).count() as f64 / scores.len() as f64;
    ratio * 100.0
}

/// Replays `messages` in order through one variant.
///
/// # Errors
/// [`Error::OneClass`] when warm-up leaves messages of only one class to
/// score.
pub fn run(messages: &[Message], variant: &Variant, options: Options) -> Result<Outcome, Error> {
    let featurizer = (variant.new)();
    let mut model = Counts::default();
    let (mut ham, mut spam) = (Vec::new(), Vec::new());

    for (index, message) in messages.iter().enumerate() {
        let tokens = featurizer.tokens(&message.raw, &model);
        if index >= options.warmup {
            let score = classify::score(&model, &tokens).score;
            if message.spam {
                spam.push(score);
            } else {
                ham.push(score);
            }
        }
        model.train(
            &tokens,
            if message.spam {
                Label::Spam
            } else {
                Label::Good
            },
        );
    }
    if ham.is_empty() || spam.is_empty() {
        return Err(Error::OneClass);
    }
    Ok(Outcome {
        loss_pct: (1.0 - auc(&ham, &spam)) * 100.0,
        spam_caught: share(&spam, |s| s >= SPAM_CUT),
        ham_flagged: share(&ham, |s| s >= SPAM_CUT),
        ham_unsure: share(&ham, |s| s > GOOD_CUT && s < SPAM_CUT),
        ham: ham.len(),
        spam: spam.len(),
    })
}

/// The chance a random spam scores above a random good message, counting
/// ties as half: the Mann-Whitney statistic from average ranks.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    reason = "corpus sizes are far below 2^52"
)]
pub fn auc(ham: &[f64], spam: &[f64]) -> f64 {
    let mut all: Vec<(f64, bool)> = ham
        .iter()
        .map(|s| (*s, false))
        .chain(spam.iter().map(|s| (*s, true)))
        .collect();
    all.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut rank_sum = 0.0;
    let mut start = 0;
    while start < all.len() {
        let mut end = start;
        while end < all.len() && all[end].0.total_cmp(&all[start].0).is_eq() {
            end += 1;
        }
        let mean_rank = (start + end + 1) as f64 / 2.0;
        rank_sum += mean_rank
            * all[start..end]
                .iter()
                .filter(|(_, is_spam)| *is_spam)
                .count() as f64;
        start = end;
    }
    let (spam_count, ham_count) = (spam.len() as f64, ham.len() as f64);
    (rank_sum - spam_count * (spam_count + 1.0) / 2.0) / (spam_count * ham_count)
}

fn shuffled(messages: &[Message], seed: u64) -> Vec<Message> {
    let mut rng = fastrand::Rng::with_seed(seed.wrapping_mul(SHUFFLE_STREAM));
    let mut out = messages.to_vec();
    rng.shuffle(&mut out);
    out
}

/// Runs every variant `runs` times: the first in the corpus's order, the
/// others shuffled the same way for every variant, so they can be paired.
///
/// # Errors
/// The first variant error, named after the variant.
pub fn compare(
    messages: &[Message],
    variants: &[Variant],
    runs: usize,
    options: Options,
) -> Result<Vec<Vec<Outcome>>, Error> {
    let orders: Vec<Vec<Message>> = (0..runs)
        .map(|run| {
            if run == 0 {
                messages.to_vec()
            } else {
                shuffled(messages, run as u64)
            }
        })
        .collect();

    std::thread::scope(|scope| {
        let handles: Vec<Vec<_>> = variants
            .iter()
            .map(|variant| {
                orders
                    .iter()
                    .map(|order| scope.spawn(move || run(order, variant, options)))
                    .collect()
            })
            .collect();
        handles
            .into_iter()
            .zip(variants)
            .map(|(runs, variant)| {
                runs.into_iter()
                    .map(|handle| {
                        handle
                            .join()
                            .map_err(|_| Error::Io(format!("{}: panicked", variant.name)))?
                            .map_err(|err| Error::Io(format!("{}: {err}", variant.name)))
                    })
                    .collect()
            })
            .collect()
    })
}

fn mean_loss(runs: &[Outcome]) -> f64 {
    #[allow(clippy::cast_precision_loss, reason = "a handful of runs")]
    let count = runs.len() as f64;
    runs.iter().map(|r| r.loss_pct).sum::<f64>() / count
}

fn paired_change(baseline: &[Outcome], runs: &[Outcome]) -> String {
    let diffs: Vec<f64> = runs
        .iter()
        .zip(baseline)
        .map(|(run, base)| run.loss_pct - base.loss_pct)
        .collect();
    let better = diffs.iter().filter(|d| **d < 0.0).count();
    #[allow(clippy::cast_precision_loss, reason = "a handful of runs")]
    let mean = diffs.iter().sum::<f64>() / diffs.len() as f64;
    let best = diffs.iter().copied().fold(f64::INFINITY, f64::min);
    let worst = diffs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    format!(
        "{mean:+.3} ({best:+.3}..{worst:+.3}), better in {better} of {}",
        diffs.len()
    )
}

/// Prints one row per variant.
///
/// # Errors
/// When `out` can't be written.
pub fn write_report(
    out: &mut impl Write,
    variants: &[Variant],
    results: &[Vec<Outcome>],
) -> std::io::Result<()> {
    let mut rows = vec![
        [
            "variant",
            "chrono (1-AUC)%",
            "mean",
            "vs first: mean (best..worst)",
            "spam>=0.9 %",
            "ham>=0.9 %",
            "ham unsure %",
        ]
        .map(str::to_owned)
        .to_vec(),
    ];
    for (index, variant) in variants.iter().enumerate() {
        let runs = &results[index];
        let change = if index == 0 {
            "-".to_owned()
        } else {
            paired_change(&results[0], runs)
        };
        let first = runs[0];
        rows.push(vec![
            variant.name.clone(),
            format!("{:.3}", first.loss_pct),
            format!("{:.3}", mean_loss(runs)),
            change,
            format!("{:.1}", first.spam_caught),
            format!("{:.2}", first.ham_flagged),
            format!("{:.1}", first.ham_unsure),
        ]);
    }
    crate::report::write_table(out, &rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(spam: bool, n: usize) -> Message {
        let text = if spam {
            format!(
                "From: pharma{n}@junk.example\r\nTo: me@example.org\r\nSubject: cheap pills {n}\r\n\r\nbuy cheap pills now winner prize"
            )
        } else {
            format!(
                "From: friend{n}@home.example\r\nTo: me@example.org\r\nSubject: lunch {n}\r\n\r\nlunch tomorrow meeting agenda notes"
            )
        };
        Message {
            raw: text.into_bytes(),
            spam,
        }
    }

    fn corpus(each: usize) -> Vec<Message> {
        (0..each)
            .flat_map(|n| [message(false, n), message(true, n)])
            .collect()
    }

    #[test]
    fn auc_is_the_chance_spam_outscores_good_with_ties_as_half() {
        assert!((auc(&[0.1, 0.2], &[0.8, 0.9]) - 1.0).abs() < 1e-12);
        assert!(auc(&[0.8, 0.9], &[0.1, 0.2]).abs() < 1e-12);
        assert!((auc(&[0.5, 0.5], &[0.5, 0.5]) - 0.5).abs() < 1e-12);
        assert!((auc(&[0.1, 0.6], &[0.5, 0.9]) - 0.75).abs() < 1e-12);
    }

    #[test]
    fn a_separable_corpus_is_learnt_and_warmup_is_not_scored() {
        let outcome = run(&corpus(30), &baseline(&[]), Options { warmup: 10 }).unwrap();

        assert_eq!((outcome.ham, outcome.spam), (25, 25));
        assert!(
            outcome.loss_pct < 1.0 && outcome.spam_caught > 90.0 && outcome.ham_flagged < 1.0,
            "{outcome:?}"
        );
    }

    #[test]
    fn scoring_nothing_of_a_class_is_an_error() {
        let all_ham: Vec<Message> = (0..5).map(|n| message(false, n)).collect();

        assert!(matches!(
            run(&all_ham, &baseline(&[]), Options { warmup: 0 }),
            Err(Error::OneClass)
        ));
        assert!(matches!(
            run(&corpus(3), &baseline(&[]), Options { warmup: 100 }),
            Err(Error::OneClass)
        ));
    }

    #[test]
    fn the_recipient_variant_drops_recipient_tokens_only() {
        let raw = &message(false, 1).raw;
        let with = Production { own: vec![] }.tokens(raw, &Counts::default());
        let without = WithoutRecipients.tokens(raw, &Counts::default());

        assert!(with.iter().any(|t| t.starts_with("to:")));
        assert!(
            !without
                .iter()
                .any(|t| t.starts_with("to:") || t.starts_with("to-domain:"))
        );
        assert!(without.iter().any(|t| t.contains("lunch")));
    }

    #[test]
    fn compare_runs_every_variant_the_same_number_of_times_repeatably() {
        let variants = standard(&[]);
        let messages = corpus(20);

        let first = compare(&messages, &variants, 3, Options { warmup: 5 }).unwrap();
        let again = compare(&messages, &variants, 3, Options { warmup: 5 }).unwrap();

        assert_eq!((first.len(), first[0].len(), first[1].len()), (2, 3, 3));
        assert_eq!(first, again);
        assert_eq!(
            first[0][0],
            run(&messages, &variants[0], Options { warmup: 5 }).unwrap()
        );
    }

    #[test]
    fn the_report_pairs_each_variant_with_the_first() {
        let outcome = |loss| Outcome {
            loss_pct: loss,
            spam_caught: 99.0,
            ham_flagged: 0.5,
            ham_unsure: 2.0,
            ham: 10,
            spam: 10,
        };
        let variants = standard(&[]);
        let results = vec![
            vec![outcome(1.0), outcome(2.0)],
            vec![outcome(0.5), outcome(2.5)],
        ];
        let mut out = Vec::new();

        write_report(&mut out, &variants, &results).unwrap();

        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert!(
            lines[0].starts_with("variant") && lines[0].contains("ham unsure %"),
            "{text}"
        );
        assert!(
            lines[1].contains("baseline (tamis today)")
                && lines[1]
                    .split_whitespace()
                    .skip(3)
                    .take(3)
                    .eq(["1.000", "1.500", "-"]),
            "{text}"
        );
        assert!(
            lines[2].contains("+0.000 (-0.500..+0.500), better in 1 of 2"),
            "{text}"
        );
    }
}
