//! Operator-added status vendors — the Services panel's list, as data.
//!
//! The panel's five vendors are a closed enum in `app/src-tauri/src/services.rs`,
//! so a stack that depends on Railway, Resend or Clerk cannot watch them without
//! a rebuild — and with releases blocked (#15), "without a rebuild" means "not at
//! all". This module is the record that moves that list out of code; the epic is
//! #284, and it adds no transport: `crates/servicestatus`'s Statuspage adapter is
//! already parameterized by `base_url`.
//!
//! Shaped after [`crate::Host`] and [`crate::VendorAccount`] — a `Uuid`-keyed
//! entry the operator owns, in the order they arranged it. Unlike both of those
//! it names **no credential**: a Statuspage summary is public, which is the
//! property that gives this tier no onboarding ceiling.
//!
//! This module is the type alone. The probe that discovers components, the
//! Settings surface and the panel wiring are later slices of #284.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One operator-added vendor: where to read its status page, and **which
/// component of it this cockpit actually depends on**.
///
/// A vendor is not a URL. `CLAUDE_API_COMPONENT` names the API rather than
/// `claude.ai` because this stack calls the API and the web app can be down
/// while it is fine; `VERCEL_BUILDS_COMPONENT` names Builds rather than the edge
/// network because the Vercel outage this cockpit cares about is one that stops
/// a deploy. That judgement is the value in an entry, and it is the operator's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusVendor {
    /// Stable identity. Deliberately *not* `#[serde(default)]`, for the same
    /// reason [`crate::Host::id`] isn't: a nil-UUID fallback would let two
    /// id-less entries collapse onto one record.
    pub id: Uuid,
    /// Operator-chosen display name, e.g. `Railway`.
    pub label: String,
    /// Scheme and host only, e.g. `https://status.railway.app`. The adapter
    /// appends its own path (`/api/v2/summary.json`).
    pub base_url: String,
    /// The component this vendor is watched through, as the status page names
    /// it — opaque, e.g. `k8w3r06qmzrp`.
    ///
    /// **Stored, never re-derived.** Re-resolving it by name on each poll would
    /// mean a vendor renaming a component silently repoints the tile at a
    /// different one, and the panel would keep looking like it was working.
    pub component_id: String,
    /// The component's name at the moment the operator picked it.
    ///
    /// Carried *beside* the id, not in place of it, and this is the pairing
    /// that makes drift noticeable: the id is what is polled, the label is what
    /// the operator recognises. When the two stop agreeing, that disagreement
    /// is visible instead of being resolved by guesswork — the alternative,
    /// keeping only the id, hides the rename entirely, and keeping only the
    /// label makes the tile follow whatever the vendor renames next.
    pub component_label: String,
    /// Disabled vendors stay configured but are not polled.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

impl StatusVendor {
    /// A new vendor under a fresh id, enabled, recording the component it was
    /// handed — both halves of it.
    #[must_use]
    pub fn new(
        label: impl Into<String>,
        base_url: impl Into<String>,
        component_id: impl Into<String>,
        component_label: impl Into<String>,
    ) -> Self {
        StatusVendor {
            id: Uuid::new_v4(),
            label: label.into(),
            base_url: base_url.into(),
            component_id: component_id.into(),
            component_label: component_label.into(),
            enabled: true,
        }
    }
}

const fn enabled_by_default() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vendor_stores_the_component_it_was_given() {
        let v = StatusVendor::new("Railway", "https://status.railway.app", "abc123", "API");
        assert_eq!(v.component_id, "abc123");
        assert_eq!(v.component_label, "API");
        assert!(v.enabled);
    }

    #[test]
    fn vendors_round_trip_through_json() {
        let v = StatusVendor::new("Railway", "https://status.railway.app", "abc123", "API");
        let text = serde_json::to_string(&v).expect("serialize");
        assert_eq!(serde_json::from_str::<StatusVendor>(&text).expect("de"), v);
    }

    #[test]
    fn ids_are_unique_per_vendor() {
        let a = StatusVendor::new("Railway", "https://status.railway.app", "abc123", "API");
        let b = StatusVendor::new("Resend", "https://resend-status.com", "def456", "Sending");
        assert_ne!(a.id, b.id);
        assert_ne!(a.id, Uuid::nil());
    }

    #[test]
    fn an_entry_without_an_id_is_rejected() {
        let json = r#"{"label":"Railway","base_url":"https://status.railway.app","component_id":"abc123","component_label":"API"}"#;
        assert!(
            serde_json::from_str::<StatusVendor>(json).is_err(),
            "a vendor with no identity must not load — two of them would collapse onto one record"
        );
    }

    #[test]
    fn a_vendor_is_enabled_when_the_flag_is_absent() {
        let id = Uuid::new_v4();
        let json = format!(
            r#"{{"id":"{id}","label":"Railway","base_url":"https://status.railway.app","component_id":"abc123","component_label":"API"}}"#
        );
        let vendor: StatusVendor = serde_json::from_str(&json).expect("deserialize");
        assert!(vendor.enabled);
    }

    /// The component the operator picked is what loads back, whatever the
    /// vendor has since renamed it to. Written as raw JSON because the point is
    /// what a *stored* entry does, not what the constructor does.
    #[test]
    fn a_stored_component_is_not_re_derived_from_the_label() {
        let id = Uuid::new_v4();
        let json = format!(
            r#"{{"id":"{id}","label":"Railway","base_url":"https://status.railway.app","component_id":"abc123","component_label":"API"}}"#
        );
        let vendor: StatusVendor = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(vendor.component_id, "abc123");
        assert_eq!(
            vendor.component_label, "API",
            "the label the operator picked is kept, so a rename is visible rather than silent"
        );
    }

    #[test]
    fn unknown_fields_are_tolerated() {
        let id = Uuid::new_v4();
        let json = format!(
            r#"{{"id":"{id}","label":"Railway","base_url":"https://status.railway.app","component_id":"abc123","component_label":"API","probed_at":"2026-08-14T00:00:00Z"}}"#
        );
        let vendor: StatusVendor = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(vendor.label, "Railway");
    }
}
