//! The DevCanopy agent's JSON contract.
//!
//! One definition, serialised by the agent and deserialised by the app. That
//! is the point: the Swift app defines these types a second time, which is
//! why `HostMetricsError.decodeFailed` exists ("agent/app version skew").
//! Field names mirror `agent/src/metrics.rs` exactly.

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
