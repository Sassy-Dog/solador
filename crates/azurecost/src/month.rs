//! UTC calendar arithmetic the export's folder layout and the spend projection
//! need. Port of the date helpers in
//! `AzureCostCSV`.
//!
//! Everything here takes `now` as an argument. Nothing reads the wall clock, so
//! the projection is a pure function of (spend, instant) and its tests pin real
//! calendar edges — February, 31-day months, the 1st, the last day — instead of
//! whatever day CI happens to run on.

use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc};

/// Number of days in the given UTC calendar month.
///
/// `None` only for a year outside chrono's range, which no export folder can
/// name; callers mirror the original fallbacks rather than panicking on it.
#[must_use]
pub fn days_in_month(year: i32, month: u32) -> Option<u32> {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .and_then(|first_of_next| first_of_next.pred_opt())
        .map(|d| d.day())
}

/// The export partitions its output by month into a folder named for the
/// calendar month's date range, e.g. `20260601-20260630`. Compute it for the
/// month containing `date` (UTC) so a read lists just that month's runs instead
/// of the export's whole history.
#[must_use]
pub fn month_range_folder(date: DateTime<Utc>) -> String {
    let (year, month) = (date.year(), date.month());
    let last_day = days_in_month(year, month).unwrap_or(28);
    format!("{year:04}{month:02}01-{year:04}{month:02}{last_day:02}")
}

/// First instant of the calendar month before the one containing `date` (UTC).
/// Feeds [`month_range_folder`] for the prior-month (`last-month`) export.
#[must_use]
pub fn prior_month_date(date: DateTime<Utc>) -> DateTime<Utc> {
    let (year, month) = if date.month() == 1 {
        (date.year() - 1, 12)
    } else {
        (date.year(), date.month() - 1)
    };
    first_instant_of(year, month).unwrap_or(date)
}

fn first_instant_of(year: i32, month: u32) -> Option<DateTime<Utc>> {
    NaiveDate::from_ymd_opt(year, month, 1)?
        .and_hms_opt(0, 0, 0)
        .map(|naive| Utc.from_utc_datetime(&naive))
}

/// Linearly project month-to-date spend to a full-month total: spend so far,
/// scaled by (days in month / elapsed days).
///
/// Elapsed counts the current, partial day, so day 1 never divides by zero. On
/// the last day of a month elapsed equals the day count, so a completed month
/// projects to itself — which is why a carried-forward end-of-month snapshot
/// correctly shows projected == MTD.
#[must_use]
pub fn project_monthly_spend(spend_mtd: f64, now: DateTime<Utc>) -> f64 {
    let elapsed_days = now.day(); // 1-based, includes today
    let days_in_month = days_in_month(now.year(), now.month()).unwrap_or(30);
    if elapsed_days == 0 {
        return spend_mtd;
    }
    spend_mtd / f64::from(elapsed_days) * f64::from(days_in_month)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::utc;

    #[test]
    fn month_range_folder_names_the_calendar_month_folder() {
        assert_eq!(
            month_range_folder(utc(2026, 6, 15, 12, 0)),
            "20260601-20260630"
        );
        // 28-day February, and zero-padding of single-digit months/days.
        assert_eq!(
            month_range_folder(utc(2027, 2, 3, 0, 0)),
            "20270201-20270228"
        );
        // UTC, not local time — an end-of-month UTC instant stays in that month.
        assert_eq!(
            month_range_folder(utc(2026, 12, 31, 23, 30)),
            "20261201-20261231"
        );
    }

    #[test]
    fn february_gains_a_day_in_a_leap_year() {
        assert_eq!(
            month_range_folder(utc(2028, 2, 10, 0, 0)),
            "20280201-20280229"
        );
    }

    #[test]
    fn prior_month_date_rolls_back_one_calendar_month() {
        assert_eq!(
            month_range_folder(prior_month_date(utc(2026, 6, 15, 0, 0))),
            "20260501-20260531"
        );
        // January rolls back into the previous December.
        assert_eq!(
            month_range_folder(prior_month_date(utc(2026, 1, 10, 0, 0))),
            "20251201-20251231"
        );
    }

    /// The rollover fallback stamps this instant onto the summary, so it must
    /// be the *first* instant of the month, not "same day, last month".
    #[test]
    fn prior_month_date_is_the_first_instant_of_that_month() {
        assert_eq!(
            prior_month_date(utc(2026, 7, 1, 0, 0)),
            utc(2026, 6, 1, 0, 0)
        );
        // A 31st has no counterpart in a 30-day month; day-1 anchoring is why
        // that is not a problem.
        assert_eq!(
            prior_month_date(utc(2026, 7, 31, 18, 45)),
            utc(2026, 6, 1, 0, 0)
        );
    }

    #[test]
    fn project_monthly_spend_extrapolates_mid_month() {
        // 15 of 30 days elapsed at $300 → full-month projection $600.
        assert!((project_monthly_spend(300.0, utc(2026, 6, 15, 0, 0)) - 600.0).abs() < 1e-6);
    }

    #[test]
    fn project_monthly_spend_on_the_last_day_equals_mtd() {
        // Elapsed == days in month, so a completed month projects to itself —
        // why a stale end-of-month snapshot shows projected == MTD.
        assert!((project_monthly_spend(688.46, utc(2026, 6, 30, 23, 0)) - 688.46).abs() < 1e-6);
        assert!((project_monthly_spend(500.0, utc(2027, 2, 28, 12, 0)) - 500.0).abs() < 1e-6);
    }

    #[test]
    fn project_monthly_spend_on_the_first_day_does_not_divide_by_zero() {
        // Day 1: elapsed = 1 → MTD × days in month (July = 31).
        assert!((project_monthly_spend(20.0, utc(2026, 7, 1, 6, 0)) - 620.0).abs() < 1e-6);
    }
}
