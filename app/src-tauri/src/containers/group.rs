//! Partitioning one host section's containers against the grouping rules, and
//! evaluating presence for the ones that should be there and aren't.
//!
//! Port of `ContainerGrouping.partition`/`displayRows` and
//! `Presence.state`/`label` (`DevCanopy/Services/Containers/`,
//! `DevCanopy/Services/Presence/`). Pure: no I/O, no wall clock — `now` is
//! always the section's last **successful** poll, so a failing source freezes
//! its absence clocks instead of ageing entities toward a false alarm.

use std::collections::BTreeMap;

use store::{ContainerGroupRule, ContainerPresenceRecord, ContainerRuleAction};
use viewmodel::format::duration;

/// One collapsed row summarising every container a rule matched in a section.
///
/// A configured collapse rule is a *standing* row: it exists with zero matches
/// too, so an idle runner pool reads as `×0` rather than vanishing — the panel
/// must show that the pool is empty, not that it is unconfigured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aggregate {
    pub label: String,
    pub total: usize,
    pub running: usize,
    /// `None` for an empty group: there is no container to derive a runtime
    /// from, and a fabricated one is worse than none.
    pub dominant_runtime: Option<String>,
    pub expected_count: Option<u32>,
}

impl Aggregate {
    /// Whether this group has fewer members than the rule says it should.
    pub fn is_short(&self) -> bool {
        self.expected_count
            .is_some_and(|expected| self.total < expected as usize)
    }

    /// `×3` or, with an expectation, `×3/4`.
    pub fn count_text(&self) -> String {
        match self.expected_count {
            Some(expected) => format!("×{}/{expected}", self.total),
            None => format!("×{}", self.total),
        }
    }
}

/// Presence of an entity the user expects to exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceState {
    /// Briefly absent — normal ephemeral churn (amber).
    Recycling { absence_secs: u64 },
    /// Absent beyond grace — an alarm (red).
    Missing { absence_secs: u64 },
}

impl PresenceState {
    /// Classifies an absence.
    ///
    /// Twin of `github::presence::state`, in whole seconds rather than
    /// `DateTime<Utc>`: presence records persist as unix seconds (a monotonic
    /// instant cannot survive a relaunch) and this crate does not depend on
    /// the GitHub client. Same grace, and the label ladder below is
    /// `viewmodel::format::duration`, which both spellings share — so the two
    /// cannot drift on the boundary that matters.
    pub fn classify(last_seen: u64, now: u64, grace_secs: u64) -> Self {
        let absence_secs = now.saturating_sub(last_seen);
        if absence_secs < grace_secs {
            PresenceState::Recycling { absence_secs }
        } else {
            PresenceState::Missing { absence_secs }
        }
    }

    /// "recycling 40s" / "missing 12m".
    pub fn label(self) -> String {
        match self {
            PresenceState::Recycling { absence_secs } => {
                format!("recycling {}", duration(absence_secs))
            }
            PresenceState::Missing { absence_secs } => {
                format!("missing {}", duration(absence_secs))
            }
        }
    }

    pub fn is_missing(self) -> bool {
        matches!(self, PresenceState::Missing { .. })
    }
}

/// A standing row for an expected container that the current poll did not see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedAbsent {
    pub name: String,
    /// `None` → render no runtime tag. A hand-typed expectation whose entity
    /// has never been observed belongs to no runtime, and guessing one would
    /// be a fabricated fact.
    pub runtime: Option<String>,
    pub state: PresenceState,
}

/// One renderable row of a host section. Identity is the **name**, so an
/// entity flipping present ↔ absent keeps its place in the list.
#[derive(Debug, Clone, PartialEq)]
pub enum DisplayRow {
    Present(wire::Container),
    Absent(ExpectedAbsent),
}

impl DisplayRow {
    pub fn name(&self) -> &str {
        match self {
            DisplayRow::Present(container) => &container.name,
            DisplayRow::Absent(absent) => &absent.name,
        }
    }
}

/// What partitioning one host section produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Partition {
    pub individual: Vec<wire::Container>,
    pub aggregates: Vec<Aggregate>,
    pub expected_absent: Vec<ExpectedAbsent>,
}

