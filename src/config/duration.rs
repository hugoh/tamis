//! Durations written as strings such as "2m" or "720h30m".

use std::time::Duration;

use serde::{Deserialize, Deserializer, de};

/// # Errors
/// Returns a message saying what is wrong with `text`.
pub fn parse(text: &str) -> Result<Duration, String> {
    humantime::parse_duration(text).map_err(|err| format!("duration {text:?}: {err}"))
}

/// For `#[serde(deserialize_with = "...")]`.
///
/// # Errors
/// Fails when the value is not a duration string.
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    let text = String::deserialize(deserializer)?;
    parse(&text).map_err(de::Error::custom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_duration_strings() {
        let cases = [
            ("0", Duration::ZERO),
            ("0s", Duration::ZERO),
            ("2m", Duration::from_secs(120)),
            ("90s", Duration::from_secs(90)),
            ("1.5h", Duration::from_mins(90)),
            ("720h30m", Duration::from_mins(43230)),
            ("300ms", Duration::from_millis(300)),
            ("1h2m3s", Duration::from_secs(3723)),
        ];
        for (text, want) in cases {
            assert_eq!(parse(text), Ok(want), "{text}");
        }
    }

    #[test]
    fn refuses_malformed_and_negative_durations() {
        for text in [
            "",
            "soon",
            "5",
            "1x",
            "h",
            "-5s",
            "1.s5",
            "99999999999999999999h",
        ] {
            assert!(parse(text).is_err(), "{text:?}");
        }
    }
}
