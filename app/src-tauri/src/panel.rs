//! The two pieces of chrome every panel shares: the refresh-health footer
//! (`DevCanopy/Views/Cockpit/PanelStatusFooter.swift`) and the thin progress
//! bar (`CockpitProgressBar.swift`).
//!
//! Swift passes the footer a `staleAfter` per panel — 30s for Containers, 150s
//! for GitHub Runners — and the *ladder* is identical for all of them. One
//! definition here means two panels can never disagree about whether an error
//! outranks staleness, or about where a minute becomes an hour. Same argument
//! for the bar: the Usage panel's Sentry quota and the Azure Cost panel's
//! budget are the same widget at the same thresholds.

use serde_json::{json, Value};
use viewmodel::color;
use viewmodel::format::relative_age;

/// What the app knows about a panel's credential.
///
/// Three states, because two cannot express the first frame. Every panel used to
/// store this as a `bool` that a completed fetch set to `true`, so the value at
/// startup — before anyone had looked — was indistinguishable from "we looked
/// and there is nothing there". Both Repos and Runners opened on *"connect a
/// GitHub token in Settings"* and Azure Cost on *"Add an Azure Cost SAS URL in
/// Settings"*, telling the operator to re-paste credentials that were never
/// missing, until the first poll landed seconds later.
///
/// The same rule as `HostSnapshot`'s Optionals and the cockpit's em dash, one
/// layer up: **unknown is representable**, and a defaulted state is as much a
/// fabrication as a defaulted number.
///
/// [`Unreadable`](crate::Credential::Unreadable) is deliberately *not* a variant
/// — a locked credential store is neither of the two claims below, and each
/// panel already carries its own field for it that outranks this one.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Configured {
    /// No pass has read the credential store yet. Renders as loading: the panel
    /// has nothing to say about how it is configured, so it says nothing.
    #[default]
    Unknown,
    /// A pass read the store and found nothing. The only state entitled to paint
    /// a setup instruction, because it is the only one that observed the absence.
    Absent,
    /// A pass read a non-empty credential. Says nothing about whether the
    /// provider then *accepted* it — a rejected token is a fetch failure and is
    /// reported as one, never as a missing credential.
    Present,
}

impl Configured {
    /// Whether the panel is still waiting to find out how it is configured.
    #[must_use]
    pub fn is_unknown(self) -> bool {
        self == Configured::Unknown
    }

    /// Whether a pass has observed that there is no credential. Note this is
    /// **not** `!is_present()`: [`Unknown`](Self::Unknown) is neither.
    #[must_use]
    pub fn is_absent(self) -> bool {
        self == Configured::Absent
    }

    /// Whether a pass has read a credential.
    #[must_use]
    pub fn is_present(self) -> bool {
        self == Configured::Present
    }
}

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
///
/// **`last_updated` must be the last *success*, not the last attempt.** The
/// error arm renders it as `last ok {age}`, which is a promise about this
/// argument that only the caller can keep. Two callers used to pass "when we
/// last looked" — a Docker daemon that had never once answered reported
/// `⚠ couldn't read docker · last ok 0s ago`, a reassurance about a reading
/// that never existed. A panel that needs both clocks keeps them as separate
/// fields and passes the success one here. `None` means nothing has ever
/// succeeded, and the suffix is dropped rather than guessed.
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

