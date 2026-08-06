//! The user's cockpit arrangement — which panel sits where, how wide, and at
//! which window widths.
//!
//! A **profile** is one arrangement plus the width it starts applying at, and
//! the store holds a list of them. The cockpit picks the widest profile whose
//! `min_width` the window clears, so "hosts as tabs in a third-of-a-4K column,
//! side by side when I maximise" is two profiles rather than one compromise.
//!
//! Inside a profile the arrangement is an **ordered list of slots**, not rows: a
//! slot is a panel id plus a span name, and the rows fall out of packing those
//! spans (a row holds four quarters). One list is what an editor can move an
//! entry up and down in; rows would make "move this panel" mean two different
//! operations depending on where in a row it sat.
//!
//! This crate deliberately does **not** know what a valid panel id, span name or
//! overflow mode is. Those are `viewmodel::cockpit`'s and `settings`'
//! vocabulary, and what lands here is strings — the app validates them on the
//! way in and on the way out. A file naming a panel this build has never heard
//! of is a file from a newer build, and it degrades to the default layout rather
//! than to a cockpit with a hole in it.

use serde::{Deserialize, Deserializer, Serialize};

/// One panel's placement: which panel, and how much of a row it takes.
///
/// Both fields are plain strings for the reason in the module docs — the
/// spelling is `viewmodel`'s, and the validation lives with it.
///
/// `panel` carries no `#[serde(default)]`, which makes it this shape's
/// discriminator: [`lenient_layout`]'s legacy path accepts any array that
/// parses as slots, and with every field optional *any* array of objects would.
/// A slot that does not even name a panel is not a slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutSlot {
    /// A `PanelKind::id` — `hosts`, `ghWorkflows`, …
    pub panel: String,
    /// A `PanelSpan::as_str` — `full`, `half`, `quarter`.
    #[serde(default)]
    pub span: String,
}

impl LayoutSlot {
    #[must_use]
    pub fn new(panel: impl Into<String>, span: impl Into<String>) -> Self {
        LayoutSlot {
            panel: panel.into(),
            span: span.into(),
        }
    }
}

/// One arrangement and the window width it starts applying at.
///
/// `slots` carries no `#[serde(default)]` **on purpose**, and it is what tells
/// this shape apart from the legacy one: before profiles existed, `layout` was a
/// bare array of [`LayoutSlot`]. Every field of a slot is optional, so a slot
/// would otherwise deserialize happily as an all-default profile and a saved
/// layout would silently become an empty one. Requiring `slots` makes the legacy
/// array fail this shape cleanly, which is the fallback [`lenient_layout`] takes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutProfile {
    /// The narrowest cockpit width this profile applies at. The lowest profile
    /// also covers everything below itself — there is always something to
    /// render, whatever the window does.
    #[serde(default)]
    pub min_width: f64,
    /// `HostOverflowMode::as_str` — `stack` or `tabs`. Per profile, because
    /// "tabs when narrow, side by side when wide" is the whole point.
    #[serde(default)]
    pub host_overflow: String,
    pub slots: Vec<LayoutSlot>,
}

impl LayoutProfile {
    #[must_use]
    pub fn new(min_width: f64, host_overflow: impl Into<String>, slots: Vec<LayoutSlot>) -> Self {
        LayoutProfile {
            min_width,
            host_overflow: host_overflow.into(),
            slots,
        }
    }
}

