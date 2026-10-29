//! UTC timestamps without a date library (the core crate has none): crash
//! report names, report headers and the update log.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch (0 if the clock is before it).
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `(year, month, day, hour, minute, second)` in UTC.
pub fn utc_parts(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (
        year,
        month,
        day,
        (rem / 3_600) as u32,
        (rem % 3_600 / 60) as u32,
        (rem % 60) as u32,
    )
}

/// `2026-10-27T21:14:03Z`.
pub fn iso(secs: i64) -> String {
    let (y, mo, d, h, mi, s) = utc_parts(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// `2026-10-27 21:14 UTC`, for people.
pub fn human(secs: i64) -> String {
    let (y, mo, d, h, mi, _) = utc_parts(secs);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02} UTC")
}

/// `20261027-211403Z`: sortable and valid in file names everywhere.
pub fn compact(secs: i64) -> String {
    let (y, mo, d, h, mi, s) = utc_parts(secs);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}Z")
}

/// Seconds since the epoch of an ISO-8601 UTC time as GitHub writes it
/// (`2026-10-27T21:14:03Z`), or of a [`compact`] stamp.
pub fn parse_utc(s: &str) -> Option<i64> {
    let s = s.trim();
    let digits: String = s.chars().filter(char::is_ascii_digit).collect();
    if digits.len() < 14 {
        return None;
    }
    let n = |a: usize, b: usize| digits[a..b].parse::<i64>().ok();
    let (y, mo, d, h, mi, sec) = (
        n(0, 4)?,
        n(4, 6)?,
        n(6, 8)?,
        n(8, 10)?,
        n(10, 12)?,
        n(12, 14)?,
    );
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    // days_from_civil
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2.rem_euclid(400);
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3_600 + mi * 60 + sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_parses_round_trip() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        // 2026-10-27T21:14:03Z
        let t = 1_793_135_643;
        assert_eq!(iso(t), "2026-10-27T21:14:03Z");
        assert_eq!(compact(t), "20261027-211403Z");
        assert_eq!(human(t), "2026-10-27 21:14 UTC");
        assert_eq!(parse_utc("2026-10-27T21:14:03Z"), Some(t));
        assert_eq!(parse_utc("20261027-211403Z"), Some(t));
        assert_eq!(parse_utc(&iso(951_782_400)), Some(951_782_400)); // 2000-02-29
        assert_eq!(parse_utc("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_utc("yesterday"), None);
    }
}
