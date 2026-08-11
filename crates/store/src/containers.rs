//! Container grouping rules and presence memory — the persisted half of the
//! Containers/VMs panel.
//!
//! Port of `DevCanopy/Services/Containers/ContainerGrouping.swift` (the rule
//! model, its glob matcher and the seeded defaults) and the persisted half of
//! `ContainerPresenceStore.swift`. The *evaluation* of those rules lives with
//! the panel (`app/src-tauri/src/containers/`); what lives here is only what
//! survives a relaunch, exactly as `Host` and `TrackedRepo` do.
//!
//! Swift keeps both in `UserDefaults` under their own keys, which is why its
//! loader has to answer "what does undecodable mean?". The answer is carried
//! over verbatim, because it is a real user-facing decision and not an
//! artifact of `UserDefaults`: **absent or unreadable rules mean "never
//! configured" and yield [`seeded_rules`]; a decoded *empty* list means the
//! user deliberately cleared every rule and is respected as-is.**

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Deserializer, Serialize};

/// The panel's section key for the machine the app itself runs on. Shared with
/// the rule model so a host-scoped rule can name it (`ContainerGroupRule.
/// localHostScope` in Swift).
pub const LOCAL_HOST_SCOPE: &str = "this machine";

/// Absence grace before an expected-but-absent container escalates from
/// "recycling" (amber, normal churn) to "missing" (red, alarm), in seconds.
///
/// Same 300s as `Presence.defaultGrace` (Swift) and
/// `github::presence::DEFAULT_GRACE_SECS`: the mac runner slots recycle in 1–4
/// minutes, so five minutes of absence means wedged.
pub const DEFAULT_GRACE_SECS: u64 = 300;

/// What a rule does with the containers its pattern matches.
///
/// `Collapse` folds them into one aggregate row, `Hide` drops them from the
/// panel's rows entirely (the rollup counts still include them, so cruft
/// building up stays visible in the numbers), and `Expect` keeps a standing
/// presence row while the entity is absent instead of letting the row vanish
/// with an ephemeral VM.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerRuleAction {
    #[default]
    Collapse,
    Hide,
    Expect,
}

impl ContainerRuleAction {
    /// Every action, in the order the Swift picker offers them.
    pub const ALL: [ContainerRuleAction; 3] = [
        ContainerRuleAction::Collapse,
        ContainerRuleAction::Hide,
        ContainerRuleAction::Expect,
    ];

    /// The persisted spelling — what [`Serialize`] writes and what an editor
    /// sends back.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ContainerRuleAction::Collapse => "collapse",
            ContainerRuleAction::Hide => "hide",
            ContainerRuleAction::Expect => "expect",
        }
    }

    /// Parses an action an *editor* sent, strictly.
    ///
    /// Deliberately not the [`Deserialize`] impl's tolerance: that one reads a
    /// **file**, where an unknown string means "written by a newer build" and
    /// degrading to `Collapse` saves the rest of the rule. This one reads a
    /// **command**, where an unknown string means the caller sent something no
    /// picker can produce — and silently rewriting that as `Collapse` would
    /// persist an edit nobody made.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        ContainerRuleAction::ALL
            .into_iter()
            .find(|action| action.as_str() == raw)
    }
}

/// Unknown action strings degrade to [`ContainerRuleAction::Collapse`] rather
/// than failing the decode.
///
/// Straight from the Swift decoder's note: a rule written by a *newer* build
/// must not throw here, because the throw would be caught one level up and
/// silently reset the user's entire rule list to the seeded defaults. Losing
/// one rule's action is recoverable; losing every rule is not.
impl<'de> Deserialize<'de> for ContainerRuleAction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Ok(match raw.as_str() {
            "hide" => ContainerRuleAction::Hide,
            "expect" => ContainerRuleAction::Expect,
            _ => ContainerRuleAction::Collapse,
        })
    }
}

