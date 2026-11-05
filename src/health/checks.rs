use crate::journal::Verdict;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;

use super::{Check, ConfigInfo, RulesInfo, Status, Web, name};
use crate::engine::{AuthPin, AuthSource};
use crate::store::{Decision, Stats};

/// Fewer than this many of one kind makes the balance a warning.
const MIN_PER_CLASS: i64 = 50;
const MAX_IMBALANCE: i64 = 10;
/// Rates need this many judged decisions to mean anything.
const MIN_SAMPLE: usize = 20;
const MAX_UNCERTAIN: f64 = 0.3;
const MAX_CORRECTIONS: f64 = 0.1;
const STALE_AFTER: Duration = Duration::from_hours(24);

/// Keeps the decisions tamis made on its own.
pub(super) fn judgements(recent: &[Decision]) -> Vec<&Decision> {
    recent
        .iter()
        .filter(|d| d.reason.is_own_judgement())
        .collect()
}

pub(super) fn config_file(info: &ConfigInfo) -> Check {
    Check::new(
        name::CONFIG,
        Status::Ok,
        format!(
            "{}: {}, {} mode; state {}",
            info.path, info.account, info.mode, info.state
        ),
    )
}

pub(super) fn folders(info: &ConfigInfo) -> Check {
    Check::new(
        name::FOLDERS,
        Status::Ok,
        format!("watching [{}], training {}", info.watch, info.train),
    )
}

pub(super) fn filing_rules(rules: &RulesInfo) -> Check {
    if rules.script.is_empty() {
        return Check::new(name::RULES, Status::Skip, "not configured");
    }
    if let Some(error) = &rules.error {
        return Check::new(
            name::RULES,
            Status::Fail,
            format!("{}: {error}", rules.script),
        );
    }
    if rules.targets.is_empty() {
        return Check::new(
            name::RULES,
            Status::Warn,
            format!(
                "{}: no fileinto commands, so nothing is filed",
                rules.script
            ),
        );
    }
    let noun = if rules.targets.len() == 1 {
        "folder"
    } else {
        "folders"
    };
    Check::new(
        name::RULES,
        Status::Ok,
        format!(
            "{}: files into {} {noun}: {}",
            rules.script,
            rules.targets.len(),
            rules.targets.join(", ")
        ),
    )
}

pub(super) fn web_interface(web: &Web) -> Check {
    if !web.enabled {
        Check::new(name::WEB, Status::Skip, "not enabled")
    } else if !web.reachable {
        Check::new(
            name::WEB,
            Status::Warn,
            format!(
                "nothing is listening on {}; is 'tamis run' going?",
                web.listen
            ),
        )
    } else {
        Check::new(
            name::WEB,
            Status::Ok,
            format!("listening on {}, at {}", web.listen, web.origin),
        )
    }
}

pub(super) fn passkeys(count: usize) -> Check {
    if count == 0 {
        Check::new(
            name::PASSKEYS,
            Status::Warn,
            "none enrolled; run 'tamis web enroll'",
        )
    } else {
        Check::new(name::PASSKEYS, Status::Ok, format!("{count} enrolled"))
    }
}

pub(super) fn balance(stats: &Stats) -> Check {
    let detail = format!("{} spam, {} good", stats.spam, stats.good);
    let (fewest, most) = (stats.spam.min(stats.good), stats.spam.max(stats.good));

    if fewest == 0 {
        Check::new(
            name::BALANCE,
            Status::Fail,
            format!("{detail}: it needs examples of both"),
        )
    } else if fewest < MIN_PER_CLASS {
        Check::new(
            name::BALANCE,
            Status::Warn,
            format!("{detail}: fewer than 50 of one kind"),
        )
    } else if most > MAX_IMBALANCE * fewest {
        Check::new(
            name::BALANCE,
            Status::Warn,
            format!("{detail}: more than 10 to 1"),
        )
    } else {
        Check::new(name::BALANCE, Status::Ok, detail)
    }
}

pub(super) fn database(stats: &Stats) -> Check {
    Check::new(name::DATABASE, Status::Ok, stats.database_line())
}

pub(super) fn uncertain_rate(judged: &[&Decision]) -> Check {
    if judged.len() < MIN_SAMPLE {
        return too_few(name::UNCERTAIN, judged.len());
    }
    let uncertain = judged
        .iter()
        .filter(|d| d.verdict == Verdict::Uncertain)
        .count();
    rate(
        name::UNCERTAIN,
        uncertain,
        judged.len(),
        MAX_UNCERTAIN,
        "uncertain",
    )
}

