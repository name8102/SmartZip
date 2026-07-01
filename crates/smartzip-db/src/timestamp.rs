//! Timestamp helpers for history rows.
//!
//! We deliberately avoid pulling in `chrono` / `time` for one function.
//! SQLite handles most timestamps through `CURRENT_TIMESTAMP`, but the
//! engine's recorder needs to feed a UTC ISO-8601 string into the
//! `created_at` / `started_at` / `finished_at` columns to guarantee a
//! single monotonic clock across multi-row inserts within one task.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current time formatted as `YYYY-MM-DDTHH:MM:SSZ` in UTC.
pub fn now_utc_iso8601() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format_utc_seconds(seconds)
}

/// Format `seconds` since the Unix epoch as UTC ISO-8601.
pub fn format_utc_seconds(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let rem = seconds.rem_euclid(86_400) as u32;
    let (year, month, day) = civil_from_days(days);
    let hour = rem / 3600;
    let minute = (rem / 60) % 60;
    let second = rem % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

// Howard Hinnant's civil-from-days algorithm.
// See http://howardhinnant.github.io/date_algorithms.html.
fn civil_from_days(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 {
        z / 146_097
    } else {
        (z - 146_096) / 146_097
    };
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year as i32, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_formats_to_1970_01_01() {
        assert_eq!(format_utc_seconds(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn known_date_2000_01_01() {
        // 946684800 == 2000-01-01T00:00:00Z
        assert_eq!(format_utc_seconds(946_684_800), "2000-01-01T00:00:00Z");
    }

    #[test]
    fn known_date_2026_07_02() {
        // 1782000000 == 2026-06-20T17:20:00Z; use a value we can compute
        // by hand: 2026-07-02T00:00:00Z corresponds to
        // 56 years and 6 months + 2 days after 1970 => 1782950400.
        assert_eq!(format_utc_seconds(1_782_950_400), "2026-07-02T00:00:00Z");
    }

    #[test]
    fn now_returns_sortable_string() {
        let value = now_utc_iso8601();
        assert_eq!(value.len(), 20);
        assert!(value.ends_with('Z'));
        assert!(value.chars().nth(4) == Some('-'));
    }
}
