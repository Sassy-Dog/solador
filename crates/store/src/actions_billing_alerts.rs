//! What the Actions billing tile flags (#770): the spike rule's multiple and
//! floor, and which repos are meant to run only on self-hosted runners.
//!
//! Shared by every Actions billing tile, so they live in settings, not on a
//! tile. Saving refuses, in its own sentence per case, and never clamps.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// A day spikes at more than this many times the repo's 7-day average.
pub const DEFAULT_SPIKE_MULTIPLE: f64 = 3.0;
/// ... and at least this many hosted minutes.
pub const DEFAULT_SPIKE_FLOOR_MINUTES: u32 = 1_000;

pub const REFUSE_MULTIPLE: &str = "Enter a spike multiple above 1, such as 3.";
pub const REFUSE_FLOOR: &str = "Enter the spike floor as a whole number of minutes, at least 1.";
pub const REFUSE_OWNER_IN_REPO: &str =
    "Enter the repository name without its owner (headroom, not Sassy-Dog/headroom).";
pub const REFUSE_EMPTY_REPO: &str =
    "A self-hosted repo name is empty. Separate names with commas, with none left blank.";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActionsBillingAlerts {
    pub spike_multiple: f64,
    pub spike_floor_minutes: u32,
    /// Repos that should never run hosted macOS minutes, as `repositoryName`
    /// spells them (compared case-insensitively). Empty is the rule off.
    pub self_hosted_repos: Vec<String>,
}

impl Default for ActionsBillingAlerts {
    fn default() -> Self {
        Self {
            spike_multiple: DEFAULT_SPIKE_MULTIPLE,
            spike_floor_minutes: DEFAULT_SPIKE_FLOOR_MINUTES,
            self_hosted_repos: Vec::new(),
        }
    }
}

fn valid_multiple(n: f64) -> bool {
    n.is_finite() && n > 1.0
}

/// Trim, drop nothing, fold case-insensitive duplicates (the first spelling
/// wins).
fn folded(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for name in names {
        if !kept.iter().any(|k| k.to_lowercase() == name.to_lowercase()) {
            kept.push(name);
        }
    }
    kept
}

impl ActionsBillingAlerts {
    /// Strict, for a save from Configure: `spikeMultiple` (a number above 1),
    /// `spikeFloorMinutes` (a whole number, at least 1) and `selfHostedRepos`
    /// (text: names separated by commas or line breaks; blank is an empty list).
    ///
    /// # Errors
    /// One sentence per refusal; nothing is partially applied.
    pub fn parse(value: &Value) -> Result<Self, &'static str> {
        let spike_multiple = value["spikeMultiple"]
            .as_f64()
            .filter(|n| valid_multiple(*n))
            .ok_or(REFUSE_MULTIPLE)?;
        let spike_floor_minutes = value["spikeFloorMinutes"]
            .as_u64()
            .filter(|n| *n >= 1)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or(REFUSE_FLOOR)?;
        let text = value["selfHostedRepos"].as_str().unwrap_or_default().trim();
        let mut names = Vec::new();
        if !text.is_empty() {
            // Commas separate names; a line break also does, and a blank line is
            // not a name. A comma with nothing between it and the next one (or
            // the ends) is a typo, and is refused.
            for piece in text.split(',') {
                let before = names.len();
                names.extend(
                    piece
                        .lines()
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned),
                );
                if names.len() == before {
                    return Err(REFUSE_EMPTY_REPO);
                }
            }
        }
        // The usage report's `repositoryName` is the bare repo name, so an
        // `owner/repo` entry could never match: refused, not stripped.
        if names.iter().any(|name| name.contains('/')) {
            return Err(REFUSE_OWNER_IN_REPO);
        }
        Ok(Self {
            spike_multiple,
            spike_floor_minutes,
            self_hosted_repos: folded(names),
        })
    }

    /// Lenient, for the stored file: a hand-edited bad value falls back to its
    /// default and never stops the rest of the store loading.
    fn from_stored(value: &Value) -> Self {
        let defaults = Self::default();
        Self {
            spike_multiple: value["spike_multiple"]
                .as_f64()
                .filter(|n| valid_multiple(*n))
                .unwrap_or(defaults.spike_multiple),
            spike_floor_minutes: value["spike_floor_minutes"]
                .as_u64()
                .filter(|n| *n >= 1)
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(defaults.spike_floor_minutes),
            self_hosted_repos: folded(
                value["self_hosted_repos"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned),
            ),
        }
    }

    /// The list as one line of text, as the Configure field shows it.
    #[must_use]
    pub fn self_hosted_text(&self) -> String {
        self.self_hosted_repos.join(", ")
    }
}

