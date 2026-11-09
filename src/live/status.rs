use std::sync::{Mutex, PoisonError};

use jiff::Timestamp;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Where the daemon saves its [`Snapshot`] in the state database, for the
/// commands that run in another process to read.
pub const SETTING_KEY: &str = "daemon_status";

/// The timestamp a file written before `null` meant "never" used for it.
const OLD_NEVER_PREFIX: &str = "0001-01-01";

mod time_or_never {
    use super::{Deserialize, Deserializer, OLD_NEVER_PREFIX, Serializer, Timestamp};

    #[allow(clippy::ref_option, reason = "the signature serde's `with` needs")]
    pub fn serialize<S: Serializer>(
        value: &Option<Timestamp>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(at) => serializer.serialize_str(&at.to_string()),
            None => serializer.serialize_none(),
        }
    }

    /// Reads `null` or an absent field as never. An empty string or the zero
    /// time, which earlier files wrote for never, still reads as never.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Timestamp>, D::Error> {
        let Some(text) = Option::<String>::deserialize(deserializer)? else {
            return Ok(None);
        };
        if text.is_empty() || text.starts_with(OLD_NEVER_PREFIX) {
            return Ok(None);
        }
        text.parse().map(Some).map_err(serde::de::Error::custom)
    }
}

/// A copy of the status at one moment, as the daemon saves it for the CLI and
/// web interface to read.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Snapshot {
    #[serde(with = "time_or_never")]
    pub started: Option<Timestamp>,
    pub passes: u32,
    #[serde(with = "time_or_never")]
    pub last_pass: Option<Timestamp>,
    pub last_summary: String,
    pub last_error: String,
    #[serde(with = "time_or_never")]
    pub last_error_at: Option<Timestamp>,
    /// How many passes have failed in a row; cleared by a pass that succeeds.
    pub failures: u32,
    #[serde(with = "time_or_never")]
    pub failing_since: Option<Timestamp>,
    /// When the next try is due; cleared by a pass that succeeds.
    #[serde(with = "time_or_never")]
    pub retry_at: Option<Timestamp>,
    /// The failures began with a rejected login.
    pub rejected: bool,
    /// When a pass last succeeded after failures.
    #[serde(with = "time_or_never")]
    pub recovered_at: Option<Timestamp>,
    /// When the daemon last saved this, so that a reader can tell a daemon
    /// that has stopped from one that is merely quiet.
    #[serde(with = "time_or_never")]
    pub updated: Option<Timestamp>,
}

impl Snapshot {
    /// The form a snapshot is saved in.
    #[must_use]
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_owned())
    }

    /// Reads what [`Snapshot::encode`] wrote.
    ///
    /// # Errors
    /// When `text` isn't a saved snapshot.
    pub fn decode(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }
}

type Persist = Box<dyn Fn(&Snapshot) + Send + Sync>;

/// The daemon's live state.
pub struct Status {
    inner: Mutex<Inner>,
}

struct Inner {
    snapshot: Snapshot,
    persist: Option<Persist>,
}

impl Status {
    /// A status for a daemon that started at `started`.
    #[must_use]
    pub fn new(started: Timestamp) -> Self {
        Self {
            inner: Mutex::new(Inner {
                snapshot: Snapshot {
                    started: Some(started),
                    ..Snapshot::default()
                },
                persist: None,
            }),
        }
    }

    /// Has `save` called with the new snapshot each time a pass ends or
    /// fails, and on [`Status::save`], so that other processes can see how the
    /// daemon is.
    pub fn persist(&self, save: impl Fn(&Snapshot) + Send + Sync + 'static) {
        self.lock().persist = Some(Box::new(save));
    }

    /// Passes the current state to the function given to [`Status::persist`].
    pub fn save(&self, at: Timestamp) {
        self.update(|snap| snap.updated = Some(at), true);
    }

    /// Records a successful pass.
    pub fn pass_done(&self, when: Timestamp, summary: &str) {
        self.update(
            |snap| {
                snap.passes += 1;
                snap.last_pass = Some(when);
                summary.clone_into(&mut snap.last_summary);
                if snap.failures > 0 {
                    snap.recovered_at = Some(when);
                }
                snap.failures = 0;
                snap.failing_since = None;
                snap.retry_at = None;
                snap.rejected = false;
                snap.updated = Some(when);
            },
            true,
        );
    }

