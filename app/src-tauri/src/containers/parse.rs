//! Tool-output parsing and the per-runtime merge — the pure half of local
//! container discovery.
//!
//! Ports `ContainerParsing` and `LocalContainerMerge` from
//! `DevCanopy/Services/Containers/`. No I/O here; spawning lives in
//! [`super::local`].

use std::collections::{BTreeMap, BTreeSet};

/// A container runtime / VM manager this machine may have installed.
///
/// Local only. A *remote* host's runtimes arrive as `wire::Container::runtime`
/// strings and are deliberately not parsed into this enum — the agent may
/// learn a runtime this build has never heard of, and an unknown label must
/// render as itself rather than fail a decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LocalRuntime {
    Docker,
    Podman,
    Tart,
}

impl LocalRuntime {
    /// Every runtime, in the Swift `CaseIterable` declaration order — which is
    /// also the order they are polled and therefore the order an error string
    /// names them in.
    pub const ALL: [LocalRuntime; 3] = [
        LocalRuntime::Docker,
        LocalRuntime::Podman,
        LocalRuntime::Tart,
    ];

    /// The executable name, the wire `runtime` value and the display label all
    /// at once: Swift's `rawValue`, `toolName` and
    /// `displayName.lowercased()` are the same string for all three runtimes,
    /// and keeping them one value is what stops the panel labelling a
    /// container with a runtime no `ps` invocation could have produced.
    pub fn id(self) -> &'static str {
        match self {
            LocalRuntime::Docker => "docker",
            LocalRuntime::Podman => "podman",
            LocalRuntime::Tart => "tart",
        }
    }
}

/// `docker ps -a --format '{{.Names}}|{{.Status}}|{{.Image}}'` — and the
/// identical podman invocation.
///
/// `is_running` is decided by the status text beginning with `"Up"`, which is
/// docker's own vocabulary ("Up 3 hours", "Exited (0) 2 minutes ago"); a line
/// with no name, or with fewer than two fields, is not a container and is
/// dropped rather than rendered as a blank row.
pub fn parse_ps_output(output: &str, runtime: LocalRuntime) -> Vec<wire::Container> {
    output
        .split('\n')
        .filter_map(|raw| {
            let line = raw.trim();
            if line.is_empty() {
                return None;
            }
            let fields: Vec<&str> = line.split('|').collect();
            if fields.len() < 2 {
                return None;
            }
            let name = fields[0].trim();
            if name.is_empty() {
                return None;
            }
            let status_text = fields[1].trim();
            let image = fields.get(2).map(|i| i.trim()).unwrap_or_default();
            Some(wire::Container {
                name: name.to_owned(),
                status_text: status_text.to_owned(),
                is_running: status_text.starts_with("Up"),
                runtime: runtime.id().to_owned(),
                image: (!image.is_empty()).then(|| image.to_owned()),
            })
        })
        .collect()
}

/// `tart list` — a whitespace-aligned table:
///
/// ```text
/// Source Name        Disk Size State
/// local  ci-runner-1 50   20   running
/// ```
///
/// Name is the second column and state the last, so a future column added in
/// the middle does not shift either. The header row is skipped by its first
/// column, and a VM has no image (`None`, never an invented one).
pub fn parse_tart_list(output: &str) -> Vec<wire::Container> {
    output
        .split('\n')
        .filter_map(|raw| {
            let line = raw.trim();
            if line.is_empty() {
                return None;
            }
            let columns: Vec<&str> = line.split_whitespace().collect();
            // Need at least a name and a state.
            if columns.len() < 2 || columns[0] == "Source" {
                return None;
            }
            let state = columns[columns.len() - 1];
            Some(wire::Container {
                name: columns[1].to_owned(),
                status_text: state.to_owned(),
                is_running: state == "running",
                runtime: LocalRuntime::Tart.id().to_owned(),
                image: None,
            })
        })
        .collect()
}

/// What one merge pass produced.
pub struct MergeOutcome {
    /// Every container to render, fresh and retained alike.
    pub merged: Vec<wire::Container>,
    /// The last-known map to carry into the next poll.
    pub last_known: BTreeMap<LocalRuntime, Vec<wire::Container>>,
    /// Runtimes whose poll failed, in poll order — the footer names them.
    pub errored: Vec<&'static str>,
    /// Runtimes that actually answered. Presence clocks may only advance for
    /// these: a failed source proves nothing about a container's absence.
    pub succeeded: BTreeSet<LocalRuntime>,
}

