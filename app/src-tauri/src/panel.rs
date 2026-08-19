//! The two pieces of chrome every panel shares: the refresh-health footer
//! (`PanelStatusFooter`) and the thin progress
//! bar (`CockpitProgressBar`).
//!
//! The original passes the footer a `staleAfter` per panel — 30s for Containers, 150s
//! for GitHub Runners — and the *ladder* is identical for all of them. One
//! definition here means two panels can never disagree about whether an error
//! outranks staleness, or about where a minute becomes an hour. Same argument
//! for the bar: the Usage panel's Sentry quota and the Azure Cost panel's
//! budget are the same widget at the same thresholds.

use serde_json::{json, Value};
use viewmodel::color;
use viewmodel::format::relative_age;
use viewmodel::freshness::Freshness;

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
    footer_line(None, last_updated, error, now, stale_after)
}

/// [`status_footer`], with the section that raised it named.
///
/// For a panel whose body is several independently-polled sections sharing one
/// header line. Naming nothing is correct while a warning sits *inside* the
/// section it describes, and becomes a lie the moment it does not: the Usage
/// panel's Neon and Sentry sections emit the byte-identical
/// `⚠ stale · updated 23h ago`, so hoisting both to the header unattributed
/// produced a line that said the same thing twice and identified neither.
///
/// `source` is the section's **own id**, not a second display name: the
/// warning's job is to send a reader to a block on the card, and an attribution
/// free to drift from the id that block is keyed by would eventually point at
/// nothing.
#[must_use]
pub fn attributed_status_footer(
    source: &str,
    last_updated: Option<u64>,
    error: Option<&str>,
    now: u64,
    stale_after: u64,
) -> Value {
    footer_line(Some(source), last_updated, error, now, stale_after)
}

/// The ladder both spellings share.
///
/// `source` is spliced immediately after the `⚠`, so an unattributed line is
/// byte-identical to what it has always been and an attributed one reads
/// `⚠ neon: stale · updated 23h ago`. One ladder rather than two, for the reason
/// the module doc gives: two panels must never disagree about whether an error
/// outranks staleness, and neither must two spellings of the same panel.
fn footer_line(
    source: Option<&str>,
    last_updated: Option<u64>,
    error: Option<&str>,
    now: u64,
    stale_after: u64,
) -> Value {
    let subject = source.map_or_else(String::new, |source| format!("{source}: "));
    let text = match (error, last_updated) {
        (Some(error), Some(updated)) => Some(format!(
            "⚠ {subject}{error} · last ok {}",
            relative_age(now.saturating_sub(updated))
        )),
        (Some(error), None) => Some(format!("⚠ {subject}{error}")),
        (None, Some(updated)) if now.saturating_sub(updated) > stale_after => Some(format!(
            "⚠ {subject}stale · updated {}",
            relative_age(now.saturating_sub(updated))
        )),
        (None, _) => None,
    };
    match text {
        Some(text) => json!({ "text": text, "color": color::hex(color::AMBER) }),
        None => Value::Null,
    }
}

/// Several panel warnings folded into the one header line, or `Null` when none
/// of them fired.
///
/// A panel whose body is several independently-polled sections still has exactly
/// one header, and the header is the only place a warning costs no height —
/// which is the whole point (`.panel-stale` in `app.css`, and
/// [#351](https://github.com/Sassy-Dog/solador/issues/351)). A warning rendered
/// beside its section instead makes the card a line taller the moment it fires,
/// `.panel-row` stretches every other card in the row to match, and one Neon
/// read going stale rearranges the cockpit.
///
/// Order is the caller's, and should be the order the sections themselves render
/// in, so the line reads down the card.
///
/// **The separator is the `⚠` each segment already carries**, which is why they
/// are joined on a plain space. `·` is taken: [`status_footer`] uses it *inside*
/// a segment to separate the reason from the clock, so joining on it too would
/// make `⚠ neon: stale · updated 23h ago · ⚠ sentry: …` ambiguous about which
/// half belongs to which provider.
///
/// The colour is the first firing warning's rather than a constant restated
/// here: this composes what the ladder decided and does not get a second opinion
/// about it.
#[must_use]
pub fn merged_footer(footers: &[Value]) -> Value {
    merged_line(footers, " ")
}

