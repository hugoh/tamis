//! `status`, `doctor` and `monitor`: what the daemon and the filter are doing.

use std::io::Write;
use std::time::Duration;

use anyhow::{Result, anyhow};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::Serialize;

use super::super::style::{
    BOLD, BOLD_RED, BOLD_YELLOW, Cell, DIM, GREEN, Style, cell, write_table,
};
use super::super::{App, Reported};
use super::common::{open, palette};
use crate::config::Config;
use crate::engine::{AuthSource, resolve_auth_pin};
use crate::health::{self, Report, Status};
use crate::monitor::{self, Check as MonitorCheck, Inputs, Pinger};
use crate::report::{write_json, write_table as write_plain_table};
use crate::store::Store;

/// A pass is cut off after this long; the heartbeat's grace allows for it.
const PASS_TIMEOUT: Duration = Duration::from_mins(10);
const MAX_PASS_RETRY: Duration = Duration::from_mins(15);

pub(in super::super) fn status(app: &mut App) -> Result<()> {
    let (cfg, store) = open(app)?;
    let stats = store.stats()?;

    let rows: Vec<Vec<String>> = vec![
        row(
            "Mode:",
            if cfg.policy.mode == crate::config::Mode::Live {
                "live"
            } else {
                "shadow"
            },
        ),
        row(
            "Account:",
            &format!("{} on {}", cfg.account.user, cfg.account.addr()),
        ),
        row("Watching:", &format!("[{}]", cfg.folders.watch.join(" "))),
        row(
            "Training folders:",
            &format!("{}, {}", cfg.folders.train_spam, cfg.folders.train_good),
        ),
        row(
            "Learnt:",
            &format!(
                "{} spam, {} good messages ({} tokens)",
                stats.spam, stats.good, stats.tokens
            ),
        ),
        row("Whitelist:", &format!("{} addresses", stats.addresses)),
        row("Auth server id:", &auth_server_id_line(&cfg, &store)),
        row("Daemon:", &daemon_line(&cfg, &store)),
        row("State:", &cfg.state.display().to_string()),
    ];
    write_plain_table(&mut app.env.stdout, &rows)?;
    Ok(())
}

fn row(label: &str, value: &str) -> Vec<String> {
    vec![label.to_owned(), value.to_owned()]
}

/// How `tamis run` last reported itself to be doing.
fn daemon_line(cfg: &Config, store: &Store) -> String {
    match health::load_daemon(store, cfg.policy.poll_interval) {
        Err(err) => format!("unknown: {err}"),
        Ok(info) => {
            let (status, detail) = info.describe(Timestamp::now(), &TimeZone::system());
            if matches!(status, Status::Warn | Status::Fail) {
                format!("{}: {detail}", status.as_str().to_uppercase())
            } else {
                detail
            }
        }
    }
}

/// Which authserv-id tamis pins `Authentication-Results` to, and where that
/// came from.
fn auth_server_id_line(cfg: &Config, store: &Store) -> String {
    let pin = match resolve_auth_pin(store, cfg) {
        Ok(pin) => pin,
        Err(err) => return format!("unknown: {err}"),
    };
    match pin.source {
        AuthSource::Stale => format!(
            "not used: learnt for {}, the account now uses {} (run tamis bootstrap)",
            pin.learnt_for, pin.host
        ),
        AuthSource::Off => "off (configured)".to_owned(),
        AuthSource::Unset => {
            "not set; only the topmost header counts (run tamis bootstrap to learn it)".to_owned()
        }
        AuthSource::Configured => format!("{} (configured)", pin.id),
        AuthSource::Learnt => format!("{} (learnt)", pin.id),
    }
}

pub(in super::super) fn doctor(app: &mut App, as_json: bool) -> Result<()> {
    let report = if let Err(err) = Config::load(&*app.env.getenv, &app.config_path) {
        let in_rules = err.to_string().contains("rules.script:");
        health::config_failed(&err.to_string(), in_rules)
    } else {
        let (cfg, store) = open(app)?;
        health::collect(&store, &cfg)?
    };
    write_health(app, &report, as_json)
}

/// Prints the report, and fails the command if a check failed.
fn write_health(app: &mut App, report: &Report, as_json: bool) -> Result<()> {
    if as_json {
        write_json(&mut app.env.stdout, report)?;
    } else {
        write_health_text(app, report)?;
    }
    if report.failed() {
        Err(Reported.into())
    } else {
        Ok(())
    }
}

fn status_style(status: Status) -> Style {
    match status {
        Status::Ok => GREEN,
        Status::Warn => BOLD_YELLOW,
        Status::Fail => BOLD_RED,
        Status::Skip => DIM,
    }
}

