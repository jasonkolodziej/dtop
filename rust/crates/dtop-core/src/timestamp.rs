//! Docker's RFC 3339 timestamps (`2024-05-01T12:34:56.123456789Z`, optional offsets) as seconds
//! since the Unix epoch, keeping sub-second precision. Docker's zero time (`0001-01-01…`) is None.

use std::fmt;

/// An exact instant: whole seconds since the epoch plus nanoseconds. Log cursors need this:
/// an `f64` near today's epoch only resolves about 240 ns, so distinct log lines written a
/// nanosecond apart would compare equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp {
    pub seconds: i64,
    /// 0..1_000_000_000
    pub nanos: u32,
}

impl Timestamp {
    pub fn as_f64(self) -> f64 {
        self.seconds as f64 + f64::from(self.nanos) / 1_000_000_000.0
    }
}

/// Docker's `seconds.nanoseconds` form for `since`/`until`, exact to the nanosecond.
impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:09}", self.seconds, self.nanos)
    }
}

/// Seconds since the epoch, as a float (for rates and ages, where nanoseconds do not matter).
pub fn parse(text: &str) -> Option<f64> {
    parse_exact(text).map(Timestamp::as_f64)
}

/// The exact instant; digits beyond nanoseconds are ignored.
pub fn parse_exact(text: &str) -> Option<Timestamp> {
    let b = text.as_bytes();
    if b.len() < 20 {
        return None;
    }
    let number = |start: usize, len: usize| -> Option<i64> {
        let slice = b.get(start..start + len)?;
        slice.iter().try_fold(0i64, |acc, &c| {
            c.is_ascii_digit().then(|| acc * 10 + i64::from(c - b'0'))
        })
    };
    let year = number(0, 4)?;
    let month = number(5, 2)?;
    let day = number(8, 2)?;
    let hour = number(11, 2)?;
    let minute = number(14, 2)?;
    let second = number(17, 2)?;
    if b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't' | b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    if !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(month, year)
        || hour >= 24
        || minute >= 60
        || second >= 61
    {
        return None;
    }
    if year <= 1 {
        return None;
    }
    let mut index = 19;
    let mut nanos: u32 = 0;
    if b.get(index) == Some(&b'.') {
        index += 1;
        let mut scale: u32 = 100_000_000;
        while let Some(c) = b.get(index).filter(|c| c.is_ascii_digit()) {
            nanos += u32::from(c - b'0') * scale;
            scale /= 10;
            index += 1;
        }
    }
    let offset = match b.get(index)? {
        b'Z' | b'z' => 0,
        sign @ (b'+' | b'-') => {
            let offset_hour = number(index + 1, 2)?;
            if b.get(index + 3) != Some(&b':') {
                return None;
            }
            let offset_minute = number(index + 4, 2)?;
            let value = offset_hour * 3600 + offset_minute * 60;
            if *sign == b'+' {
                value
            } else {
                -value
            }
        }
        _ => return None,
    };
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset;
    Some(Timestamp { seconds, nanos })
}

/// Days in `month` (1–12) of `year`, proleptic Gregorian.
pub fn days_in_month(month: i64, year: i64) -> i64 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Howard Hinnant's days-from-civil algorithm.
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nanoseconds_and_offsets() {
        assert_eq!(parse("1970-01-01T00:00:00Z"), Some(0.0));
        let t = parse("2024-05-01T12:34:56.5Z").unwrap();
        assert!((t - 1_714_566_896.5).abs() < 1e-6);
        assert_eq!(parse("1970-01-01T01:00:00+01:00"), Some(0.0));
    }

    #[test]
    fn keeps_every_nanosecond() {
        let a = parse_exact("2024-05-01T12:34:56.123456789Z").unwrap();
        let b = parse_exact("2024-05-01T12:34:56.123456790Z").unwrap();
        assert_eq!(a.nanos, 123_456_789);
        assert!(a < b);
        assert_eq!(a.to_string(), "1714566896.123456789");
        assert_eq!(
            parse_exact("2024-05-01T12:34:56Z").unwrap().to_string(),
            "1714566896.000000000"
        );
        assert_eq!(
            parse_exact("2024-05-01T12:34:56.1234567891Z")
                .unwrap()
                .nanos,
            123_456_789
        );
        assert_eq!(
            parse_exact("2024-05-01T12:34:56.5Z").unwrap().nanos,
            500_000_000
        );
    }

    #[test]
    fn rejects_zero_time_and_garbage() {
        assert_eq!(parse("0001-01-01T00:00:00Z"), None);
        assert_eq!(parse("2024-02-30T00:00:00Z"), None);
        assert_eq!(parse("not a timestamp at all"), None);
        assert_eq!(parse("2024-05-01T12:34:56"), None);
    }
}