/// A user-editable rule over container/VM names.
///
/// Edited from Settings → Hosts, exactly as Swift's
/// `ContainerGroupRulesSection` does; what lives here is the model, its glob
/// matcher and the seeds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerGroupRule {
    /// Glob over the whole container name — see [`matches_glob`].
    pub pattern: String,
    /// The aggregate row's label. Empty for hide/expect rules, which render no
    /// aggregate.
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub action: ContainerRuleAction,
    /// Host section this rule applies to, for matching *and* rendering. `None`
    /// = every host.
    #[serde(default)]
    pub host: Option<String>,
    /// Collapse rules only: how many matches there *should* be. Renders
    /// `×matched/expected` and warns amber when short. `None` = no
    /// expectation.
    #[serde(default)]
    pub expected_count: Option<u32>,
}

impl ContainerGroupRule {
    /// A rule with no host scope and no expected count.
    #[must_use]
    pub fn new(
        pattern: impl Into<String>,
        label: impl Into<String>,
        action: ContainerRuleAction,
    ) -> Self {
        ContainerGroupRule {
            pattern: pattern.into(),
            label: label.into(),
            action,
            host: None,
            expected_count: None,
        }
    }

    /// Scopes this rule to one host section.
    #[must_use]
    pub fn on_host(mut self, host: impl Into<String>) -> Self {
        self.host = Some(host.into());
        self
    }

    /// Whether this rule's pattern claims `name`.
    #[must_use]
    pub fn matches(&self, name: &str) -> bool {
        matches_glob(name, &self.pattern)
    }

    /// Whether this rule is in play for `host` — its own host, or every host.
    #[must_use]
    pub fn applies_to(&self, host: &str) -> bool {
        self.host.as_deref().is_none_or(|scope| scope == host)
    }

    /// An exact-name rule (no glob) seeds a presence expectation for a name
    /// nothing has ever reported; a glob only ever learns observed names.
    #[must_use]
    pub fn is_exact_name(&self) -> bool {
        !self.pattern.contains('*')
    }
}

/// Whether `name` matches `pattern`.
///
/// The contract is the Swift one, and each half of it is load-bearing: the
/// **whole** name must match (anchored, not substring), `*` matches any run of
/// characters including none, every other character is **literal** — a `.` in
/// `ghcr.io/*` must not quietly become "any character" — and matching is
/// case-sensitive.
///
/// Swift gets this by escaping the pattern into an `NSRegularExpression`;
/// doing the same here would mean a regex dependency to express a two-token
/// language, so the glob is matched directly. That is also why the literal
/// halves are compared with `starts_with`/`ends_with`/`find` rather than
/// translated to regex: there is no metacharacter left to leak.
#[must_use]
pub fn matches_glob(name: &str, pattern: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    // No star: the pattern is one literal, and the whole name must be it.
    let (Some(first), Some(last)) = (parts.first(), parts.last()) else {
        return false;
    };
    if parts.len() == 1 {
        return name == *first;
    }
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    // Interior literals must appear in order, each after the previous one.
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    // The trailing literal may not overlap what the leading one already
    // consumed, which is why this is a length-checked `ends_with`.
    rest.len() >= last.len() && rest.ends_with(last)
}

/// The rules a store that has never configured any starts with: none.
///
/// Empty on purpose. The three shipped defaults named one operator's machines
/// — two of them pinned `.on_host("ubu-01")` — and grouping is per-deployment
/// by nature. A shipped example rule silently groups a stranger's containers by
/// a rule they never wrote, which is harder to diagnose than no grouping at
/// all: the panel looks like it is working and quietly hides or folds rows.
///
/// **Order remains the contract** for whatever the operator does configure:
/// matching is first-match-wins, so a hide rule above a collapse rule changes
/// what the panel shows. That is a property of [`crate::Store::container_rules`]
/// and its consumers, not of this empty seed.
#[must_use]
pub fn seeded_rules() -> Vec<ContainerGroupRule> {
    Vec::new()
}

/// The seeded rules as a borrowable slice, so [`crate::Store::container_rules`]
/// can hand out one shape whether or not the store has its own.
pub(crate) fn seeded_rules_ref() -> &'static [ContainerGroupRule] {
    static SEEDED: OnceLock<Vec<ContainerGroupRule>> = OnceLock::new();
    SEEDED.get_or_init(seeded_rules)
}

