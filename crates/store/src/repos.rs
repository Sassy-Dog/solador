//! The tracked repo portfolio — the Rust mirror of SwiftData's `TrackedRepo`
//! (`DevCanopy/Models/TrackedRepo.swift`) and the one-time seed in
//! `Services/GitHub/PortfolioRepos.swift`.

use serde::{Deserialize, Serialize};

/// The org every seeded slug belongs to (`PortfolioRepos.org`).
pub const ORG: &str = "Sassy-Dog";

/// The list a brand-new store is seeded with, in Swift's order.
///
/// Seeding happens exactly once — when no store file has ever existed (see
/// [`crate::Store::open_in`]). After that the set is user-editable and editing
/// this array does not retro-edit an already-seeded store, matching
/// `PortfolioStore`'s contract.
pub const SEED_SLUGS: [&str; 6] = [
    "Sassy-Dog/velovate",
    "Sassy-Dog/qr-ninja",
    "Sassy-Dog/tailoredtip",
    "Sassy-Dog/what2wear",
    "Sassy-Dog/devcanopy",
    "Sassy-Dog/platform",
];

/// One repo in the tracked portfolio. The `owner/name` slug is the identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackedRepo {
    /// `owner/name`, e.g. `Sassy-Dog/velovate`.
    pub slug: String,
    /// Disabled repos stay configured but are not fetched.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    /// Extra workflows (beyond the default `ci.yml` push+PR view) whose
    /// failures should turn this repo red. `None`/empty keeps the legacy
    /// behaviour — kept optional, exactly as the Swift model is, so a store
    /// written before the field existed still opens.
    #[serde(default)]
    pub watched_workflows: Option<Vec<String>>,
}

impl TrackedRepo {
    /// A newly tracked repo: enabled, no extra watched workflows.
    #[must_use]
    pub fn new(slug: impl Into<String>) -> Self {
        TrackedRepo {
            slug: slug.into(),
            enabled: true,
            watched_workflows: None,
        }
    }

    /// Repo name after the slash, e.g. `velovate`.
    #[must_use]
    pub fn name(&self) -> &str {
        self.slug.rsplit('/').next().unwrap_or("")
    }
}

/// The seed set as owned records — what a first-run store starts with.
#[must_use]
pub fn seeded_repos() -> Vec<TrackedRepo> {
    SEED_SLUGS
        .iter()
        .map(|slug| TrackedRepo::new(*slug))
        .collect()
}

const fn enabled_by_default() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_matches_the_swift_portfolio() {
        let seeded = seeded_repos();
        assert_eq!(seeded.len(), 6);
        assert_eq!(
            seeded.iter().map(|r| r.slug.as_str()).collect::<Vec<_>>(),
            vec![
                "Sassy-Dog/velovate",
                "Sassy-Dog/qr-ninja",
                "Sassy-Dog/tailoredtip",
                "Sassy-Dog/what2wear",
                "Sassy-Dog/devcanopy",
                "Sassy-Dog/platform",
            ]
        );
        assert!(seeded
            .iter()
            .all(|r| r.enabled && r.watched_workflows.is_none()));
        assert!(seeded
            .iter()
            .all(|r| r.slug.starts_with(&format!("{ORG}/"))));
    }

    #[test]
    fn name_is_the_slug_after_the_slash() {
        assert_eq!(TrackedRepo::new("Sassy-Dog/velovate").name(), "velovate");
        assert_eq!(TrackedRepo::new("velovate").name(), "velovate");
    }

    #[test]
    fn round_trips_through_json() {
        let repo = TrackedRepo {
            slug: "Sassy-Dog/velovate".into(),
            enabled: false,
            watched_workflows: Some(vec!["Release".into(), "deploy.yml".into()]),
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
            serde_json::from_str(r#"{"slug":"Sassy-Dog/platform"}"#).expect("deserialize");
        assert!(repo.enabled);
        assert!(repo.watched_workflows.is_none());
    }
}