pub(super) fn correction_rate(judged: &[&Decision]) -> Check {
    if judged.len() < MIN_SAMPLE {
        return too_few(name::CORRECTIONS, judged.len());
    }
    rate(
        name::CORRECTIONS,
        corrected_count(judged.iter().copied()),
        judged.len(),
        MAX_CORRECTIONS,
        "corrected",
    )
}

/// How many of the decisions were later corrected.
fn corrected_count<'a>(decisions: impl Iterator<Item = &'a Decision>) -> usize {
    decisions.filter(|d| d.corrected_by > 0).count()
}

fn too_few(check: &str, judged: usize) -> Check {
    Check::new(
        check,
        Status::Skip,
        format!("{judged} judged decisions, need {MIN_SAMPLE}"),
    )
}

#[allow(clippy::cast_precision_loss, reason = "counts of at most a hundred")]
fn rate(check: &str, count: usize, total: usize, limit: f64, what: &str) -> Check {
    let share = count as f64 / total as f64;
    let detail = format!(
        "{count} of the last {total} judged ({:.0}%) {what}",
        share * 100.0
    );
    Check::new(
        check,
        if share > limit {
            Status::Warn
        } else {
            Status::Ok
        },
        detail,
    )
}

pub(super) fn last_decision(recent: &[Decision], now: Timestamp) -> Check {
    let Some(newest) = recent.first() else {
        return Check::new(
            name::LAST,
            Status::Warn,
            "none yet; has 'tamis run' ever run?",
        );
    };

    let age = Duration::from_secs(now.duration_since(newest.at).as_secs().unsigned_abs());
    let shown = Duration::from_mins((age.as_secs() + 30) / 60);
    if age > STALE_AFTER {
        Check::new(
            name::LAST,
            Status::Warn,
            format!("{} ago; is 'tamis run' running?", super::go_duration(shown)),
        )
    } else {
        Check::new(
            name::LAST,
            Status::Ok,
            format!("{} ago", super::go_duration(shown)),
        )
    }
}

pub(super) fn auth_id(pin: &AuthPin) -> Check {
    match pin.source {
        AuthSource::Stale => Check::new(
            name::AUTH_ID,
            Status::Warn,
            format!(
                "learnt for {} but the account now uses {}, so it is not used; run 'tamis bootstrap'",
                pin.learnt_for, pin.host
            ),
        ),
        AuthSource::Unset => Check::new(
            name::AUTH_ID,
            Status::Skip,
            "not learnt; only the topmost header counts (run 'tamis bootstrap' to learn it)",
        ),
        AuthSource::Off => Check::new(
            name::AUTH_ID,
            Status::Ok,
            "off: only the topmost header counts",
        ),
        AuthSource::Configured => Check::new(
            name::AUTH_ID,
            Status::Ok,
            format!("{} (configured)", pin.id),
        ),
        AuthSource::Learnt => Check::new(name::AUTH_ID, Status::Ok, format!("{} (learnt)", pin.id)),
    }
}

pub(super) fn journal(stats: &Stats, retention: Duration, zone: &TimeZone) -> Check {
    let Some(oldest) = stats.oldest_decision.filter(|_| stats.decisions > 0) else {
        return Check::new(name::JOURNAL, Status::Skip, "empty");
    };

    let kept = if retention.is_zero() {
        "kept forever".to_owned()
    } else {
        format!("kept {} days", retention.as_secs() / 86_400)
    };
    let since = oldest.to_zoned(zone.clone()).strftime("%Y-%m-%d");
    Check::new(
        name::JOURNAL,
        Status::Ok,
        format!("{} decisions since {since}, {kept}", stats.decisions),
    )
}

pub(super) fn recent_verdicts(recent: &[Decision], judged: &[&Decision]) -> Check {
    if recent.is_empty() {
        return Check::new(name::RECENT, Status::Skip, "no decisions yet");
    }
    let count = |verdict: Verdict| judged.iter().filter(|d| d.verdict == verdict).count();
    Check::new(
        name::RECENT,
        Status::Ok,
        format!(
            "last {}: {} good, {} spam, {} uncertain; {} corrected",
            recent.len(),
            count(Verdict::Good),
            count(Verdict::Spam),
            count(Verdict::Uncertain),
            corrected_count(judged.iter().copied()),
        ),
    )
}