impl<'de> Deserialize<'de> for ActionsBillingAlerts {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_stored(&Value::deserialize(deserializer)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn input(multiple: Value, floor: Value, repos: &str) -> Value {
        json!({"spikeMultiple": multiple, "spikeFloorMinutes": floor, "selfHostedRepos": repos})
    }

    #[test]
    fn the_defaults_are_three_times_a_thousand_minutes_and_an_empty_list() {
        let d = ActionsBillingAlerts::default();
        assert_eq!(d.spike_multiple, 3.0);
        assert_eq!(d.spike_floor_minutes, 1000);
        assert!(d.self_hosted_repos.is_empty());
    }

    #[test]
    fn a_store_without_the_key_loads_with_the_defaults() {
        let settings: crate::Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(
            settings.actions_billing_alerts,
            ActionsBillingAlerts::default()
        );
        let settings: crate::Settings =
            serde_json::from_value(json!({"github_org": "acme"})).unwrap();
        assert_eq!(
            settings.actions_billing_alerts,
            ActionsBillingAlerts::default()
        );
    }

    #[test]
    fn a_valid_input_is_taken_whole_and_the_list_is_trimmed_and_folded() {
        let parsed = ActionsBillingAlerts::parse(&input(
            json!(2.5),
            json!(500),
            " Alpha, beta\n alpha ,BETA",
        ))
        .unwrap();
        assert_eq!(parsed.spike_multiple, 2.5);
        assert_eq!(parsed.spike_floor_minutes, 500);
        assert_eq!(parsed.self_hosted_repos, ["Alpha", "beta"]);
        assert_eq!(parsed.self_hosted_text(), "Alpha, beta");
    }

    #[test]
    fn a_blank_list_is_the_rule_off_and_a_trailing_line_break_is_not_a_blank_name() {
        for text in ["", "   ", "\n"] {
            let parsed = ActionsBillingAlerts::parse(&input(json!(3), json!(1000), text)).unwrap();
            assert!(parsed.self_hosted_repos.is_empty(), "{text:?}");
        }
        let parsed =
            ActionsBillingAlerts::parse(&input(json!(3), json!(1000), "alpha,\nbeta\n")).unwrap();
        assert_eq!(parsed.self_hosted_repos, ["alpha", "beta"]);
    }

    #[test]
    fn each_refusal_has_its_own_sentence() {
        let refuse = |multiple: Value, floor: Value, repos: &str| {
            ActionsBillingAlerts::parse(&input(multiple, floor, repos)).unwrap_err()
        };
        for bad in [
            json!(1),
            json!(0.5),
            json!(0),
            json!(-3),
            json!(null),
            json!("3"),
        ] {
            assert_eq!(refuse(bad, json!(1000), ""), REFUSE_MULTIPLE);
        }
        for bad in [
            json!(0),
            json!(-5),
            json!(12.5),
            json!(null),
            json!("100"),
            json!(5_000_000_000_u64),
        ] {
            assert_eq!(refuse(json!(3), bad, ""), REFUSE_FLOOR);
        }
        assert_eq!(refuse(json!(3), json!(1000), "a,,b"), REFUSE_EMPTY_REPO);
        assert_eq!(refuse(json!(3), json!(1000), "a, ,b"), REFUSE_EMPTY_REPO);
        assert_eq!(refuse(json!(3), json!(1000), ",a"), REFUSE_EMPTY_REPO);
        for owned in ["Sassy-Dog/headroom", "a, org/b", "a/"] {
            assert_eq!(refuse(json!(3), json!(1000), owned), REFUSE_OWNER_IN_REPO);
        }
        let sentences = [
            REFUSE_MULTIPLE,
            REFUSE_FLOOR,
            REFUSE_EMPTY_REPO,
            REFUSE_OWNER_IN_REPO,
        ];
        assert_eq!(
            sentences
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            4
        );
        assert!(ActionsBillingAlerts::parse(&json!({})).is_err());
    }

    #[test]
    fn hand_edited_bad_values_fall_back_without_losing_the_rest_of_the_store() {
        let settings: crate::Settings = serde_json::from_value(json!({
            "github_org": "acme",
            "actions_billing_alerts": {
                "spike_multiple": 0.5,
                "spike_floor_minutes": -4,
                "self_hosted_repos": ["keep", " ", 7, "KEEP"]
            }
        }))
        .unwrap();
        assert_eq!(settings.github_org, "acme");
        let alerts = settings.actions_billing_alerts;
        assert_eq!(alerts.spike_multiple, 3.0);
        assert_eq!(alerts.spike_floor_minutes, 1000);
        assert_eq!(alerts.self_hosted_repos, ["keep"]);
    }

    #[test]
    fn a_saved_setting_survives_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = crate::Store::open_in(dir.path(), false).unwrap();
        store.settings_mut().actions_billing_alerts =
            ActionsBillingAlerts::parse(&input(json!(4), json!(250), "alpha")).unwrap();
        store.save().unwrap();
        let reopened = crate::Store::open_in(dir.path(), false).unwrap();
        let alerts = &reopened.settings().actions_billing_alerts;
        assert_eq!(alerts.spike_multiple, 4.0);
        assert_eq!(alerts.spike_floor_minutes, 250);
        assert_eq!(alerts.self_hosted_repos, ["alpha"]);
    }
}
