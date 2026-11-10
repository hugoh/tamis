use std::time::Duration;

use serde::Serialize;

use crate::retry::Backoff;

const DAY: Duration = Duration::from_hours(24);
/// Lets the daily quality ping be late, as after a restart.
const QUALITY_GRACE: Duration = Duration::from_hours(2);
/// The shortest period healthchecks.io accepts.
const MIN_PERIOD: Duration = Duration::from_mins(1);
/// Added to the longest a pass may take, for the settle wait and the network.
const PASS_SLACK: Duration = Duration::from_mins(1);

/// How to set up one check on the service. The field names are those of the
/// healthchecks.io API, in seconds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Check {
    pub name: String,
    /// The period: how long the service waits for a ping.
    pub timeout: u64,
    /// How much longer it waits before alerting.
    pub grace: u64,
}

/// How soon the service alerts for one kind of trouble.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Alert {
    pub when: String,
    #[serde(rename = "afterSeconds")]
    pub after_seconds: u64,
}

/// The checks to create on the service, and when each kind of trouble would
/// alert.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Settings {
    pub heartbeat: Check,
    pub quality: Check,
    pub alerts: Vec<Alert>,
}

/// What the settings are worked out from.
#[derive(Clone, Copy, Debug)]
pub struct Inputs {
    /// `policy.poll_interval`: a healthy daemon passes at least this often.
    pub poll: Duration,
    /// `monitor.fail_after`.
    pub fail_after: u32,
    /// The longest a pass may take before it is cut short.
    pub pass_timeout: Duration,
    /// The longest wait after failed passes.
    pub max_retry: Duration,
}

/// Works out the checks' settings and when they would alert.
#[must_use]
pub fn recommend(inputs: &Inputs) -> Settings {
    let period =
        Duration::from_secs(inputs.poll.as_secs() + u64::from(inputs.poll.subsec_millis() >= 500))
            .max(MIN_PERIOD);
    let grace = inputs.pass_timeout + PASS_SLACK;

    Settings {
        heartbeat: Check {
            name: "tamis heartbeat".to_owned(),
            timeout: period.as_secs(),
            grace: grace.as_secs(),
        },
        quality: Check {
            name: "tamis quality".to_owned(),
            timeout: DAY.as_secs(),
            grace: QUALITY_GRACE.as_secs(),
        },
        alerts: vec![
            alert(
                "the daemon stops, hangs or its host goes down",
                period + grace,
            ),
            alert(
                &format!(
                    "the mail server is unreachable (fail_after = {})",
                    inputs.fail_after
                ),
                failure_wait(inputs),
            ),
            alert(
                &format!(
                    "the login is rejected (fail_after = {})",
                    inputs.fail_after
                ),
                failure_wait(inputs),
            ),
            alert("the filter's training or decisions look unhealthy", DAY),
        ],
    }
}

fn alert(when: &str, after: Duration) -> Alert {
    Alert {
        when: when.to_owned(),
        after_seconds: after.as_secs(),
    }
}

/// About how long the `fail_after`-th failed pass in a row comes after the
/// first, adding the waits between them.
fn failure_wait(inputs: &Inputs) -> Duration {
    let mut pace = Backoff::new(inputs.poll, inputs.max_retry);
    (1..inputs.fail_after).map(|_| pace.next_nominal()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> Inputs {
        Inputs {
            poll: Duration::from_mins(2),
            fail_after: 3,
            pass_timeout: Duration::from_mins(10),
            max_retry: Duration::from_mins(15),
        }
    }

    #[test]
    fn the_checks_follow_the_poll_interval_and_pass_timeout() {
        let settings = recommend(&inputs());

        assert_eq!(
            (settings.heartbeat.timeout, settings.heartbeat.grace),
            (120, 660)
        );
        assert_eq!(
            (settings.quality.timeout, settings.quality.grace),
            (86_400, 7_200)
        );
    }

    #[test]
    fn each_kind_of_trouble_has_its_alert_time() {
        let alerts: Vec<u64> = recommend(&inputs())
            .alerts
            .iter()
            .map(|a| a.after_seconds)
            .collect();

        // stops: period + grace; unreachable: the waits before the 3rd failure
        // (2m + 4m); login: the same; quality: a day.
        assert_eq!(alerts, [780, 360, 360, 86_400]);
    }

    #[test]
    fn short_poll_intervals_are_raised_to_the_minimum_period() {
        let settings = recommend(&Inputs {
            poll: Duration::from_secs(10),
            ..inputs()
        });

        assert_eq!(settings.heartbeat.timeout, 60);
    }

    #[test]
    fn waits_stop_growing_at_the_retry_limit() {
        let inputs = Inputs {
            fail_after: 6,
            max_retry: Duration::from_mins(5),
            ..inputs()
        };

        // 2m, 4m, 5m, 5m, 5m
        assert_eq!(failure_wait(&inputs), Duration::from_mins(21));
    }
}
