//! Every word and colour of the Usage panel's **plan** rows: a Claude or Codex
//! subscription's own limit windows, as `crates/subscription` reads them.
//!
//! The types here mirror that crate's without depending on it. It carries an
//! HTTP stack and the view layer needs none, which is the arrangement
//! [`crate::agent_release`] has with `crates/agentrelease`.
//!
//! # A plan row is not a token row
//!
//! The panel's `5H` / `WEEK` token rows are this machine's consumption, from
//! local logs, with no ceiling and so no bar. A plan row is the subscription's
//! utilisation of its own allowance, which has a ceiling the service states.
//! The labels keep them apart: `CLAUDE PLAN (5H)` is never `5H`.
//!
//! # Unknown is not zero, and a past reset is not a present reading
//!
//! - An absent window has no row; nothing here makes a 0%.
//! - A window with no reset instant has no countdown, never "resets now".
//! - A window whose reset has already passed is **muted, with no bar**. Its
//!   percentage was true of a window that has since rolled over, so an old
//!   100% must not paint a red that is no longer the case.
//! - A window with no stated length is named by its slot (`PRIMARY`,
//!   `SECONDARY`), never given a `5H` or `WEEK` it did not state.

use crate::color;

/// The muted value of a provider that is on and still waiting on its first read.
pub const LOADING_VALUE: &str = "reading…";

/// The value of a provider with nothing to show: a failure before any reading,
/// or a reading with no windows. The header line carries the reason.
pub const NO_READING_VALUE: &str = "—";

/// The bar's thresholds, as fractions. The same edges as [`color::usage_color`]
/// (70% and 90%, the CPU/RAM meter defaults), so a row's text and its bar can
/// never disagree about when amber starts.
pub const BAR_AMBER_AT: f64 = 0.7;
pub const BAR_RED_AT: f64 = 0.9;

/// How long a window is. Mirrors `subscription::WindowSpan`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Span {
    /// A stated length, in whole minutes.
    Minutes(u32),
    /// A window the service sent without a length, in its first slot.
    Primary,
    /// The same, in its second slot.
    Secondary,
}

/// One window. Mirrors `subscription::PlanWindow`, with the reset as Unix
/// seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub span: Span,
    /// The model a window is scoped to (`"Fable"`), or `None` for the whole plan.
    pub scope: Option<String>,
    /// Utilisation, as the service reported it: finite and non-negative, and
    /// possibly above 100.
    pub used_percent: f64,
    pub resets_at: Option<i64>,
}

/// One rendered row.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub label: String,
    pub value: String,
    pub color: u32,
    /// The bar's fill, as a fraction, or `None` for no bar.
    pub bar: Option<f64>,
}

/// A window length as a label: `5H`, `WEEK`, else the largest whole unit.
#[must_use]
pub fn span_label(span: Span) -> String {
    match span {
        Span::Minutes(300) => "5H".to_owned(),
        Span::Minutes(10_080) => "WEEK".to_owned(),
        Span::Minutes(m) if m > 0 && m % 1_440 == 0 => format!("{}D", m / 1_440),
        Span::Minutes(m) if m > 0 && m % 60 == 0 => format!("{}H", m / 60),
        Span::Minutes(m) => format!("{m}M"),
        Span::Primary => "PRIMARY".to_owned(),
        Span::Secondary => "SECONDARY".to_owned(),
    }
}

/// `CLAUDE PLAN (5H)`, `CLAUDE PLAN (WEEK · FABLE)`.
#[must_use]
pub fn label(provider: &str, window: &Window) -> String {
    let span = span_label(window.span);
    match &window.scope {
        Some(scope) => format!("{provider} PLAN ({span} · {})", scope.to_uppercase()),
        None => format!("{provider} PLAN ({span})"),
    }
}

/// Time until a reset, to two units: `45s`, `14m`, `2h 14m`, `3d 4h`.
#[must_use]
pub fn countdown(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3_600, secs % 3_600 / 60);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{m}m")
    } else if secs < 86_400 {
        format!("{h}h {m}m")
    } else {
        format!("{d}d {h}h")
    }
}

/// One window's row, at render time `now` (Unix seconds).
#[must_use]
pub fn row(provider: &str, window: &Window, now: i64) -> Row {
    let used = window.used_percent;
    let percent = format!("{used:.0}%");
    let label = label(provider, window);
    match window.resets_at {
        Some(at) if at <= now => Row {
            label,
            value: format!("{percent} · reset since read"),
            color: color::MUTED,
            bar: None,
        },
        reset => Row {
            label,
            value: match reset {
                Some(at) => format!(
                    "{percent} · resets in {}",
                    countdown(u64::try_from(at - now).unwrap_or(0))
                ),
                None => percent,
            },
            color: color::usage_color(used),
            bar: Some((used / 100.0).clamp(0.0, 1.0)),
        },
    }
}

/// The provider's single row while it has no window to show.
#[must_use]
pub fn placeholder_row(provider: &str, loading: bool) -> Row {
    Row {
        label: format!("{provider} PLAN"),
        value: if loading {
            LOADING_VALUE
        } else {
            NO_READING_VALUE
        }
        .to_owned(),
        color: color::MUTED,
        bar: None,
    }
}

