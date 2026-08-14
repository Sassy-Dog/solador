//! The tracked repo portfolio — the Rust mirror of the original persistence layer's `TrackedRepo`.
//!
//! There is no seed. A portfolio is per-operator by nature, so there is no
//! defensible default: shipping one author's repositories meant every other
//! user's first launch opened on rows of 404s against repositories they cannot
//! read. The Repos panel asks for a repo instead (`NO_REPOS_MESSAGE`).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One repo in the tracked portfolio. The `owner/name` slug is the identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackedRepo {
    /// `owner/name`, e.g. `acme/gadget`.
    pub slug: String,
    /// Disabled repos stay configured but are not fetched.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    /// Extra workflows (beyond the default `ci.yml` push+PR view) whose
    /// failures should turn this repo red. `None`/empty keeps the legacy
    /// behaviour — kept optional, exactly as the original model is, so a store
    /// written before the field existed still opens.
    #[serde(default)]
    pub watched_workflows: Option<Vec<String>>,
    /// Which [`crate::VendorAccount`] fetches this repo. `None` means
    /// unattributed — a store written before v2 with no GitHub token, or an
    /// account since removed. **Never** defaulted to "the first account":
    /// inventing an owner is this crate's fabrication rule applied to
    /// configuration, and a defaulted state is as much a fabrication as a
    /// defaulted number.
    #[serde(default)]
    pub account_id: Option<Uuid>,
}

impl TrackedRepo {
    /// A newly tracked repo: enabled, no extra watched workflows, and
    /// **unattributed**. The caller names the account, because only the caller
    /// knows which one added it.
    #[must_use]
    pub fn new(slug: impl Into<String>) -> Self {
        TrackedRepo {
            slug: slug.into(),
            enabled: true,
            watched_workflows: None,
            account_id: None,
        }
    }

    /// Repo name after the slash, e.g. `gadget`.
    #[must_use]
    pub fn name(&self) -> &str {
        self.slug.rsplit('/').next().unwrap_or("")
    }
}

/// What a brand-new store starts with: nothing.
///
/// Kept as a function rather than inlining `Vec::new()` at the one call site in
/// [`crate::Store::open_in`], because *what a fresh store begins with* is a
/// product decision that deserves a name and a place to be argued with.
#[must_use]
pub fn seeded_repos() -> Vec<TrackedRepo> {
    Vec::new()
}

const fn enabled_by_default() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_store_seeds_no_repos() {
        assert!(
            seeded_repos().is_empty(),
            "a first-run store must not ship one operator's portfolio to everyone"
        );
    }

    #[test]
    fn name_is_the_slug_after_the_slash() {
        assert_eq!(TrackedRepo::new("acme/gadget").name(), "gadget");
        assert_eq!(TrackedRepo::new("gadget").name(), "gadget");
    }

    #[test]
    fn round_trips_through_json() {
        let repo = TrackedRepo {
            slug: "acme/gadget".into(),
            enabled: false,
            watched_workflows: Some(vec!["Release".into(), "deploy.yml".into()]),
            account_id: Some(Uuid::new_v4()),
        };
        let json = serde_json::to_string(&repo).expect("serialize");
        assert_eq!(
            serde_json::from_str::<TrackedRepo>(&json).expect("deserialize"),
            repo
        );
    }

    #[test]
    fn optional_fields_fall_back_when_absent() {
        let repo: TrackedRepo =
            serde_json::from_str(r#"{"slug":"acme/toolkit"}"#).expect("deserialize");
        assert!(repo.enabled);
        assert!(repo.watched_workflows.is_none());
    }

    /// A repo entry written before v2 carries no `account_id`, and reads as
    /// unattributed rather than as "belongs to whichever account is first".
    #[test]
    fn a_repo_written_before_v2_is_unattributed_rather_than_guessed_at() {
        let repo: TrackedRepo =
            serde_json::from_str(r#"{"slug":"acme/toolkit","enabled":true}"#).expect("deserialize");
        assert_eq!(
            repo.account_id, None,
            "an absent account_id is unknown ownership, never an invented owner"
        );
        assert_eq!(TrackedRepo::new("acme/toolkit").account_id, None);
    }
}
