//! Request body parsing and field validation (SPEC 3.1, 3.6 to 3.10).

use crate::error::ApiError;
use crate::json::{self, Json, Object};

/// Most milliunits an amount may have either side of 0 (SPEC 3.7).
pub const MAX_AMOUNT: i64 = 1_000_000_000_000;
/// Longest name (SPEC 3.9), in Unicode scalar values.
pub const MAX_NAME: usize = 100;
/// Longest payee name (SPEC 3.9).
pub const MAX_PAYEE_NAME: usize = 200;
/// Longest note or memo (SPEC 3.9).
pub const MAX_NOTE: usize = 500;

fn bad(message: impl Into<String>) -> ApiError {
    ApiError::bad_request(message)
}

/// Parses a POST/PATCH body, which must be a JSON object (SPEC 3.1, 3.4).
pub fn parse_object(body: &[u8]) -> Result<Object, ApiError> {
    match json::parse(body).map_err(bad)? {
        Json::Object(map) => Ok(map),
        _ => Err(bad("the body must be a JSON object")),
    }
}

/// A field that must be present.
pub fn required<'a>(obj: &'a Object, key: &str) -> Result<&'a Json, ApiError> {
    obj.get(key).ok_or_else(|| bad(format!("{key} is required")))
}

/// Validates a field when it is present (SPEC 3.6).
pub fn optional<'a, T>(
    obj: &'a Object,
    key: &str,
    check: impl FnOnce(&str, &'a Json) -> Result<T, ApiError>,
) -> Result<Option<T>, ApiError> {
    obj.get(key).map(|v| check(key, v)).transpose()
}

pub fn string<'a>(field: &str, v: &'a Json) -> Result<&'a str, ApiError> {
    match v {
        Json::String(s) => Ok(s),
        _ => Err(bad(format!("{field} must be a string"))),
    }
}

/// A name of 1 to `max` scalar values, not all whitespace (SPEC 3.9).
pub fn name_of_length(field: &str, v: &Json, max: usize) -> Result<String, ApiError> {
    let s = string(field, v)?;
    let len = s.chars().count();
    if len == 0 || len > max {
        return Err(bad(format!("{field} must have 1 to {max} characters")));
    }
    // char::is_whitespace is Unicode's White_Space property.
    if s.chars().all(char::is_whitespace) {
        return Err(bad(format!("{field} must not be only whitespace")));
    }
    Ok(s.to_owned())
}

pub fn name(field: &str, v: &Json) -> Result<String, ApiError> {
    name_of_length(field, v, MAX_NAME)
}

pub fn payee_name(field: &str, v: &Json) -> Result<String, ApiError> {
    name_of_length(field, v, MAX_PAYEE_NAME)
}

/// A note or memo: `null` or at most 500 scalar values (SPEC 3.9).
pub fn note(field: &str, v: &Json) -> Result<Option<String>, ApiError> {
    match v {
        Json::Null => Ok(None),
        Json::String(s) if s.chars().count() <= MAX_NOTE => Ok(Some(s.clone())),
        _ => Err(bad(format!("{field} must be null or a string of at most {MAX_NOTE} characters"))),
    }
}

pub fn boolean(field: &str, v: &Json) -> Result<bool, ApiError> {
    match v {
        Json::Bool(b) => Ok(*b),
        _ => Err(bad(format!("{field} must be true or false"))),
    }
}

/// An amount in milliunits: a JSON integer with no fraction or exponent,
/// within ±[`MAX_AMOUNT`]. `-0` is 0 (SPEC 3.7).
pub fn amount(field: &str, v: &Json) -> Result<i64, ApiError> {
    let parsed = match v {
        Json::Number(raw) if !raw.contains(['.', 'e', 'E']) => raw.parse::<i64>().ok(),
        _ => None,
    };
    match parsed {
        Some(n) if (-MAX_AMOUNT..=MAX_AMOUNT).contains(&n) => Ok(n),
        _ => Err(bad(format!("{field} must be an integer from -{MAX_AMOUNT} to {MAX_AMOUNT}"))),
    }
}

fn is_leap(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

pub fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Parses `n` ASCII digits.
fn digits(s: &[u8]) -> Option<u32> {
    s.iter().try_fold(0u32, |acc, &b| b.is_ascii_digit().then(|| acc * 10 + u32::from(b - b'0')))
}

/// Parses a month `YYYY-MM` from `1900-01` to `2999-12` (SPEC 3.8).
pub fn parse_month(s: &str) -> Option<(u32, u32)> {
    let b = s.as_bytes();
    if b.len() != 7 || b[4] != b'-' {
        return None;
    }
    let year = digits(&b[..4])?;
    let month = digits(&b[5..7])?;
    ((1900..=2999).contains(&year) && (1..=12).contains(&month)).then_some((year, month))
}

/// Parses a real calendar date `YYYY-MM-DD` from `1900-01-01` to
/// `2999-12-31` (SPEC 3.8).
pub fn parse_date(s: &str) -> Option<(u32, u32, u32)> {
    let b = s.as_bytes();
    if b.len() != 10 || b[7] != b'-' {
        return None;
    }
    let (year, month) = parse_month(&s[..7])?;
    let day = digits(&b[8..10])?;
    (1..=days_in_month(year, month)).contains(&day).then_some((year, month, day))
}

pub fn date(field: &str, v: &Json) -> Result<String, ApiError> {
    match v {
        Json::String(s) if parse_date(s).is_some() => Ok(s.clone()),
        _ => Err(bad(format!("{field} must be a date YYYY-MM-DD from 1900-01-01 to 2999-12-31"))),
    }
}

pub fn currency(field: &str, v: &Json) -> Result<String, ApiError> {
    match v {
        Json::String(s) if s.len() == 3 && s.bytes().all(|b| b.is_ascii_uppercase()) => Ok(s.clone()),
        _ => Err(bad(format!("{field} must be three uppercase ASCII letters"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn num(s: &str) -> Json {
        Json::Number(s.into())
    }

    #[test]
    fn amounts() {
        assert_eq!(amount("a", &num("-0")).unwrap(), 0);
        assert_eq!(amount("a", &num("1000000000000")).unwrap(), MAX_AMOUNT);
        assert_eq!(amount("a", &num("-1000000000000")).unwrap(), -MAX_AMOUNT);
        for bad in ["1000000000001", "-1000000000001", "1.0", "1e3", "1E3", "-0.0", "99999999999999999999"] {
            assert!(amount("a", &num(bad)).is_err(), "{bad}");
        }
        assert!(amount("a", &Json::String("1".into())).is_err());
        assert!(amount("a", &Json::Null).is_err());
    }

    #[test]
    fn dates() {
        for good in ["1900-01-01", "2999-12-31", "2024-02-29", "2000-02-29", "2026-10-31"] {
            assert!(parse_date(good).is_some(), "{good}");
        }
        for bad in [
            "1899-12-31", "3000-01-01", "2100-02-29", "2026-02-29", "2026-04-31", "2026-13-01", "2026-00-10",
            "2026-01-00", "2026-1-01", "2026/01/01", "2026-01-01T00:00", " 2026-01-01", "+026-01-01", "２０２６-01-01",
            "",
        ] {
            assert!(parse_date(bad).is_none(), "{bad}");
        }
        assert!(parse_month("2026-10").is_some());
        assert!(parse_month("1899-12").is_none());
        assert!(parse_month("2026-1").is_none());
    }
}
