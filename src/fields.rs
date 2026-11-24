//! Request body parsing and field validation (SPEC 3.1, 3.6 to 3.10).
//!
//! Each check takes the field's name, for its error message, and its JSON
//! value, and returns the value it stands for or a 400.

use serde::de::{DeserializeOwned, IntoDeserializer, value};

use crate::calendar::{self, Month};
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

/// A JSON object from a request, read field by field.
pub struct Body(Object);

impl Body {
    /// Parses a POST/PATCH body, which must be a JSON object (SPEC 3.1, 3.4).
    pub fn parse(bytes: &[u8]) -> Result<Self, ApiError> {
        match json::parse(bytes).map_err(ApiError::bad_request)? {
            Json::Object(map) => Ok(Self(map)),
            _ => Err(ApiError::bad_request("the body must be a JSON object")),
        }
    }

    /// A JSON object nested in a body, such as a subtransaction, copied out
    /// so it can be read the same way.
    pub fn object(field: &str, v: &Json) -> Result<Self, ApiError> {
        match v {
            Json::Object(map) => Ok(Self(map.clone())),
            _ => Err(ApiError::bad_request(format!("{field} must be an object"))),
        }
    }

    /// A field that must be present, checked by `check`.
    pub fn required<'a, T>(
        &'a self,
        key: &str,
        check: impl FnOnce(&str, &'a Json) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        self.optional(key, check)?.ok_or_else(|| missing(key))
    }

    /// A field checked by `check` whenever it is present (SPEC 3.6).
    pub fn optional<'a, T>(
        &'a self,
        key: &str,
        check: impl FnOnce(&str, &'a Json) -> Result<T, ApiError>,
    ) -> Result<Option<T>, ApiError> {
        self.0.get(key).map(|v| check(key, v)).transpose()
    }

    /// A nullable field (SPEC 3.10): left out, `null`, or a value that must
    /// pass `check`.
    pub fn nullable<'a, T>(
        &'a self,
        key: &str,
        check: impl FnOnce(&str, &'a Json) -> Result<T, ApiError>,
    ) -> Result<Patch<T>, ApiError> {
        Ok(match self.0.get(key) {
            None => Patch::Absent,
            Some(Json::Null) => Patch::Null,
            Some(v) => Patch::Set(check(key, v)?),
        })
    }
}

/// What a body says about a nullable field (SPEC 3.10).
#[derive(Debug, PartialEq, Eq)]
pub enum Patch<T> {
    /// The body leaves the field out.
    Absent,
    /// The body sets the field to `null`.
    Null,
    /// The body sets the field to a value.
    Set(T),
}

impl<T> Patch<T> {
    /// The field's value when leaving it out means `null`, as on a new
    /// object.
    pub fn or_null(self) -> Option<T> {
        match self {
            Self::Absent | Self::Null => None,
            Self::Set(value) => Some(value),
        }
    }

    /// The field's value after the body is applied to `current`: unchanged
    /// if the body leaves the field out.
    pub fn applied_to(self, current: Option<&T>) -> Option<T>
    where
        T: Clone,
    {
        match self {
            Self::Absent => current.cloned(),
            Self::Null => None,
            Self::Set(value) => Some(value),
        }
    }

    /// Applies the body to `field`, leaving it as it is if the body leaves
    /// the field out.
    pub fn apply_to(self, field: &mut Option<T>) {
        match self {
            Self::Absent => {}
            Self::Null => *field = None,
            Self::Set(value) => *field = Some(value),
        }
    }
}

/// The error for a required field that isn't there.
pub fn missing(key: &str) -> ApiError {
    ApiError::bad_request(format!("{key} is required"))
}

pub fn string(field: &str, v: &Json) -> Result<String, ApiError> {
    match v {
        Json::String(s) => Ok(s.clone()),
        _ => Err(ApiError::bad_request(format!("{field} must be a string"))),
    }
}

