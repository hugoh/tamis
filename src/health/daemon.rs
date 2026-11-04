use std::fmt::Write as _;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;

use super::{Check, Status, elapsed, name};
use crate::live::{SETTING_KEY, Snapshot};
use crate::store::{self, Store};

/// How long past the time it was due the daemon may stay silent before it is
/// called gone: a pass may take up to ten minutes.
const QUIET_GRACE: Duration = Duration::from_mins(11);

/// What `tamis run` last saved about itself. `saved` is `None` when it has
/// never saved anything.
#[derive(Clone, Debug)]
pub struct DaemonInfo {
    pub saved: Option<Snapshot>,
    /// `policy.poll_interval`: how often the daemon should report.
    pub poll: Duration,
}

impl DaemonInfo {
    /// Judges how the daemon is doing and says so in a line.
    #[must_use]
    pub fn describe(&self, now: Timestamp, zone: &TimeZone) -> (Status, String) {
        let check = check(self, now, zone);
        (check.status, check.detail)
    }
}

/// Reads what the daemon saved in the state database. A record that can't be
/// read counts as none.
///
/// # Errors
/// Fails when the store can't be read.
pub fn load_daemon(store: &Store, poll: Duration) -> Result<DaemonInfo, store::Error> {
    let saved = store
        .setting(SETTING_KEY)?
        .and_then(|text| Snapshot::decode(&text).ok());
    Ok(DaemonInfo { saved, poll })
}

pub(super) fn check(info: &DaemonInfo, now: Timestamp, zone: &TimeZone) -> Check {
    let Some(snap) = &info.saved else {
        return Check::new(
            name::DAEMON,
            Status::Skip,
            "nothing saved yet; has 'tamis run' ever run?",
        );
    };

    let updated = snap.updated.unwrap_or(Timestamp::UNIX_EPOCH);
    let mut due = updated.checked_add(info.poll).unwrap_or(updated);
    if let Some(retry_at) = snap.retry_at.filter(|_| snap.failures > 0) {
        due = retry_at;
    }

    let clock = |at: Option<Timestamp>| {
        at.map_or_else(
            || "?".to_owned(),
            |at| at.to_zoned(zone.clone()).strftime("%H:%M:%S").to_string(),
        )
    };
    let since_updated = Duration::from_secs(now.duration_since(updated).as_secs().unsigned_abs());

    if now > due.checked_add(QUIET_GRACE).unwrap_or(due) {
        let stamp = updated.to_zoned(zone.clone()).strftime("%Y-%m-%d %H:%M:%S");
        return Check::new(
            name::DAEMON,
            Status::Warn,
            format!(
                "last heard from {} ago ({stamp}); is 'tamis run' going?",
                elapsed(since_updated)
            ),
        );
    }
    if snap.rejected {
        return Check::new(
            name::DAEMON,
            Status::Fail,
            format!(
                "the server rejects the login, since {} ({} tries); check the password, next try {}",
                clock(snap.failing_since),
                snap.failures,
                clock(snap.retry_at),
            ),
        );
    }
    if snap.failures > 0 {
        return Check::new(
            name::DAEMON,
            Status::Warn,
            format!(
                "{} passes failed in a row since {}: {}; next try {}",
                snap.failures,
                clock(snap.failing_since),
                snap.last_error,
                clock(snap.retry_at),
            ),
        );
    }
    if snap.passes == 0 {
        return Check::new(
            name::DAEMON,
            Status::Ok,
            format!("running since {}, no pass yet", clock(snap.started)),
        );
    }

    let since_pass = snap.last_pass.map_or(since_updated, |at| {
        Duration::from_secs(now.duration_since(at).as_secs().unsigned_abs())
    });
    let mut detail = format!(
        "last pass {} ago: {}",
        elapsed(since_pass),
        snap.last_summary
    );
    if let Some(recovered) = snap.recovered_at {
        let ago = Duration::from_secs(now.duration_since(recovered).as_secs().unsigned_abs());
        let _ = write!(
            detail,
            "; recovered {} ago from a failure at {}",
            elapsed(ago),
            clock(snap.last_error_at)
        );
    }
    Check::new(name::DAEMON, Status::Ok, detail)
}
