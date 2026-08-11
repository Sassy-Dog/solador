//! The Containers / VMs panel: local runtimes plus every remote agent's
//! `/v1/containers`, grouped by host, with a standing presence row for the
//! ephemeral entities that recycle out of discovery between jobs.
//!
//! Port of `DevCanopy/Views/Cockpit/Panels/ContainersPanel.swift` and the
//! services beneath it. Same discipline as `crate::settings` and
//! `crates/viewmodel`: **every string and colour the panel paints is made
//! here**, so the frontend only lays them out and cannot invent a label the
//! Swift app never had.
//!
//! The rules-editing UI is deliberately *not* in this slice (Swift keeps it in
//! Settings → Hosts): the engine and its seeds ship now, editing follows.

pub mod group;
pub mod local;
pub mod parse;
pub mod presence;

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use store::{
    records_for_host, ContainerGroupRule, ContainerPresenceRecord, DEFAULT_GRACE_SECS,
    LOCAL_HOST_SCOPE,
};
use viewmodel::cockpit::PanelKind;
use viewmodel::color;

use group::{DisplayRow, Partition};
use parse::{LocalRuntime, MergeOutcome};

/// How long without a successful local poll before the footer says so.
/// `PanelStatusFooter(..., staleAfter: 30)` in Swift.
pub const STALE_AFTER_SECS: u64 = 30;

/// Poll cadence for containers, local and remote alike — Swift's
/// `LocalContainerService.start(interval: 10)` and the remote container task's
/// own 10s loop. Deliberately slower than the 1s metrics tick: a container
/// list changes on human timescales, and `docker ps` is a process spawn.
pub const POLL_INTERVAL_SECS: u64 = 10;

/// The sentence before the first `docker ps` has returned.
///
/// Not "no containers detected", which is a finding. This panel starts with
/// every list empty, so until a pass completes those empties are the absence of
/// a measurement rather than a measurement of absence.
pub const LOADING_MESSAGE: &str = "looking for containers…";

/// Seconds since the UNIX epoch — [`crate::panel::now_unix`], re-exported here
/// because this panel's callers already reach for it under this name.
pub use crate::panel::now_unix;

/// Everything the panel renders from, and the memory that survives one bad
/// poll.
///
/// The clocks in here are wall-clock unix seconds rather than `Instant`s
/// because they are compared against presence records that outlive the
/// process.
#[derive(Debug, Default)]
pub struct ContainersState {
    /// This machine's containers, merged across runtimes (fresh + retained).
    local: Vec<wire::Container>,
    /// Runtimes actually installed here. Empty means "no container runtimes",
    /// which is a different sentence from "no containers".
    detected: Vec<LocalRuntime>,
    /// Per-runtime last-known lists, so one failing tool cannot blank its rows.
    last_known: BTreeMap<LocalRuntime, Vec<wire::Container>>,
    /// When the local poll last *completed*, whether or not it learned
    /// anything. `None` means no pass has run, which is what separates "there
    /// are no containers" from "we have not looked".
    local_last_updated: Option<u64>,
    /// When the local poll last completed **without a failing runtime**.
    ///
    /// Separate from `local_last_updated` because [`crate::panel::status_footer`]
    /// renders its argument as `last ok {age}`. Feeding it "when we last looked"
    /// made a Docker that had never once answered report
    /// `⚠ couldn't read docker · last ok 0s ago` — a reassurance about a reading
    /// that never happened.
    local_last_success: Option<u64>,
    local_error: Option<String>,
    /// Remote sections, keyed by host name and only ever written on a
    /// *successful* fetch — a failed poll leaves the previous list in place
    /// rather than emptying the section (`RemoteHostsCoordinator`, Swift).
    remote: BTreeMap<String, Vec<wire::Container>>,
    /// Per-section last **successful** poll. In-memory only, exactly as in
    /// Swift: after a relaunch no absent row appears until we have actually
    /// looked at that host again, so a restart cannot manufacture an alarm.
    last_success: BTreeMap<String, u64>,
}

impl ContainersState {
    pub fn new() -> Self {
        Self::default()
    }

    /// The last-known lists to feed the next local merge.
    pub fn last_known(&self) -> BTreeMap<LocalRuntime, Vec<wire::Container>> {
        self.last_known.clone()
    }

