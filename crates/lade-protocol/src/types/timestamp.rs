//! RFC 3339 UTC timestamps with millisecond precision, and their Unix-millisecond conversions.

use super::Timestamp;
use crate::schema;

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

impl Timestamp {
    pub fn from_unix_ms(ms: i64) -> Option<Self> {
        if ms < 0 {
            return None;
        }
        let (days, rem) = (ms / 86_400_000, ms % 86_400_000);
        let (year, month, day) = civil_from_days(days);
        if year > 9999 {
            return None;
        }
        Some(Self(format!(
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
            rem / 3_600_000,
            rem / 60_000 % 60,
            rem / 1000 % 60,
            rem % 1000
        )))
    }
    pub fn unix_ms(&self) -> i64 {
        let [y, mo, d, h, mi, s, ms] = schema::timestamp_parts(&self.0)
            .expect("validated timestamp")
            .map(i64::from);
        days_from_civil(y, mo, d) * 86_400_000 + h * 3_600_000 + mi * 60_000 + s * 1000 + ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_round_trip_unix_milliseconds() {
        for (ms, text) in [
            (0, "1970-01-01T00:00:00.000Z"),
            (951_782_400_000, "2000-02-29T00:00:00.000Z"),
            (1_791_635_696_789, "2026-10-10T12:34:56.789Z"),
            (253_402_300_799_999, "9999-12-31T23:59:59.999Z"),
        ] {
            let t = Timestamp::from_unix_ms(ms).unwrap();
            assert_eq!(t.as_str(), text);
            assert_eq!(t.unix_ms(), ms);
            assert_eq!(Timestamp::parse(text).unwrap(), t);
        }
        assert_eq!(Timestamp::from_unix_ms(-1), None);
        assert_eq!(Timestamp::from_unix_ms(253_402_300_800_000), None);
        for bad in [
            "2026-02-29T00:00:00.000Z",
            "2026-10-10T24:00:00.000Z",
            "2026-10-10T12:34:60.000Z",
            "2026-10-10T12:34:56Z",
            "2026-10-10T12:34:56.789z",
            "2026-10-10t12:34:56.789Z",
            "2026-10-10T12:34:56.789+00:00",
            "1969-12-31T23:59:59.999Z",
            "2026-13-01T00:00:00.000Z",
            "2026-00-01T00:00:00.000Z",
            "2026-10-00T00:00:00.000Z",
        ] {
            assert!(Timestamp::parse(bad).is_err(), "{bad}");
        }
    }
}