    /// Records a failure of a pass or of waiting for the next one.
    pub fn failed(&self, when: Timestamp, error: &str) {
        self.update(
            |snap| {
                error.clone_into(&mut snap.last_error);
                snap.last_error_at = Some(when);
                if snap.failures == 0 {
                    snap.failing_since = Some(when);
                    snap.rejected = false;
                }
                snap.failures += 1;
                snap.updated = Some(when);
            },
            true,
        );
    }

    /// Records when the next try after a failure is due.
    pub fn retry_at(&self, at: Timestamp) {
        self.update(|snap| snap.retry_at = Some(at), true);
    }

    /// Records that the failure was a rejected login.
    pub fn login_rejected(&self) {
        self.update(|snap| snap.rejected = true, true);
    }

    /// A copy of the current state.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        self.lock().snapshot.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn update(&self, change: impl FnOnce(&mut Snapshot), save: bool) {
        let mut inner = self.lock();
        change(&mut inner.snapshot);
        if save && let Some(persist) = &inner.persist {
            persist(&inner.snapshot);
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).unwrap()
    }

    #[test]
    fn passes_and_failures_are_counted_and_cleared() {
        let status = Status::new(at(100));

        status.failed(at(200), "boom");
        status.failed(at(260), "boom again");
        status.retry_at(at(400));
        status.login_rejected();
        let failing = status.snapshot();
        assert_eq!(
            (
                failing.failures,
                failing.failing_since,
                failing.last_error.as_str()
            ),
            (2, Some(at(200)), "boom again")
        );
        assert_eq!(
            (failing.retry_at, failing.rejected, failing.last_error_at),
            (Some(at(400)), true, Some(at(260)))
        );

        status.pass_done(at(500), "scanned 1");
        let healthy = status.snapshot();
        assert_eq!(
            (healthy.passes, healthy.failures, healthy.recovered_at),
            (1, 0, Some(at(500)))
        );
        assert_eq!(
            (healthy.failing_since, healthy.retry_at, healthy.rejected),
            (None, None, false)
        );
        assert_eq!(
            healthy.last_error, "boom again",
            "the last error is kept for the page"
        );

        status.pass_done(at(600), "scanned 0");
        assert_eq!(
            status.snapshot().recovered_at,
            Some(at(500)),
            "only a pass after failures counts as a recovery"
        );
    }

    #[test]
    fn a_new_streak_clears_an_old_rejection() {
        let status = Status::new(at(0));
        status.failed(at(1), "x");
        status.login_rejected();
        status.pass_done(at(2), "ok");

        status.failed(at(3), "network");

        assert!(!status.snapshot().rejected);
    }

    #[test]
    fn snapshots_round_trip_through_their_saved_form() {
        let status = Status::new(at(1_700_000_000));
        status.failed(at(1_700_000_100), "boom");

        let snapshot = status.snapshot();

        assert_eq!(Snapshot::decode(&snapshot.encode()).unwrap(), snapshot);
        assert!(
            snapshot.encode().contains("\"lastPass\":null"),
            "never is written as null"
        );
    }

    #[test]
    fn a_status_saved_with_zero_times_can_still_be_read() {
        let saved = r#"{"started":"2026-11-02T13:01:07.123456789+01:00","passes":785,"lastPass":"2026-11-03T13:02:00Z",
            "lastSummary":"scanned 0","lastError":"","lastErrorAt":"0001-01-01T00:00:00Z","failures":0,
            "failingSince":"0001-01-01T00:00:00Z","retryAt":"0001-01-01T00:00:00Z","rejected":false,
            "recoveredAt":"2026-11-03T10:16:56Z","updated":"2026-11-03T13:02:00Z"}"#;

        let snapshot = Snapshot::decode(saved).unwrap();

        assert_eq!((snapshot.passes, snapshot.failures), (785, 0));
        assert_eq!(snapshot.last_error_at, None);
        assert_eq!(
            snapshot.recovered_at,
            Some("2026-11-03T10:16:56Z".parse().unwrap())
        );
        let nulls = Snapshot::decode(r#"{"passes":3,"lastPass":null,"retryAt":null}"#).unwrap();
        assert_eq!(
            (nulls.passes, nulls.last_pass, nulls.retry_at),
            (3, None, None)
        );
        assert!(Snapshot::decode("not json").is_err());
        assert_eq!(Snapshot::decode("{}").unwrap(), Snapshot::default());
    }
}
