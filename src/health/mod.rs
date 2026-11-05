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

use crate::engine::AuthPin;
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
    pub const RECENT: &str = "recent verdicts";
    pub const AUTH_ID: &str = "provider auth id";
    pub const WEB: &str = "web interface";
    pub const PASSKEYS: &str = "web passkeys";
    pub const CONFIG: &str = "config file";
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
        name::CONFIG | name::AUTH_ID => section::CONFIG,
        name::RULES => section::RULES,
        name::DAEMON => section::DAEMON,
        name::BALANCE => section::TRAINING,
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

    /// The checks of how well the filter is learning and deciding that failed:
    /// the training and the decisions, not the setup (configuration,
    /// rules, web interface) or the daemon itself.
    #[must_use]
    pub fn quality_problems(&self) -> Vec<&Check> {
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
        let problems = self.quality_problems();
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
        checks::auth_id(&input.auth),
        checks::filing_rules(&input.rules),
        daemon::check(&input.daemon, input.now, &input.zone),
        checks::balance(&input.stats),
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
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::engine::AuthSource;
    use crate::live::{Snapshot, Waiting};

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).unwrap()
    }

    /// 2026-11-03 13:02:00 UTC.
    const NOW: i64 = 1_793_710_920;

    fn decision(verdict: &str, reason: &str, corrected_by: i64, age_minutes: i64) -> Decision {
        Decision {
            at: at(NOW - age_minutes * 60),
            verdict: verdict.parse().unwrap(),
            reason: reason.parse().unwrap(),
            corrected_by,
            ..Decision::default()
        }
    }

    fn input() -> Input {
        Input {
            stats: Stats::default(),
            recent: Vec::new(),
            retention: Duration::from_hours(365 * 24),
            auth: AuthPin {
                id: String::new(),
                source: AuthSource::Unset,
                learnt_for: String::new(),
                host: String::new(),
            },
            web: Web::default(),
            config: ConfigInfo {
                path: "/c.toml".into(),
                account: "me on imap.example.org".into(),
                mode: "live".into(),
            },
            rules: RulesInfo::default(),
            daemon: DaemonInfo {
                saved: None,
                poll: Duration::from_mins(2),
            },
            now: at(NOW),
            zone: TimeZone::UTC,
        }
    }

    fn find<'a>(report: &'a Report, check: &str) -> &'a Check {
        report.checks.iter().find(|c| c.name == check).unwrap()
    }

    #[test]
    fn the_class_balance_needs_both_kinds_in_reasonable_proportion() {
        let cases = [
            (
                0,
                90,
                Status::Fail,
                "0 spam, 90 good: it needs examples of both",
            ),
            (
                30,
                90,
                Status::Warn,
                "30 spam, 90 good: fewer than 50 of one kind",
            ),
            (
                50,
                600,
                Status::Warn,
                "50 spam, 600 good: more than 10 to 1",
            ),
            (460, 1013, Status::Ok, "460 spam, 1013 good"),
        ];
        for (spam, good, status, detail) in cases {
            let mut input = input();
            input.stats = Stats {
                spam,
                good,
                ..Stats::default()
            };

            let report = run(&input);

            let check = find(&report, name::BALANCE);
            assert_eq!((check.status, check.detail.as_str()), (status, detail));
            assert_eq!(check.section, "Training");
        }
    }

    fn judged(uncertain: usize, corrected: usize, total: usize) -> Vec<Decision> {
        (0..total)
            .map(|n| {
                let verdict = if n < uncertain { "uncertain" } else { "good" };
                decision(verdict, "bayes", i64::from(n < corrected) * 7, 5)
            })
            .collect()
    }

    #[test]
    fn the_rates_need_a_sample_and_a_limit() {
        let mut few = input();
        few.recent = judged(5, 5, 10);
        let mut fine = input();
        fine.recent = judged(5, 2, 78);
        let mut busy = input();
        busy.recent = judged(30, 11, 78);

        let few = run(&few);
        assert_eq!(find(&few, name::UNCERTAIN).status, Status::Skip);
        assert_eq!(
            find(&few, name::CORRECTIONS).detail,
            "10 judged decisions, need 20"
        );

        let fine = run(&fine);
        assert_eq!(
            find(&fine, name::UNCERTAIN).detail,
            "5 of the last 78 judged (6%) uncertain"
        );
        assert_eq!(find(&fine, name::CORRECTIONS).status, Status::Ok);

        let busy = run(&busy);
        assert_eq!(find(&busy, name::UNCERTAIN).status, Status::Warn);
        let corrections = find(&busy, name::CORRECTIONS);
        assert_eq!(
            (corrections.status, corrections.detail.as_str()),
            (Status::Warn, "11 of the last 78 judged (14%) corrected")
        );
    }

    #[test]
    fn only_tamis_own_judgements_count_towards_the_rates() {
        let mut input = input();
        input.recent = judged(0, 0, 25);
        input
            .recent
            .extend((0..40).map(|_| decision("good", "rule", 9, 1)));

        let report = run(&input);

        assert_eq!(
            find(&report, name::CORRECTIONS).detail,
            "0 of the last 25 judged (0%) corrected"
        );
        assert_eq!(
            find(&report, name::RECENT).detail,
            "last 65: 25 good, 0 spam, 0 uncertain; 0 corrected"
        );
    }

    #[test]
    fn the_last_decision_must_be_recent() {
        let cases = [
            (vec![], Status::Warn, "none yet; has 'tamis run' ever run?"),
            (
                vec![decision("good", "bayes", 0, 17)],
                Status::Ok,
                "17m0s ago",
            ),
            (
                vec![decision("good", "bayes", 0, 26 * 60)],
                Status::Warn,
                "26h0m0s ago; is 'tamis run' running?",
            ),
        ];
        for (recent, status, detail) in cases {
            let mut input = input();
            input.recent = recent;

            let report = run(&input);

            let check = find(&report, name::LAST);
            assert_eq!((check.status, check.detail.as_str()), (status, detail));
        }
    }

    #[test]
    fn the_journal_and_auth_id_are_described() {
        let mut input = input();
        input.stats = Stats {
            decisions: 179,
            oldest_decision: Some(at(1_793_000_000)),
            ..Stats::default()
        };
        input.auth = AuthPin {
            id: "icloud.com".into(),
            source: AuthSource::Learnt,
            learnt_for: String::new(),
            host: String::new(),
        };

        let report = run(&input);

        assert_eq!(
            find(&report, name::JOURNAL).detail,
            "179 decisions since 2026-10-26, kept 365 days"
        );
        assert_eq!(find(&report, name::AUTH_ID).detail, "icloud.com (learnt)");

        input.retention = Duration::ZERO;
        input.auth = AuthPin {
            id: String::new(),
            source: AuthSource::Stale,
            learnt_for: "a.example".into(),
            host: "b.example".into(),
        };
        let report = run(&input);
        assert!(
            find(&report, name::JOURNAL)
                .detail
                .ends_with("kept forever")
        );
        assert_eq!(find(&report, name::AUTH_ID).status, Status::Warn);
    }

    #[test]
    fn the_filing_rules_are_described() {
        let cases = [
            (RulesInfo::default(), Status::Skip, "not configured"),
            (
                RulesInfo {
                    script: "r.sieve".into(),
                    error: Some("parse rules: bad".into()),
                    ..RulesInfo::default()
                },
                Status::Fail,
                "r.sieve: parse rules: bad",
            ),
            (
                RulesInfo {
                    script: "r.sieve".into(),
                    ..RulesInfo::default()
                },
                Status::Warn,
                "r.sieve: no fileinto commands, so nothing is filed",
            ),
            (
                RulesInfo {
                    script: "r.sieve".into(),
                    targets: vec!["A".into()],
                    error: None,
                },
                Status::Ok,
                "r.sieve: files into 1 folder: A",
            ),
            (
                RulesInfo {
                    script: "r.sieve".into(),
                    targets: vec!["A".into(), "B".into()],
                    error: None,
                },
                Status::Ok,
                "r.sieve: files into 2 folders: A, B",
            ),
        ];
        for (rules, status, detail) in cases {
            let mut input = input();
            input.rules = rules;

            let report = run(&input);

            let check = find(&report, name::RULES);
            assert_eq!((check.status, check.detail.as_str()), (status, detail));
        }
    }

    #[test]
    fn the_web_interface_and_its_passkeys_are_described_when_enabled() {
        let mut off = input();
        let report = run(&off);
        assert_eq!(find(&report, name::WEB).status, Status::Skip);
        assert!(report.checks.iter().all(|c| c.name != name::PASSKEYS));

        off.web = Web {
            enabled: true,
            listen: "127.0.0.1:8484".into(),
            origin: "https://t.example".into(),
            reachable: false,
            passkeys: 0,
        };
        let report = run(&off);
        assert_eq!(
            find(&report, name::WEB).detail,
            "nothing is listening on 127.0.0.1:8484; is 'tamis run' going?"
        );
        assert_eq!(
            find(&report, name::PASSKEYS).detail,
            "none enrolled; run 'tamis web enroll'"
        );

        off.web.reachable = true;
        off.web.passkeys = 3;
        let report = run(&off);
        assert_eq!(
            find(&report, name::WEB).detail,
            "listening on 127.0.0.1:8484, at https://t.example"
        );
        assert_eq!(find(&report, name::PASSKEYS).detail, "3 enrolled");
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            started: Some(at(NOW - 86_400)),
            passes: 785,
            last_pass: Some(at(NOW - 47)),
            last_summary: "scanned 0, spam 0".into(),
            updated: Some(at(NOW - 47)),
            waiting: Some(Waiting::Idle),
            ..Snapshot::default()
        }
    }

    fn daemon_check(snapshot: Option<Snapshot>) -> Check {
        let mut input = input();
        input.daemon.saved = snapshot;
        find(&run(&input), name::DAEMON).clone()
    }

    #[test]
    fn the_daemon_is_judged_from_what_it_saved() {
        let none = daemon_check(None);
        assert_eq!(
            (none.status, none.detail.as_str()),
            (Status::Skip, "nothing saved yet; has 'tamis run' ever run?")
        );

        let healthy = daemon_check(Some(snapshot()));
        assert_eq!(
            (healthy.status, healthy.detail.as_str()),
            (Status::Ok, "last pass 47s ago: scanned 0, spam 0")
        );

        let recovered = daemon_check(Some(Snapshot {
            recovered_at: Some(at(NOW - 2 * 3600 - 46 * 60)),
            last_error_at: Some(at(1_793_700_909)),
            ..snapshot()
        }));
        assert_eq!(
            recovered.detail,
            "last pass 47s ago: scanned 0, spam 0; recovered 2h46m0s ago from a failure at 10:15:09"
        );

        let fresh = daemon_check(Some(Snapshot {
            passes: 0,
            ..snapshot()
        }));
        assert!(
            fresh
                .detail
                .starts_with("running since 13:02:00, no pass yet"),
            "{}",
            fresh.detail
        );
    }

    #[test]
    fn a_daemon_in_trouble_or_gone_is_flagged() {
        let failing = daemon_check(Some(Snapshot {
            failures: 3,
            failing_since: Some(at(NOW - 600)),
            retry_at: Some(at(NOW + 120)),
            last_error: "connection: down".into(),
            ..snapshot()
        }));
        assert_eq!(failing.status, Status::Warn);
        assert_eq!(
            failing.detail,
            "3 passes failed in a row since 12:52:00: connection: down; next try 13:04:00"
        );

        let rejected = daemon_check(Some(Snapshot {
            failures: 2,
            rejected: true,
            failing_since: Some(at(NOW - 600)),
            retry_at: Some(at(NOW + 600)),
            ..snapshot()
        }));
        assert_eq!(rejected.status, Status::Fail);
        assert!(
            rejected
                .detail
                .starts_with("the server rejects the login, since 12:52:00 (2 tries)"),
            "{}",
            rejected.detail
        );

        let gone = daemon_check(Some(Snapshot {
            updated: Some(at(NOW - 3 * 3600)),
            ..snapshot()
        }));
        assert_eq!(gone.status, Status::Warn);
        assert_eq!(
            gone.detail,
            "last heard from 3h0m0s ago (2026-11-03 10:02:00); is 'tamis run' going?"
        );

        let waiting_out_a_retry = daemon_check(Some(Snapshot {
            updated: Some(at(NOW - 3600)),
            failures: 1,
            retry_at: Some(at(NOW + 600)),
            ..snapshot()
        }));
        assert_ne!(
            waiting_out_a_retry.detail.split(' ').next(),
            Some("last"),
            "a long back-off is not silence"
        );
    }

    #[test]
    fn quality_problems_are_the_training_and_decision_failures() {
        let mut input = input();
        input.stats = Stats {
            spam: 0,
            good: 90,
            ..Stats::default()
        };
        input.recent = judged(30, 11, 78);
        input.rules = RulesInfo {
            script: "r".into(),
            error: Some("bad".into()),
            ..RulesInfo::default()
        };

        let report = run(&input);

        let names: Vec<&str> = report
            .quality_problems()
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["class balance"],
            "warnings and the failing rules are not a quality problem"
        );
        assert!(report.failed(), "but they do fail the report");
    }

    #[test]
    fn the_quality_ping_lists_what_is_wrong() {
        let mut healthy = input();
        healthy.stats = Stats {
            spam: 460,
            good: 1013,
            ..Stats::default()
        };
        healthy.recent = judged(5, 2, 78);
        assert_eq!(run(&healthy).quality_body(), None);

        let mut input = input();
        input.stats = Stats {
            spam: 460,
            good: 1013,
            ..Stats::default()
        };
        input.recent = judged(5, 11, 78);

        assert_eq!(
            run(&input).quality_body(),
            None,
            "a warning is for doctor, not for the ping"
        );

        input.stats = Stats {
            spam: 0,
            good: 1013,
            ..Stats::default()
        };
        assert_eq!(
            run(&input).quality_body().as_deref(),
            Some("FAIL class balance: 0 spam, 1013 good: it needs examples of both\n")
        );
    }

    #[test]
    fn a_configuration_that_does_not_load_has_its_own_report() {
        let config = config_failed("invalid config: x", false);
        let rules = config_failed("rules.script: bad", true);

        assert_eq!(
            (
                config.checks[0].name.as_str(),
                config.checks[0].section.as_str()
            ),
            ("config file", "Configuration")
        );
        assert_eq!(rules.checks[0].section, "Rules");
        assert!(config.failed());
    }

    #[test]
    fn the_report_serializes_as_the_go_one_did() {
        let report = Report {
            checks: vec![
                Check::new(name::JOURNAL, Status::Skip, ""),
                Check::new(name::LAST, Status::Warn, "x"),
            ],
        };

        assert_eq!(
            serde_json::to_string(&report).unwrap(),
            r#"{"checks":[{"section":"Decisions","name":"journal","status":"skip"},{"section":"Decisions","name":"last decision","status":"warn","detail":"x"}]}"#
        );
    }

    #[test]
    fn durations_read_like_gos() {
        let cases = [
            (Duration::from_secs(0), "0s"),
            (Duration::from_millis(47_400), "47s"),
            (Duration::from_millis(47_600), "48s"),
            (Duration::from_mins(17), "17m0s"),
            (Duration::from_secs(2 * 3600 + 46 * 60 + 29), "2h46m0s"),
            (Duration::from_secs(2 * 3600 + 46 * 60 + 31), "2h47m0s"),
        ];
        for (span, want) in cases {
            assert_eq!(elapsed(span), want, "{span:?}");
        }
    }

    #[test]
    fn collecting_reads_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(&dir.path().join("state.db")).unwrap();
        let cfg = Config {
            account: crate::config::Account {
                host: "h".into(),
                user: "u".into(),
                ..crate::config::Account::default()
            },
            ..Config::default()
        };
        store
            .record_decision(&Decision {
                at: Timestamp::now(),
                reason: crate::journal::Reason::Bayes,
                verdict: crate::journal::Verdict::Good,
                ..Decision::default()
            })
            .unwrap();
        store
            .set_setting(
                crate::live::SETTING_KEY,
                &Snapshot {
                    passes: 1,
                    ..Snapshot::default()
                }
                .encode(),
            )
            .unwrap();

        let report = collect(&store, &cfg).unwrap();

        assert_eq!(report.checks.len(), 11);
        assert_eq!(find(&report, name::LAST).status, Status::Ok);
        assert_eq!(find(&report, name::WEB).status, Status::Skip);
        assert_eq!(
            load_daemon(&store, Duration::from_mins(2))
                .unwrap()
                .saved
                .map(|s| s.passes),
            Some(1)
        );
        store
            .set_setting(crate::live::SETTING_KEY, "garbage")
            .unwrap();
        assert!(
            load_daemon(&store, Duration::ZERO).unwrap().saved.is_none(),
            "a record that can't be read counts as none"
        );
    }

    #[test]
    fn collecting_reports_whether_the_web_interface_answers() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(&dir.path().join("state.db")).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut cfg = Config::default();
        cfg.web.enabled = true;
        cfg.web.listen = listener.local_addr().unwrap().to_string();

        let up = collect(&store, &cfg).unwrap();
        assert_eq!(find(&up, name::WEB).status, Status::Ok);

        drop(listener);
        let down = collect(&store, &cfg).unwrap();
        assert_eq!(find(&down, name::WEB).status, Status::Warn);
    }
}
