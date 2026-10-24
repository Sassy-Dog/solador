//! `Retry-After`, read the way orca reads it.
//!
//! orca caps the wait at a day: "a corrupt/hostile Retry-After must not gate
//! usage refreshes for days" (`main/rate-limits/usage-retry-after.ts:3-4`). A
//! 429 then gates every retry until the wait is up
//! (`service/service-polling.ts:109-111`). Solador's shell does the same with
//! the number this returns.

use chrono::{DateTime, Utc};

/// The longest wait a `Retry-After` may impose.
pub(crate) const MAX_RETRY_AFTER_SECS: u64 = 24 * 60 * 60;

/// Seconds to wait, from a `Retry-After` header that is either delta-seconds
/// or an HTTP-date. Returns `None` for an absent or unparseable header.
///
/// A date already in the past is `0`, meaning the service says "now". A
/// negative delta is not a wait at all, so it is `None`.
pub(crate) fn retry_after_secs(value: Option<&str>, now: DateTime<Utc>) -> Option<u64> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }
    if value.bytes().all(|b| b.is_ascii_digit()) {
        let secs: u64 = value.parse().ok()?;
        return Some(secs.min(MAX_RETRY_AFTER_SECS));
    }
    let at = DateTime::parse_from_rfc2822(value).ok()?;
    let wait = (at.with_timezone(&Utc) - now).num_seconds();
    Some(u64::try_from(wait).unwrap_or(0).min(MAX_RETRY_AFTER_SECS))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-24T12:00:00Z")
            .expect("ts")
            .into()
    }

    #[test]
    fn delta_seconds_are_read_and_capped_at_a_day() {
        assert_eq!(retry_after_secs(Some("120"), now()), Some(120));
        assert_eq!(retry_after_secs(Some(" 0 "), now()), Some(0));
        assert_eq!(
            retry_after_secs(Some("9999999"), now()),
            Some(MAX_RETRY_AFTER_SECS)
        );
    }

    #[test]
    fn an_http_date_is_a_wait_from_now() {
        assert_eq!(
            retry_after_secs(Some("Sat, 24 Oct 2026 13:00:00 GMT"), now()),
            Some(3600)
        );
        // Already passed: the service says "now", not "never".
        assert_eq!(
            retry_after_secs(Some("Sat, 24 Oct 2026 11:00:00 GMT"), now()),
            Some(0)
        );
        // A week out is a hostile or corrupt header; a day is the most it gets.
        assert_eq!(
            retry_after_secs(Some("Sat, 31 Oct 2026 12:00:00 GMT"), now()),
            Some(MAX_RETRY_AFTER_SECS)
        );
    }

    #[test]
    fn nothing_parseable_is_no_wait_rather_than_a_guess() {
        for value in [None, Some(""), Some("-5"), Some("soon"), Some("1.5")] {
            assert_eq!(retry_after_secs(value, now()), None, "{value:?}");
        }
    }
}