/// Several sections' freshness clocks folded into the one header line, or `Null`
/// when every one of them is current.
///
/// The [`merged_footer`] argument, one clock over
/// ([#355](https://github.com/Sassy-Dog/solador/issues/355)). A per-section
/// `as of 23h ago` rendered beside its own rows is absent while the section is
/// healthy and present when it is not, so it costs the card a line of height the
/// moment a poll is missed — the identical growth the warnings were hoisted out
/// of the body to stop, one line instead of six. Segments come from
/// [`attributed_freshness_payload`], so each names the section it dates.
///
/// **The separator is `·`, not the space [`merged_footer`] joins on**, and the
/// difference is not cosmetic. A warning segment opens with a `⚠`, which is
/// itself the boundary between segments; a freshness segment opens with an
/// ordinary word, so `neon: as of 23h ago sentry: as of 23h ago` runs two
/// sentences together with nothing marking where one ends. `·` is free here for
/// the same reason it is taken there: [`freshness_payload`]'s line has no
/// interior `·` to be confused with.
#[must_use]
pub fn merged_freshness(clocks: &[Value]) -> Value {
    merged_line(clocks, " · ")
}

/// The fold both merges share: the firing segments' text joined, in the caller's
/// order, carrying the first firing segment's colour.
///
/// Order is the caller's, and should be the order the sections themselves render
/// in, so the line reads down the card. A segment that is not firing carries a
/// null `text` — or is `Null` outright — and contributes nothing; `Null` indexes
/// to `Null` rather than panicking, so a caller may pass either spelling.
///
/// The colour is the first firing segment's rather than a constant restated
/// here: this composes what the ladder (or [`Freshness::classify`]) decided and
/// does not get a second opinion about it.
fn merged_line(segments: &[Value], separator: &str) -> Value {
    let firing: Vec<&str> = segments.iter().filter_map(|s| s["text"].as_str()).collect();
    if firing.is_empty() {
        return Value::Null;
    }
    let color = segments
        .iter()
        .find(|s| !s["text"].is_null())
        .map_or(Value::Null, |s| s["color"].clone());
    json!({ "text": firing.join(separator), "color": color })
}

/// How old the figure a panel is painting is, as the frontend receives it.
///
/// **This is not [`status_footer`], and must not be folded into it.** They are
/// two clocks answering two questions: the footer asks *"did the last attempt
/// fail, or is the poller late"*, this asks *"how old is the number on screen
/// right now"*. A reading stops being live the moment one cadence has passed;
/// a poller is not worth warning about until it is later than that. In between,
/// a panel renders a dated, dimmed figure and **no** warning — which is
/// precisely the state neither field can express alone.
///
/// The shape is `{state, measured_secs_ago, text, color}`:
///
/// - `state` is `"live"` / `"stale"` / `"unknown"`, already decided by
///   [`Freshness::classify`]. The frontend branches on this string and never
///   compares an age to a cadence itself — the threshold lives in `viewmodel`,
///   where a test can see it.
/// - `measured_secs_ago` is `null` for [`Freshness::Unknown`] **and only
///   there**: nothing has ever been read, so there is no age. A `0` would paint
///   the never-read panel as the freshest thing in the cockpit.
/// - `text` is non-null only when the reading is stale, because that is the
///   only state with something to add: live is painted as it always was, and
///   unknown has no figure to qualify. It is built here rather than in JS for
///   the reason every other string is — [`relative_age`] is one definition, and
///   a second one in the frontend would be free to disagree about where a
///   minute becomes an hour.
#[must_use]
pub fn freshness_payload(freshness: Freshness) -> Value {
    // `stale_age` is deliberately not `measured_secs_ago` filtered after the
    // fact: the line and the age are two different questions, and one match
    // arm per variant is what makes "live carries an age but paints nothing"
    // legible rather than an omission a later edit would helpfully "fix".
    let (state, measured_secs_ago, stale_age) = match freshness {
        Freshness::Live { measured_secs_ago } => ("live", Some(measured_secs_ago), None),
        Freshness::Stale { measured_secs_ago } => {
            ("stale", Some(measured_secs_ago), Some(measured_secs_ago))
        }
        Freshness::Unknown => ("unknown", None, None),
    };
    json!({
        "state": state,
        "measured_secs_ago": measured_secs_ago,
        "text": stale_age.map(|secs| format!("as of {}", relative_age(secs))),
        "color": stale_age.map(|_| color::hex(color::AMBER)),
    })
}