    /// Records one local discovery pass.
    pub fn apply_local(&mut self, detected: Vec<LocalRuntime>, outcome: MergeOutcome, now: u64) {
        self.local_error = outcome.error_message();
        self.local = outcome.merged;
        self.last_known = outcome.last_known;
        self.detected = detected;
        // Set on every pass, failures included: this is "when we last looked",
        // and it is what tells the panel it may speak at all.
        self.local_last_updated = Some(now);
        // …and this one only when every runtime answered, because the footer
        // renders it as "last ok". A pass where docker failed is not an ok.
        if self.local_error.is_none() {
            self.local_last_success = Some(now);
        }
    }

    /// Records one host's successful container fetch.
    pub fn apply_remote(&mut self, host: String, containers: Vec<wire::Container>) {
        self.remote.insert(host, containers);
    }

    /// Advances a section's successful-poll clock. Absent entities age against
    /// this, so it must only ever be called for a poll that actually heard
    /// something.
    pub fn advance_clock(&mut self, host: &str, now: u64) {
        self.last_success.insert(host.to_owned(), now);
    }

    /// Drops sections (and their clocks) for hosts that are no longer
    /// configured, so a removed host cannot leave a ghost section behind.
    pub fn retain_hosts(&mut self, configured: &BTreeSet<String>) {
        self.remote.retain(|host, _| configured.contains(host));
        self.last_success
            .retain(|host, _| host == LOCAL_HOST_SCOPE || configured.contains(host));
    }
}

/// One host section, ready to render.
struct Section<'a> {
    host: &'a str,
    containers: &'a [wire::Container],
    /// Only the local section can have *no runtimes at all*; a remote agent
    /// that answered has some, by construction.
    no_runtimes: bool,
}

/// The whole panel payload.
///
/// `now` is the wall clock at render time — the footer's staleness is measured
/// against it, while every *presence* clock is a last-successful-poll time
/// from [`ContainersState`], never this.
pub fn view(
    state: &ContainersState,
    rules: &[ContainerGroupRule],
    presence: &BTreeMap<String, ContainerPresenceRecord>,
    now: u64,
) -> Value {
    let mut sections = vec![Section {
        host: LOCAL_HOST_SCOPE,
        containers: &state.local,
        no_runtimes: state.detected.is_empty(),
    }];
    sections.extend(state.remote.iter().map(|(host, containers)| Section {
        host,
        containers,
        no_runtimes: false,
    }));

    let mut rendered = Vec::new();
    let mut missing = 0;
    for section in &sections {
        let partition = group::partition(
            section.containers,
            rules,
            section.host,
            &records_for_host(presence, section.host),
            state.last_success.get(section.host).copied(),
            DEFAULT_GRACE_SECS,
        );
        missing += partition.missing_count();
        rendered.push(section_view(section, &partition));
    }

    // Before the first pass this panel has looked at nothing, so it cannot
    // report that there is nothing — it used to open on "no containers
    // detected" on a machine running twenty of them, because every list starts
    // empty and `empty` never asked whether a poll had happened.
    let looked = state.local_last_updated.is_some();

    // "Nothing here" is one sentence, not an empty grid: an unconfigured panel
    // and a broken one must not look the same.
    let empty = looked
        && state.detected.is_empty()
        && state.local.is_empty()
        && state.remote.values().all(|list| list.is_empty());

    json!({
        "id": PanelKind::Containers.id(),
        "title": PanelKind::Containers.title(),
        // No counts before the first pass: "0 total · 0 up · 0 stopped" is three
        // measurements nobody took. Same rule as the Usage panel's empty
        // trailing string.
        "trailing": if looked { json!(trailing_label(state, missing)) } else { json!("") },
        "empty": match (looked, empty) {
            (false, _) => json!({ "message": LOADING_MESSAGE }),
            (true, true) => json!({ "message": "no containers detected" }),
            (true, false) => Value::Null,
        },
        // Sections are dropped entirely in the empty case, matching the Swift
        // panel: one sentence, not a stack of empty host headings.
        "sections": if empty || !looked { json!([]) } else { json!(rendered) },
        "footer": footer(state.local_last_success, state.local_error.as_deref(), now),
        // Drives the frontend's refresh cadence while the panel fills in.
        "loading": !looked,
    })
}

