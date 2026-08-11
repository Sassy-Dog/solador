//! Recording what a poll saw — the write side of presence memory.
//!
//! Port of `ContainerPresenceStore.note(...)`
//! (`DevCanopy/Services/Containers/ContainerPresenceStore.swift`), over the
//! records `crates/store` persists.
//!
//! **Clock discipline is the whole point of this file.** A record's `last_seen`
//! advances only when the container's own runtime polled successfully, and the
//! host clock advances only on a successful poll — so a failing source freezes
//! every clock it owns instead of ageing its entities toward a false "missing"
//! alarm. Get that wrong and the panel turns red because *the reading* broke,
//! not because anything did.

use std::collections::{BTreeMap, BTreeSet};

use store::{presence_key, ContainerGroupRule, ContainerPresenceRecord, ContainerRuleAction};

/// What one recorded poll changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoteOutcome {
    /// Whether any record was added, updated or pruned — the store is only
    /// written when this is true, so a poll that learned nothing does not
    /// rewrite the file.
    pub records_changed: bool,
    /// Whether this host's "last successful poll" clock may advance. Absent
    /// entities age against that clock, so it must not move on a poll that
    /// heard nothing.
    pub clock_advances: bool,
}

/// Folds one poll's result into the presence records.
///
/// `succeeded` names the runtimes whose poll actually answered, and `None`
/// means "the whole host reported" — the shape of a remote
/// `GET /v1/containers`, which either returns the host's full list or fails
/// outright. A local poll passes `Some(..)` because its runtimes fail
/// independently.
///
/// Returns what changed; the caller owns persisting it and moving the clock.
pub fn note(
    records: &mut BTreeMap<String, ContainerPresenceRecord>,
    host: &str,
    containers: &[wire::Container],
    succeeded: Option<&BTreeSet<&str>>,
    rules: &[ContainerGroupRule],
    now: u64,
) -> NoteOutcome {
    let applicable: Vec<&ContainerGroupRule> = rules
        .iter()
        .filter(|rule| rule.applies_to(host) && rule.action == ContainerRuleAction::Expect)
        .collect();

    let is_expected = |name: &str| -> bool { applicable.iter().any(|rule| rule.matches(name)) };
    // A record whose runtime is `None` (seeded, never observed) belongs to no
    // source, so no failing source protects it.
    let has_fresh_facts = |runtime: Option<&str>| -> bool {
        match (succeeded, runtime) {
            (None, _) | (_, None) => true,
            (Some(ok), Some(runtime)) => ok.contains(runtime),
        }
    };

    let mut updated = records.clone();

    for container in containers {
        if is_expected(&container.name) && has_fresh_facts(Some(&container.runtime)) {
            updated.insert(
                presence_key(host, &container.name),
                ContainerPresenceRecord {
                    last_seen: now,
                    runtime: Some(container.runtime.clone()),
                },
            );
        }
    }

    // Seed exact-name expectations that nothing has ever reported: the clock
    // starts at the rule's first sighting-less poll, so a typo'd or
    // decommissioned name escalates to "missing" instead of sitting silent
    // forever. Globs learn observed names only — they never invent one.
    for rule in applicable.iter().filter(|rule| rule.is_exact_name()) {
        updated
            .entry(presence_key(host, &rule.pattern))
            .or_insert(ContainerPresenceRecord {
                last_seen: now,
                runtime: None,
            });
    }

    // Prune this host's records whose expect rule is gone — unless the
    // record's runtime is currently failing, in which case the record is
    // frozen: no fresh facts, no mutations of any kind.
    let prefix = format!("{host}|");
    let doomed: Vec<String> = updated
        .iter()
        .filter(|(key, record)| {
            key.strip_prefix(&prefix).is_some_and(|name| {
                !is_expected(name) && has_fresh_facts(record.runtime.as_deref())
            })
        })
        .map(|(key, _)| key.clone())
        .collect();
    for key in doomed {
        updated.remove(&key);
    }

    // The host clock advances when anything reported. A still-failing runtime's
    // absent entities do age against it: the conservative alternative — freezing
    // the whole host because one runtime is down — would hide real alarms, and
    // that runtime's *present* entities are already protected by last-known
    // retention upstream.
    let clock_advances = succeeded.is_none_or(|ok| !ok.is_empty());
    let records_changed = updated != *records;
    if records_changed {
        *records = updated;
    }
    NoteOutcome {
        records_changed,
        clock_advances,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::{records_for_host, LOCAL_HOST_SCOPE};

    const NOW: u64 = 1_700_000_000;

    fn vm(name: &str, runtime: &str) -> wire::Container {
        wire::Container {
            name: name.to_owned(),
            status_text: "running".to_owned(),
            is_running: true,
            runtime: runtime.to_owned(),
            image: None,
        }
    }

    fn expect_rule(pattern: &str) -> ContainerGroupRule {
        ContainerGroupRule::new(pattern, "", ContainerRuleAction::Expect)
    }

    fn ok(runtimes: &[&'static str]) -> BTreeSet<&'static str> {
        runtimes.iter().copied().collect()
    }

    /// A local poll: runtimes fail independently, so `succeeded` is explicit.
    fn note_local(
        records: &mut BTreeMap<String, ContainerPresenceRecord>,
        containers: &[wire::Container],
        succeeded: &BTreeSet<&str>,
        rules: &[ContainerGroupRule],
        now: u64,
    ) -> NoteOutcome {
        note(
            records,
            LOCAL_HOST_SCOPE,
            containers,
            Some(succeeded),
            rules,
            now,
        )
    }

    #[test]
    fn an_expected_container_is_recorded_and_the_clock_advances() {
        let mut records = BTreeMap::new();
        let outcome = note_local(
            &mut records,
            &[vm("vm-1", "tart")],
            &ok(&["tart"]),
            &[expect_rule("vm-*")],
            NOW,
        );
        assert!(outcome.records_changed);
        assert!(outcome.clock_advances);
        let mine = records_for_host(&records, LOCAL_HOST_SCOPE);
        assert_eq!(mine["vm-1"].last_seen, NOW);
        assert_eq!(mine["vm-1"].runtime.as_deref(), Some("tart"));
    }

    #[test]
    fn containers_with_no_expect_rule_are_not_recorded() {
        let mut records = BTreeMap::new();
        let outcome = note_local(
            &mut records,
            &[vm("vm-1", "tart")],
            &ok(&["tart"]),
            &[ContainerGroupRule::new(
                "vm-*",
                "vms",
                ContainerRuleAction::Collapse,
            )],
            NOW,
        );
        assert!(!outcome.records_changed);
        assert!(records.is_empty());
    }

    #[test]
    fn a_failed_runtime_neither_records_nor_advances_the_clock() {
        let mut records = BTreeMap::new();
        // Every runtime failed: the merged list is last-known data, which is a
        // retained UI row, not a sighting.
        let outcome = note_local(
            &mut records,
            &[vm("vm-1", "tart")],
            &BTreeSet::new(),
            &[expect_rule("vm-*")],
            NOW,
        );
        assert!(!outcome.records_changed);
        assert!(
            !outcome.clock_advances,
            "a poll that heard nothing must not age anything toward 'missing'"
        );
        assert!(records.is_empty());
    }

    #[test]
    fn one_failing_runtime_does_not_stop_another_from_recording() {
        let mut records = BTreeMap::new();
        let outcome = note_local(
            &mut records,
            &[vm("vm-1", "tart"), vm("api-1", "podman")],
            &ok(&["podman"]),
            &[expect_rule("*")],
            NOW,
        );
        assert!(outcome.clock_advances);
        let mine = records_for_host(&records, LOCAL_HOST_SCOPE);
        assert!(mine.contains_key("api-1"));
        assert!(
            !mine.contains_key("vm-1"),
            "tart failed, so its retained rows are not sightings"
        );
    }

    #[test]
    fn a_failing_runtimes_record_survives_rule_removal_until_it_recovers() {
        let mut records = BTreeMap::new();
        note_local(
            &mut records,
            &[vm("vm-1", "tart")],
            &ok(&["tart"]),
            &[expect_rule("vm-*")],
            NOW,
        );

        // The rule is gone, but tart is failing: freeze, don't prune.
        note_local(&mut records, &[], &BTreeSet::new(), &[], NOW + 10);
        assert!(records_for_host(&records, LOCAL_HOST_SCOPE).contains_key("vm-1"));

        // tart recovers and the rule is still gone: now it prunes.
        let outcome = note_local(&mut records, &[], &ok(&["tart"]), &[], NOW + 20);
        assert!(outcome.records_changed);
        assert!(records_for_host(&records, LOCAL_HOST_SCOPE).is_empty());
    }

    #[test]
    fn an_exact_name_expect_rule_seeds_an_unobserved_record() {
        let mut records = BTreeMap::new();
        note_local(
            &mut records,
            &[],
            &ok(&["tart"]),
            &[expect_rule("vm-1")],
            NOW,
        );
        let mine = records_for_host(&records, LOCAL_HOST_SCOPE);
        assert_eq!(mine["vm-1"].last_seen, NOW);
        assert_eq!(
            mine["vm-1"].runtime, None,
            "nothing has reported it, so it belongs to no runtime"
        );
    }

    #[test]
    fn a_glob_expect_rule_seeds_nothing() {
        let mut records = BTreeMap::new();
        note_local(
            &mut records,
            &[],
            &ok(&["tart"]),
            &[expect_rule("vm-*")],
            NOW,
        );
        assert!(records.is_empty(), "a glob must never invent a name");
    }

    #[test]
    fn seeding_does_not_reset_an_existing_record() {
        let mut records = BTreeMap::new();
        let rules = [expect_rule("vm-1")];
        note_local(
            &mut records,
            &[vm("vm-1", "tart")],
            &ok(&["tart"]),
            &rules,
            NOW,
        );
        // A later poll that does not see it must leave `last_seen` where it
        // was — otherwise absence could never accumulate.
        note_local(&mut records, &[], &ok(&["tart"]), &rules, NOW + 500);
        let mine = records_for_host(&records, LOCAL_HOST_SCOPE);
        assert_eq!(mine["vm-1"].last_seen, NOW);
        assert_eq!(mine["vm-1"].runtime.as_deref(), Some("tart"));
    }

    #[test]
    fn pruning_drops_records_whose_rule_is_gone() {
        let mut records = BTreeMap::new();
        note_local(
            &mut records,
            &[vm("vm-1", "tart")],
            &ok(&["tart"]),
            &[expect_rule("vm-*")],
            NOW,
        );
        note_local(
            &mut records,
            &[vm("vm-1", "tart")],
            &ok(&["tart"]),
            &[],
            NOW + 10,
        );
        assert!(records.is_empty());
    }

    #[test]
    fn a_local_prune_spares_another_hosts_records() {
        let mut records = BTreeMap::new();
        note(
            &mut records,
            "ubu-01",
            &[vm("vm-1", "tart")],
            None,
            &[expect_rule("vm-*")],
            NOW,
        );
        // No rules at all now, but the local poll may only prune local keys.
        note_local(&mut records, &[], &ok(&["tart"]), &[], NOW + 10);
        assert!(records_for_host(&records, "ubu-01").contains_key("vm-1"));
    }

    #[test]
    fn a_host_scoped_rule_only_records_on_its_host() {
        let rules = [expect_rule("vm-*").on_host("ubu-01")];
        let mut records = BTreeMap::new();
        note_local(
            &mut records,
            &[vm("vm-1", "tart")],
            &ok(&["tart"]),
            &rules,
            NOW,
        );
        assert!(records.is_empty());

        note(
            &mut records,
            "ubu-01",
            &[vm("vm-1", "tart")],
            None,
            &rules,
            NOW,
        );
        assert!(records_for_host(&records, "ubu-01").contains_key("vm-1"));
    }

    #[test]
    fn a_remote_poll_records_and_advances_its_own_clock() {
        let mut records = BTreeMap::new();
        let outcome = note(
            &mut records,
            "ubu-01",
            &[vm("vm-1", "tart")],
            None,
            &[expect_rule("vm-*")],
            NOW,
        );
        assert!(outcome.records_changed);
        assert!(outcome.clock_advances);
        assert_eq!(records_for_host(&records, "ubu-01")["vm-1"].last_seen, NOW);
    }

    #[test]
    fn a_repeat_poll_that_learns_nothing_reports_no_change() {
        let mut records = BTreeMap::new();
        let rules = [expect_rule("vm-1")];
        note_local(
            &mut records,
            &[vm("vm-1", "tart")],
            &ok(&["tart"]),
            &rules,
            NOW,
        );
        let outcome = note_local(
            &mut records,
            &[vm("vm-1", "tart")],
            &ok(&["tart"]),
            &rules,
            NOW,
        );
        assert!(
            !outcome.records_changed,
            "an unchanged poll must not rewrite the store file"
        );
    }
}
