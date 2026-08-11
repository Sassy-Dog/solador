//! The Solador agent's JSON contract.
//!
//! One definition, serialised by the agent and deserialised by the app. That
//! is the point: the Swift app defines these types a second time, which is
//! why `HostMetricsError.decodeFailed` exists ("agent/app version skew").
//! Field names mirror the agent sources exactly — `agent/src/metrics.rs` for
//! [`Snapshot`], `agent/src/containers.rs` for [`Container`], and the `json!`
//! literal in `agent/src/server.rs::health_handler` for [`Health`].
//!
//! # Unknown is representable
//!
//! Every field a producer may be unable to measure — [`Memory::pressure`],
//! [`Cpu::thermal_state`], the [`Gpu`] fields, the [`Disk`] and [`Network`]
//! rates — is an `Option` carrying [`Volume::fstype`]'s exact serde pair:
//! `#[serde(default)]` so an absent key decodes to `None` rather than failing,
//! and `skip_serializing_if = "Option::is_none"` so an unknown is *omitted*
//! rather than emitted as `null`. `Some(0.0)` therefore means measured zero —
//! an idle disk, a cool CPU — and `None` means nobody measured it. Consumers
//! render the two differently: a number and an em dash.
//!
//! **The limit:** this only stops the *contract* fabricating. Agents predating
//! #183 hardcode `pressure: 0.0` and `thermalState: 0` for figures Linux never
//! measures, and those literals decode here as `Some(0.0)`/`Some(0)` —
//! indistinguishable from a real reading, because on the wire they *are* one.
//! That fabrication can only die at the source, which is what #183 does; until
//! that agent is rolled out, a remote card can still paint a green
//! `Pressure: 0%`. Nothing on this side can tell.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Snapshot {
    pub timestamp: String,
    pub cpu: Cpu,
    pub memory: Memory,
    /// `default` only, deliberately without `skip_serializing_if`: an agent
    /// that measures no rates at all may omit the whole object rather than
    /// sending one with both keys missing, and that must decode as unknown
    /// rather than fail. Same one-sided tolerance [`Container::image`] has —
    /// it adds nothing to what this contract *emits*.
    #[serde(default)]
    pub disk: Disk,
    /// Optional for the same reason as [`Snapshot::disk`].
    #[serde(default)]
    pub network: Network,
    /// Optional for the same reason as [`Snapshot::disk`]: a host with no
    /// adapter may omit the object outright. An omitted `gpu` and a
    /// [`Gpu::unknown`] are the same value, so [`Gpu::is_present`] reads both
    /// as "no GPU".
    #[serde(default)]
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
    /// The thermal pressure level, 0–3. `None` where the platform exposes no
    /// thermal source at all — Linux is that platform, which is why every
    /// pre-#183 agent sends a hardcoded `0` here and every remote card reads
    /// "Normal". See the module docs for why this side cannot undo that.
    #[serde(
        default,
        rename = "thermalState",
        skip_serializing_if = "Option::is_none"
    )]
    pub thermal_state: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Memory {
    #[serde(rename = "usedGB")]
    pub used_gb: f64,
    #[serde(rename = "totalGB")]
    pub total_gb: f64,
    #[serde(rename = "swapUsedGB")]
    pub swap_used_gb: f64,
    /// Memory pressure 0–100. `None` where the platform has no such figure —
    /// Linux again, and macOS's own is derived from mach page counts that
    /// `sysinfo` does not expose. A defaulted `0` paints a permanently green
    /// pressure badge, which is the fabrication this `Option` exists to stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressure: Option<f64>,
}

/// Disk I/O rates. Both `None` before a producer has two cumulative readings to
/// diff — a rate is a delta, and the first sample has nothing to subtract from.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Disk {
    #[serde(default, rename = "readMBps", skip_serializing_if = "Option::is_none")]
    pub read_mbps: Option<f64>,
    #[serde(default, rename = "writeMBps", skip_serializing_if = "Option::is_none")]
    pub write_mbps: Option<f64>,
}

/// Network rates. `None` for the same reason as [`Disk`]'s.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Network {
    #[serde(
        default,
        rename = "downloadMBps",
        skip_serializing_if = "Option::is_none"
    )]
    pub download_mbps: Option<f64>,
    #[serde(
        default,
        rename = "uploadMBps",
        skip_serializing_if = "Option::is_none"
    )]
    pub upload_mbps: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Gpu {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<f64>,
    #[serde(
        default,
        rename = "vramUsedGB",
        skip_serializing_if = "Option::is_none"
    )]
    pub vram_used_gb: Option<f64>,
    #[serde(
        default,
        rename = "vramTotalGB",
        skip_serializing_if = "Option::is_none"
    )]
    pub vram_total_gb: Option<f64>,
}

impl Gpu {
    /// A GPU nobody measured: every field absent, so nothing about it is
    /// serialised at all. This is what a producer with no portable GPU read
    /// should emit — [`is_present`](Self::is_present) reads it as "no GPU" and
    /// the view-model renders "—".
    ///
    /// Identical to `Gpu::default()`, named so a call site says which of the
    /// two empty-looking GPUs it means.
    pub fn unknown() -> Self {
        Gpu::default()
    }

    /// The all-zero GPU **agents before #183 emit** when a host has no portable
    /// GPU metrics source: `usage`, `vramUsedGB` and `vramTotalGB` all present
    /// and all `0.0`.
    ///
    /// Kept because that payload is still on the wire and this crate has to be
    /// able to express (and test) it — not because it is what a producer should
    /// send. New producers want [`unknown`](Self::unknown), which omits the
    /// keys instead of claiming a measurement of zero.
    pub fn zeros() -> Self {
        Gpu {
            usage: Some(0.0),
            vram_used_gb: Some(0.0),
            vram_total_gb: Some(0.0),
        }
    }

    /// Whether this host actually reports a GPU.
    ///
    /// VRAM capacity is the discriminator, not `usage`: a real GPU sitting idle
    /// reports `usage == 0.0` and must still render as `0%`, while a host with
    /// no adapter reports zeros (or, post-#183, nothing) and must render as
    /// unknown. Consumers deciding "number or em dash" should ask this, never
    /// re-derive it.
    ///
    /// Absent capacity and a capacity of zero are the same answer here — which
    /// is what keeps [`zeros`](Self::zeros) and [`unknown`](Self::unknown) both
    /// reading as absent across the #183 rollout.
    pub fn is_present(&self) -> bool {
        self.vram_total_gb.is_some_and(|total| total > 0.0)
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
    /// How full this volume is, 0–100.
    ///
    /// `None` when it reports no capacity: a share of nothing is not zero
    /// percent, it is unanswerable, and the old `0.0` painted a volume nobody
    /// could measure as a comfortably empty green bar. Same rule as every other
    /// unknown in this contract — the caller renders "—".
    pub fn percent_used(&self) -> Option<f64> {
        (self.total_gb > 0.0).then(|| self.used_gb / self.total_gb * 100.0)
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