/// One of the wire names of the serde enum `T`, such as an account type.
pub fn variant<T: DeserializeOwned>(field: &str, v: &Json) -> Result<T, ApiError> {
    let Json::String(s) = v else {
        return Err(ApiError::bad_request(format!("{field} must be a string")));
    };
    T::deserialize(s.as_str().into_deserializer())
        .map_err(|e: value::Error| ApiError::bad_request(format!("{field}: {e}")))
}

/// A name of 1 to `max` scalar values, not all whitespace (SPEC 3.9).
fn name_of_length(field: &str, v: &Json, max: usize) -> Result<String, ApiError> {
    let s = string(field, v)?;
    let len = s.chars().count();
    if len == 0 || len > max {
        return Err(ApiError::bad_request(format!(
            "{field} must have 1 to {max} characters"
        )));
    }
    // char::is_whitespace is Unicode's White_Space property.
    if s.chars().all(char::is_whitespace) {
        return Err(ApiError::bad_request(format!(
            "{field} must not be only whitespace"
        )));
    }
    Ok(s)
}

pub fn name(field: &str, v: &Json) -> Result<String, ApiError> {
    name_of_length(field, v, MAX_NAME)
}

pub fn payee_name(field: &str, v: &Json) -> Result<String, ApiError> {
    name_of_length(field, v, MAX_PAYEE_NAME)
}

/// The text of a note or memo, which is nullable: at most 500 scalar values
/// (SPEC 3.9).
pub fn note(field: &str, v: &Json) -> Result<String, ApiError> {
    match v {
        Json::String(s) if s.chars().count() <= MAX_NOTE => Ok(s.clone()),
        _ => Err(ApiError::bad_request(format!(
            "{field} must be null or a string of at most {MAX_NOTE} characters"
        ))),
    }
}

/// An array, each of whose items passes `check`.
pub fn array<T>(
    field: &str,
    v: &Json,
    check: impl Fn(&str, &Json) -> Result<T, ApiError>,
) -> Result<Vec<T>, ApiError> {
    let Json::Array(items) = v else {
        return Err(ApiError::bad_request(format!("{field} must be an array")));
    };
    let item = format!("each of {field}");
    items.iter().map(|v| check(&item, v)).collect()
}

pub fn boolean(field: &str, v: &Json) -> Result<bool, ApiError> {
    match v {
        Json::Bool(b) => Ok(*b),
        _ => Err(ApiError::bad_request(format!(
            "{field} must be true or false"
        ))),
    }
}

/// Whether `n` milliunits is within the range of amounts (SPEC 3.7).
pub fn is_amount(n: i64) -> bool {
    (-MAX_AMOUNT..=MAX_AMOUNT).contains(&n)
}

/// A JSON integer written without a fraction or exponent (SPEC 3.7, 18.3),
/// if it fits an `i64`. `-0` is 0.
fn integer(v: &Json) -> Option<i64> {
    match v {
        Json::Number(raw) if !raw.contains(['.', 'e', 'E']) => raw.parse().ok(),
        _ => None,
    }
}

/// An amount in milliunits: a JSON integer within ±[`MAX_AMOUNT`]
/// (SPEC 3.7).
pub fn amount(field: &str, v: &Json) -> Result<i64, ApiError> {
    match integer(v) {
        Some(n) if is_amount(n) => Ok(n),
        _ => Err(ApiError::bad_request(format!(
            "{field} must be an integer from -{MAX_AMOUNT} to {MAX_AMOUNT}"
        ))),
    }
}

/// A day of the month: a JSON integer from 1 to 31 (SPEC 18.3).
pub fn day_of_month(field: &str, v: &Json) -> Result<u8, ApiError> {
    integer(v)
        .and_then(|n| u8::try_from(n).ok())
        .filter(|d| (1..=31).contains(d))
        .ok_or_else(|| ApiError::bad_request(format!("{field} must be an integer from 1 to 31")))
}

/// The error for a month that isn't one (SPEC 3.8).
pub fn invalid_month(field: &str) -> ApiError {
    ApiError::bad_request(format!(
        "{field} must be a month YYYY-MM from 1900-01 to 2999-12"
    ))
}