/// [`freshness_payload`], with the section the clock dates named.
///
/// The [`attributed_status_footer`] argument applied to the other clock. A panel
/// whose body is several independently-polled sections has one header and one
/// clock per section, and those clocks are byte-identical the moment two
/// sections were last read at the same time — `as of 23h ago` twice over names
/// neither of them. `source` is the section's **own id** for the same reason the
/// warning's is: the line's job is to send a reader to a block on the card.
///
/// Only the line is renamed. `state` and `measured_secs_ago` are the
/// classification and belong to the section, not to the string, so they are
/// passed through untouched — and `text` stays null for
/// [`Freshness::Live`] and [`Freshness::Unknown`], which is what keeps a current
/// or never-read section out of [`merged_freshness`]'s line entirely rather than
/// contributing an attributed blank.
#[must_use]
pub fn attributed_freshness_payload(source: &str, freshness: Freshness) -> Value {
    let mut payload = freshness_payload(freshness);
    let named = payload["text"]
        .as_str()
        .map(|text| format!("{source}: {text}"));
    if let Some(named) = named {
        payload["text"] = Value::String(named);
    }
    payload
}

/// One thin progress bar: how much of the track to fill, and what colour.
///
/// Port of `CockpitProgressBar` (ported), including its one subtlety — **the
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

    /// Every panel's staleness window, pinned against the original panel that
    /// owns it.
    ///
    /// The ladder above being right is worth nothing if a panel hands it the
    /// wrong window, and a drifted constant is otherwise invisible: a footer
    /// that appears an hour late looks exactly like a panel that is simply
    /// fresh. the original ground truth, panel for panel:
    ///
    /// | panel | window | the original |
    /// |---|---|---|
    /// | Containers | 30s | `ContainersPanel` |
    /// | Runners | 150s | `GHRunnersPanel` |
    /// | Claude usage | 150s | `ClaudeUsagePanel` |
    /// | Neon + Sentry | 5400s | `ClaudeUsagePanel`, `:83` |
    /// | Azure Cost | 18000s | `AzureCostPanel` |
    ///
    /// Hosts, Repos and OpenClaw are absent on purpose — none of the three
    /// renders a status footer on either side. Hosts carries staleness on the
    /// per-card connection dot instead, Repos degrades the per-repo dot to
    /// unreachable, and OpenClaw is event-driven so it has no cadence to be
    /// late against.
    #[test]
    fn every_panels_staleness_window_matches_its_original_panel() {
        let windows: [(&str, u64, u64); 5] = [
            ("containers", crate::containers::STALE_AFTER_SECS, 30),
            ("runners", crate::github::RUNNERS_STALE_AFTER_SECS, 150),
            ("claude usage", crate::usage::CLAUDE_STALE_AFTER_SECS, 150),
            (
                "neon + sentry",
                crate::usage::PROVIDER_STALE_AFTER_SECS,
                90 * 60,
            ),
            // Azure Cost's window is derived from the operator's cadence
            // (#302), so what is pinned here is the *default* cadence still
            // producing the 5h the original panel passed.
            (
                "azure cost",
                crate::azure::stale_after_secs(u64::from(
                    store::settings::PanelInterval::AzureCost
                        .spec()
                        .default_secs,
                )),
                5 * 60 * 60,
            ),
        ];
        for (panel, window, expected) in windows {
            assert_eq!(
                window, expected,
                "{panel} drifted from its documented window"
            );
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

    // MARK: attribution

    /// Every arm of the ladder carries the source, and nothing else about the
    /// line moves: the reason, the clock and the order they appear in are the
    /// unattributed spelling's, verbatim.
    #[test]
    fn an_attributed_footer_names_its_source_on_every_arm() {
        assert_eq!(
            text_of(&attributed_status_footer(
                "neon",
                Some(NOW - 5400),
                None,
                NOW,
                150
            )),
            Some("⚠ neon: stale · updated 1h ago")
        );
        assert_eq!(
            text_of(&attributed_status_footer(
                "neon",
                Some(NOW - 240),
                Some("Neon API request failed (HTTP 500)"),
                NOW,
                150
            )),
            Some("⚠ neon: Neon API request failed (HTTP 500) · last ok 4m ago")
        );
        assert_eq!(
            text_of(&attributed_status_footer(
                "neon",
                None,
                Some("boom"),
                NOW,
                150
            )),
            Some("⚠ neon: boom")
        );
    }

    /// A healthy section names nothing, because there is nothing to name. The
    /// attribution is part of the warning, not a label the section always wears.
    #[test]
    fn an_attributed_footer_is_still_silent_when_the_section_is_healthy() {
        assert_eq!(
            attributed_status_footer("neon", Some(NOW), None, NOW, 150),
            Value::Null
        );
    }

    /// The unattributed spelling is byte-identical to what it always was — the
    /// refactor that introduced `source` must not have moved a single character
    /// of the line every other panel renders.
    #[test]
    fn the_unattributed_spelling_is_unchanged() {
        for (updated, error) in [
            (Some(NOW - 240), Some("couldn't read runners")),
            (None, Some("couldn't read runners")),
            (Some(NOW - 90), None),
        ] {
            assert_eq!(
                status_footer(updated, error, NOW, 30),
                footer_line(None, updated, error, NOW, 30)
            );
        }
        assert_eq!(
            text_of(&status_footer(Some(NOW - 90), None, NOW, 30)),
            Some("⚠ stale · updated 1m ago")
        );
    }

    // MARK: merged_footer

    /// The failure attribution exists to prevent: two sections emitting the
    /// *byte-identical* line. Merged without a source they say the same thing
    /// twice and identify neither; merged with one they are two facts.
    #[test]
    fn two_sections_with_the_same_symptom_stay_distinguishable() {
        let neon = attributed_status_footer("neon", Some(NOW - 82_800), None, NOW, 5400);
        let sentry = attributed_status_footer("sentry", Some(NOW - 82_800), None, NOW, 5400);
        assert_ne!(neon["text"], sentry["text"]);
        assert_eq!(
            merged_footer(&[neon, sentry])["text"],
            "⚠ neon: stale · updated 23h ago ⚠ sentry: stale · updated 23h ago"
        );
    }

    /// The panel's own warning and its sections' share the one line, in the
    /// order they were handed over — which is the order the card reads down.
    /// Neither is dropped when both are present.
    #[test]
    fn the_panels_own_warning_and_its_sections_share_the_line() {
        let claude = status_footer(Some(NOW - 600), None, NOW, 150);
        let neon = attributed_status_footer("neon", None, Some("boom"), NOW, 5400);
        let merged = merged_footer(&[claude, Value::Null, neon]);
        assert_eq!(
            merged["text"], "⚠ stale · updated 10m ago ⚠ neon: boom",
            "the healthy section between them contributes nothing at all"
        );
        assert_eq!(merged["color"], color::hex(color::AMBER));
    }

    /// Nothing fired, so there is no line — not an empty string, which would
    /// still be a rendered element and still cost the header a warning colour.
    #[test]
    fn a_panel_with_nothing_to_report_merges_to_null() {
        assert_eq!(merged_footer(&[]), Value::Null);
        assert_eq!(
            merged_footer(&[Value::Null, status_footer(Some(NOW), None, NOW, 150)]),
            Value::Null
        );
    }

    /// The colour is carried over from the ladder, never restated here — so a
    /// later change to what a warning looks like cannot leave this function
    /// painting the old one.
    #[test]
    fn the_merged_colour_comes_from_the_first_firing_warning() {
        let firing = status_footer(Some(NOW - 600), None, NOW, 150);
        let expected = firing["color"].clone();
        assert_eq!(merged_footer(&[Value::Null, firing])["color"], expected);
    }

    // MARK: freshness_payload

    #[test]
    fn a_live_reading_publishes_its_age_and_nothing_to_paint() {
        let payload = freshness_payload(Freshness::classify(Some(600), 4 * 3600));
        assert_eq!(payload["state"], "live");
        assert_eq!(payload["measured_secs_ago"], 600);
        assert!(payload["text"].is_null(), "live renders as it always did");
        assert!(payload["color"].is_null());
    }

    #[test]
    fn a_stale_reading_publishes_the_state_the_age_and_the_as_of_line() {
        let payload = freshness_payload(Freshness::classify(Some(23 * 3600), 4 * 3600));
        assert_eq!(payload["state"], "stale");
        assert_eq!(payload["measured_secs_ago"], 23 * 3600);
        assert_eq!(payload["text"], "as of 23h ago");
        assert_eq!(payload["color"], color::hex(color::AMBER));
    }

    /// The failure this shape exists to prevent: never-measured degrading into
    /// an age of zero, which would publish as the freshest reading on the panel.
    #[test]
    fn a_never_measured_reading_publishes_no_age_at_all() {
        let payload = freshness_payload(Freshness::Unknown);
        assert_eq!(payload["state"], "unknown");
        assert!(payload["measured_secs_ago"].is_null());
        assert_ne!(payload["measured_secs_ago"], 0);
        assert!(payload["text"].is_null(), "there is no figure to qualify");
    }

    // MARK: attributed freshness

    /// The line is named and the classification is not: `state` and
    /// `measured_secs_ago` describe the section, `text` is the string a header
    /// carrying several of them has to keep apart.
    #[test]
    fn an_attributed_clock_names_only_its_line() {
        let payload =
            attributed_freshness_payload("neon", Freshness::classify(Some(23 * 3600), 3600));
        assert_eq!(payload["state"], "stale");
        assert_eq!(payload["measured_secs_ago"], 23 * 3600);
        assert_eq!(payload["text"], "neon: as of 23h ago");
        assert_eq!(payload["color"], color::hex(color::AMBER));
    }

    /// A current or never-read section is *not* labelled with its own name and
    /// left blank — it publishes no line at all, exactly as the unattributed
    /// spelling does, so it contributes nothing to the merged header.
    #[test]
    fn an_attributed_clock_is_still_silent_when_there_is_nothing_to_date() {
        for freshness in [Freshness::classify(Some(600), 3600), Freshness::Unknown] {
            let payload = attributed_freshness_payload("neon", freshness);
            assert!(
                payload["text"].is_null(),
                "{freshness:?} must not publish an attributed blank"
            );
            assert_eq!(
                payload,
                freshness_payload(freshness),
                "{freshness:?} is byte-identical to the unattributed spelling"
            );
        }
    }

    // MARK: merged_freshness

    /// The reason these clocks are merged rather than rendered one per section:
    /// two sections last read at the same moment emit the byte-identical line,
    /// and the header has to say which is which.
    #[test]
    fn two_sections_dated_alike_stay_distinguishable_on_the_one_line() {
        let stale = Freshness::classify(Some(23 * 3600), 3600);
        let neon = attributed_freshness_payload("neon", stale);
        let sentry = attributed_freshness_payload("sentry", stale);
        assert_ne!(neon["text"], sentry["text"]);
        assert_eq!(
            freshness_payload(stale)["text"],
            "as of 23h ago",
            "and unattributed they would be this line, twice"
        );
        assert_eq!(
            merged_freshness(&[neon, sentry])["text"],
            "neon: as of 23h ago · sentry: as of 23h ago"
        );
    }

    /// The separator is the one thing that differs from [`merged_footer`], and
    /// it differs because a freshness segment carries no `⚠` to mark where the
    /// previous one ended.
    #[test]
    fn freshness_segments_are_separated_and_warnings_are_not() {
        let stale = Freshness::classify(Some(23 * 3600), 3600);
        let clocks = merged_freshness(&[
            attributed_freshness_payload("neon", stale),
            attributed_freshness_payload("sentry", stale),
        ]);
        assert!(clocks["text"].as_str().unwrap().contains(" · "));

        let warnings = merged_footer(&[
            attributed_status_footer("neon", Some(NOW - 82_800), None, NOW, 5400),
            attributed_status_footer("sentry", Some(NOW - 82_800), None, NOW, 5400),
        ]);
        assert!(!warnings["text"].as_str().unwrap().contains("ago · ⚠"));
    }

    /// Nothing is dated, so there is no line — not an empty one, which would
    /// still be a rendered element reserving a header's worth of amber.
    #[test]
    fn a_panel_whose_sections_are_all_current_merges_to_null() {
        assert_eq!(merged_freshness(&[]), Value::Null);
        assert_eq!(
            merged_freshness(&[
                attributed_freshness_payload("neon", Freshness::classify(Some(600), 3600)),
                attributed_freshness_payload("sentry", Freshness::Unknown),
            ]),
            Value::Null
        );
    }

    /// One dated section among current ones contributes one segment and no
    /// separator — the line names what it can date and invents nothing for the
    /// rest.
    #[test]
    fn a_current_section_contributes_no_segment() {
        let merged = merged_freshness(&[
            attributed_freshness_payload("neon", Freshness::classify(Some(600), 3600)),
            attributed_freshness_payload("sentry", Freshness::classify(Some(23 * 3600), 3600)),
        ]);
        assert_eq!(merged["text"], "sentry: as of 23h ago");
        assert_eq!(merged["color"], color::hex(color::AMBER));
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
