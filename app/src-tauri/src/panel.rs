//! The one refresh-health footer every panel shares. Port of
//! `DevCanopy/Views/Cockpit/PanelStatusFooter.swift`.
//!
//! Swift passes this view a `staleAfter` per panel — 30s for Containers, 150s
//! for GitHub Runners — and the *ladder* is identical for all of them. One
//! definition here means two panels can never disagree about whether an error
//! outranks staleness, or about where a minute becomes an hour.

use serde_json::{json, Value};
use viewmodel::color;
use viewmodel::format::relative_age;

/// Seconds since the UNIX epoch.
///
/// Wall clock, not `Instant`, because these values are compared against records
/// that outlive the process (container presence, the runner roster). A clock
/// behind the epoch yields `0` rather than panicking.
#[must_use]
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// The footer line for one panel, or `Null` when it is healthy and fresh.
///
/// Ladder, in order: an error wins over staleness and carries how long ago the
/// last good reading was; otherwise a reading older than `stale_after` says so;
/// otherwise nothing at all is rendered, which is what keeps the cockpit
/// glanceable — a warning line means something precisely because it is absent
/// the rest of the time.
///
/// All times are unix seconds. `now.saturating_sub(updated)` rather than a
/// signed difference: a clock that jumped backwards reads as "just now", never
/// as a negative age.
#[must_use]
pub fn status_footer(
    last_updated: Option<u64>,
    error: Option<&str>,
    now: u64,
    stale_after: u64,
) -> Value {
    let text = match (error, last_updated) {
        (Some(error), Some(updated)) => Some(format!(
            "⚠ {error} · last ok {}",
            relative_age(now.saturating_sub(updated))
        )),
        (Some(error), None) => Some(format!("⚠ {error}")),
        (None, Some(updated)) if now.saturating_sub(updated) > stale_after => Some(format!(
            "⚠ stale · updated {}",
            relative_age(now.saturating_sub(updated))
        )),
        (None, _) => None,
    };
    match text {
        Some(text) => json!({ "text": text, "color": color::hex(color::AMBER) }),
        None => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_700_000_000;

    fn text_of(footer: &Value) -> Option<&str> {
        footer["text"].as_str()
    }

    #[test]
    fn a_healthy_fresh_panel_renders_no_footer() {
        assert_eq!(status_footer(Some(NOW), None, NOW, 30), Value::Null);
        assert_eq!(status_footer(Some(NOW - 30), None, NOW, 30), Value::Null);
        assert_eq!(status_footer(None, None, NOW, 30), Value::Null);
    }

    #[test]
    fn staleness_is_measured_against_the_panels_own_window() {
        // 90s old is stale for Containers (30s) and fine for Runners (150s) —
        // the whole reason the window is an argument.
        assert_eq!(
            text_of(&status_footer(Some(NOW - 90), None, NOW, 30)),
            Some("⚠ stale · updated 1m ago")
        );
        assert_eq!(status_footer(Some(NOW - 90), None, NOW, 150), Value::Null);
    }

    /// An error outranks staleness, and says when the last good reading was —
    /// "it is broken" and "the numbers on screen are from 4m ago" are two
    /// different facts and the operator needs both.
    #[test]
    fn an_error_wins_and_carries_the_last_good_reading() {
        assert_eq!(
            text_of(&status_footer(
                Some(NOW - 240),
                Some("couldn't read runners"),
                NOW,
                150
            )),
            Some("⚠ couldn't read runners · last ok 4m ago")
        );
    }

    /// Nothing has ever succeeded, so there is no "last ok" to name — and none
    /// is invented.
    #[test]
    fn an_error_with_no_successful_reading_omits_the_suffix() {
        assert_eq!(
            text_of(&status_footer(
                None,
                Some("couldn't read runners"),
                NOW,
                150
            )),
            Some("⚠ couldn't read runners")
        );
    }

    #[test]
    fn a_backwards_clock_reads_as_just_now_rather_than_a_negative_age() {
        assert_eq!(
            text_of(&status_footer(Some(NOW + 60), Some("boom"), NOW, 150)),
            Some("⚠ boom · last ok 0s ago")
        );
    }

    #[test]
    fn the_footer_is_always_amber() {
        let footer = status_footer(Some(NOW - 900), None, NOW, 150);
        assert_eq!(footer["color"], color::hex(color::AMBER));
    }
}
