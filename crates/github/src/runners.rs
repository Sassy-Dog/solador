//! Self-hosted runner DTOs and the idle/busy/offline mapping the Runners panel
//! renders. Port of `DevCanopy/Services/GitHub/GHRunner.swift`.

use serde::{Deserialize, Serialize};

/// `GET /orgs/{org}/actions/runners`.
#[derive(Debug, Clone, Deserialize)]
pub struct RunnersResponse {
    pub total_count: u32,
    pub runners: Vec<RunnerDto>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunnerDto {
    pub id: i64,
    pub name: String,
    pub os: String,
    /// `"online"` | `"offline"`.
    pub status: String,
    pub busy: bool,
    #[serde(default)]
    pub labels: Option<Vec<RunnerLabel>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunnerLabel {
    pub name: String,
}

/// The serialised names match the Swift `RunnerOS` raw values, so a roster
/// written by either side reads on the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunnerOs {
    #[serde(rename = "macOS")]
    MacOs,
    #[serde(rename = "linux")]
    Linux,
    #[serde(rename = "windows")]
    Windows,
    #[serde(rename = "other")]
    Other,
}

impl RunnerOs {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            RunnerOs::MacOs => "macOS",
            RunnerOs::Linux => "Linux",
            RunnerOs::Windows => "Windows",
            RunnerOs::Other => "Other",
        }
    }

    /// The **persisted** spelling — the Swift `RunnerOS` raw value, and what
    /// the `Serialize`/`Deserialize` impls above write.
    ///
    /// Deliberately not [`RunnerOs::label`]: that is display text (`"Linux"`,
    /// `"Windows"`, `"Other"`) and this is a storage format (`"linux"`,
    /// `"windows"`, `"other"`). They differ for three of the four cases, and a
    /// roster written with the display spelling would read back as `Other` on
    /// every entry.
    #[must_use]
    pub const fn as_raw(self) -> &'static str {
        match self {
            RunnerOs::MacOs => "macOS",
            RunnerOs::Linux => "linux",
            RunnerOs::Windows => "windows",
            RunnerOs::Other => "other",
        }
    }

    /// Inverse of [`RunnerOs::as_raw`]. An unrecognised value reads as
    /// [`RunnerOs::Other`] rather than failing the whole roster: one entry
    /// written by a newer build must not cost us every remembered runner.
    #[must_use]
    pub fn from_raw(raw: &str) -> Self {
        match raw {
            "macOS" => RunnerOs::MacOs,
            "linux" => RunnerOs::Linux,
            "windows" => RunnerOs::Windows,
            _ => RunnerOs::Other,
        }
    }

    /// Panel display order: macOS, then Linux, then Windows, then everything
    /// else.
    #[must_use]
    pub fn rank(self) -> u8 {
        match self {
            RunnerOs::MacOs => 0,
            RunnerOs::Linux => 1,
            RunnerOs::Windows => 2,
            RunnerOs::Other => 3,
        }
    }
}

/// Idle/busy/offline — the thing the panel shows at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerState {
    Idle,
    Busy,
    Offline,
}