/// The layout as it arrives from the file, tolerating both a value this build
/// cannot read and one an older build wrote.
///
/// Two shapes are accepted. A list of [`LayoutProfile`]s is today's. A bare list
/// of [`LayoutSlot`]s is what shipped before breakpoints existed, and it becomes
/// **one profile at width 0** — the same arrangement at every size, which is
/// exactly what that file meant. Its `host_overflow` is left empty for the app
/// to fill from the General preference that used to own that decision, so
/// migrating costs the user nothing and loses nothing.
///
/// Anything else reads as "never configured", the same rule
/// [`crate::containers::lenient_rules`] follows: a malformed layout must not
/// take the *whole store* down with it, and the default arrangement is the one
/// answer that is always renderable.
pub(crate) fn lenient_layout<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<LayoutProfile>>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(value) = Option::<serde_json::Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    if let Ok(profiles) = serde_json::from_value::<Vec<LayoutProfile>>(value.clone()) {
        return Ok(Some(profiles));
    }
    Ok(serde_json::from_value::<Vec<LayoutSlot>>(value)
        .ok()
        .map(|slots| vec![LayoutProfile::new(0.0, String::new(), slots)]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct Holder {
        #[serde(default, deserialize_with = "lenient_layout")]
        layout: Option<Vec<LayoutProfile>>,
    }

    fn layout_of(json: &str) -> Option<Vec<LayoutProfile>> {
        serde_json::from_str::<Holder>(json)
            .expect("the holder itself must load")
            .layout
    }

    fn slots() -> Vec<LayoutSlot> {
        vec![
            LayoutSlot::new("hosts", "full"),
            LayoutSlot::new("claudeUsage", "quarter"),
        ]
    }

    #[test]
    fn a_written_layout_round_trips_in_order() {
        let profiles = vec![
            LayoutProfile::new(0.0, "tabs", slots()),
            LayoutProfile::new(1400.0, "stack", slots()),
        ];
        let json = serde_json::to_string(&profiles).expect("serialize");
        assert_eq!(
            serde_json::from_str::<Vec<LayoutProfile>>(&json).expect("deserialize"),
            profiles,
            "order, widths and overflow are the whole content of this value"
        );
    }

    #[test]
    fn an_absent_or_null_layout_is_never_configured() {
        assert_eq!(layout_of("{}"), None);
        assert_eq!(layout_of(r#"{"layout": null}"#), None);
    }

    /// The forward-compatibility rule, at the one place it is load-bearing: a
    /// layout this build cannot parse leaves the rest of the store readable.
    #[test]
    fn an_unreadable_layout_does_not_take_the_store_down() {
        assert_eq!(layout_of(r#"{"layout": "nonsense"}"#), None);
        assert_eq!(layout_of(r#"{"layout": [{"slots": 7}]}"#), None);
    }

    /// The migration that matters: a file written before breakpoints existed
    /// carried a bare slot array, and it means "this arrangement at every
    /// width" — one profile at 0.
    ///
    /// Its `host_overflow` is deliberately left empty rather than guessed:
    /// that decision lived in the General preference at the time, and only the
    /// app can read it.
    #[test]
    fn a_legacy_slot_array_becomes_one_profile_at_width_zero() {
        let legacy = r#"{"layout": [{"panel": "hosts", "span": "full"},
                                    {"panel": "claudeUsage", "span": "quarter"}]}"#;
        assert_eq!(
            layout_of(legacy),
            Some(vec![LayoutProfile::new(0.0, "", slots())])
        );
    }

    /// The discriminator, pinned: every field of a slot is optional, so without
    /// `slots` being required a legacy array would deserialize as a list of
    /// all-default profiles and silently become an empty layout.
    #[test]
    fn a_slot_never_passes_for_a_profile() {
        assert!(
            serde_json::from_value::<Vec<LayoutProfile>>(
                serde_json::json!([{ "panel": "hosts", "span": "full" }])
            )
            .is_err(),
            "a slot must not parse as a profile, or the migration silently drops it"
        );
    }

    /// An *empty* list is not the same as an absent one — but it is also not a
    /// layout anyone can render, so the app treats it as "use the default"
    /// (`normalized_profiles` fills it back out). Nothing is dropped here,
    /// because this layer is storage and not policy.
    #[test]
    fn an_empty_list_survives_as_an_empty_list() {
        assert_eq!(layout_of(r#"{"layout": []}"#), Some(vec![]));
    }

    /// A profile missing everything but its slots still parses — width 0, no
    /// overflow opinion. The app's validation decides what those mean.
    #[test]
    fn a_partial_profile_parses_and_is_completed_later() {
        assert_eq!(
            layout_of(r#"{"layout": [{"slots": [{"panel": "hosts"}]}]}"#),
            Some(vec![LayoutProfile::new(
                0.0,
                "",
                vec![LayoutSlot::new("hosts", "")]
            )])
        );
    }
}
