//! Vendor accounts — a credential per *account*, not per vendor.
//!
//! Modelled on [`crate::Host`], the one place this crate already got the
//! cardinality right: connection details persisted here, the secret itself in
//! the OS credential store, keyed per entity. Every other vendor is 1:1 today
//! ([`crate::SecretKey::GitHubAccessToken`] and friends name exactly one
//! credential each), which makes "this repo belongs to my other GitHub
//! account" unrepresentable rather than unimplemented — see #283.
//!
//! This module is the type alone. The parameterized secret key is #287, the
//! v1 → v2 schema migration and repo attribution are #288.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Which vendor an account authenticates against.
///
/// **Strict on the way in, deliberately.** Unlike
/// [`crate::ContainerRuleAction`] — where an unrecognised string degrades to a
/// safe default rather than discarding the user's whole rule list — an unknown
/// vendor has no safe default: reading it as `GitHub` would point a credential
/// at an API it does not belong to. Unknown is refused, and the caller decides
/// what to do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VendorKind {
    /// GitHub, behind the Repos and GitHub Runners panels.
    GitHub,
}

/// One vendor account: an operator-labelled identity plus the name of the
/// credential-store item holding its token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VendorAccount {
    /// Stable identity. Deliberately *not* `#[serde(default)]`, for the same
    /// reason [`crate::Host::id`] isn't: a nil-UUID fallback would let two
    /// id-less entries collapse onto one credential.
    pub id: Uuid,
    /// Which vendor this account authenticates against.
    pub vendor: VendorKind,
    /// Operator-chosen display name, e.g. the account handle.
    pub label: String,
    /// Disabled accounts stay configured but are not polled.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    /// The credential-store item name this account's token lives under.
    ///
    /// **Stored, not derived from [`Self::id`]** — the load-bearing decision
    /// in #283. The account the v1 → v2 migration creates keeps pointing at
    /// the pre-existing `github_access_token` item, so that migration performs
    /// **no credential-store writes at all**; only accounts created afterwards
    /// get a `vendor-<uuid>` item of their own. Renaming a keychain item
    /// during a store migration is the `LEGACY_SERVICE` hazard in miniature:
    /// orphaned is worse than deleted.
    pub secret_account: String,
}

impl VendorAccount {
    /// A new account under a fresh id, enabled, reading the credential-store
    /// item it is handed.
    #[must_use]
    pub fn new(
        vendor: VendorKind,
        label: impl Into<String>,
        secret_account: impl Into<String>,
    ) -> Self {
        VendorAccount {
            id: Uuid::new_v4(),
            vendor,
            label: label.into(),
            enabled: true,
            secret_account: secret_account.into(),
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
    fn a_new_account_records_the_keychain_item_it_was_given() {
        let a = VendorAccount::new(VendorKind::GitHub, "cpmadrid", "vendor-abc");
        assert_eq!(a.secret_account, "vendor-abc");
        assert!(a.enabled);
        assert_eq!(a.vendor, VendorKind::GitHub);
    }

    #[test]
    fn accounts_round_trip_through_json() {
        let a = VendorAccount::new(VendorKind::GitHub, "work", "github_access_token");
        let text = serde_json::to_string(&a).expect("serialize");
        assert_eq!(serde_json::from_str::<VendorAccount>(&text).expect("de"), a);
    }

    #[test]
    fn an_entry_without_an_id_is_rejected() {
        let json = r#"{"vendor":"github","label":"work","secret_account":"github_access_token"}"#;
        assert!(
            serde_json::from_str::<VendorAccount>(json).is_err(),
            "an account with no identity must not load — two of them would share one credential"
        );
    }

    #[test]
    fn an_account_is_enabled_when_the_flag_is_absent() {
        let id = Uuid::new_v4();
        let json = format!(
            r#"{{"id":"{id}","vendor":"github","label":"work","secret_account":"github_access_token"}}"#
        );
        let account: VendorAccount = serde_json::from_str(&json).expect("deserialize");
        assert!(account.enabled);
    }

    #[test]
    fn an_unrecognised_vendor_is_refused_rather_than_defaulted() {
        let id = Uuid::new_v4();
        let json =
            format!(r#"{{"id":"{id}","vendor":"gitlab","label":"work","secret_account":"x"}}"#);
        assert!(
            serde_json::from_str::<VendorAccount>(&json).is_err(),
            "a vendor this build does not know must not read as GitHub"
        );
    }
}