/// Prints the checks under a heading for each section, with problems in
/// capitals and in colour.
fn write_health_text(app: &mut App, report: &Report) -> Result<()> {
    let colors = palette(app)?;
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    let mut section = "";

    for check in &report.checks {
        if check.section != section {
            if !section.is_empty() {
                rows.push(vec![Vec::new()]);
            }
            section = &check.section;
            rows.push(vec![cell(&check.section, BOLD)]);
        }
        let text = if matches!(check.status, Status::Warn | Status::Fail) {
            format!("{:<4}", check.status.as_str().to_uppercase())
        } else {
            check.status.as_str().to_owned()
        };
        rows.push(vec![
            vec![("  ".to_owned(), ""), (text, status_style(check.status))],
            cell(&check.name, ""),
            cell(&check.detail, DIM),
        ]);
    }
    write_table(&mut app.env.stdout, colors, &rows)
}

pub(in super::super) fn monitor(app: &mut App, as_json: bool) -> Result<()> {
    let cfg = Config::load(&*app.env.getenv, &app.config_path)?;
    let settings = monitor::recommend(&Inputs {
        poll: cfg.policy.poll_interval,
        fail_after: cfg.monitor.fail_after,
        pass_timeout: PASS_TIMEOUT,
        max_retry: MAX_PASS_RETRY,
    });

    if as_json {
        write_json(&mut app.env.stdout, &settings)?;
        return Ok(());
    }
    write_monitor(app, &cfg, &settings)
}

fn write_monitor(app: &mut App, cfg: &Config, settings: &monitor::Settings) -> Result<()> {
    let mut rows: Vec<Vec<String>> = vec![
        vec!["Create two checks on the service:".to_owned()],
        ["", "CHECK", "PERIOD", "GRACE", "CONFIG KEY"]
            .map(str::to_owned)
            .to_vec(),
        check_row(
            &settings.heartbeat,
            "monitor.ping_url",
            !cfg.monitor.ping_url.is_empty(),
        ),
        check_row(
            &settings.quality,
            "monitor.quality_url",
            !cfg.monitor.quality_url.is_empty(),
        ),
        vec![String::new()],
        vec!["They would alert after:".to_owned()],
    ];
    for alert in &settings.alerts {
        rows.push(vec![
            String::new(),
            span_text(alert.after_seconds),
            alert.when.clone(),
        ]);
    }
    write_plain_table(&mut app.env.stdout, &rows)?;

    writeln!(
        app.env.stdout,
        "\nOr create them through the healthchecks.io API:"
    )?;
    for check in [&settings.heartbeat, &settings.quality] {
        #[derive(Serialize)]
        struct ApiCheck<'a> {
            name: &'a str,
            timeout: u64,
            grace: u64,
            unique: [&'static str; 1],
        }
        let payload = serde_json::to_string(&ApiCheck {
            name: &check.name,
            timeout: check.timeout,
            grace: check.grace,
            unique: ["name"],
        })?;
        writeln!(
            app.env.stdout,
            "  curl -s https://healthchecks.io/api/v3/checks/ -H \"X-Api-Key: $KEY\" -d '{payload}'"
        )?;
    }
    Ok(())
}

fn check_row(check: &MonitorCheck, key: &str, configured: bool) -> Vec<String> {
    let state = if configured {
        key.to_owned()
    } else {
        format!("{key} (not set)")
    };
    vec![
        String::new(),
        check.name.clone(),
        span_text(check.timeout),
        span_text(check.grace),
        state,
    ]
}

/// Seconds the way people say them: "2m", "11m", "1d".
fn span_text(seconds: u64) -> String {
    const DAY: u64 = 86_400;
    match seconds {
        0 => "at once".to_owned(),
        s if s % DAY == 0 => format!("{}d", s / DAY),
        s if s % 3600 == 0 => format!("{}h", s / 3600),
        s if s % 60 == 0 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

pub(in super::super) async fn monitor_test(app: &mut App) -> Result<()> {
    let cfg = Config::load(&*app.env.getenv, &app.config_path)?;
    if cfg.monitor.ping_url.is_empty() && cfg.monitor.quality_url.is_empty() {
        writeln!(
            app.env.stdout,
            "No monitor.ping_url or monitor.quality_url configured."
        )?;
        return Ok(());
    }

    let mut failed = None;
    for (name, url) in [
        ("heartbeat", &cfg.monitor.ping_url),
        ("quality", &cfg.monitor.quality_url),
    ] {
        if url.is_empty() {
            continue;
        }
        let pinger = Pinger::new(url).map_err(|err| {
            anyhow!(
                "monitor.{}: {err}",
                if name == "heartbeat" {
                    "ping_url"
                } else {
                    "quality_url"
                }
            )
        })?;
        let outcome = pinger.test("tamis monitor test").await;
        pinger.close().await;
        match outcome {
            Ok(()) => writeln!(app.env.stdout, "{name}: ok")?,
            Err(err) => {
                writeln!(app.env.stdout, "{name}: FAILED: {err}")?;
                failed = Some(anyhow!("{name}: {err}"));
            }
        }
    }
    failed.map_or(Ok(()), Err)
}
