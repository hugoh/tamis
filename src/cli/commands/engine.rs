//! `bootstrap`, `undo` and `learn`: commands that act on the mailbox.

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};

use super::super::{App, BootstrapArgs, Reported};
use super::common::{engine_for, open};
use crate::classify::Label;
use crate::config::{Config, parse_duration};
use crate::daemon::PASS_TIMEOUT;
use crate::engine::{BootstrapOptions, BootstrapSummary, Source};

const DEFAULT_SENT_LIMIT: usize = 2000;
const DEFAULT_CORPUS_SIZE: usize = 500;
const BOOTSTRAP_TIMEOUT: Duration = Duration::from_hours(1);

pub(in super::super) async fn bootstrap(app: &mut App, args: &BootstrapArgs) -> Result<()> {
    let (cfg, store) = open(app)?;
    let (cfg, store) = (Arc::new(cfg), Arc::new(store));
    let opts = choose_sources(&cfg, args)?;

    let mut engine = engine_for(app, &cfg, &store).await?;
    let outcome = tokio::time::timeout(BOOTSTRAP_TIMEOUT, engine.bootstrap(&opts)).await;
    engine.into_mailbox().close().await;
    let summary = outcome.map_err(|_| anyhow!("bootstrap took too long"))??;

    print_bootstrap(app, &cfg, &opts, &summary)
}

/// Fills the options from the flags. Naming any of `--sent`, `--spam` or
/// `--good` limits bootstrap to what was named. Refreshing re-learns mail and
/// leaves the whitelist and the watch position alone, unless `--sent` is named.
fn choose_sources(cfg: &Config, args: &BootstrapArgs) -> Result<BootstrapOptions> {
    let named = args.sent.is_some() || !args.spam.is_empty() || !args.good.is_empty();
    let want_spam = !named || !args.spam.is_empty();
    let want_good = !named || !args.good.is_empty();

    let good_min_age =
        parse_duration(&args.good_min_age).map_err(|err| anyhow!("--good-min-age: {err}"))?;
    let sent_limit = match args.sent {
        Some(limit) => limit,
        None if named || args.refresh => 0,
        None => DEFAULT_SENT_LIMIT,
    };

    Ok(BootstrapOptions {
        sent_limit,
        spam: if want_spam {
            to_sources(&args.spam, &cfg.folders.junk)?
        } else {
            Vec::new()
        },
        good: if want_good {
            to_sources(&args.good, &cfg.folders.watch[0])?
        } else {
            Vec::new()
        },
        good_min_age,
        keep_cursors: named || args.refresh,
        refresh: args.refresh,
    })
}

/// Turns `FOLDER=N` flag values into bootstrap sources, in folder order,
/// defaulting to `fallback` when none are given.
fn to_sources(values: &[String], fallback: &str) -> Result<Vec<Source>> {
    if values.is_empty() {
        return Ok(vec![Source {
            folder: fallback.to_owned(),
            limit: DEFAULT_CORPUS_SIZE,
        }]);
    }

    let mut sources = BTreeMap::new();
    for value in values {
        let bad = |why: &str| anyhow!("{value}: {why}: want FOLDER=N with N > 0");
        let (folder, count) = value.rsplit_once('=').ok_or_else(|| bad("missing ="))?;
        if folder.is_empty() {
            bail!("={count}: missing folder: want FOLDER=N with N > 0");
        }
        let limit: usize = count.parse().map_err(|_| bad("not a number"))?;
        if limit < 1 {
            return Err(bad("count must be positive"));
        }
        sources.insert(folder.to_owned(), limit);
    }
    Ok(sources
        .into_iter()
        .map(|(folder, limit)| Source { folder, limit })
        .collect())
}

fn print_bootstrap(
    app: &mut App,
    cfg: &Config,
    opts: &BootstrapOptions,
    summary: &BootstrapSummary,
) -> Result<()> {
    let out = &mut app.env.stdout;
    if opts.sent_limit > 0 {
        writeln!(
            out,
            "Whitelisted {} addresses from {}.",
            summary.addresses, cfg.folders.sent
        )?;
    }

    let mut trained = Vec::new();
    if !opts.spam.is_empty() {
        trained.push(format!("{} spam", summary.spam));
    }
    if !opts.good.is_empty() {
        trained.push(format!("{} good", summary.good));
    }
    if !trained.is_empty() {
        writeln!(out, "Trained {} messages.", trained.join(" and "))?;
    }
    if summary.refreshed > 0 {
        writeln!(
            out,
            "Refreshed {} messages that had been learnt before.",
            summary.refreshed
        )?;
    }
    if let Some(domain) = &summary.auth_server_id {
        writeln!(
            out,
            "Your provider's authentication results come from {domain}; set account.auth_server_id to override."
        )?;
    }
    if !opts.keep_cursors {
        let mode = if cfg.policy.mode == crate::config::Mode::Live {
            "live"
        } else {
            "shadow"
        };
        writeln!(
            out,
            "Watching [{}] from now on, in {mode} mode. Next: tamis run",
            cfg.folders.watch.join(" ")
        )?;
    }
    Ok(())
}