/// One host section: its heading, its rows, and the sentence that replaces
/// them when there are none.
fn section_view(section: &Section<'_>, partition: &Partition) -> Value {
    let rows = partition.display_rows();
    let empty = rows.is_empty().then(|| {
        json!({
            "message": if section.no_runtimes { "no container runtimes" } else { "no containers" },
        })
    });

    // Entity rows first, then the rule-driven aggregates — which render even
    // in an "empty" section, because a configured collapse rule is a standing
    // row and `×0` is the fact worth showing.
    let mut row_values: Vec<Value> = rows.iter().map(entity_row).collect();
    row_values.extend(partition.aggregates.iter().map(aggregate_row));

    json!({
        "host": section.host,
        "label": section.host.to_uppercase(),
        "empty": empty.unwrap_or(Value::Null),
        "rows": row_values,
    })
}

/// A present container/VM, or the standing row of one that should be here.
fn entity_row(row: &DisplayRow) -> Value {
    match row {
        DisplayRow::Present(container) => {
            let tint = if container.is_running {
                color::GREEN
            } else {
                color::MUTED
            };
            row_value(
                "present",
                &container.name,
                Some(runtime_label(&container.runtime)),
                tint,
                &container.status_text,
                tint,
            )
        }
        DisplayRow::Absent(absent) => {
            let tint = if absent.state.is_missing() {
                color::RED
            } else {
                color::AMBER
            };
            row_value(
                "absent",
                &absent.name,
                absent.runtime.as_deref().map(runtime_label),
                tint,
                &absent.state.label(),
                tint,
            )
        }
    }
}

/// One collapsed group: the match count sits in the name, the running count
/// where a container's status text would be.
fn aggregate_row(aggregate: &group::Aggregate) -> Value {
    let dot = if aggregate.is_short() {
        color::AMBER
    } else if aggregate.running > 0 {
        color::GREEN
    } else {
        color::MUTED
    };
    let status_tint = if aggregate.running > 0 {
        color::GREEN
    } else {
        color::MUTED
    };
    row_value(
        "aggregate",
        &format!("{} {}", aggregate.label, aggregate.count_text()),
        aggregate.dominant_runtime.as_deref().map(runtime_label),
        dot,
        &format!("{} running", aggregate.running),
        status_tint,
    )
}

fn row_value(
    kind: &str,
    name: &str,
    runtime: Option<String>,
    dot: u32,
    status: &str,
    status_color: u32,
) -> Value {
    json!({
        "kind": kind,
        "name": name,
        "runtime": runtime,
        "dotColor": color::hex(dot),
        "status": status,
        "statusColor": color::hex(status_color),
    })
}

/// The runtime tag under a row's name.
///
/// Lower-cased rather than mapped through an enum: Swift renders
/// `displayName.lowercased()`, which for docker/podman/tart is the raw value —
/// and a runtime this build has never heard of (an agent taught a new one)
/// must render as itself rather than disappear.
fn runtime_label(runtime: &str) -> String {
    runtime.to_lowercase()
}

/// `"12 total · 5 up · 7 stopped"`, plus `" · 1 missing"` when something
/// expected is absent beyond grace.
///
/// The totals count **everything the runtimes reported**, including containers
/// a rule hid or collapsed — cruft building up (unreaped VMs, exited job
/// containers) has to stay visible in the numbers even once it is out of the
/// rows. Missing entities get their own count because they are exactly what
/// the totals can no longer see.
fn trailing_label(state: &ContainersState, missing: usize) -> String {
    let total = state.local.len() + state.remote.values().map(Vec::len).sum::<usize>();
    let running = state.local.iter().filter(|c| c.is_running).count()
        + state
            .remote
            .values()
            .flatten()
            .filter(|c| c.is_running)
            .count();
    let mut label = format!("{total} total · {running} up · {} stopped", total - running);
    if missing > 0 {
        label.push_str(&format!(" · {missing} missing"));
    }
    label
}

/// The panel's refresh-health line, or `Null` when it is healthy and fresh.
///
/// The ladder itself is [`crate::panel::status_footer`], shared with the
/// GitHub Runners panel; all this adds is *this* panel's staleness window.
///
/// Local-only, matching Swift: the Swift panel footer watches
/// `LocalContainerService`, and a remote host's reachability is the Hosts
/// panel's story to tell, told there with its own error card.
fn footer(last_updated: Option<u64>, error: Option<&str>, now: u64) -> Value {
    crate::panel::status_footer(last_updated, error, now, STALE_AFTER_SECS)
}

