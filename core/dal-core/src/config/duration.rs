// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! TOML duration strings: one helper shared by duration-valued keys.
//!
//! The accepted spellings are `<n>s`, `<n>m`, `<n>h`, and their plain
//! integer seconds form. The parser rejects zero, negatives (the integer
//! form cannot be negative anyway), and anything with mixed units.

use std::time::Duration;

/// Parses one TOML value as a duration.
///
/// A string takes `<n>s`, `<n>m`, or `<n>h`; an integer is seconds.
/// Zero and negative amounts fail: a zero-duration timeout would close
/// every question before a human sees it.
///
/// # Errors
/// Returns the offending value for the caller's invalid-value error.
pub(super) fn parse(value: &toml::Value) -> Result<Duration, ()> {
    match value {
        toml::Value::Integer(seconds) => positive_secs(*seconds),
        toml::Value::String(text) => parse_spelling(text),
        _ => Err(()),
    }
}

fn positive_secs(seconds: i64) -> Result<Duration, ()> {
    if seconds <= 0 {
        return Err(());
    }
    u64::try_from(seconds)
        .map(Duration::from_secs)
        .map_err(|_| ())
}

fn parse_spelling(text: &str) -> Result<Duration, ()> {
    let (digits, unit) = text
        .split_at_checked(text.len().saturating_sub(1))
        .ok_or(())?;
    let multiplier = match unit {
        "s" => 1_u64,
        "m" => 60,
        "h" => 3600,
        _ => return Err(()),
    };
    let amount: u64 = digits.parse().map_err(|_| ())?;
    let secs = amount.checked_mul(multiplier).ok_or(())?;
    if secs == 0 {
        return Err(());
    }
    Ok(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::parse;

    #[test]
    fn accepted_spellings() {
        assert_eq!(
            parse(&toml::Value::from("30m")),
            Ok(Duration::from_mins(30))
        );
        assert_eq!(
            parse(&toml::Value::from("90s")),
            Ok(Duration::from_secs(90))
        );
        assert_eq!(parse(&toml::Value::from("2h")), Ok(Duration::from_hours(2)));
        assert_eq!(
            parse(&toml::Value::from(120_i64)),
            Ok(Duration::from_mins(2))
        );
    }

    #[test]
    fn rejected_spellings() {
        for text in [
            "", "m", "30", "0s", "1.5m", "-5m", "1d", "1w", "30mm", "12M",
        ] {
            assert_eq!(parse(&toml::Value::from(text)), Err(()), "{text}");
        }
        assert_eq!(parse(&toml::Value::from(0_i64)), Err(()));
        assert_eq!(parse(&toml::Value::from(-1_i64)), Err(()));
        assert_eq!(parse(&toml::Value::Boolean(true)), Err(()));
    }
}