impl Partition {
    /// Present and absent rows merged into one name-sorted list — the order
    /// the panel renders. Aggregates are **not** in here: they are rule-driven
    /// and render after the entity rows.
    pub fn display_rows(&self) -> Vec<DisplayRow> {
        let mut rows: Vec<DisplayRow> = self
            .individual
            .iter()
            .cloned()
            .map(DisplayRow::Present)
            .chain(self.expected_absent.iter().cloned().map(DisplayRow::Absent))
            .collect();
        rows.sort_by(|a, b| a.name().cmp(b.name()));
        rows
    }

    /// Expected entities absent beyond grace. These are exactly what the
    /// panel's totals can no longer see, which is why they get their own count.
    pub fn missing_count(&self) -> usize {
        self.expected_absent
            .iter()
            .filter(|absent| absent.state.is_missing())
            .count()
    }
}

/// Splits one host section's containers into individually-rendered rows, one
/// aggregate per applicable collapse rule, and the expected-but-absent rows.
///
/// The rules that matter, all inherited from Swift:
/// - Only rules scoped to `host` (or unscoped) apply — for matching **and**
///   rendering, so a scoped collapse rule's standing `×0` row appears on its
///   own host only.
/// - **First matching rule wins.** That is also what arbitrates collapse vs
///   hide vs expect when patterns overlap: rule order is the user's tie-break.
/// - Individual rows sort by name (then runtime), so `ps`'s newest-first
///   arrival order can't reshuffle the panel between polls.
/// - `now` is the section's last successful poll. `None` means we have never
///   successfully looked, so **nothing is reported absent** — an app that just
///   launched must not alarm about a host it has not reached yet.
pub fn partition(
    containers: &[wire::Container],
    rules: &[ContainerGroupRule],
    host: &str,
    presence: &BTreeMap<String, ContainerPresenceRecord>,
    now: Option<u64>,
    grace_secs: u64,
) -> Partition {
    let applicable: Vec<&ContainerGroupRule> =
        rules.iter().filter(|rule| rule.applies_to(host)).collect();

    let mut matched: BTreeMap<usize, Vec<&wire::Container>> = BTreeMap::new();
    let mut individual: Vec<wire::Container> = Vec::new();

    for container in containers {
        match applicable
            .iter()
            .position(|rule| rule.matches(&container.name))
        {
            Some(index) => match applicable[index].action {
                ContainerRuleAction::Collapse => {
                    matched.entry(index).or_default().push(container);
                }
                ContainerRuleAction::Hide => {}
                // An expected container renders as its own row while present;
                // first-match-wins then lets an expect rule shield a name from
                // a later collapse or hide rule.
                ContainerRuleAction::Expect => individual.push(container.clone()),
            },
            None => individual.push(container.clone()),
        }
    }

    individual.sort_by(|a, b| (&a.name, &a.runtime).cmp(&(&b.name, &b.runtime)));

    let aggregates = applicable
        .iter()
        .enumerate()
        .filter(|(_, rule)| rule.action == ContainerRuleAction::Collapse)
        .map(|(index, rule)| {
            let members = matched.get(&index).cloned().unwrap_or_default();
            Aggregate {
                label: rule.label.clone(),
                total: members.len(),
                running: members.iter().filter(|c| c.is_running).count(),
                dominant_runtime: dominant_runtime(&members),
                expected_count: rule.expected_count,
            }
        })
        .collect();

    let mut expected_absent = Vec::new();
    if let Some(now) = now {
        for (name, record) in presence {
            if containers.iter().any(|c| &c.name == name) {
                continue;
            }
            // First-match-wins again, so a hide rule ordered above the expect
            // rule suppresses the absent row exactly as it suppresses the
            // present one.
            let claimed_by_expect = applicable
                .iter()
                .find(|rule| rule.matches(name))
                .is_some_and(|rule| rule.action == ContainerRuleAction::Expect);
            if !claimed_by_expect {
                continue;
            }
            expected_absent.push(ExpectedAbsent {
                name: name.clone(),
                runtime: record.runtime.clone(),
                state: PresenceState::classify(record.last_seen, now, grace_secs),
            });
        }
        expected_absent.sort_by(|a, b| a.name.cmp(&b.name));
    }

    Partition {
        individual,
        aggregates,
        expected_absent,
    }
}

