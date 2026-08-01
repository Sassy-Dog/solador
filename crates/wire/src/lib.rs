//! The DevCanopy agent's JSON contract.
//!
//! One definition, serialised by the agent and deserialised by the app. That
//! is the point: the Swift app defines these types a second time, which is
//! why `HostMetricsError.decodeFailed` exists ("agent/app version skew").
//! Field names mirror the agent sources exactly — `agent/src/metrics.rs` for
//! [`Snapshot`], `agent/src/containers.rs` for [`Container`], and the `json!`
//! literal in `agent/src/server.rs::health_handler` for [`Health`].

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Snapshot {
    pub timestamp: String,
    pub cpu: Cpu,
    pub memory: Memory,
    pub disk: Disk,
    pub network: Network,
    pub gpu: Gpu,
    #[serde(default)]
    pub battery: Option<Battery>,
    #[serde(default)]
    pub volumes: Vec<Volume>,
    #[serde(default)]
    pub processes: Vec<Process>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Cpu {
    #[serde(rename = "totalUsage")]
    pub total_usage: f64,
    #[serde(rename = "coreUsages")]
    pub core_usages: Vec<f64>,
    pub model: String,
    #[serde(rename = "thermalState")]
    pub thermal_state: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Memory {
    #[serde(rename = "usedGB")]
    pub used_gb: f64,
    #[serde(rename = "totalGB")]
    pub total_gb: f64,
    #[serde(rename = "swapUsedGB")]
    pub swap_used_gb: f64,
    pub pressure: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Disk {
    #[serde(rename = "readMBps")]
    pub read_mbps: f64,
    #[serde(rename = "writeMBps")]
    pub write_mbps: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Network {
    #[serde(rename = "downloadMBps")]
    pub download_mbps: f64,
    #[serde(rename = "uploadMBps")]
    pub upload_mbps: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Gpu {
    pub usage: f64,
    #[serde(rename = "vramUsedGB")]
    pub vram_used_gb: f64,
    #[serde(rename = "vramTotalGB")]
    pub vram_total_gb: f64,
}

impl Gpu {
    /// All-zero GPU placeholder, used when a host has no portable GPU metrics
    /// source. The agent emits this rather than omitting the field.
    pub fn zeros() -> Self {
        Gpu {
            usage: 0.0,
            vram_used_gb: 0.0,
            vram_total_gb: 0.0,
        }
    }

    /// Whether this host actually reports a GPU.
    ///
    /// VRAM capacity is the discriminator, not `usage`: a real GPU sitting idle
    /// reports `usage == 0.0` and must still render as `0%`, while a host with
    /// no adapter reports zeros throughout and must render as unknown. Consumers
    /// deciding "number or em dash" should ask this, never re-derive it.
    pub fn is_present(&self) -> bool {
        self.vram_total_gb > 0.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Battery {
    pub level: f64,
    #[serde(rename = "isCharging")]
    pub is_charging: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Volume {
    pub mount: String,
    #[serde(rename = "usedGB")]
    pub used_gb: f64,
    #[serde(rename = "totalGB")]
    pub total_gb: f64,
    // `default` for lenient decoding, `skip_serializing_if` to preserve the
    // agent's wire behaviour: a volume with no fstype OMITS the key rather than
    // emitting `"fstype": null`. Dropping either half is a wire change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fstype: Option<String>,
}

impl Volume {
    pub fn percent_used(&self) -> f64 {
        if self.total_gb <= 0.0 {
            0.0
        } else {
            self.used_gb / self.total_gb * 100.0
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Process {
    pub pid: i64,
    pub name: String,
    #[serde(rename = "cpuPercent")]
    pub cpu_percent: f64,
    #[serde(rename = "memoryMB")]
    pub memory_mb: f64,
}

/// One container or VM from `GET /v1/containers`.
///
/// Keys are copied from the `#[serde(rename)]` attributes on
/// `agent/src/containers.rs::Container` and match the Swift `ContainerInfo`
/// decoder (`DevCanopy/Services/Containers/ContainerInfo.swift`) key for key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Container {
    pub name: String,
    #[serde(rename = "statusText")]
    pub status_text: String,
    #[serde(rename = "isRunning")]
    pub is_running: bool,
    /// One of `"docker"`, `"podman"`, `"tart"`. Deliberately the agent's
    /// `String` rather than an enum: a runtime added agent-side would then
    /// fail the whole list's decode instead of arriving as an unknown label.
    pub runtime: String,
    /// Image reference; `null` for tart VMs.
    ///
    /// Note the asymmetry with [`Volume::fstype`]: the agent has no
    /// `skip_serializing_if` here, so an imageless container emits
    /// `"image": null` rather than omitting the key (pinned by
    /// `container_serializes_to_exact_wire_keys` in `agent/src/containers.rs`).
    /// `default` only adds decode tolerance; it must not gain a
    /// `skip_serializing_if`, which would change what this contract emits.
    #[serde(default)]
    pub image: Option<String>,
}

/// The `GET /v1/health` payload — the Settings "Test" probe.
///
/// Keys are copied from the `json!` literal in
/// `agent/src/server.rs::health_handler` and match the Swift `HealthInfo`
/// decoder (`DevCanopy/Services/HostMetrics/RemoteHostMetricsService.swift`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Health {
    /// `"ok"`, or `"degraded"` when the agent's sampler has gone stale and is
    /// therefore serving frozen numbers.
    pub status: String,
    pub hostname: String,
    pub version: String,
    /// Optional because agents older than #35 don't send it — same tolerance
    /// the Swift decoder gives it (`let sampleAgeSeconds: Int?`). An older
    /// agent must decode, not fail.
    #[serde(default, rename = "sampleAgeSeconds")]
    pub sample_age_seconds: Option<u64>,
    /// Optional for the same reason as [`Health::sample_age_seconds`].
    #[serde(default, rename = "samplerStale")]
    pub sampler_stale: Option<bool>,
}