/// A populated state, for the offline fixtures the Playwright suite renders
/// against (`--dump-containers`) and for the tests below.
///
/// Hand-built rather than polled, so it is byte-stable across regenerations
/// and covers the states a real machine will not reliably produce on demand:
/// a collapsed group, an expected-but-absent VM in both presence states, and a
/// remote section beside the local one.
pub fn fixture_state(
    now: u64,
) -> (
    ContainersState,
    Vec<ContainerGroupRule>,
    BTreeMap<String, ContainerPresenceRecord>,
) {
    use store::{presence_key, ContainerRuleAction};

    let container = |name: &str, status: &str, running: bool, runtime: &str| wire::Container {
        name: name.to_owned(),
        status_text: status.to_owned(),
        is_running: running,
        runtime: runtime.to_owned(),
        image: None,
    };

    let mut state = ContainersState::new();
    state.detected = vec![LocalRuntime::Docker, LocalRuntime::Tart];
    state.local = vec![
        container("acme-db", "Up 3 hours", true, "docker"),
        container("acme-cache", "Exited (0) 2 minutes ago", false, "docker"),
        container("vm-2", "running", true, "tart"),
    ];
    state.local_last_updated = Some(now);
    state.advance_clock(LOCAL_HOST_SCOPE, now);

    state.apply_remote(
        "ubu-01".to_owned(),
        vec![
            container("acme-ci-runner-1", "Up 12 minutes", true, "podman"),
            container(
                "acme-ci-runner-2",
                "Exited (0) 1 minute ago",
                false,
                "podman",
            ),
            container("postgres", "Up 6 days", true, "podman"),
        ],
    );
    state.advance_clock("ubu-01", now);

    let rules = vec![
        ContainerGroupRule::new(
            "acme-ci-runner-*",
            "ghr runners",
            ContainerRuleAction::Collapse,
        )
        .on_host("ubu-01"),
        ContainerGroupRule::new("vm-*", "", ContainerRuleAction::Expect).on_host(LOCAL_HOST_SCOPE),
    ];

    let presence = BTreeMap::from([
        (
            presence_key(LOCAL_HOST_SCOPE, "vm-1"),
            ContainerPresenceRecord {
                last_seen: now - 40,
                runtime: Some("tart".to_owned()),
            },
        ),
        (
            presence_key(LOCAL_HOST_SCOPE, "vm-3"),
            ContainerPresenceRecord {
                last_seen: now - 720,
                runtime: Some("tart".to_owned()),
            },
        ),
    ]);

    (state, rules, presence)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_700_000_000;

    fn container(name: &str, running: bool) -> wire::Container {
        wire::Container {
            name: name.to_owned(),
            status_text: if running { "Up 2 hours" } else { "Exited (0)" }.to_owned(),
            is_running: running,
            runtime: "docker".to_owned(),
            image: None,
        }
    }

    fn rows(section: &Value) -> &Vec<Value> {
        section["rows"].as_array().expect("rows")
    }

    fn fixture_view() -> Value {
        let (state, rules, presence) = fixture_state(NOW);
        view(&state, &rules, &presence, NOW)
    }

    /// A state whose local pass has completed and found no runtimes at all.
    ///
    /// The distinction most of these tests need: `ContainersState::new()` is the
    /// frame *before* anyone ran `docker ps`, and it deliberately no longer
    /// renders any finding.
    fn looked() -> ContainersState {
        let mut state = ContainersState::new();
        state.apply_local(vec![], parse::merge(vec![], BTreeMap::new()), NOW);
        state
    }

    /// The reported bug: every list starts empty, so the panel used to open on a
    /// confident "no containers detected" — on a machine running twenty of them
    /// — before the first `docker ps` had returned.
    #[test]
    fn a_panel_that_has_not_looked_yet_says_so_rather_than_reporting_nothing() {
        let payload = view(&ContainersState::new(), &[], &BTreeMap::new(), NOW);
        assert_eq!(payload["empty"]["message"], LOADING_MESSAGE);
        assert_eq!(payload["loading"], true);
        assert!(payload["sections"].as_array().expect("sections").is_empty());
        assert_eq!(
            payload["trailing"], "",
            "0 total · 0 up · 0 stopped is three measurements nobody took"
        );
        assert_eq!(payload["footer"], Value::Null, "nothing to be stale yet");
    }

    #[test]
    fn an_unconfigured_panel_says_so_instead_of_rendering_nothing() {
        let payload = view(&looked(), &[], &BTreeMap::new(), NOW);
        assert_eq!(payload["empty"]["message"], "no containers detected");
        assert_eq!(payload["loading"], false, "we looked; this is not loading");
        assert!(payload["sections"].as_array().expect("sections").is_empty());
        assert_eq!(payload["trailing"], "0 total · 0 up · 0 stopped");
    }

    #[test]
    fn a_machine_with_no_runtimes_reads_differently_from_one_with_no_containers() {
        // No runtimes at all, but a remote host is reporting: the local
        // section still renders, and it must say which of the two it is.
        let mut state = looked();
        state.apply_remote("ubu-01".to_owned(), vec![container("web", true)]);
        let payload = view(&state, &[], &BTreeMap::new(), NOW);
        let sections = payload["sections"].as_array().expect("sections");
        assert_eq!(sections[0]["host"], LOCAL_HOST_SCOPE);
        assert_eq!(sections[0]["empty"]["message"], "no container runtimes");

        let mut with_runtime = ContainersState::new();
        with_runtime.apply_local(
            vec![LocalRuntime::Docker],
            parse::merge(vec![(LocalRuntime::Docker, Some(vec![]))], BTreeMap::new()),
            NOW,
        );
        with_runtime.apply_remote("ubu-01".to_owned(), vec![container("web", true)]);
        let payload = view(&with_runtime, &[], &BTreeMap::new(), NOW);
        assert_eq!(
            payload["sections"][0]["empty"]["message"], "no containers",
            "an installed runtime reporting nothing is a different sentence"
        );
    }

    #[test]
    fn the_local_section_comes_first_and_the_remotes_follow_sorted() {
        let mut state = ContainersState::new();
        state.apply_remote("zulu".to_owned(), vec![container("z", true)]);
        state.apply_remote("alpha".to_owned(), vec![container("a", true)]);
        state.apply_local(
            vec![LocalRuntime::Docker],
            parse::merge(
                vec![(LocalRuntime::Docker, Some(vec![container("local-1", true)]))],
                BTreeMap::new(),
            ),
            NOW,
        );

        let payload = view(&state, &[], &BTreeMap::new(), NOW);
        let hosts: Vec<&str> = payload["sections"]
            .as_array()
            .expect("sections")
            .iter()
            .map(|s| s["host"].as_str().expect("host"))
            .collect();
        assert_eq!(hosts, vec![LOCAL_HOST_SCOPE, "alpha", "zulu"]);
        assert_eq!(payload["sections"][0]["label"], "THIS MACHINE");
    }

    #[test]
    fn totals_count_every_container_including_the_ones_rules_hid() {
        let mut state = ContainersState::new();
        state.apply_local(
            vec![LocalRuntime::Docker],
            parse::merge(
                vec![(
                    LocalRuntime::Docker,
                    Some(vec![
                        container("ghcr.io/base", false),
                        container("web", true),
                        container("db", true),
                    ]),
                )],
                BTreeMap::new(),
            ),
            NOW,
        );
        // The test's own rule, not the seed's: nothing is seeded any more, and
        // this test is *about* what a hide rule does to the totals, so the rule
        // doing the hiding belongs in front of the reader.
        let rules = vec![store::ContainerGroupRule::new(
            "ghcr.io/*",
            "",
            store::ContainerRuleAction::Hide,
        )];
        let payload = view(&state, &rules, &BTreeMap::new(), NOW);
        assert_eq!(
            payload["trailing"], "3 total · 2 up · 1 stopped",
            "a hidden container is still cruft on the machine and must stay in the numbers"
        );
        let names: Vec<&str> = rows(&payload["sections"][0])
            .iter()
            .map(|r| r["name"].as_str().expect("name"))
            .collect();
        assert_eq!(names, vec!["db", "web"]);
    }

    #[test]
    fn a_present_row_carries_its_status_and_a_running_colour() {
        let (state, rules, presence) = fixture_state(NOW);
        let payload = view(&state, &rules, &presence, NOW);
        let local = &payload["sections"][0];
        let running = rows(local)
            .iter()
            .find(|r| r["name"] == "acme-db")
            .expect("db row");
        assert_eq!(running["kind"], "present");
        assert_eq!(running["status"], "Up 3 hours");
        assert_eq!(running["runtime"], "docker");
        assert_eq!(running["dotColor"], color::hex(color::GREEN));
        assert_eq!(running["statusColor"], color::hex(color::GREEN));

        let stopped = rows(local)
            .iter()
            .find(|r| r["name"] == "acme-cache")
            .expect("cache row");
        assert_eq!(stopped["status"], "Exited (0) 2 minutes ago");
        assert_eq!(stopped["dotColor"], color::hex(color::MUTED));
    }

    #[test]
    fn absent_expected_rows_are_amber_while_recycling_and_red_once_missing() {
        let payload = fixture_view();
        let local = &payload["sections"][0];
        let recycling = rows(local)
            .iter()
            .find(|r| r["name"] == "vm-1")
            .expect("vm-1");
        assert_eq!(recycling["kind"], "absent");
        assert_eq!(recycling["status"], "recycling 40s");
        assert_eq!(recycling["dotColor"], color::hex(color::AMBER));

        let missing = rows(local)
            .iter()
            .find(|r| r["name"] == "vm-3")
            .expect("vm-3");
        assert_eq!(missing["status"], "missing 12m");
        assert_eq!(missing["dotColor"], color::hex(color::RED));
        assert_eq!(missing["runtime"], "tart");
    }

    #[test]
    fn the_trailing_label_counts_missing_entities_separately() {
        let payload = fixture_view();
        assert_eq!(
            payload["trailing"],
            "6 total · 4 up · 2 stopped · 1 missing"
        );
    }

    #[test]
    fn an_aggregate_row_renders_its_count_and_running_total() {
        let payload = fixture_view();
        let remote = &payload["sections"][1];
        assert_eq!(remote["host"], "ubu-01");
        let names: Vec<&str> = rows(remote)
            .iter()
            .map(|r| r["name"].as_str().expect("name"))
            .collect();
        assert_eq!(
            names,
            vec!["postgres", "ghr runners ×2"],
            "entity rows first, then the rule-driven aggregate"
        );
        let aggregate = rows(remote).last().expect("aggregate");
        assert_eq!(aggregate["kind"], "aggregate");
        assert_eq!(aggregate["status"], "1 running");
        assert_eq!(aggregate["runtime"], "podman");
        assert_eq!(aggregate["dotColor"], color::hex(color::GREEN));
    }

    #[test]
    fn a_standing_aggregate_renders_in_a_section_with_no_entity_rows() {
        let mut state = ContainersState::new();
        state.apply_local(
            vec![LocalRuntime::Docker],
            parse::merge(vec![(LocalRuntime::Docker, Some(vec![]))], BTreeMap::new()),
            NOW,
        );
        let mut rule = ContainerGroupRule::new(
            "api-*",
            "workflow jobs",
            store::ContainerRuleAction::Collapse,
        );
        rule.expected_count = Some(4);
        let payload = view(&state, &[rule], &BTreeMap::new(), NOW);
        let section = &payload["sections"][0];
        assert_eq!(section["empty"]["message"], "no containers");
        let aggregate = &rows(section)[0];
        assert_eq!(aggregate["name"], "workflow jobs ×0/4");
        assert_eq!(
            aggregate["dotColor"],
            color::hex(color::AMBER),
            "a group short of its expected count warns"
        );
        assert_eq!(aggregate["runtime"], Value::Null);
    }

    #[test]
    fn a_healthy_fresh_panel_renders_no_footer() {
        let payload = fixture_view();
        assert_eq!(payload["footer"], Value::Null);
    }

    #[test]
    fn a_stale_reading_says_so_rather_than_passing_as_current() {
        let (mut state, rules, presence) = fixture_state(NOW);
        state.local_last_success = Some(NOW - 120);
        let payload = view(&state, &rules, &presence, NOW);
        assert_eq!(payload["footer"]["text"], "⚠ stale · updated 2m ago");
        assert_eq!(payload["footer"]["color"], color::hex(color::AMBER));

        state.local_last_success = Some(NOW - STALE_AFTER_SECS);
        let fresh_enough = view(&state, &rules, &presence, NOW);
        assert_eq!(
            fresh_enough["footer"],
            Value::Null,
            "exactly at the threshold is not yet stale"
        );
    }

    #[test]
    fn a_failed_runtime_names_itself_in_the_footer_and_keeps_its_rows() {
        let mut state = ContainersState::new();
        // First poll succeeds...
        state.apply_local(
            vec![LocalRuntime::Docker, LocalRuntime::Tart],
            parse::merge(
                vec![
                    (LocalRuntime::Docker, Some(vec![container("web", true)])),
                    (LocalRuntime::Tart, Some(vec![container("vm-1", true)])),
                ],
                BTreeMap::new(),
            ),
            NOW - 60,
        );
        // ...then tart breaks.
        let last_known = state.last_known();
        state.apply_local(
            vec![LocalRuntime::Docker, LocalRuntime::Tart],
            parse::merge(
                vec![
                    (LocalRuntime::Docker, Some(vec![container("web", true)])),
                    (LocalRuntime::Tart, None),
                ],
                last_known,
            ),
            NOW,
        );

        let payload = view(&state, &[], &BTreeMap::new(), NOW);
        // A minute, which is when tart last actually answered — not the `0s ago`
        // this asserted while the footer was fed "when we last looked". The
        // failing pass advances that clock, so a permanently broken runtime
        // reported itself as freshly ok on every single poll.
        assert_eq!(
            payload["footer"]["text"],
            "⚠ couldn't read tart · last ok 1m ago"
        );
        let names: Vec<&str> = rows(&payload["sections"][0])
            .iter()
            .map(|r| r["name"].as_str().expect("name"))
            .collect();
        assert_eq!(
            names,
            vec!["vm-1", "web"],
            "a broken reading must not blank rows for VMs that still exist"
        );
    }

    #[test]
    fn a_removed_host_takes_its_section_and_its_clock_with_it() {
        let mut state = ContainersState::new();
        state.apply_local(
            vec![LocalRuntime::Docker],
            parse::merge(
                vec![(LocalRuntime::Docker, Some(vec![container("local-1", true)]))],
                BTreeMap::new(),
            ),
            NOW,
        );
        state.apply_remote("ubu-01".to_owned(), vec![container("web", true)]);
        state.advance_clock("ubu-01", NOW);
        state.advance_clock(LOCAL_HOST_SCOPE, NOW);

        state.retain_hosts(&BTreeSet::new());
        let payload = view(&state, &[], &BTreeMap::new(), NOW);
        let sections = payload["sections"].as_array().expect("sections");
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0]["host"], LOCAL_HOST_SCOPE);
        assert!(!state.last_success.contains_key("ubu-01"));
        assert!(
            state.last_success.contains_key(LOCAL_HOST_SCOPE),
            "the local section is never 'unconfigured'"
        );
    }

    /// A runtime that has never once answered has no "last ok" to report, and
    /// `status_footer` drops the suffix rather than inventing one.
    #[test]
    fn a_runtime_that_never_answered_reports_no_last_ok_at_all() {
        let mut state = ContainersState::new();
        state.apply_local(
            vec![LocalRuntime::Docker],
            parse::merge(vec![(LocalRuntime::Docker, None)], BTreeMap::new()),
            NOW,
        );
        let payload = view(&state, &[], &BTreeMap::new(), NOW);
        assert_eq!(payload["footer"]["text"], "⚠ couldn't read docker");
        assert_eq!(
            payload["loading"], false,
            "the pass completed — it just did not go well"
        );
    }

    #[test]
    fn a_failed_remote_poll_keeps_the_hosts_previous_rows() {
        // `apply_remote` is only ever called on success, so a failed poll is
        // simply the absence of a call — this pins that the section survives it.
        let mut state = looked();
        state.apply_remote("ubu-01".to_owned(), vec![container("web", true)]);
        state.retain_hosts(&BTreeSet::from(["ubu-01".to_owned()]));
        let payload = view(&state, &[], &BTreeMap::new(), NOW);
        assert_eq!(payload["sections"][1]["rows"][0]["name"], "web");
    }

    #[test]
    fn the_panel_carries_the_shared_title_and_id() {
        let payload = fixture_view();
        assert_eq!(payload["id"], "containers");
        assert_eq!(payload["title"], "Containers / VMs");
    }
}
