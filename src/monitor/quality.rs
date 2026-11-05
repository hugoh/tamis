use std::time::Duration;

use jiff::Timestamp;

use crate::store::{self, Store};

/// How often the quality check is pinged.
const QUALITY_EVERY: Duration = Duration::from_hours(24);
/// The store key for when the quality check is next due.
const QUALITY_NEXT: &str = "quality_next";

/// Says whether the quality check is due at `now`, and if so books the next
/// one a day later. The schedule is kept in the state database, so restarting
/// the daemon neither pings again at once nor moves the daily ping, which the
/// service expects about once a day.
///
/// # Errors
/// Fails when the store can't be read or written.
pub fn take_quality_slot(store: &Store, now: Timestamp) -> Result<bool, store::Error> {
    let due = store
        .setting(QUALITY_NEXT)?
        .and_then(|saved| saved.parse::<i64>().ok())
        .and_then(|seconds| Timestamp::from_second(seconds).ok());
    if due.is_some_and(|due| now < due) {
        return Ok(false);
    }

    let next = now.checked_add(QUALITY_EVERY).unwrap_or(now);
    store.set_setting(QUALITY_NEXT, &next.as_second().to_string())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).unwrap()
    }

    #[test]
    fn the_check_is_due_once_a_day() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state.db")).unwrap();

        assert!(
            take_quality_slot(&store, at(1_000)).unwrap(),
            "never pinged: due"
        );
        assert!(!take_quality_slot(&store, at(1_000 + 3_600)).unwrap());
        assert!(!take_quality_slot(&store, at(1_000 + 86_399)).unwrap());
        assert!(take_quality_slot(&store, at(1_000 + 86_400)).unwrap());
    }

    #[test]
    fn a_restart_does_not_move_the_schedule() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");

        assert!(take_quality_slot(&Store::open(&path).unwrap(), at(1_000)).unwrap());

        let restarted = Store::open(&path).unwrap();
        assert!(
            !take_quality_slot(&restarted, at(1_000 + 600)).unwrap(),
            "a restart is not a reason to ping"
        );
    }

    #[test]
    fn an_unreadable_schedule_counts_as_due() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        store.set_setting(QUALITY_NEXT, "garbage").unwrap();

        assert!(take_quality_slot(&store, at(5)).unwrap());
    }
}