/// One thin progress bar: how much of the track to fill, and what colour.
///
/// Port of `CockpitProgressBar` (Swift), including its one subtlety — **the
/// width is clamped and the colour is not**. An over-quota bar pins full rather
/// than overflowing its track, but it still reads red, because a bar sitting at
/// 100% in green would say "at budget" when the truth is "past it".
///
/// `fraction` is the raw ratio. A non-finite one (a divide by a zero
/// denominator) is not a measurement, so it renders an empty green bar rather
/// than a `NaN` width the frontend would silently drop.
#[must_use]
pub fn progress_bar(fraction: f64, amber_at: f64, red_at: f64) -> Value {
    let raw = if fraction.is_finite() { fraction } else { 0.0 };
    let color = if raw >= red_at {
        color::RED
    } else if raw >= amber_at {
        color::AMBER
    } else {
        color::GREEN
    };
    json!({
        "fraction": raw.clamp(0.0, 1.0),
        "color": color::hex(color),
    })
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

    /// Every panel's staleness window, pinned against the Swift panel that
    /// owns it.
    ///
    /// The ladder above being right is worth nothing if a panel hands it the
    /// wrong window, and a drifted constant is otherwise invisible: a footer
    /// that appears an hour late looks exactly like a panel that is simply
    /// fresh. Swift ground truth, panel for panel:
    ///
    /// | panel | window | Swift |
    /// |---|---|---|
    /// | Containers | 30s | `Views/Cockpit/Panels/ContainersPanel.swift:40` |
    /// | Runners | 150s | `Views/Cockpit/Panels/GHRunnersPanel.swift:37` |
    /// | Claude usage | 150s | `Views/Cockpit/Panels/ClaudeUsagePanel.swift:43` |
    /// | Neon + Sentry | 5400s | `ClaudeUsagePanel.swift:60`, `:83` |
    /// | Azure Cost | 18000s | `Views/Cockpit/Panels/AzureCostPanel.swift:27` |
    ///
    /// Hosts, Repos and OpenClaw are absent on purpose — none of the three
    /// renders a status footer on either side. Hosts carries staleness on the
    /// per-card connection dot instead, Repos degrades the per-repo dot to
    /// unreachable, and OpenClaw is event-driven so it has no cadence to be
    /// late against.
    #[test]
    fn every_panels_staleness_window_matches_its_swift_panel() {
        let windows: [(&str, u64, u64); 5] = [
            ("containers", crate::containers::STALE_AFTER_SECS, 30),
            ("runners", crate::github::RUNNERS_STALE_AFTER_SECS, 150),
            ("claude usage", crate::usage::CLAUDE_STALE_AFTER_SECS, 150),
            (
                "neon + sentry",
                crate::usage::PROVIDER_STALE_AFTER_SECS,
                90 * 60,
            ),
            ("azure cost", crate::azure::STALE_AFTER_SECS, 5 * 60 * 60),
        ];
        for (panel, window, swift) in windows {
            assert_eq!(window, swift, "{panel} drifted from its Swift panel");
            // And the constant is live, not merely declared: exactly at the
            // window is still fresh, one second past it is a footer.
            assert_eq!(
                status_footer(Some(NOW - window), None, NOW, window),
                Value::Null,
                "{panel} should be fresh at exactly its window"
            );
            assert!(
                text_of(&status_footer(Some(NOW - window - 1), None, NOW, window))
                    .is_some_and(|text| text.starts_with("⚠ stale")),
                "{panel} should be stale one second past its window"
            );
        }
    }

    #[test]
    fn the_footer_is_always_amber() {
        let footer = status_footer(Some(NOW - 900), None, NOW, 150);
        assert_eq!(footer["color"], color::hex(color::AMBER));
    }

    // MARK: progress_bar

    #[test]
    fn the_bar_steps_green_amber_red_at_its_thresholds() {
        assert_eq!(
            progress_bar(0.5, 0.9, 1.0)["color"],
            color::hex(color::GREEN)
        );
        assert_eq!(
            progress_bar(0.899, 0.9, 1.0)["color"],
            color::hex(color::GREEN)
        );
        assert_eq!(
            progress_bar(0.9, 0.9, 1.0)["color"],
            color::hex(color::AMBER)
        );
        assert_eq!(progress_bar(1.0, 0.9, 1.0)["color"], color::hex(color::RED));
    }

    /// The one asymmetry worth pinning: an over-budget bar pins its width full
    /// and keeps its red. Clamping the colour too would paint 300% of budget
    /// exactly like 100% of it.
    #[test]
    fn an_over_budget_bar_pins_full_but_still_reads_red() {
        let bar = progress_bar(3.0, 0.9, 1.0);
        assert_eq!(bar["fraction"], 1.0);
        assert_eq!(bar["color"], color::hex(color::RED));
    }

    #[test]
    fn a_negative_fraction_floors_at_empty() {
        assert_eq!(progress_bar(-2.0, 0.9, 1.0)["fraction"], 0.0);
    }

    /// A zero denominator upstream yields `NaN`/`inf`, which is not a
    /// measurement. It must not travel as a width the frontend then drops
    /// silently, and it must not read as "over budget" either.
    #[test]
    fn a_non_finite_fraction_renders_an_empty_green_bar() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let bar = progress_bar(bad, 0.9, 1.0);
            assert_eq!(bar["fraction"], 0.0, "for {bad}");
            assert_eq!(bar["color"], color::hex(color::GREEN), "for {bad}");
        }
    }
}
