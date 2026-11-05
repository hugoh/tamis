//! `doctor` and `monitor`: what the daemon and the filter are doing.

use std::io::Write;
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde::Serialize;

use super::super::{App, Reported};
use super::common::{open, palette};
use crate::config::Config;
use crate::health::{self, Report, Status};
use crate::monitor::{self, Check as MonitorCheck, Inputs, Pinger};
use crate::output::style::{
    BOLD, BOLD_RED, BOLD_YELLOW, Cell, DIM, GREEN, PLAIN, Style, cell, write_table,
};
use crate::output::{write_json, write_table as write_plain_table};

/// A pass is cut off after this long; the heartbeat's grace allows for it.
const PASS_TIMEOUT: Duration = Duration::from_mins(10);
const MAX_PASS_RETRY: Duration = Duration::from_mins(15);

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

    for (index, (section, checks)) in report.sections().enumerate() {
        if index > 0 {
            rows.push(vec![Vec::new()]);
        }
        rows.push(vec![cell(section, BOLD)]);
        for check in checks {
            let text = if matches!(check.status, Status::Warn | Status::Fail) {
                format!("{:<4}", check.status.as_str().to_uppercase())
            } else {
                check.status.as_str().to_owned()
            };
            rows.push(vec![
                vec![("  ".to_owned(), PLAIN), (text, status_style(check.status))],
                cell(&check.name, PLAIN),
                cell(&check.detail, DIM),
            ]);
        }
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
