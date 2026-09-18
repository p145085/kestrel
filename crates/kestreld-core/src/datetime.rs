//! Formatting Unix timestamps for protocol text.
//!
//! `RPL_CREATED` and `RPL_TOPICWHOTIME` want a human-readable date. That is
//! the only date handling the server does, so it is done here rather than by
//! pulling a calendar library into a crate that is otherwise dependency-free.

/// Format Unix seconds as `YYYY-MM-DD HH:MM:SS UTC`.
#[must_use]
pub fn format_utc(unix_seconds: u64) -> String {
    let secs = i64::try_from(unix_seconds).unwrap_or(i64::MAX);
    let days = secs.div_euclid(86_400);
    let time_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (time_of_day / 3600, time_of_day / 60 % 60, time_of_day % 60);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
}

/// Convert days since the Unix epoch to a civil (proleptic Gregorian) date.
///
/// Howard Hinnant's `civil_from_days`, which shifts the year to start in March
/// so that the leap day lands at the end of the year and month lengths follow
/// a simple repeating pattern.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Shift the epoch from 1970-01-01 to 0000-03-01.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365; // [0, 399]
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
    let shifted_month = (5 * day_of_year + 2) / 153; // [0, 11], March = 0
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1; // [1, 31]
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    }; // [1, 12]
    // Both are in range by construction; convert rather than cast so that a
    // future change to the arithmetic fails loudly instead of wrapping.
    let day = u32::try_from(day).expect("day of month is within 1..=31");
    let month = u32::try_from(month).expect("month is within 1..=12");
    let year = year + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::format_utc;

    #[test]
    fn formats_the_epoch() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00 UTC");
    }

    #[test]
    fn formats_known_timestamps() {
        assert_eq!(format_utc(1_000_000_000), "2001-09-09 01:46:40 UTC");
        assert_eq!(format_utc(1_700_000_000), "2023-11-14 22:13:20 UTC");
        assert_eq!(format_utc(1_758_153_600), "2025-09-18 00:00:00 UTC");
    }

    #[test]
    fn handles_leap_days() {
        // 2024 was a leap year; 2100 will not be.
        assert_eq!(format_utc(1_709_164_800), "2024-02-29 00:00:00 UTC");
        assert_eq!(format_utc(4_107_542_400), "2100-03-01 00:00:00 UTC");
    }

    #[test]
    fn handles_end_of_day() {
        assert_eq!(format_utc(86_399), "1970-01-01 23:59:59 UTC");
        assert_eq!(format_utc(86_400), "1970-01-02 00:00:00 UTC");
    }
}
