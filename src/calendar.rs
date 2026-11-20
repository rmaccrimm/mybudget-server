//! Dates and months (SPEC 3.8).
//!
//! Dates are kept as the `YYYY-MM-DD` strings clients send, which already
//! sort in calendar order. Months are a [`Month`], so the budget can step
//! from one to the next.

use std::fmt;
use std::str::FromStr;

use serde::{Serialize, Serializer};

/// A month from `1900-01` to `2999-12`, ordered by time.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Month {
    /// `year * 12 + month - 1`, so consecutive months differ by 1.
    index: i32,
}

impl Month {
    fn new(year: u32, month: u32) -> Self {
        let index = i32::try_from(year * 12 + month - 1).expect("a year below 3000 fits");
        Self { index }
    }

    fn year(self) -> i32 {
        self.index.div_euclid(12)
    }

    /// 1 to 12.
    fn number(self) -> i32 {
        self.index.rem_euclid(12) + 1
    }

    pub fn prev(self) -> Self {
        Self {
            index: self.index - 1,
        }
    }

    pub fn next(self) -> Self {
        Self {
            index: self.index + 1,
        }
    }

    /// The month a stored date belongs to (SPEC 3.8).
    pub fn of_date(date: &str) -> Self {
        date[..7].parse().expect("stored dates are valid")
    }

    fn days(self) -> u32 {
        let year = self.year();
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        match self.number() {
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => 31,
        }
    }
}

/// Parses `n` ASCII digits.
fn digits(s: &[u8]) -> Option<u32> {
    s.iter().try_fold(0u32, |acc, &b| {
        b.is_ascii_digit().then(|| acc * 10 + u32::from(b - b'0'))
    })
}

/// Not a month `YYYY-MM` from `1900-01` to `2999-12`.
#[derive(Debug)]
pub struct InvalidMonth;

impl FromStr for Month {
    type Err = InvalidMonth;

    fn from_str(s: &str) -> Result<Self, InvalidMonth> {
        let b = s.as_bytes();
        if b.len() != 7 || b[4] != b'-' {
            return Err(InvalidMonth);
        }
        match (digits(&b[..4]), digits(&b[5..])) {
            (Some(year @ 1900..=2999), Some(month @ 1..=12)) => Ok(Self::new(year, month)),
            _ => Err(InvalidMonth),
        }
    }
}

impl fmt::Display for Month {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}", self.year(), self.number())
    }
}

impl Serialize for Month {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// Whether `s` is a real calendar date `YYYY-MM-DD` from `1900-01-01` to
/// `2999-12-31`.
pub fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    // Byte 7 is the ASCII `-`, so slicing there is on a char boundary.
    if b.len() != 10 || b[7] != b'-' {
        return false;
    }
    match (s[..7].parse::<Month>(), digits(&b[8..])) {
        (Ok(month), Some(day)) => (1..=month.days()).contains(&day),
        _ => false,
    }
}

/// The number of days from 1970-01-01 to a stored date, by Howard Hinnant's
/// `days_from_civil`.
fn day_number(date: &str) -> i64 {
    let b = date.as_bytes();
    let field = |range: std::ops::Range<usize>| {
        i64::from(digits(&b[range]).expect("stored dates are valid"))
    };
    let (month, day) = (field(5..7), field(8..10));
    let year = field(0..4) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// How many days apart two stored dates are (SPEC 17.9, 17.10).
pub fn days_apart(a: &str, b: &str) -> u64 {
    (day_number(a) - day_number(b)).unsigned_abs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distances() {
        assert_eq!(day_number("1970-01-01"), 0);
        assert_eq!(day_number("2000-03-01"), 11_017);
        assert_eq!(days_apart("2026-10-05", "2026-10-04"), 1);
        assert_eq!(days_apart("2024-02-28", "2024-03-01"), 2);
        assert_eq!(days_apart("2026-12-31", "2027-01-01"), 1);
        assert_eq!(days_apart("1900-01-01", "2999-12-31"), 401_766);
    }

    #[test]
    fn dates() {
        for good in [
            "1900-01-01",
            "2999-12-31",
            "2024-02-29",
            "2000-02-29",
            "2026-10-31",
        ] {
            assert!(is_date(good), "{good}");
        }
        for bad in [
            "1899-12-31",
            "3000-01-01",
            "2100-02-29",
            "2026-02-29",
            "2026-04-31",
            "2026-13-01",
            "2026-00-10",
            "2026-01-00",
            "2026-1-01",
            "2026/01/01",
            "2026-01-01T00:00",
            " 2026-01-01",
            "+026-01-01",
            "２０２６-01-01",
            "",
        ] {
            assert!(!is_date(bad), "{bad}");
        }
    }

    #[test]
    fn months() {
        for s in ["1900-01", "2026-10", "2026-12", "2999-12"] {
            assert_eq!(s.parse::<Month>().unwrap().to_string(), s);
        }
        for bad in [
            "1899-12",
            "3000-01",
            "2026-1",
            "2026-13",
            "2026-00",
            "2026-10-01",
        ] {
            assert!(bad.parse::<Month>().is_err(), "{bad}");
        }
        let dec: Month = "2026-12".parse().unwrap();
        assert_eq!(dec.next(), "2027-01".parse().unwrap());
        assert_eq!(dec.next().prev(), dec);
        assert_eq!(Month::of_date("2026-12-31"), dec);
    }
}
