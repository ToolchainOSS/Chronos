//! Pure civil-date helpers used to name and bound daily time partitions.
//!
//! History is stored in one partition per UTC day so that retention pruning is a
//! cheap `DROP TABLE` of whole partitions rather than a bloating `DELETE`. These
//! functions convert between a Unix epoch and a `(year, month, day)` civil date
//! without pulling in a calendar dependency, using Howard Hinnant's well known
//! `days_from_civil` / `civil_from_days` algorithms (valid for the proleptic
//! Gregorian calendar). They are pure and fully unit tested.
//!
//! Style note: comments avoid em dashes; they use colons, semicolons, and
//! parentheses instead.

/// Seconds in one UTC day.
const SECS_PER_DAY: i64 = 86_400;

/// The number of whole days since the Unix epoch (1970-01-01) for an instant,
/// flooring toward negative infinity so pre-epoch instants land on the correct
/// day.
pub fn day_index(epoch_secs: f64) -> i64 {
    (epoch_secs as i64).div_euclid(SECS_PER_DAY)
}

/// Convert a count of days since the Unix epoch to a `(year, month, day)` civil
/// date (Howard Hinnant's `civil_from_days`).
pub fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = (y + if m <= 2 { 1 } else { 0 }) as i32;
    (year, m as u32, d)
}

/// Convert a `(year, month, day)` civil date to a count of days since the Unix
/// epoch (Howard Hinnant's `days_from_civil`; the inverse of
/// [`civil_from_days`]).
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = (if m <= 2 { y - 1 } else { y }) as i64;
    let m = m as i64;
    let d = d as i64;
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 }; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// The partition table name for a given day index (for example
/// `anomaly_events_20260707`).
pub fn partition_name(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("anomaly_events_{y:04}{m:02}{d:02}")
}

/// The `YYYY-MM-DD` boundary literal for a given day index, used in
/// `FOR VALUES FROM (...) TO (...)` partition bounds.
pub fn date_string(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Parse the day index back out of a partition name produced by
/// [`partition_name`]. Returns `None` when the name does not match the expected
/// `anomaly_events_YYYYMMDD` shape.
pub fn parse_partition_day(name: &str) -> Option<i64> {
    let digits = name.strip_prefix("anomaly_events_")?;
    if digits.len() != 8 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let y: i32 = digits[0..4].parse().ok()?;
    let m: u32 = digits[4..6].parse().ok()?;
    let d: u32 = digits[6..8].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_day_zero() {
        assert_eq!(day_index(0.0), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn known_date_round_trips() {
        // 2026-07-07 is 20641 days after the epoch.
        let day = days_from_civil(2026, 7, 7);
        assert_eq!(civil_from_days(day), (2026, 7, 7));
        assert_eq!(partition_name(day), "anomaly_events_20260707");
        assert_eq!(date_string(day), "2026-07-07");
    }

    #[test]
    fn day_index_floors_within_a_day() {
        let base = days_from_civil(2026, 7, 7) * SECS_PER_DAY;
        assert_eq!(day_index(base as f64), days_from_civil(2026, 7, 7));
        assert_eq!(
            day_index((base + 86_399) as f64),
            days_from_civil(2026, 7, 7)
        );
        assert_eq!(
            day_index((base + 86_400) as f64),
            days_from_civil(2026, 7, 8)
        );
    }

    #[test]
    fn partition_name_parses_back() {
        let day = days_from_civil(2026, 7, 7);
        let name = partition_name(day);
        assert_eq!(parse_partition_day(&name), Some(day));
    }

    #[test]
    fn rejects_malformed_partition_names() {
        assert_eq!(parse_partition_day("anomaly_events_2026070"), None);
        assert_eq!(parse_partition_day("anomaly_events_abcdefgh"), None);
        assert_eq!(parse_partition_day("something_else_20260707"), None);
        assert_eq!(parse_partition_day("anomaly_events_20261307"), None); // month 13
    }

    #[test]
    fn leap_day_round_trips() {
        let day = days_from_civil(2024, 2, 29);
        assert_eq!(civil_from_days(day), (2024, 2, 29));
    }
}