pub(in super::super) async fn undo(app: &mut App, ids: &[i64]) -> Result<()> {
    on_decisions(app, ids, Action::Undo).await
}

pub(in super::super) async fn learn(
    app: &mut App,
    ids: &[i64],
    label: Option<Label>,
) -> Result<()> {
    on_decisions(app, ids, Action::Learn(label)).await
}

#[derive(Clone, Copy)]
enum Action {
    Undo,
    Learn(Option<Label>),
}

/// Applies `action` to each decision in turn, reporting each outcome, and
/// fails if any of them did.
async fn on_decisions(app: &mut App, ids: &[i64], action: Action) -> Result<()> {
    let (cfg, store) = open(app)?;
    let (cfg, store) = (Arc::new(cfg), Arc::new(store));
    let mut engine = engine_for(app, &cfg, &store).await?;

    let mut failed = false;
    for &id in ids {
        let work = async {
            match action {
                Action::Undo => engine.undo(id).await,
                Action::Learn(label) => engine.learn(id, label).await,
            }
        };
        match tokio::time::timeout(PASS_TIMEOUT, work).await {
            Ok(Ok(summary)) => writeln!(app.env.stdout, "{id}: {summary}")?,
            Ok(Err(err)) => {
                writeln!(app.env.stderr, "{id}: {err}")?;
                failed = true;
            }
            Err(_) => {
                writeln!(app.env.stderr, "{id}: timed out")?;
                failed = true;
            }
        }
    }
    engine.into_mailbox().close().await;
    if failed { Err(Reported.into()) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::default()
    }

    fn args() -> BootstrapArgs {
        BootstrapArgs {
            sent: None,
            spam: Vec::new(),
            good: Vec::new(),
            good_min_age: "168h".to_owned(),
            refresh: false,
        }
    }

    fn sources(sources: &[Source]) -> Vec<(String, usize)> {
        sources
            .iter()
            .map(|s| (s.folder.clone(), s.limit))
            .collect()
    }

    #[test]
    fn bootstrap_defaults_to_everything_and_starts_watching() {
        let opts = choose_sources(&cfg(), &args()).unwrap();

        assert_eq!(opts.sent_limit, 2000);
        assert_eq!(sources(&opts.spam), [("Junk".to_owned(), 500)]);
        assert_eq!(sources(&opts.good), [("INBOX".to_owned(), 500)]);
        assert_eq!(opts.good_min_age, Duration::from_hours(168));
        assert!(!opts.keep_cursors && !opts.refresh);
    }

    #[test]
    fn naming_a_kind_limits_bootstrap_to_it_and_leaves_the_cursors_alone() {
        let spam_only = choose_sources(
            &cfg(),
            &BootstrapArgs {
                spam: vec!["Junk=10".into()],
                ..args()
            },
        )
        .unwrap();
        assert_eq!(
            (
                spam_only.sent_limit,
                spam_only.good.len(),
                spam_only.keep_cursors
            ),
            (0, 0, true)
        );
        assert_eq!(sources(&spam_only.spam), [("Junk".to_owned(), 10)]);

        let sent_only = choose_sources(
            &cfg(),
            &BootstrapArgs {
                sent: Some(50),
                ..args()
            },
        )
        .unwrap();
        assert_eq!(
            (
                sent_only.sent_limit,
                sent_only.spam.len(),
                sent_only.good.len()
            ),
            (50, 0, 0)
        );
    }

    #[test]
    fn refreshing_leaves_the_whitelist_and_watch_position_alone_unless_sent_is_named() {
        let refresh = choose_sources(
            &cfg(),
            &BootstrapArgs {
                refresh: true,
                ..args()
            },
        )
        .unwrap();
        assert_eq!(
            (refresh.sent_limit, refresh.keep_cursors, refresh.refresh),
            (0, true, true)
        );
        assert_eq!((refresh.spam.len(), refresh.good.len()), (1, 1));

        let with_sent = choose_sources(
            &cfg(),
            &BootstrapArgs {
                refresh: true,
                sent: Some(30),
                ..args()
            },
        )
        .unwrap();
        assert_eq!(with_sent.sent_limit, 30);
    }

    #[test]
    fn folder_counts_are_parsed_sorted_and_checked() {
        let ok = to_sources(
            &[
                "Zed=2".to_owned(),
                "Archive/2024=7".to_owned(),
                "Zed=3".to_owned(),
            ],
            "x",
        )
        .unwrap();
        assert_eq!(
            sources(&ok),
            [("Archive/2024".to_owned(), 7), ("Zed".to_owned(), 3)]
        );

        for bad in ["Junk", "=5", "Junk=0", "Junk=-1", "Junk=many"] {
            let err = to_sources(&[bad.to_owned()], "x").unwrap_err().to_string();
            assert!(err.contains("want FOLDER=N with N > 0"), "{bad}: {err}");
        }
        let err = choose_sources(
            &cfg(),
            &BootstrapArgs {
                good_min_age: "soon".into(),
                ..args()
            },
        )
        .unwrap_err();
        assert!(err.to_string().starts_with("--good-min-age:"), "{err}");
    }
}