/// Most frequent runtime among a group's members; ties break toward the
/// alphabetically smaller label so the answer is deterministic. `None` for an
/// empty group — never invent one.
fn dominant_runtime(members: &[&wire::Container]) -> Option<String> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for member in members {
        *counts.entry(member.runtime.as_str()).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by(|(left_name, left), (right_name, right)| {
            left.cmp(right).then(right_name.cmp(left_name))
        })
        .map(|(runtime, _)| runtime.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::{presence_key, records_for_host, LOCAL_HOST_SCOPE};

    const GRACE: u64 = store::DEFAULT_GRACE_SECS;

    fn container(name: &str, running: bool, runtime: &str) -> wire::Container {
        wire::Container {
            name: name.to_owned(),
            status_text: if running { "Up 2 hours" } else { "Exited (0)" }.to_owned(),
            is_running: running,
            runtime: runtime.to_owned(),
            image: None,
        }
    }

    fn collapse(pattern: &str, label: &str) -> ContainerGroupRule {
        ContainerGroupRule::new(pattern, label, ContainerRuleAction::Collapse)
    }

    fn expect_rule(pattern: &str) -> ContainerGroupRule {
        ContainerGroupRule::new(pattern, "", ContainerRuleAction::Expect)
    }

    fn record(absent_for: u64, runtime: Option<&str>) -> ContainerPresenceRecord {
        ContainerPresenceRecord {
            last_seen: 10_000 - absent_for,
            runtime: runtime.map(ToOwned::to_owned),
        }
    }

    fn records(
        entries: &[(&str, ContainerPresenceRecord)],
    ) -> BTreeMap<String, ContainerPresenceRecord> {
        entries
            .iter()
            .map(|(name, record)| ((*name).to_owned(), record.clone()))
            .collect()
    }

    fn part(containers: &[wire::Container], rules: &[ContainerGroupRule], host: &str) -> Partition {
        partition(containers, rules, host, &BTreeMap::new(), None, GRACE)
    }

    fn names(containers: &[wire::Container]) -> Vec<&str> {
        containers.iter().map(|c| c.name.as_str()).collect()
    }

    #[test]
    fn individuals_sort_by_name_regardless_of_arrival_order() {
        let containers = [
            container("zebra", true, "docker"),
            container("alpha", true, "docker"),
            container("mid", false, "podman"),
        ];
        let parts = part(&containers, &[], LOCAL_HOST_SCOPE);
        assert_eq!(names(&parts.individual), vec!["alpha", "mid", "zebra"]);
    }

    #[test]
    fn the_same_name_on_two_runtimes_sorts_deterministically() {
        let containers = [
            container("twin", true, "podman"),
            container("twin", true, "docker"),
        ];
        let parts = part(&containers, &[], LOCAL_HOST_SCOPE);
        assert_eq!(
            parts
                .individual
                .iter()
                .map(|c| c.runtime.as_str())
                .collect::<Vec<_>>(),
            vec!["docker", "podman"]
        );
    }

    #[test]
    fn an_aggregate_counts_total_and_running() {
        let containers = [
            container("api-1", true, "podman"),
            container("api-2", false, "podman"),
            container("api-3", true, "podman"),
            container("web", true, "docker"),
        ];
        let parts = part(&containers, &[collapse("api-*", "jobs")], LOCAL_HOST_SCOPE);
        assert_eq!(names(&parts.individual), vec!["web"]);
        assert_eq!(parts.aggregates.len(), 1);
        assert_eq!(parts.aggregates[0].label, "jobs");
        assert_eq!(parts.aggregates[0].total, 3);
        assert_eq!(parts.aggregates[0].running, 2);
        assert_eq!(
            parts.aggregates[0].dominant_runtime.as_deref(),
            Some("podman")
        );
        assert_eq!(parts.aggregates[0].count_text(), "×3");
        assert!(!parts.aggregates[0].is_short());
    }

    #[test]
    fn an_all_stopped_aggregate_has_zero_running() {
        let containers = [
            container("api-1", false, "podman"),
            container("api-2", false, "podman"),
        ];
        let parts = part(&containers, &[collapse("api-*", "jobs")], LOCAL_HOST_SCOPE);
        assert_eq!(parts.aggregates[0].total, 2);
        assert_eq!(parts.aggregates[0].running, 0);
    }

    #[test]
    fn a_collapse_rule_with_no_matches_still_renders_a_standing_row() {
        let parts = part(&[], &[collapse("api-*", "jobs")], LOCAL_HOST_SCOPE);
        assert_eq!(parts.aggregates.len(), 1);
        assert_eq!(parts.aggregates[0].total, 0);
        assert_eq!(parts.aggregates[0].count_text(), "×0");
        assert_eq!(
            parts.aggregates[0].dominant_runtime, None,
            "an empty group has no runtime to report, and must not invent one"
        );
    }

    #[test]
    fn the_first_matching_rule_wins_on_overlap() {
        let containers = [container("api-1", true, "podman")];
        let parts = part(
            &containers,
            &[collapse("api-*", "first"), collapse("*", "second")],
            LOCAL_HOST_SCOPE,
        );
        assert_eq!(parts.aggregates[0].total, 1);
        assert_eq!(parts.aggregates[1].total, 0);
    }

    #[test]
    fn the_dominant_runtime_is_the_most_frequent() {
        let containers = [
            container("api-1", true, "podman"),
            container("api-2", true, "podman"),
            container("api-3", true, "docker"),
        ];
        let parts = part(&containers, &[collapse("api-*", "jobs")], LOCAL_HOST_SCOPE);
        assert_eq!(
            parts.aggregates[0].dominant_runtime.as_deref(),
            Some("podman")
        );
    }

    #[test]
    fn a_dominant_runtime_tie_breaks_toward_the_smaller_label() {
        let containers = [
            container("api-1", true, "podman"),
            container("api-2", true, "docker"),
        ];
        let parts = part(&containers, &[collapse("api-*", "jobs")], LOCAL_HOST_SCOPE);
        assert_eq!(
            parts.aggregates[0].dominant_runtime.as_deref(),
            Some("docker")
        );
    }

    #[test]
    fn no_rules_leaves_everything_individual() {
        let containers = [
            container("web", true, "docker"),
            container("db", false, "docker"),
        ];
        let parts = part(&containers, &[], LOCAL_HOST_SCOPE);
        assert_eq!(parts.individual.len(), 2);
        assert!(parts.aggregates.is_empty());
    }

    #[test]
    fn aggregates_follow_rule_order_not_match_order() {
        let containers = [
            container("b-1", true, "docker"),
            container("a-1", true, "docker"),
        ];
        let parts = part(
            &containers,
            &[collapse("a-*", "alpha"), collapse("b-*", "beta")],
            LOCAL_HOST_SCOPE,
        );
        assert_eq!(
            parts
                .aggregates
                .iter()
                .map(|a| a.label.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );
    }

    #[test]
    fn a_scoped_rule_applies_only_on_its_host() {
        let containers = [container("api-1", true, "podman")];
        let rules = [collapse("api-*", "jobs").on_host("ubu-3xdv")];

        let scoped = part(&containers, &rules, "ubu-3xdv");
        assert!(scoped.individual.is_empty());
        assert_eq!(scoped.aggregates.len(), 1);

        let elsewhere = part(&containers, &rules, LOCAL_HOST_SCOPE);
        assert_eq!(names(&elsewhere.individual), vec!["api-1"]);
        assert!(
            elsewhere.aggregates.is_empty(),
            "a scoped rule must not render its standing row on another host"
        );
    }

    #[test]
    fn a_scoped_hide_hides_only_on_its_host() {
        let containers = [container("ghcr.io/img", false, "docker")];
        let rules = [
            ContainerGroupRule::new("ghcr.io/*", "", ContainerRuleAction::Hide).on_host("ubu-3xdv"),
        ];
        assert!(part(&containers, &rules, "ubu-3xdv").individual.is_empty());
        assert_eq!(
            names(&part(&containers, &rules, LOCAL_HOST_SCOPE).individual),
            vec!["ghcr.io/img"]
        );
    }

    #[test]
    fn a_hide_rule_drops_matches_from_rows_and_aggregates_alike() {
        let containers = [
            container("ghcr.io/img", false, "docker"),
            container("web", true, "docker"),
        ];
        let rules = [ContainerGroupRule::new(
            "ghcr.io/*",
            "",
            ContainerRuleAction::Hide,
        )];
        let parts = part(&containers, &rules, LOCAL_HOST_SCOPE);
        assert_eq!(names(&parts.individual), vec!["web"]);
        assert!(parts.aggregates.is_empty());
    }

    #[test]
    fn rule_order_arbitrates_collapse_versus_hide() {
        let containers = [container("api-1", true, "podman")];

        let hide_first = part(
            &containers,
            &[
                ContainerGroupRule::new("api-*", "", ContainerRuleAction::Hide),
                collapse("api-*", "jobs"),
            ],
            LOCAL_HOST_SCOPE,
        );
        assert!(hide_first.individual.is_empty());
        assert_eq!(hide_first.aggregates[0].total, 0);

        let collapse_first = part(
            &containers,
            &[
                collapse("api-*", "jobs"),
                ContainerGroupRule::new("api-*", "", ContainerRuleAction::Hide),
            ],
            LOCAL_HOST_SCOPE,
        );
        assert_eq!(collapse_first.aggregates[0].total, 1);
    }

    #[test]
    fn an_expected_present_container_renders_individually_and_beats_later_rules() {
        let containers = [container("vm-1", true, "tart")];
        let parts = part(
            &containers,
            &[expect_rule("vm-*"), collapse("vm-*", "vms")],
            LOCAL_HOST_SCOPE,
        );
        assert_eq!(names(&parts.individual), vec!["vm-1"]);
        assert_eq!(parts.aggregates[0].total, 0);
    }

    #[test]
    fn an_absent_expected_record_recycles_under_grace() {
        let parts = partition(
            &[],
            &[expect_rule("vm-*")],
            LOCAL_HOST_SCOPE,
            &records(&[("vm-1", record(40, Some("tart")))]),
            Some(10_000),
            GRACE,
        );
        assert_eq!(parts.expected_absent.len(), 1);
        assert_eq!(parts.expected_absent[0].name, "vm-1");
        assert_eq!(parts.expected_absent[0].runtime.as_deref(), Some("tart"));
        assert_eq!(parts.expected_absent[0].state.label(), "recycling 40s");
        assert_eq!(parts.missing_count(), 0);
    }

    #[test]
    fn an_absent_expected_record_goes_missing_beyond_grace() {
        let parts = partition(
            &[],
            &[expect_rule("vm-*")],
            LOCAL_HOST_SCOPE,
            &records(&[("vm-1", record(720, Some("tart")))]),
            Some(10_000),
            GRACE,
        );
        assert_eq!(parts.expected_absent[0].state.label(), "missing 12m");
        assert_eq!(parts.missing_count(), 1);
    }

    #[test]
    fn an_unobserved_expectation_carries_no_runtime_tag() {
        let parts = partition(
            &[],
            &[expect_rule("vm-1")],
            LOCAL_HOST_SCOPE,
            &records(&[("vm-1", record(40, None))]),
            Some(10_000),
            GRACE,
        );
        assert_eq!(parts.expected_absent[0].runtime, None);
    }

    #[test]
    fn a_host_that_has_never_reported_produces_no_absent_rows() {
        let parts = partition(
            &[],
            &[expect_rule("vm-*")],
            LOCAL_HOST_SCOPE,
            &records(&[("vm-1", record(9_999, Some("tart")))]),
            None,
            GRACE,
        );
        assert!(
            parts.expected_absent.is_empty(),
            "never having looked is not evidence of absence"
        );
    }

    #[test]
    fn a_host_scoped_expectation_emits_only_on_its_host() {
        let presence = records(&[("vm-1", record(40, Some("tart")))]);
        let rules = [expect_rule("vm-*").on_host("ubu-3xdv")];
        assert_eq!(
            partition(&[], &rules, "ubu-3xdv", &presence, Some(10_000), GRACE)
                .expected_absent
                .len(),
            1
        );
        assert!(partition(
            &[],
            &rules,
            LOCAL_HOST_SCOPE,
            &presence,
            Some(10_000),
            GRACE
        )
        .expected_absent
        .is_empty());
    }

    #[test]
    fn a_hide_rule_above_an_expect_rule_suppresses_the_absent_row() {
        let parts = partition(
            &[],
            &[
                ContainerGroupRule::new("vm-*", "", ContainerRuleAction::Hide),
                expect_rule("vm-*"),
            ],
            LOCAL_HOST_SCOPE,
            &records(&[("vm-1", record(40, Some("tart")))]),
            Some(10_000),
            GRACE,
        );
        assert!(parts.expected_absent.is_empty());
    }

    #[test]
    fn a_record_with_no_matching_expect_rule_emits_nothing() {
        let parts = partition(
            &[],
            &[collapse("vm-*", "vms")],
            LOCAL_HOST_SCOPE,
            &records(&[("vm-1", record(40, Some("tart")))]),
            Some(10_000),
            GRACE,
        );
        assert!(parts.expected_absent.is_empty());
    }

    #[test]
    fn a_present_container_is_not_also_reported_absent() {
        let parts = partition(
            &[container("vm-1", true, "tart")],
            &[expect_rule("vm-*")],
            LOCAL_HOST_SCOPE,
            &records(&[("vm-1", record(40, Some("tart")))]),
            Some(10_000),
            GRACE,
        );
        assert!(parts.expected_absent.is_empty());
        assert_eq!(names(&parts.individual), vec!["vm-1"]);
    }

    #[test]
    fn absent_rows_sort_by_name() {
        let parts = partition(
            &[],
            &[expect_rule("vm-*")],
            LOCAL_HOST_SCOPE,
            &records(&[
                ("vm-9", record(40, Some("tart"))),
                ("vm-1", record(40, Some("tart"))),
            ]),
            Some(10_000),
            GRACE,
        );
        assert_eq!(
            parts
                .expected_absent
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>(),
            vec!["vm-1", "vm-9"]
        );
    }

    #[test]
    fn an_aggregate_carries_its_expected_count_and_warns_when_short() {
        let mut rule = collapse("api-*", "jobs");
        rule.expected_count = Some(4);
        let containers = [container("api-1", true, "podman")];
        let parts = part(&containers, &[rule], LOCAL_HOST_SCOPE);
        assert_eq!(parts.aggregates[0].count_text(), "×1/4");
        assert!(parts.aggregates[0].is_short());
    }

    #[test]
    fn display_rows_merge_present_and_absent_by_name() {
        let parts = partition(
            &[
                container("vm-2", true, "tart"),
                container("vm-4", true, "tart"),
            ],
            &[expect_rule("vm-*")],
            LOCAL_HOST_SCOPE,
            &records(&[
                ("vm-1", record(40, Some("tart"))),
                ("vm-3", record(40, Some("tart"))),
            ]),
            Some(10_000),
            GRACE,
        );
        let rows = parts.display_rows();
        assert_eq!(
            rows.iter().map(DisplayRow::name).collect::<Vec<_>>(),
            vec!["vm-1", "vm-2", "vm-3", "vm-4"]
        );
        assert!(matches!(rows[0], DisplayRow::Absent(_)));
        assert!(matches!(rows[1], DisplayRow::Present(_)));
    }

    #[test]
    fn presence_states_walk_the_grace_boundary() {
        assert_eq!(
            PresenceState::classify(0, 299, GRACE),
            PresenceState::Recycling { absence_secs: 299 }
        );
        assert_eq!(
            PresenceState::classify(0, 300, GRACE),
            PresenceState::Missing { absence_secs: 300 }
        );
        // A clock that ran backwards reads as "just seen", never as a negative
        // age that would format as nonsense.
        assert_eq!(
            PresenceState::classify(500, 100, GRACE),
            PresenceState::Recycling { absence_secs: 0 }
        );
    }

    #[test]
    fn the_records_a_section_sees_are_only_its_own() {
        let mut all = BTreeMap::new();
        all.insert(
            presence_key(LOCAL_HOST_SCOPE, "vm-1"),
            record(40, Some("tart")),
        );
        all.insert(
            presence_key("ubu-3xdv", "vm-1"),
            record(9_999, Some("tart")),
        );

        let parts = partition(
            &[],
            &[expect_rule("vm-*")],
            LOCAL_HOST_SCOPE,
            &records_for_host(&all, LOCAL_HOST_SCOPE),
            Some(10_000),
            GRACE,
        );
        assert_eq!(parts.expected_absent[0].state.label(), "recycling 40s");
    }
}
