//! Paces the daemon's reconnection attempts: quick at first, then slower and
//! slower, so an outage is noticed promptly but not hammered.

use std::time::{Duration, SystemTime};

use backon::{BackoffBuilder as _, ExponentialBackoff, ExponentialBuilder};
use tracing::Level;

/// Logs an event at a level chosen at run time, such as one from [`level`].
macro_rules! log_at {
    ($level:expr, $($rest:tt)*) => {
        match $level {
            tracing::Level::ERROR => tracing::error!($($rest)*),
            tracing::Level::WARN => tracing::warn!($($rest)*),
            tracing::Level::INFO => tracing::info!($($rest)*),
            tracing::Level::DEBUG => tracing::debug!($($rest)*),
            _ => tracing::trace!($($rest)*),
        }
    };
}
pub(crate) use log_at;

/// Spaces the reminders once a streak is long.
const REMINDER_EVERY: u32 = 20;
/// Spaces the reminders of a rejected login: with the usual 15 minutes
/// between tries, it is hourly.
const REJECTION_EVERY: u32 = 4;

/// A sequence of delays that starts at `first`, doubles on each call to
/// [`Backoff::next_delay`] up to `limit`, and adds a random share of each
/// delay so that several connections don't retry in step. It never gives up;
/// [`Backoff::reset`] starts it over after a success. The limit is raised to
/// `first` when smaller.
#[derive(Debug)]
pub struct Backoff {
    plan: ExponentialBuilder,
    delays: ExponentialBackoff,
    nominal: ExponentialBackoff,
}

impl Backoff {
    #[must_use]
    pub fn new(first: Duration, limit: Duration) -> Self {
        let plan = ExponentialBuilder::default()
            .with_min_delay(first)
            .with_max_delay(first.max(limit))
            .without_max_times();
        Self {
            delays: plan.with_jitter().build(),
            nominal: plan.build(),
            plan,
        }
    }

    pub fn next_delay(&mut self) -> Duration {
        self.delays.next().unwrap_or(Duration::MAX)
    }

    /// The next delay without jitter: for working out when things would happen,
    /// and for a lone sender with nobody to retry in step with.
    pub fn next_nominal(&mut self) -> Duration {
        self.nominal.next().unwrap_or(Duration::MAX)
    }

    pub fn reset(&mut self) {
        self.delays = self.plan.with_jitter().build();
        self.nominal = self.plan.build();
    }
}

/// Counts consecutive failures of one thing, for logging the start of a
/// trouble, a few reminders while it lasts, and its end.
#[derive(Clone, Copy, Debug, Default)]
pub struct Streak {
    failures: u32,
    since: Option<SystemTime>,
}

impl Streak {
    /// Records a failure at `now` and returns how many have happened in a row.
    pub fn fail(&mut self, now: SystemTime) -> u32 {
        if self.failures == 0 {
            self.since = Some(now);
        }
        self.failures += 1;
        self.failures
    }

    /// How many failures there have been in a row.
    #[must_use]
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// When the current streak began; `None` when there is none.
    #[must_use]
    pub fn since(&self) -> Option<SystemTime> {
        self.since
    }

    /// Ends the streak at `now`. Returns how many failures there were and for
    /// how long, or `None` when there was no streak to end.
    pub fn recover(&mut self, now: SystemTime) -> Option<(u32, Duration)> {
        let since = self.since.filter(|_| self.failures > 0)?;
        let failures = self.failures;
        *self = Self::default();

        let downtime = now.duration_since(since).unwrap_or_default();
        Some((
            failures,
            Duration::from_secs(downtime.as_secs() + u64::from(downtime.subsec_millis() >= 500)),
        ))
    }
}

/// Says whether the nth failure in a row deserves a log line above debug: the
/// first few, then ever more rarely, so that a long outage leaves a handful of
/// lines.
#[must_use]
pub fn notable(failures: u32) -> bool {
    matches!(failures, 1 | 2 | 3 | 5 | 10)
        || (failures > 0 && failures.is_multiple_of(REMINDER_EVERY))
}

/// How loudly to log the nth failure in a row: `first` for the first, a
/// warning for the notable ones after it, debug for the rest.
#[must_use]
pub fn level(failures: u32, first: Level) -> Level {
    if failures == 1 {
        first
    } else if notable(failures) {
        Level::WARN
    } else {
        Level::DEBUG
    }
}

/// How loudly to log the nth rejected login in a row. Nothing changes between
/// attempts, so it is an error once, then a warning now and then.
#[must_use]
pub fn rejected_level(failures: u32) -> Level {
    if failures == 1 {
        Level::ERROR
    } else if failures.is_multiple_of(REJECTION_EVERY) {
        Level::WARN
    } else {
        Level::DEBUG
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn backoff_grows_to_its_limit_and_resets() {
        let mut backoff = Backoff::new(SECOND, 10 * SECOND);

        let mut last = Duration::ZERO;
        for _ in 0..10 {
            last = backoff.next_delay();
            assert!(
                last <= 20 * SECOND,
                "the limit, plus up to as much again in jitter"
            );
        }
        assert!(last >= 10 * SECOND, "it settles at the limit or above");

        backoff.reset();
        assert!(
            (SECOND..=2 * SECOND).contains(&backoff.next_delay()),
            "a success starts it over"
        );
    }

    #[test]
    fn backoff_limit_never_below_first() {
        for _ in 0..50 {
            let mut backoff = Backoff::new(Duration::from_secs(60), SECOND);
            assert!(backoff.next_delay() >= Duration::from_secs(60));
            assert!(backoff.next_delay() >= Duration::from_secs(60));
        }
    }

    #[test]
    fn nominal_delays_double_to_the_limit_without_jitter() {
        let mut backoff = Backoff::new(SECOND, 5 * SECOND);

        let delays: Vec<_> = (0..5).map(|_| backoff.next_nominal()).collect();

        assert_eq!(delays, [1, 2, 4, 5, 5].map(Duration::from_secs),);
    }

    #[test]
    fn streak_tracks_failures_until_recovery() {
        let mut streak = Streak::default();

        assert_eq!(streak.recover(at(0)), None, "nothing to recover from");
        assert_eq!(streak.since(), None);

        assert_eq!(streak.fail(at(0)), 1);
        assert_eq!(streak.fail(at(60)), 2);
        assert_eq!(streak.failures(), 2);
        assert_eq!(streak.since(), Some(at(0)));

        assert_eq!(streak.recover(at(600)), Some((2, Duration::from_secs(600))));
        assert_eq!(streak.fail(at(3600)), 1, "a new streak starts over");
    }

    #[test]
    fn notable_failures_thin_out() {
        let got: Vec<u32> = (1..=41).filter(|n| notable(*n)).collect();
        assert_eq!(got, [1, 2, 3, 5, 10, 20, 40]);
    }

    #[test]
    fn levels_thin_out() {
        let cases = [
            (level(1, Level::ERROR), Level::ERROR),
            (level(2, Level::ERROR), Level::WARN),
            (level(4, Level::ERROR), Level::DEBUG),
            (rejected_level(1), Level::ERROR),
            (rejected_level(3), Level::DEBUG),
            (rejected_level(4), Level::WARN),
        ];
        for (got, want) in cases {
            assert_eq!(got, want);
        }
    }
}