pub fn month(field: &str, v: &Json) -> Result<Month, ApiError> {
    match v {
        Json::String(s) => s.parse().map_err(|_| invalid_month(field)),
        _ => Err(invalid_month(field)),
    }
}

/// The error for a date that isn't one (SPEC 3.8).
pub fn invalid_date(field: &str) -> ApiError {
    ApiError::bad_request(format!(
        "{field} must be a date YYYY-MM-DD from 1900-01-01 to 2999-12-31"
    ))
}

pub fn date(field: &str, v: &Json) -> Result<String, ApiError> {
    match v {
        Json::String(s) if calendar::is_date(s) => Ok(s.clone()),
        _ => Err(invalid_date(field)),
    }
}

pub fn currency(field: &str, v: &Json) -> Result<String, ApiError> {
    match v {
        Json::String(s) if s.len() == 3 && s.bytes().all(|b| b.is_ascii_uppercase()) => {
            Ok(s.clone())
        }
        _ => Err(ApiError::bad_request(format!(
            "{field} must be three uppercase ASCII letters"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AccountType, FlagColor};

    fn num(s: &str) -> Json {
        Json::Number(s.into())
    }

    #[test]
    fn amounts() {
        assert_eq!(amount("a", &num("-0")).unwrap(), 0);
        assert_eq!(amount("a", &num("1000000000000")).unwrap(), MAX_AMOUNT);
        assert_eq!(amount("a", &num("-1000000000000")).unwrap(), -MAX_AMOUNT);
        for bad in [
            "1000000000001",
            "-1000000000001",
            "1.0",
            "1e3",
            "1E3",
            "-0.0",
            "99999999999999999999",
        ] {
            assert!(amount("a", &num(bad)).is_err(), "{bad}");
        }
        assert!(amount("a", &Json::String("1".into())).is_err());
        assert!(amount("a", &Json::Null).is_err());
    }

    #[test]
    fn days_of_the_month() {
        assert_eq!(day_of_month("d", &num("1")).unwrap(), 1);
        assert_eq!(day_of_month("d", &num("31")).unwrap(), 31);
        for bad in [
            "0",
            "-0",
            "32",
            "-1",
            "15.0",
            "1e1",
            "256",
            "99999999999999999999",
        ] {
            assert!(day_of_month("d", &num(bad)).is_err(), "{bad}");
        }
        assert!(day_of_month("d", &Json::String("15".into())).is_err());
    }

    #[test]
    fn variants() {
        let s = |s: &str| Json::String(s.into());
        assert_eq!(
            variant::<AccountType>("type", &s("creditCard")).unwrap(),
            AccountType::CreditCard
        );
        let err = variant::<AccountType>("type", &s("credit_card")).unwrap_err();
        assert!(err.message.contains("lineOfCredit"), "{}", err.message);
        assert!(variant::<AccountType>("type", &Json::Null).is_err());
    }

    #[test]
    fn nullable_fields() {
        let body = Body::parse(br#"{"flag_color": "red", "memo": null}"#).unwrap();
        assert_eq!(
            body.nullable("flag_color", variant).unwrap(),
            Patch::Set(FlagColor::Red)
        );
        assert_eq!(body.nullable("memo", note).unwrap(), Patch::Null);
        assert_eq!(body.nullable("payee_id", string).unwrap(), Patch::Absent);
        assert!(body.nullable("flag_color", boolean).is_err());

        let current = Some(FlagColor::Blue);
        assert_eq!(Patch::Absent.applied_to(current.as_ref()), current);
        assert_eq!(Patch::Null.applied_to(current.as_ref()), None);
        assert_eq!(Patch::Absent::<FlagColor>.or_null(), None);
        let mut field = current;
        Patch::Set(FlagColor::Red).apply_to(&mut field);
        assert_eq!(field, Some(FlagColor::Red));
    }
}
