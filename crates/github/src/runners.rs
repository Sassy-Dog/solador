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
    #[serde(rename = "other")]
    Other,
}

impl RunnerOs {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            RunnerOs::MacOs => "macOS",
            RunnerOs::Linux => "Linux",
            RunnerOs::Other => "Other",
        }
    }

    /// Panel display order: macOS first, then Linux, then everything else.
    #[must_use]
    pub fn rank(self) -> u8 {
        match self {
            RunnerOs::MacOs => 0,
            RunnerOs::Linux => 1,
            RunnerOs::Other => 2,
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
        ]);
        assert_eq!(runners[0].os, RunnerOs::MacOs);
        assert_eq!(runners[1].os, RunnerOs::Linux);
        assert_eq!(runners[2].os, RunnerOs::Other);
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
        ]));
        assert_eq!(summary.total, 4);
        assert_eq!(summary.online, 3);
        assert_eq!(summary.busy, 1);
        assert_eq!(summary.idle, 2);
        assert_eq!(summary.macos_online, 2);
        assert_eq!(summary.macos_total, 2);
        assert_eq!(summary.linux_online, 1);
        assert_eq!(summary.linux_total, 2);
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
            serde_json::to_string(&RunnerOs::Other).expect("encode"),
            "\"other\""
        );
    }
}
