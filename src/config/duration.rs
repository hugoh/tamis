//! Durations written as strings such as "2m" or "720h30m", with the same
//! grammar as Go's `time.ParseDuration`, except that negative values are
//! refused.

use std::time::Duration;

use serde::{Deserialize, Deserializer, de};

const UNITS: [(&str, u128); 7] = [
    ("ns", 1),
    ("us", 1_000),
    ("µs", 1_000),
    ("μs", 1_000),
    ("ms", 1_000_000),
    ("s", 1_000_000_000),
    ("m", 60 * 1_000_000_000),
];
const HOUR_NANOS: u128 = 3_600 * 1_000_000_000;

/// # Errors
/// Returns a message saying what is wrong with `text`.
pub fn parse(text: &str) -> Result<Duration, String> {
    let invalid = || format!("duration {text:?}: invalid");
    let digits = text.strip_prefix('+').unwrap_or(text);

    if digits.starts_with('-') {
        return Err(format!("duration {text:?}: must not be negative"));
    }
    if digits == "0" {
        return Ok(Duration::ZERO);
    }
    if digits.is_empty() {
        return Err(invalid());
    }

    let mut rest = digits;
    let mut total: u128 = 0;
    while !rest.is_empty() {
        let (whole, after) = take_digits(rest);
        let (fraction, after) = match after.strip_prefix('.') {
            Some(tail) => take_digits(tail),
            None => ("", after),
        };
        if whole.is_empty() && fraction.is_empty() {
            return Err(invalid());
        }

        let (unit, after) = take_unit(after)
            .ok_or_else(|| format!("duration {text:?}: missing or unknown unit"))?;
        total = total.saturating_add(scale(whole, fraction, unit).ok_or_else(invalid)?);
        rest = after;
    }

    let seconds = u64::try_from(total / 1_000_000_000).map_err(|_| invalid())?;
    let nanos = u32::try_from(total % 1_000_000_000).map_err(|_| invalid())?;
    Ok(Duration::new(seconds, nanos))
}

fn take_digits(text: &str) -> (&str, &str) {
    let end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    text.split_at(end)
}

fn take_unit(text: &str) -> Option<(u128, &str)> {
    if let Some(rest) = text.strip_prefix('h') {
        return Some((HOUR_NANOS, rest));
    }
    // "ms" must be tried before "m" and "s".
    let mut units = UNITS;
    units.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));
    units
        .iter()
        .find_map(|(name, nanos)| text.strip_prefix(name).map(|rest| (*nanos, rest)))
}

fn scale(whole: &str, fraction: &str, unit: u128) -> Option<u128> {
    let whole: u128 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let mut nanos = whole.checked_mul(unit)?;

    let fraction = &fraction[..fraction.len().min(18)];
    if !fraction.is_empty() {
        let value: u128 = fraction.parse().ok()?;
        let divisor = 10_u128.pow(u32::try_from(fraction.len()).ok()?);
        nanos = nanos.checked_add(value.checked_mul(unit)? / divisor)?;
    }
    Some(nanos)
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
    fn parses_like_go() {
        let cases = [
            ("0", Duration::ZERO),
            ("0s", Duration::ZERO),
            ("2m", Duration::from_secs(120)),
            ("90s", Duration::from_secs(90)),
            ("1.5h", Duration::from_mins(90)),
            ("720h30m", Duration::from_mins(43230)),
            ("300ms", Duration::from_millis(300)),
            ("1h2m3s", Duration::from_secs(3723)),
            ("+5s", Duration::from_secs(5)),
            (".5s", Duration::from_millis(500)),
        ];
        for (text, want) in cases {
            assert_eq!(parse(text), Ok(want), "{text}");
        }
    }

    #[test]
    fn refuses_what_go_refuses_and_negatives() {
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