impl RunnerState {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            RunnerState::Idle => "idle",
            RunnerState::Busy => "busy",
            RunnerState::Offline => "offline",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhRunner {
    pub id: i64,
    pub name: String,
    pub os: RunnerOs,
    pub state: RunnerState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunnerSummary {
    pub total: usize,
    pub online: usize,
    pub busy: usize,
    pub idle: usize,
    pub macos_online: usize,
    pub macos_total: usize,
    pub linux_online: usize,
    pub linux_total: usize,
    pub windows_online: usize,
    pub windows_total: usize,
    /// Everything that is not one of the three tracked platforms. Counted so
    /// the panel can say a runner exists on something it does not name,
    /// instead of leaving it out of the per-platform row while still counting
    /// it in the total.
    pub other_online: usize,
    pub other_total: usize,
}

/// DTOs to renderable runners.
#[must_use]
pub fn map(dtos: &[RunnerDto]) -> Vec<GhRunner> {
    dtos.iter()
        .map(|dto| GhRunner {
            id: dto.id,
            name: dto.name.clone(),
            os: os_of(dto),
            state: state_of(dto),
        })
        .collect()
}

/// Offline wins over busy: a runner GitHub reports as offline is offline no
/// matter what its stale `busy` flag says.
fn state_of(dto: &RunnerDto) -> RunnerState {
    if !dto.status.eq_ignore_ascii_case("online") {
        return RunnerState::Offline;
    }
    if dto.busy {
        RunnerState::Busy
    } else {
        RunnerState::Idle
    }
}

fn os_of(dto: &RunnerDto) -> RunnerOs {
    let os = dto.os.to_ascii_lowercase();
    if os.contains("mac") || os.contains("darwin") {
        RunnerOs::MacOs
    } else if os.contains("linux") {
        RunnerOs::Linux
    } else if os.contains("win") {
        RunnerOs::Windows
    } else {
        RunnerOs::Other
    }
}

#[must_use]
pub fn summarize(runners: &[GhRunner]) -> RunnerSummary {
    let count = |f: fn(&GhRunner) -> bool| runners.iter().filter(|r| f(r)).count();
    RunnerSummary {
        total: runners.len(),
        online: count(|r| r.state != RunnerState::Offline),
        busy: count(|r| r.state == RunnerState::Busy),
        idle: count(|r| r.state == RunnerState::Idle),
        macos_online: count(|r| r.os == RunnerOs::MacOs && r.state != RunnerState::Offline),
        macos_total: count(|r| r.os == RunnerOs::MacOs),
        linux_online: count(|r| r.os == RunnerOs::Linux && r.state != RunnerState::Offline),
        linux_total: count(|r| r.os == RunnerOs::Linux),
        windows_online: count(|r| r.os == RunnerOs::Windows && r.state != RunnerState::Offline),
        windows_total: count(|r| r.os == RunnerOs::Windows),
        other_online: count(|r| r.os == RunnerOs::Other && r.state != RunnerState::Offline),
        other_total: count(|r| r.os == RunnerOs::Other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUNNERS_FIXTURE: &str = include_str!("../tests/fixtures/runners.json");

    fn dto(name: &str, os: &str, status: &str, busy: bool) -> RunnerDto {
        serde_json::from_value(serde_json::json!({
            "id": 1,
            "name": name,
            "os": os,
            "status": status,
            "busy": busy,
            "labels": [{ "name": "self-hosted" }],
        }))
        .expect("fixture-shaped runner")
    }

    #[test]
    fn state_derivation() {
        let runners = map(&[
            dto("a", "macOS", "online", true),
            dto("b", "Linux", "online", false),
            dto("c", "Linux", "offline", false),
        ]);
        assert_eq!(runners[0].state, RunnerState::Busy);
        assert_eq!(runners[1].state, RunnerState::Idle);
        // Offline regardless of busy.
        assert_eq!(runners[2].state, RunnerState::Offline);
    }

    #[test]
    fn os_classification() {
        let runners = map(&[
            dto("a", "macOS", "online", false),
            dto("b", "Linux", "online", false),
            dto("c", "Windows", "online", false),
            dto("d", "FreeBSD", "online", false),
        ]);
        assert_eq!(runners[0].os, RunnerOs::MacOs);
        assert_eq!(runners[1].os, RunnerOs::Linux);
        assert_eq!(runners[2].os, RunnerOs::Windows);
        // Only the three tracked platforms are named; the rest is `Other`.
        assert_eq!(runners[3].os, RunnerOs::Other);
    }

    /// A runner GitHub reports as offline while its `busy` flag is still set is
    /// offline — the flag is stale, the status is not.
    #[test]
    fn offline_beats_a_stale_busy_flag() {
        let runners = map(&[dto("a", "macOS", "offline", true)]);
        assert_eq!(runners[0].state, RunnerState::Offline);
    }

    #[test]
    fn summary_counts() {
        let summary = summarize(&map(&[
            dto("m1", "macOS", "online", true),
            dto("m2", "macOS", "online", false),
            dto("l1", "Linux", "online", false),
            dto("l2", "Linux", "offline", false),
            dto("w1", "Windows", "online", false),
            dto("w2", "Windows", "offline", false),
            dto("x1", "FreeBSD", "online", false),
        ]));
        assert_eq!(summary.total, 7);
        assert_eq!(summary.online, 5);
        assert_eq!(summary.busy, 1);
        assert_eq!(summary.idle, 4);
        assert_eq!(summary.macos_online, 2);
        assert_eq!(summary.macos_total, 2);
        assert_eq!(summary.linux_online, 1);
        assert_eq!(summary.linux_total, 2);
        assert_eq!(summary.windows_online, 1);
        assert_eq!(summary.windows_total, 2);
        assert_eq!(summary.other_online, 1);
        assert_eq!(summary.other_total, 1);
    }

    /// The DTOs decode GitHub's real org-runners payload — `total_count`,
    /// `busy`, and the label array included.
    #[test]
    fn decodes_the_org_runners_fixture() {
        let resp: RunnersResponse = serde_json::from_str(RUNNERS_FIXTURE).expect("decode");
        assert_eq!(resp.total_count, 4);
        let runners = map(&resp.runners);
        assert_eq!(runners.len(), 4);
        assert_eq!(runners[0].name, "mac-s1");
        assert_eq!(runners[0].os, RunnerOs::MacOs);
        assert_eq!(runners[0].state, RunnerState::Busy);
        assert_eq!(
            resp.runners[0]
                .labels
                .as_ref()
                .map(|l| l.iter().map(|x| x.name.as_str()).collect::<Vec<_>>()),
            Some(vec!["self-hosted", "macOS", "ARM64"])
        );
        let summary = summarize(&runners);
        assert_eq!(summary.total, 4);
        assert_eq!(summary.online, 3);
    }

    /// `labels` is optional on GitHub's payload and must not be a required
    /// field — a runner without it still decodes.
    #[test]
    fn labels_are_optional() {
        let dto: RunnerDto = serde_json::from_str(
            r#"{"id":9,"name":"ubu-1","os":"Linux","status":"online","busy":false}"#,
        )
        .expect("decode");
        assert!(dto.labels.is_none());
    }

    /// The roster is persisted, so the OS raw values are a stored format — they
    /// must stay byte-identical to the Swift `RunnerOS` raw values.
    #[test]
    fn os_serialises_with_the_swift_raw_values() {
        assert_eq!(
            serde_json::to_string(&RunnerOs::MacOs).expect("encode"),
            "\"macOS\""
        );
        assert_eq!(
            serde_json::to_string(&RunnerOs::Linux).expect("encode"),
            "\"linux\""
        );
        assert_eq!(
            serde_json::to_string(&RunnerOs::Windows).expect("encode"),
            "\"windows\""
        );
        assert_eq!(
            serde_json::to_string(&RunnerOs::Other).expect("encode"),
            "\"other\""
        );
    }

    /// `as_raw` IS the serde spelling — asserted against `serde_json` rather
    /// than restated, so a `#[serde(rename)]` edited on one side and not the
    /// other fails here instead of silently splitting the stored format in two.
    #[test]
    fn as_raw_matches_the_serialised_spelling() {
        for os in [
            RunnerOs::MacOs,
            RunnerOs::Linux,
            RunnerOs::Windows,
            RunnerOs::Other,
        ] {
            let encoded = serde_json::to_string(&os).expect("encode");
            assert_eq!(encoded, format!("\"{}\"", os.as_raw()), "{os:?}");
        }
    }

    #[test]
    fn from_raw_round_trips_and_tolerates_the_unknown() {
        for os in [
            RunnerOs::MacOs,
            RunnerOs::Linux,
            RunnerOs::Windows,
            RunnerOs::Other,
        ] {
            assert_eq!(RunnerOs::from_raw(os.as_raw()), os);
        }
        // An entry a newer build wrote must not cost us the whole roster.
        assert_eq!(RunnerOs::from_raw("freebsd"), RunnerOs::Other);
        // The display spelling is NOT the stored one, and must not sneak in as
        // a second accepted value. `Windows` is the trap case: its label and
        // its raw value differ only in the first letter, so a roster written
        // with the display text would read back as `Other` and quietly lose
        // every Windows runner.
        assert_eq!(RunnerOs::from_raw("Linux"), RunnerOs::Other);
        assert_eq!(RunnerOs::from_raw("Windows"), RunnerOs::Other);
        assert_eq!(RunnerOs::from_raw("windows"), RunnerOs::Windows);
    }
}
