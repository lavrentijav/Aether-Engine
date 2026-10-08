//! Timestamped, levelled log lines on stdout.
//!
//! The server used to `println!` bare lines, so a log said neither when
//! something happened nor how bad it was. Every line now starts with the UTC
//! time and a level; the format is fixed so it can be grepped and sorted.

use std::time::{SystemTime, UNIX_EPOCH};

/// Something worth knowing.
pub fn info(msg: &str) {
    line("INFO", msg);
}

/// Something wrong that the server carries on through.
pub fn warn(msg: &str) {
    line("WARN", msg);
}

/// Something wrong that needs an operator.
pub fn error(msg: &str) {
    line("ERROR", msg);
}

fn line(level: &str, msg: &str) {
    println!("{} {level:<5} {msg}", timestamp(SystemTime::now()));
}

/// `YYYY-MM-DD HH:MM:SS` in UTC.
fn timestamp(t: SystemTime) -> String {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's
/// `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn timestamps_are_utc_calendar_time() {
        let at = |s| timestamp(UNIX_EPOCH + Duration::from_secs(s));
        assert_eq!(at(0), "1970-01-01 00:00:00");
        assert_eq!(at(951_782_400), "2000-02-29 00:00:00");
        assert_eq!(at(1_791_444_052), "2026-10-08 07:20:52");
    }
}