/// The one setup line for every provider that is off, or `None` when all are
/// on. One line however many are off, so turning the feature off costs the
/// panel one row, not one per provider.
#[must_use]
pub fn off_line(off: &[&str]) -> Option<String> {
    let names = match off {
        [] => return None,
        [one] => (*one).to_owned(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    };
    Some(format!("{names} plan limits are off — turn on in Settings"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_792_843_200; // 2026-10-24T12:00:00Z

    fn window(span: Span, used: f64, resets_at: Option<i64>) -> Window {
        Window {
            span,
            scope: None,
            used_percent: used,
            resets_at,
        }
    }

    #[test]
    fn spans_read_as_the_plan_names_them_and_unstated_ones_keep_their_slot() {
        assert_eq!(span_label(Span::Minutes(300)), "5H");
        assert_eq!(span_label(Span::Minutes(10_080)), "WEEK");
        assert_eq!(span_label(Span::Minutes(1_440)), "1D");
        assert_eq!(span_label(Span::Minutes(120)), "2H");
        assert_eq!(span_label(Span::Minutes(2)), "2M");
        assert_eq!(span_label(Span::Primary), "PRIMARY");
        assert_eq!(span_label(Span::Secondary), "SECONDARY");
    }

    #[test]
    fn a_label_names_the_plan_the_span_and_any_model_scope() {
        let session = window(Span::Minutes(300), 1.0, None);
        assert_eq!(label("CLAUDE", &session), "CLAUDE PLAN (5H)");
        let scoped = Window {
            scope: Some("Fable".into()),
            ..window(Span::Minutes(10_080), 1.0, None)
        };
        assert_eq!(label("CLAUDE", &scoped), "CLAUDE PLAN (WEEK · FABLE)");
        assert_eq!(
            label("CODEX", &window(Span::Primary, 1.0, None)),
            "CODEX PLAN (PRIMARY)"
        );
    }

    #[test]
    fn a_countdown_has_two_units_at_most() {
        assert_eq!(countdown(0), "0s");
        assert_eq!(countdown(45), "45s");
        assert_eq!(countdown(14 * 60 + 59), "14m");
        assert_eq!(countdown(2 * 3600 + 14 * 60), "2h 14m");
        assert_eq!(countdown(3 * 3600), "3h 0m");
        assert_eq!(countdown(3 * 86_400 + 4 * 3600 + 59), "3d 4h");
    }

    /// The CPU/RAM thresholds: green under 70, amber under 90, red from 90.
    #[test]
    fn a_live_window_reads_its_percentage_and_countdown_coloured_like_a_meter() {
        let r = row(
            "CLAUDE",
            &window(Span::Minutes(300), 42.0, Some(NOW + 2 * 3600 + 14 * 60)),
            NOW,
        );
        assert_eq!(r.label, "CLAUDE PLAN (5H)");
        assert_eq!(r.value, "42% · resets in 2h 14m");
        assert_eq!(r.color, color::GREEN);
        assert_eq!(r.bar, Some(0.42));

        for (used, expected) in [
            (69.4, color::GREEN),
            (70.0, color::AMBER),
            (90.0, color::RED),
        ] {
            let r = row(
                "CODEX",
                &window(Span::Minutes(300), used, Some(NOW + 60)),
                NOW,
            );
            assert_eq!(r.color, expected, "{used}");
        }
    }

    /// No reset instant is no countdown, never "resets now".
    #[test]
    fn a_window_without_a_reset_has_no_countdown() {
        let r = row("CODEX", &window(Span::Minutes(10_080), 9.0, None), NOW);
        assert_eq!(r.value, "9%");
        assert_eq!(r.bar, Some(0.09));
    }

    /// The reading is of a window that has rolled over since.
    #[test]
    fn a_window_whose_reset_has_passed_is_muted_and_barless() {
        for resets_at in [NOW, NOW - 600] {
            let r = row(
                "CLAUDE",
                &window(Span::Minutes(300), 100.0, Some(resets_at)),
                NOW,
            );
            assert_eq!(r.value, "100% · reset since read");
            assert_eq!(r.color, color::MUTED);
            assert_eq!(r.bar, None);
        }
    }

    /// Above 100 is reported as the service said it; the bar is full, not over.
    #[test]
    fn an_overrun_is_reported_and_the_bar_is_simply_full() {
        let r = row(
            "CLAUDE",
            &window(Span::Minutes(300), 104.0, Some(NOW + 60)),
            NOW,
        );
        assert_eq!(r.value, "104% · resets in 1m");
        assert_eq!(r.color, color::RED);
        assert_eq!(r.bar, Some(1.0));
    }

    #[test]
    fn a_provider_without_windows_says_reading_or_shows_the_dash() {
        let loading = placeholder_row("CLAUDE", true);
        assert_eq!(loading.label, "CLAUDE PLAN");
        assert_eq!(loading.value, LOADING_VALUE);
        assert_eq!(loading.color, color::MUTED);
        assert_eq!(loading.bar, None);
        let none = placeholder_row("CODEX", false);
        assert_eq!(none.value, NO_READING_VALUE);
        assert_eq!(none.color, color::MUTED);
    }

    /// Off is a setup instruction, never the dash a failure renders.
    #[test]
    fn the_off_line_names_every_provider_that_is_off_and_how_to_turn_it_on() {
        assert_eq!(off_line(&[]), None);
        assert_eq!(
            off_line(&["Claude"]).as_deref(),
            Some("Claude plan limits are off — turn on in Settings")
        );
        assert_eq!(
            off_line(&["Claude", "Codex"]).as_deref(),
            Some("Claude and Codex plan limits are off — turn on in Settings")
        );
    }
}
