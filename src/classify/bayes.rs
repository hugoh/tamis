const UNKNOWN_WORD_STRENGTH: f64 = 0.45;
const UNKNOWN_WORD_PROB: f64 = 0.5;
const MIN_PROB_STRENGTH: f64 = 0.1;
const MAX_DISCRIMINATORS: usize = 150;
const FISHER_FACTOR: f64 = 2.0;
const SCORE_SPAN: f64 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Label {
    Good,
    Spam,
}

impl Label {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Label::Good => "good",
            Label::Spam => "spam",
        }
    }
}

/// The token statistics a score is computed from.
pub trait Model {
    /// How many trained spam and good messages contained `token`.
    fn counts(&self, token: &str) -> (u64, u64);
    /// How many spam and good messages were trained in total.
    fn totals(&self) -> (u64, u64);
}

#[derive(Clone, Debug, PartialEq)]
pub struct Clue {
    pub token: String,
    pub prob: f64,
}

/// A spam score in [0, 1] (0.5 means no evidence) and the clues behind it,
/// strongest first.
#[derive(Clone, Debug, PartialEq)]
pub struct Score {
    pub score: f64,
    pub clues: Vec<Clue>,
}

/// Combines the tokens' spam probabilities with Fisher's chi-square method.
/// Only the most discriminating tokens count.
#[must_use]
pub fn score(model: &impl Model, tokens: &[String]) -> Score {
    let clues = discriminators(model, tokens);
    if clues.is_empty() {
        return Score {
            score: UNKNOWN_WORD_PROB,
            clues,
        };
    }

    let (log_good, log_spam) = clues.iter().fold((0.0, 0.0), |(good, spam), clue| {
        (good + clue.prob.ln(), spam + (1.0 - clue.prob).ln())
    });

    let degrees = clues.len() * 2;
    let spamness = 1.0 - chi2_q(-FISHER_FACTOR * log_spam, degrees);
    let goodness = 1.0 - chi2_q(-FISHER_FACTOR * log_good, degrees);

    Score {
        score: (spamness - goodness + 1.0) / SCORE_SPAN,
        clues,
    }
}

fn discriminators(model: &impl Model, tokens: &[String]) -> Vec<Clue> {
    let (total_spam, total_good) = model.totals();

    let mut clues: Vec<Clue> = tokens
        .iter()
        .filter_map(|token| {
            let (spam, good) = model.counts(token);
            let prob = token_prob(spam, good, total_spam, total_good);
            ((prob - UNKNOWN_WORD_PROB).abs() >= MIN_PROB_STRENGTH).then(|| Clue {
                token: token.clone(),
                prob,
            })
        })
        .collect();

    clues.sort_by(|a, b| {
        let strength = |clue: &Clue| (clue.prob - UNKNOWN_WORD_PROB).abs();
        strength(b).total_cmp(&strength(a))
    });
    clues.truncate(MAX_DISCRIMINATORS);
    clues
}

/// Robinson's smoothed probability that a message containing the token is
/// spam, pulled towards 0.5 when the token is rarely seen.
#[allow(clippy::cast_precision_loss)]
fn token_prob(spam: u64, good: u64, total_spam: u64, total_good: u64) -> f64 {
    let seen = (spam + good) as f64;
    if seen == 0.0 {
        return UNKNOWN_WORD_PROB;
    }

    let ratio = |count: u64, total: u64| {
        if total > 0 {
            count as f64 / total as f64
        } else {
            0.0
        }
    };
    let spam_ratio = ratio(spam, total_spam);
    let good_ratio = ratio(good, total_good);
    let raw = spam_ratio / (spam_ratio + good_ratio);

    UNKNOWN_WORD_STRENGTH.mul_add(UNKNOWN_WORD_PROB, seen * raw) / (UNKNOWN_WORD_STRENGTH + seen)
}

/// The probability that a chi-square variable with even `degrees` of freedom
/// exceeds `x2`.
#[allow(clippy::cast_precision_loss)]
fn chi2_q(x2: f64, degrees: usize) -> f64 {
    let m = x2 / FISHER_FACTOR;
    let mut term = (-m).exp();
    let mut sum = term;

    for i in 1..degrees / 2 {
        term *= m / i as f64;
        sum += term;
    }

    sum.min(1.0)
}
