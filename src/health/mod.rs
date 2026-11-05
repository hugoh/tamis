//! Judges whether the filter is in good shape from what the store holds: how
//! balanced its training is, how it has been deciding and whether it is still
//! deciding at all.

mod checks;
mod collect;
mod daemon;

use std::fmt::Write as _;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::Serialize;

pub use collect::collect;
pub use daemon::{DaemonInfo, load_daemon};

use crate::auth_pin::AuthPin;
use crate::store::{Decision, Stats};

/// The outcome of a single check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
    Skip,
}

impl Status {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
            Self::Skip => "skip",
        }
    }
}

/// How many recent decisions the verdict checks look at.
pub const WINDOW: i64 = 100;

/// Check names.
pub mod name {
    pub const BALANCE: &str = "class balance";
    pub const UNCERTAIN: &str = "uncertain rate";
    pub const CORRECTIONS: &str = "correction rate";
    pub const LAST: &str = "last decision";
    pub const JOURNAL: &str = "journal";
    pub const DATABASE: &str = "database";
    pub const RECENT: &str = "recent verdicts";
    pub const AUTH_ID: &str = "provider auth id";
    pub const WEB: &str = "web interface";
    pub const PASSKEYS: &str = "web passkeys";
    pub const CONFIG: &str = "config file";
    pub const FOLDERS: &str = "folders";
    pub const RULES: &str = "filing rules";
    pub const DAEMON: &str = "daemon";
}

/// Sections group the checks in a report, in this order.
pub mod section {
    pub const CONFIG: &str = "Configuration";
    pub const RULES: &str = "Rules";
    pub const DAEMON: &str = "Daemon";
    pub const TRAINING: &str = "Training";
    pub const DECISIONS: &str = "Decisions";
    pub const WEB: &str = "Web interface";
}

fn section_of(check: &str) -> &'static str {
    match check {
        name::CONFIG | name::FOLDERS | name::AUTH_ID => section::CONFIG,
        name::RULES => section::RULES,
        name::DAEMON => section::DAEMON,
        name::BALANCE | name::DATABASE => section::TRAINING,
        name::UNCERTAIN | name::CORRECTIONS | name::LAST | name::JOURNAL | name::RECENT => {
            section::DECISIONS
        }
        name::WEB | name::PASSKEYS => section::WEB,
        _ => "",
    }
}

/// The result of one health check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Check {
    pub section: String,
    pub name: String,
    pub status: Status,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

impl Check {
    pub(crate) fn new(name: &str, status: Status, detail: impl Into<String>) -> Self {
        Self {
            section: section_of(name).to_owned(),
            name: name.to_owned(),
            status,
            detail: detail.into(),
        }
    }
}

/// The checks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    /// Whether any check failed.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.checks.iter().any(|check| check.status == Status::Fail)
    }

    /// The checks in their sections, in report order: what doctor and the web
    /// Health page both show.
    pub fn sections(&self) -> impl Iterator<Item = (&str, &[Check])> {
        self.checks
            .chunk_by(|a, b| a.section == b.section)
            .map(|checks| (checks[0].section.as_str(), checks))
    }

    /// The checks of how well the filter is learning and deciding that failed:
    /// the training and the decisions, not the setup (configuration,
    /// rules, web interface) or the daemon itself. Warnings are for `doctor`
    /// and never ping.
    #[must_use]
    pub fn quality_failures(&self) -> Vec<&Check> {
        self.checks
            .iter()
            .filter(|check| {
                check.section == section::TRAINING || check.section == section::DECISIONS
            })
            .filter(|check| check.status == Status::Fail)
            .collect()
    }
}

impl Report {
    /// The text of a failing quality ping, one line per problem, or `None`
    /// when the training and the decisions look healthy.
    #[must_use]
    pub fn quality_body(&self) -> Option<String> {
        let problems = self.quality_failures();
        if problems.is_empty() {
            return None;
        }
        let mut body = String::new();
        for check in problems {
            let _ = writeln!(
                body,
                "{} {}: {}",
                check.status.as_str().to_uppercase(),
                check.name,
                check.detail
            );
        }
        Some(body)
    }
}

/// Describes the configuration that loaded.
#[derive(Clone, Debug, Default)]
pub struct ConfigInfo {
    pub path: String,
    pub account: String,
    pub mode: String,
    pub watch: String,
    pub train: String,
    pub state: String,
}

/// What compiling the filing rules found. `script` is empty when none are
/// configured.
#[derive(Clone, Debug, Default)]
pub struct RulesInfo {
    pub script: String,
    pub targets: Vec<String>,
    pub error: Option<String>,
}

/// What the checks know about the web interface.
#[derive(Clone, Debug, Default)]
pub struct Web {
    pub enabled: bool,
    pub listen: String,
    pub origin: String,
    /// Whether something accepted a connection on `listen`: that is `tamis
    /// run`, if it is going.
    pub reachable: bool,
    pub passkeys: usize,
}

/// What the checks look at. `recent` is newest first.
pub struct Input {
    pub stats: Stats,
    pub recent: Vec<Decision>,
    /// How long the journal is kept; zero means forever.
    pub retention: Duration,
    pub auth: AuthPin,
    pub web: Web,
    pub config: ConfigInfo,
    pub rules: RulesInfo,
    pub daemon: DaemonInfo,
    pub now: Timestamp,
    /// The zone times are shown in.
    pub zone: TimeZone,
}

/// Evaluates every check.
#[must_use]
pub fn run(input: &Input) -> Report {
    let judged = checks::judgements(&input.recent);
    let mut all = vec![
        checks::config_file(&input.config),
        checks::folders(&input.config),
        checks::auth_id(&input.auth),
        checks::filing_rules(&input.rules),
        daemon::check(&input.daemon, input.now, &input.zone),
        checks::balance(&input.stats),
        checks::database(&input.stats),
        checks::uncertain_rate(&judged),
        checks::correction_rate(&judged),
        checks::last_decision(&input.recent, input.now),
        checks::journal(&input.stats, input.retention, &input.zone),
        checks::recent_verdicts(&input.recent, &judged),
        checks::web_interface(&input.web),
    ];
    if input.web.enabled {
        all.push(checks::passkeys(input.web.passkeys));
    }
    Report { checks: all }
}

/// The report for a configuration that doesn't load. When the problem is in
/// the filing rules, it is shown under Rules.
#[must_use]
pub fn config_failed(error: &str, in_rules: bool) -> Report {
    let check = if in_rules { name::RULES } else { name::CONFIG };
    Report {
        checks: vec![Check::new(check, Status::Fail, error)],
    }
}

/// A duration the way Go writes them: "47s", "17m0s", "2h46m0s".
pub(crate) fn go_duration(span: Duration) -> String {
    let total = span.as_secs();
    let (hours, minutes, seconds) = (total / 3600, total % 3600 / 60, total % 60);
    if hours > 0 {
        format!("{hours}h{minutes}m{seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m{seconds}s")
    } else {
        format!("{seconds}s")
    }
}

/// How long ago, rounded to the second under a minute and to the minute above.
pub(crate) fn elapsed(span: Duration) -> String {
    let rounded = if span < Duration::from_mins(1) {
        Duration::from_secs(span.as_secs() + u64::from(span.subsec_millis() >= 500))
    } else {
        Duration::from_mins((span.as_secs() + 30) / 60)
    };
    go_duration(rounded)
}

#[cfg(test)]
mod tests;