/// Last-observed facts about one expected container name on one host.
///
/// `runtime` is `None` for a hand-typed expectation whose entity has never
/// actually been seen — the panel then renders no runtime tag rather than a
/// fabricated one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerPresenceRecord {
    /// Seconds since the UNIX epoch, and deliberately wall-clock: it is
    /// compared against a *later process's* poll time after a relaunch, which
    /// a monotonic instant cannot express.
    pub last_seen: u64,
    /// The runtime that reported it (`"docker"`/`"podman"`/`"tart"`), or
    /// `None` when nothing has ever reported it.
    #[serde(default)]
    pub runtime: Option<String>,
}

/// The presence map's key: `"<host>|<name>"`.
///
/// Container/VM names cannot contain `|` and host names realistically don't,
/// so this is injective in practice — the same assumption Swift's store makes.
#[must_use]
pub fn presence_key(host: &str, name: &str) -> String {
    format!("{host}|{name}")
}

/// Every record belonging to one host, keyed by bare name — the shape the
/// panel's partition step wants.
#[must_use]
pub fn records_for_host(
    records: &BTreeMap<String, ContainerPresenceRecord>,
    host: &str,
) -> BTreeMap<String, ContainerPresenceRecord> {
    let prefix = format!("{host}|");
    records
        .iter()
        .filter_map(|(key, record)| {
            key.strip_prefix(&prefix)
                .map(|name| (name.to_owned(), record.clone()))
        })
        .collect()
}