impl MergeOutcome {
    /// The footer's error line, or `None` when every attempted runtime
    /// answered. Swift: `"couldn't read \(errored.joined(separator: ", "))"`.
    pub fn error_message(&self) -> Option<String> {
        (!self.errored.is_empty()).then(|| format!("couldn't read {}", self.errored.join(", ")))
    }
}

/// Merges this poll's results with the previous ones, per runtime.
///
/// The invariant: **a runtime whose poll failed contributes its last-known
/// list, not nothing.** One transient `tart list` failure must not blank every
/// VM row until the next tick — the VMs did not go anywhere, only the reading
/// did. A runtime that was not attempted at all (not installed) keeps whatever
/// cache it has and contributes nothing.
pub fn merge(
    results: Vec<(LocalRuntime, Option<Vec<wire::Container>>)>,
    mut last_known: BTreeMap<LocalRuntime, Vec<wire::Container>>,
) -> MergeOutcome {
    let mut merged = Vec::new();
    let mut errored = Vec::new();
    let mut succeeded = BTreeSet::new();

    for (runtime, containers) in results {
        match containers {
            Some(fresh) => {
                merged.extend(fresh.iter().cloned());
                // Wholesale replacement, not a union: a container that is gone
                // must disappear, and merging the old list in would make
                // removal impossible.
                last_known.insert(runtime, fresh);
                succeeded.insert(runtime);
            }
            None => {
                merged.extend(last_known.get(&runtime).cloned().unwrap_or_default());
                errored.push(runtime.id());
            }
        }
    }

    MergeOutcome {
        merged,
        last_known,
        errored,
        succeeded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(containers: &[wire::Container]) -> Vec<&str> {
        containers.iter().map(|c| c.name.as_str()).collect()
    }

    fn container(name: &str, runtime: LocalRuntime) -> wire::Container {
        wire::Container {
            name: name.to_owned(),
            status_text: "Up 1 hour".to_owned(),
            is_running: true,
            runtime: runtime.id().to_owned(),
            image: None,
        }
    }

    #[test]
    fn ps_output_parses_pipe_delimited_running_and_stopped() {
        let parsed = parse_ps_output(
            "web|Up 3 hours|nginx:latest\ndb|Exited (0) 2 minutes ago|postgres:16",
            LocalRuntime::Docker,
        );
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "web");
        assert_eq!(parsed[0].status_text, "Up 3 hours");
        assert!(parsed[0].is_running);
        assert_eq!(parsed[0].runtime, "docker");
        assert_eq!(parsed[0].image.as_deref(), Some("nginx:latest"));
        assert_eq!(parsed[1].name, "db");
        assert!(!parsed[1].is_running);
        assert_eq!(parsed[1].image.as_deref(), Some("postgres:16"));
    }

    #[test]
    fn ps_output_skips_blank_and_nameless_lines() {
        let parsed = parse_ps_output(
            "\nweb|Up 3 hours|nginx\n\n |Up 1 hour|ghost\nlonely\n",
            LocalRuntime::Podman,
        );
        assert_eq!(names(&parsed), vec!["web"]);
        assert_eq!(parsed[0].runtime, "podman");
    }

    #[test]
    fn ps_output_handles_a_missing_image() {
        let parsed = parse_ps_output("web|Up 3 hours", LocalRuntime::Docker);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].image, None);
        let empty_field = parse_ps_output("web|Up 3 hours|", LocalRuntime::Docker);
        assert_eq!(empty_field[0].image, None);
    }

    #[test]
    fn empty_ps_output_yields_nothing() {
        assert!(parse_ps_output("", LocalRuntime::Docker).is_empty());
        assert!(parse_ps_output("\n\n", LocalRuntime::Docker).is_empty());
    }

    #[test]
    fn tart_list_parses_rows_and_state() {
        let parsed = parse_tart_list(
            "Source Name Disk Size State\nlocal  ci-runner-1  50  20  running\nlocal  ci-runner-2  50  20  stopped",
        );
        assert_eq!(names(&parsed), vec!["ci-runner-1", "ci-runner-2"]);
        assert!(parsed[0].is_running);
        assert_eq!(parsed[0].status_text, "running");
        assert_eq!(parsed[0].runtime, "tart");
        assert_eq!(
            parsed[0].image, None,
            "a VM has no image, and none may be invented"
        );
        assert!(!parsed[1].is_running);
    }

    #[test]
    fn tart_list_skips_the_header_and_blank_lines() {
        let parsed = parse_tart_list("Source Name Disk Size State\n\nlocal vm-1 50 20 running\n\n");
        assert_eq!(names(&parsed), vec!["vm-1"]);
    }

    #[test]
    fn empty_tart_list_yields_nothing() {
        assert!(parse_tart_list("").is_empty());
        assert!(parse_tart_list("Source Name Disk Size State").is_empty());
    }

    #[test]
    fn a_successful_runtime_contributes_fresh_data_and_updates_its_cache() {
        let outcome = merge(
            vec![(
                LocalRuntime::Docker,
                Some(vec![container("web", LocalRuntime::Docker)]),
            )],
            BTreeMap::new(),
        );
        assert_eq!(names(&outcome.merged), vec!["web"]);
        assert_eq!(
            names(&outcome.last_known[&LocalRuntime::Docker]),
            vec!["web"]
        );
        assert!(outcome.errored.is_empty());
        assert!(outcome.error_message().is_none());
        assert_eq!(outcome.succeeded, BTreeSet::from([LocalRuntime::Docker]));
    }

    #[test]
    fn a_failed_runtime_contributes_its_last_known_list() {
        let last_known = BTreeMap::from([(
            LocalRuntime::Tart,
            vec![container("vm-1", LocalRuntime::Tart)],
        )]);
        let outcome = merge(vec![(LocalRuntime::Tart, None)], last_known);
        assert_eq!(
            names(&outcome.merged),
            vec!["vm-1"],
            "one failed `tart list` must not blank every VM row"
        );
        assert_eq!(
            names(&outcome.last_known[&LocalRuntime::Tart]),
            vec!["vm-1"]
        );
        assert_eq!(outcome.errored, vec!["tart"]);
        assert_eq!(
            outcome.error_message().as_deref(),
            Some("couldn't read tart")
        );
        assert!(outcome.succeeded.is_empty());
    }

    #[test]
    fn a_failure_with_no_history_contributes_nothing() {
        let outcome = merge(vec![(LocalRuntime::Docker, None)], BTreeMap::new());
        assert!(outcome.merged.is_empty());
        assert_eq!(outcome.errored, vec!["docker"]);
    }

    #[test]
    fn a_success_replaces_the_stale_cache_wholesale() {
        let last_known = BTreeMap::from([(
            LocalRuntime::Docker,
            vec![container("old", LocalRuntime::Docker)],
        )]);
        let outcome = merge(
            vec![(
                LocalRuntime::Docker,
                Some(vec![container("new", LocalRuntime::Docker)]),
            )],
            last_known,
        );
        assert_eq!(
            names(&outcome.merged),
            vec!["new"],
            "a removed container must disappear, so the merge cannot be a union"
        );
    }

    #[test]
    fn mixed_results_merge_fresh_with_retained() {
        let last_known = BTreeMap::from([
            (
                LocalRuntime::Docker,
                vec![container("stale-web", LocalRuntime::Docker)],
            ),
            (
                LocalRuntime::Tart,
                vec![container("vm-1", LocalRuntime::Tart)],
            ),
        ]);
        let outcome = merge(
            vec![
                (
                    LocalRuntime::Docker,
                    Some(vec![container("web", LocalRuntime::Docker)]),
                ),
                (LocalRuntime::Tart, None),
            ],
            last_known,
        );
        assert_eq!(names(&outcome.merged), vec!["web", "vm-1"]);
        assert_eq!(outcome.errored, vec!["tart"]);
        assert_eq!(outcome.succeeded, BTreeSet::from([LocalRuntime::Docker]));
    }

    #[test]
    fn a_runtime_that_was_not_attempted_keeps_its_cache_and_contributes_nothing() {
        let last_known = BTreeMap::from([(
            LocalRuntime::Podman,
            vec![container("pod-1", LocalRuntime::Podman)],
        )]);
        let outcome = merge(
            vec![(
                LocalRuntime::Docker,
                Some(vec![container("web", LocalRuntime::Docker)]),
            )],
            last_known,
        );
        assert_eq!(names(&outcome.merged), vec!["web"]);
        assert_eq!(
            names(&outcome.last_known[&LocalRuntime::Podman]),
            vec!["pod-1"]
        );
        assert!(outcome.errored.is_empty());
    }

    #[test]
    fn every_runtime_reports_one_spelling_for_tool_wire_and_label() {
        assert_eq!(
            LocalRuntime::ALL.map(LocalRuntime::id),
            ["docker", "podman", "tart"]
        );
    }
}