/// Rules as they arrive from the file, tolerating a value this build cannot
/// read.
///
/// A malformed rules array must not take the *whole store* down with it — the
/// crate's rule is that a file from either side of a schema change still
/// opens. Unreadable rules therefore read as "never configured", which is what
/// makes the seeded defaults appear rather than an empty panel. (An unknown
/// `action` string is handled one level down and does **not** land here: it
/// degrades to `collapse` and keeps the rest of the rule.)
pub(crate) fn lenient_rules<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<ContainerGroupRule>>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(raw.and_then(|value| serde_json::from_value(value).ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_store_seeds_no_container_rules() {
        assert!(
            seeded_rules().is_empty(),
            "seeded rules named one operator's hosts and image registry"
        );
    }

    /// The editor's parse is the strict counterpart of the file decoder's
    /// tolerance below — and the two must disagree on exactly one input.
    #[test]
    fn an_action_round_trips_through_the_spelling_an_editor_sends() {
        for action in ContainerRuleAction::ALL {
            assert_eq!(ContainerRuleAction::parse(action.as_str()), Some(action));
            assert_eq!(
                serde_json::to_string(&action).expect("serialize"),
                format!("\"{}\"", action.as_str())
            );
        }
        for raw in ["", "quarantine", "Collapse", "COLLAPSE"] {
            assert_eq!(ContainerRuleAction::parse(raw), None, "raw {raw:?}");
        }
    }

    #[test]
    fn glob_exact_literal_match() {
        assert!(matches_glob("postgres", "postgres"));
        assert!(!matches_glob("postgres-2", "postgres"));
        assert!(!matches_glob("postgres", "postgres-2"));
    }

    #[test]
    fn glob_trailing_star() {
        assert!(matches_glob("api-", "api-*"));
        assert!(matches_glob("api-1234", "api-*"));
        assert!(!matches_glob("apx-1234", "api-*"));
        assert!(!matches_glob("xapi-1234", "api-*"));
    }

    #[test]
    fn glob_seeded_runner_pattern() {
        assert!(matches_glob("acme-ci-runner-1", "acme-ci-runner-*"));
        assert!(matches_glob("acme-ci-runner-", "acme-ci-runner-*"));
        assert!(!matches_glob("acme-ci-mac-1", "acme-ci-runner-*"));
    }

    #[test]
    fn glob_multiple_stars() {
        assert!(matches_glob("abc-mid-xyz", "abc*mid*xyz"));
        assert!(!matches_glob("abc-xyz", "abc*mid*xyz"));
    }

    #[test]
    fn glob_escapes_regex_metacharacters() {
        assert!(matches_glob("ghcr.io/org/img", "ghcr.io/*"));
        // A leaked `.` would make this match; a literal one must not.
        assert!(!matches_glob("ghcrXio/org/img", "ghcr.io/*"));
        assert!(matches_glob("a+b", "a+b"));
        assert!(!matches_glob("ab", "a+b"));
    }

    #[test]
    fn glob_is_case_sensitive() {
        assert!(!matches_glob("API-1", "api-*"));
        assert!(!matches_glob("Postgres", "postgres"));
    }

    #[test]
    fn glob_bare_star_matches_everything() {
        assert!(matches_glob("anything", "*"));
        assert!(matches_glob("", "*"));
    }

    #[test]
    fn glob_empty_pattern_matches_only_the_empty_name() {
        assert!(matches_glob("", ""));
        assert!(!matches_glob("x", ""));
    }

    #[test]
    fn glob_trailing_literal_cannot_reuse_the_leading_one() {
        // ^a.*a$ does not match "a": the two literals need two characters.
        assert!(!matches_glob("a", "a*a"));
        assert!(matches_glob("aa", "a*a"));
        assert!(matches_glob("aba", "a*a"));
    }

    #[test]
    fn host_scope_applies_to_its_host_only() {
        let scoped = ContainerGroupRule::new("api-*", "jobs", ContainerRuleAction::Collapse)
            .on_host("ubu-01");
        assert!(scoped.applies_to("ubu-01"));
        assert!(!scoped.applies_to(LOCAL_HOST_SCOPE));

        let unscoped = ContainerGroupRule::new("api-*", "jobs", ContainerRuleAction::Collapse);
        assert!(unscoped.applies_to("ubu-01"));
        assert!(unscoped.applies_to(LOCAL_HOST_SCOPE));
    }

    #[test]
    fn rules_round_trip_through_json() {
        let mut rule = ContainerGroupRule::new("tart-*", "vms", ContainerRuleAction::Expect)
            .on_host(LOCAL_HOST_SCOPE);
        rule.expected_count = Some(4);
        let json = serde_json::to_string(&rule).expect("serialize");
        assert_eq!(
            serde_json::from_str::<ContainerGroupRule>(&json).expect("deserialize"),
            rule
        );
    }

    #[test]
    fn legacy_rules_without_action_or_host_decode_as_collapse_on_all_hosts() {
        let rule: ContainerGroupRule =
            serde_json::from_str(r#"{"pattern":"api-*","label":"jobs"}"#).expect("deserialize");
        assert_eq!(rule.action, ContainerRuleAction::Collapse);
        assert_eq!(rule.host, None);
        assert_eq!(rule.expected_count, None);
    }

    #[test]
    fn an_unknown_action_degrades_to_collapse_instead_of_failing_the_rule() {
        let rule: ContainerGroupRule =
            serde_json::from_str(r#"{"pattern":"api-*","label":"jobs","action":"quarantine"}"#)
                .expect("an unknown action must not fail the decode");
        assert_eq!(rule.action, ContainerRuleAction::Collapse);
        assert_eq!(rule.label, "jobs");
    }

    #[test]
    fn presence_records_split_by_host() {
        let mut records = BTreeMap::new();
        records.insert(
            presence_key(LOCAL_HOST_SCOPE, "vm-1"),
            ContainerPresenceRecord {
                last_seen: 100,
                runtime: Some("tart".into()),
            },
        );
        records.insert(
            presence_key("ubu-01", "api-9"),
            ContainerPresenceRecord {
                last_seen: 200,
                runtime: Some("podman".into()),
            },
        );

        let local = records_for_host(&records, LOCAL_HOST_SCOPE);
        assert_eq!(local.len(), 1);
        assert_eq!(local["vm-1"].last_seen, 100);

        let remote = records_for_host(&records, "ubu-01");
        assert_eq!(remote.len(), 1);
        assert_eq!(remote["api-9"].runtime.as_deref(), Some("podman"));

        assert!(records_for_host(&records, "nuc-spare").is_empty());
    }
}
