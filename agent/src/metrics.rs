//! Host metrics sampling via `sysinfo`.
//!
//! A background task refreshes `sysinfo` roughly once per second and stores the
//! latest computed [`Snapshot`] in shared state. Disk-I/O and network values are
//! *rates* (bytes/s), which require a delta between two refreshes — hence the
//! background sampler rather than computing on-demand inside the HTTP handler.

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use sysinfo::{Disks, Networks, ProcessesToUpdate, System};
use tokio::sync::RwLock;

const BYTES_PER_GIB: f64 = 1024.0 * 1024.0 * 1024.0;
const BYTES_PER_MIB: f64 = 1024.0 * 1024.0;

/// Refresh interval for the background sampler. Disk/network rates are computed
/// as (delta bytes over this interval) / (interval seconds).
const SAMPLE_INTERVAL: Duration = Duration::from_millis(1000);

/// Re-enumerate processes every N ticks (~1 min at the 1s sample interval).
const PROCESS_SAMPLE_TICKS: u64 = 60;
/// Keep the top-N by CPU and by memory.
const PROCESS_TOP_LIMIT: usize = 5;

// ---------------------------------------------------------------------------
// Wire contract types.
//
// These serialize to the EXACT JSON shape the Swift `Codable` decoder expects.
// camelCase keys; explicit `#[serde(rename)]` where snake_case would not produce
// the canonical key (e.g. `usedGB`, `readMBps`, `vramUsedGB`).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Snapshot {
    /// RFC3339 / ISO-8601 UTC, e.g. `2026-06-04T22:00:00Z`.
    pub timestamp: String,
    pub cpu: Cpu,
    pub memory: Memory,
    pub disk: Disk,
    pub network: Network,
    pub gpu: Gpu,
    /// `null` on servers (no battery).
    pub battery: Option<Battery>,
    /// Per-mounted-volume usage. A full volume fails even when the disk has
    /// space, so each is reported separately.
    pub volumes: Vec<Volume>,
    /// Top CPU/RAM-consuming processes (sampled on a slow cadence).
    pub processes: Vec<Process>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Cpu {
    #[serde(rename = "totalUsage")]
    pub total_usage: f64,
    #[serde(rename = "coreUsages")]
    pub core_usages: Vec<f64>,
    pub model: String,
    /// 0 = nominal. Defaulted to 0 (no portable thermal source).
    #[serde(rename = "thermalState")]
    pub thermal_state: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Memory {
    #[serde(rename = "usedGB")]
    pub used_gb: f64,
    #[serde(rename = "totalGB")]
    pub total_gb: f64,
    #[serde(rename = "swapUsedGB")]
    pub swap_used_gb: f64,
    /// macOS-style memory pressure has no simple Linux analogue; defaulted to 0.0.
    /// NOTE: deliberately no `usagePercentage` key — the Swift side computes it.
    pub pressure: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Disk {
    #[serde(rename = "readMBps")]
    pub read_mbps: f64,
    #[serde(rename = "writeMBps")]
    pub write_mbps: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Network {
    #[serde(rename = "downloadMBps")]
    pub download_mbps: f64,
    #[serde(rename = "uploadMBps")]
    pub upload_mbps: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Gpu {
    pub usage: f64,
    #[serde(rename = "vramUsedGB")]
    pub vram_used_gb: f64,
    #[serde(rename = "vramTotalGB")]
    pub vram_total_gb: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Battery {
    pub level: f64,
    #[serde(rename = "isCharging")]
    pub is_charging: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Volume {
    pub mount: String,
    #[serde(rename = "usedGB")]
    pub used_gb: f64,
    #[serde(rename = "totalGB")]
    pub total_gb: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Process {
    pub pid: i64,
    pub name: String,
    /// Can exceed 100 on multi-core hosts.
    #[serde(rename = "cpuPercent")]
    pub cpu_percent: f64,
    #[serde(rename = "memoryMB")]
    pub memory_mb: f64,
}

/// Reduce all processes to the union of the top-`limit` by CPU and by memory
/// (deduped by pid), sorted by CPU descending.
fn top_processes(sys: &System, limit: usize) -> Vec<Process> {
    let all: Vec<Process> = sys
        .processes()
        .iter()
        .map(|(pid, p)| Process {
            pid: pid.as_u32() as i64,
            name: p.name().to_string_lossy().to_string(),
            cpu_percent: p.cpu_usage() as f64,
            memory_mb: p.memory() as f64 / BYTES_PER_MIB,
        })
        .collect();

    let cmp_desc = |key: fn(&Process) -> f64| {
        move |a: &Process, b: &Process| {
            key(b)
                .partial_cmp(&key(a))
                .unwrap_or(std::cmp::Ordering::Equal)
        }
    };

    let mut by_cpu = all.clone();
    by_cpu.sort_by(cmp_desc(|p| p.cpu_percent));
    let mut by_mem = all;
    by_mem.sort_by(cmp_desc(|p| p.memory_mb));

    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<Process> = Vec::new();
    for p in by_cpu.into_iter().take(limit) {
        if seen.insert(p.pid) {
            out.push(p);
        }
    }
    for p in by_mem.into_iter().take(limit) {
        if seen.insert(p.pid) {
            out.push(p);
        }
    }
    out.sort_by(cmp_desc(|p| p.cpu_percent));
    out
}

impl Gpu {
    /// All-zero GPU placeholder (no portable GPU metrics source).
    pub fn zeros() -> Self {
        Gpu {
            usage: 0.0,
            vram_used_gb: 0.0,
            vram_total_gb: 0.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Sampler.
// ---------------------------------------------------------------------------

/// Shared handle to the most recently sampled snapshot.
#[derive(Clone)]
pub struct MetricsState {
    inner: Arc<RwLock<Snapshot>>,
}

impl MetricsState {
    /// Returns the latest sampled snapshot, with a freshly-stamped timestamp.
    pub async fn latest(&self) -> Snapshot {
        let mut snap = self.inner.read().await.clone();
        snap.timestamp = now_rfc3339();
        snap
    }
}

/// Format the current instant as RFC3339/ISO-8601 UTC, e.g. `2026-06-04T22:00:00Z`.
pub fn now_rfc3339() -> String {
    use time::format_description::well_known::Rfc3339;
    use time::OffsetDateTime;
    OffsetDateTime::now_utc()
        .replace_nanosecond(0)
        .unwrap_or_else(|_| OffsetDateTime::now_utc())
        .format(&Rfc3339)
        // Rfc3339 emits `+00:00`; normalize to the canonical `Z` suffix.
        .map(|s| s.replace("+00:00", "Z"))
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// Spawn the background sampler and return a handle to the shared state.
///
/// The sampler primes one refresh, then loops: sleep ~1s, refresh again, compute
/// rates from the deltas, and store the result.
pub fn spawn_sampler() -> MetricsState {
    let state = MetricsState {
        inner: Arc::new(RwLock::new(empty_snapshot())),
    };
    let handle = state.clone();

    tokio::spawn(async move {
        let mut sys = System::new_all();
        let mut networks = Networks::new_with_refreshed_list();
        let mut disks = Disks::new_with_refreshed_list();

        // Prime CPU usage (the first reading is meaningless).
        sys.refresh_cpu_all();
        tokio::time::sleep(SAMPLE_INTERVAL).await;

        // Processes are enumerated on a slow cadence (expensive, and only needed
        // as a ~1-minute "what's hogging" view). Cached between ticks.
        let mut cached_processes: Vec<Process> = Vec::new();
        let mut tick: u64 = 0;

        loop {
            // Refresh everything; the second+ refresh yields valid deltas/usages.
            sys.refresh_cpu_all();
            sys.refresh_memory();
            networks.refresh(true);
            disks.refresh(true);

            if tick.is_multiple_of(PROCESS_SAMPLE_TICKS) {
                sys.refresh_processes(ProcessesToUpdate::All, true);
                cached_processes = top_processes(&sys, PROCESS_TOP_LIMIT);
            }

            let snap = compute_snapshot(
                &sys,
                &networks,
                &disks,
                SAMPLE_INTERVAL.as_secs_f64(),
                cached_processes.clone(),
            );
            *handle.inner.write().await = snap;

            tick = tick.wrapping_add(1);
            tokio::time::sleep(SAMPLE_INTERVAL).await;
        }
    });

    state
}

/// A zero-valued snapshot used before the first sample lands.
fn empty_snapshot() -> Snapshot {
    Snapshot {
        timestamp: now_rfc3339(),
        cpu: Cpu {
            total_usage: 0.0,
            core_usages: Vec::new(),
            model: String::new(),
            thermal_state: 0,
        },
        memory: Memory {
            used_gb: 0.0,
            total_gb: 0.0,
            swap_used_gb: 0.0,
            pressure: 0.0,
        },
        disk: Disk {
            read_mbps: 0.0,
            write_mbps: 0.0,
        },
        network: Network {
            download_mbps: 0.0,
            upload_mbps: 0.0,
        },
        gpu: Gpu::zeros(),
        battery: None,
        volumes: Vec::new(),
        processes: Vec::new(),
    }
}

/// Build a [`Snapshot`] from refreshed sysinfo state.
///
/// `interval_secs` is the elapsed time over which the network/disk byte deltas
/// were accumulated, used to convert byte deltas into bytes/sec.
fn compute_snapshot(
    sys: &System,
    networks: &Networks,
    disks: &Disks,
    interval_secs: f64,
    processes: Vec<Process>,
) -> Snapshot {
    let interval = if interval_secs > 0.0 {
        interval_secs
    } else {
        1.0
    };

    // CPU.
    let total_usage = sys.global_cpu_usage() as f64;
    let core_usages: Vec<f64> = sys.cpus().iter().map(|c| c.cpu_usage() as f64).collect();
    let model = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "Unknown".to_string());

    // Memory (bytes -> GiB).
    let used_gb = sys.used_memory() as f64 / BYTES_PER_GIB;
    let total_gb = sys.total_memory() as f64 / BYTES_PER_GIB;
    let swap_used_gb = sys.used_swap() as f64 / BYTES_PER_GIB;

    // Disk I/O: sum per-disk byte deltas over the interval, then -> MiB/s.
    let (mut read_bytes, mut written_bytes) = (0u64, 0u64);
    for disk in disks.list() {
        let usage = disk.usage();
        read_bytes = read_bytes.saturating_add(usage.read_bytes);
        written_bytes = written_bytes.saturating_add(usage.written_bytes);
    }
    let read_mbps = (read_bytes as f64 / interval) / BYTES_PER_MIB;
    let write_mbps = (written_bytes as f64 / interval) / BYTES_PER_MIB;

    // Per-volume usage (dedup by mount; skip zero-capacity pseudo-filesystems).
    let mut volumes: Vec<Volume> = Vec::new();
    for disk in disks.list() {
        let total = disk.total_space();
        if total == 0 {
            continue;
        }
        let mount = disk.mount_point().to_string_lossy().to_string();
        if volumes.iter().any(|v| v.mount == mount) {
            continue;
        }
        let used = total.saturating_sub(disk.available_space());
        volumes.push(Volume {
            mount,
            used_gb: used as f64 / BYTES_PER_GIB,
            total_gb: total as f64 / BYTES_PER_GIB,
        });
    }

    // Network: sum received/transmitted byte deltas over the interval -> MiB/s.
    let (mut rx, mut tx) = (0u64, 0u64);
    for (_name, data) in networks.iter() {
        rx = rx.saturating_add(data.received());
        tx = tx.saturating_add(data.transmitted());
    }
    let download_mbps = (rx as f64 / interval) / BYTES_PER_MIB;
    let upload_mbps = (tx as f64 / interval) / BYTES_PER_MIB;

    Snapshot {
        timestamp: now_rfc3339(),
        cpu: Cpu {
            total_usage,
            core_usages,
            model,
            thermal_state: 0,
        },
        memory: Memory {
            used_gb,
            total_gb,
            swap_used_gb,
            pressure: 0.0,
        },
        disk: Disk {
            read_mbps,
            write_mbps,
        },
        network: Network {
            download_mbps,
            upload_mbps,
        },
        gpu: Gpu::zeros(),
        battery: None,
        volumes,
        processes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    /// The canonical wire sample. Decoded byte-for-key by the Swift `Codable` type.
    /// This test is the lock on the wire format.
    fn canonical_sample() -> Value {
        json!({
            "timestamp": "2026-06-04T22:00:00Z",
            "cpu": { "totalUsage": 37.5, "coreUsages": [40.0, 35.0, 50.0, 25.0], "model": "Apple M1 Max", "thermalState": 0 },
            "memory": { "usedGB": 12.3, "totalGB": 32.0, "swapUsedGB": 0.5, "pressure": 0.0 },
            "disk": { "readMBps": 1.2, "writeMBps": 0.3 },
            "network": { "downloadMBps": 0.2, "uploadMBps": 0.1 },
            "gpu": { "usage": 0.0, "vramUsedGB": 0.0, "vramTotalGB": 0.0 },
            "battery": null,
            "volumes": [{ "mount": "/", "usedGB": 10.0, "totalGB": 100.0 }],
            "processes": [{ "pid": 123, "name": "node", "cpuPercent": 12.5, "memoryMB": 256.0 }]
        })
    }

    /// Build a Snapshot carrying the exact values from the canonical sample.
    fn canonical_snapshot() -> Snapshot {
        Snapshot {
            timestamp: "2026-06-04T22:00:00Z".to_string(),
            cpu: Cpu {
                total_usage: 37.5,
                core_usages: vec![40.0, 35.0, 50.0, 25.0],
                model: "Apple M1 Max".to_string(),
                thermal_state: 0,
            },
            memory: Memory {
                used_gb: 12.3,
                total_gb: 32.0,
                swap_used_gb: 0.5,
                pressure: 0.0,
            },
            disk: Disk {
                read_mbps: 1.2,
                write_mbps: 0.3,
            },
            network: Network {
                download_mbps: 0.2,
                upload_mbps: 0.1,
            },
            gpu: Gpu::zeros(),
            battery: None,
            volumes: vec![Volume {
                mount: "/".to_string(),
                used_gb: 10.0,
                total_gb: 100.0,
            }],
            processes: vec![Process {
                pid: 123,
                name: "node".to_string(),
                cpu_percent: 12.5,
                memory_mb: 256.0,
            }],
        }
    }

    /// CONTRACT LOCK: serialized Snapshot must equal the canonical sample exactly.
    #[test]
    fn snapshot_matches_canonical_wire_contract() {
        let value = serde_json::to_value(canonical_snapshot()).unwrap();
        assert_eq!(
            value,
            canonical_sample(),
            "serialized Snapshot diverged from the canonical wire contract"
        );
    }

    /// Assert every required key is present with the correct JSON type.
    #[test]
    fn snapshot_has_exact_keys_and_types() {
        let v = serde_json::to_value(canonical_snapshot()).unwrap();
        let obj = v.as_object().expect("snapshot is an object");

        // Top-level keys — exactly these seven, no more, no fewer.
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "battery",
                "cpu",
                "disk",
                "gpu",
                "memory",
                "network",
                "processes",
                "timestamp",
                "volumes"
            ]
        );

        assert!(v["timestamp"].is_string());

        let cpu = v["cpu"].as_object().unwrap();
        assert!(cpu["totalUsage"].is_number());
        assert!(cpu["coreUsages"].is_array());
        assert!(cpu["coreUsages"][0].is_number());
        assert!(cpu["model"].is_string());
        assert!(cpu["thermalState"].is_i64());

        let mem = v["memory"].as_object().unwrap();
        assert!(mem["usedGB"].is_number());
        assert!(mem["totalGB"].is_number());
        assert!(mem["swapUsedGB"].is_number());
        assert!(mem["pressure"].is_number());
        // Swift computes this — must NOT be present on the wire.
        assert!(!mem.contains_key("usagePercentage"));

        let disk = v["disk"].as_object().unwrap();
        assert!(disk["readMBps"].is_number());
        assert!(disk["writeMBps"].is_number());

        let net = v["network"].as_object().unwrap();
        assert!(net["downloadMBps"].is_number());
        assert!(net["uploadMBps"].is_number());

        let gpu = v["gpu"].as_object().unwrap();
        assert!(gpu["usage"].is_number());
        assert!(gpu["vramUsedGB"].is_number());
        assert!(gpu["vramTotalGB"].is_number());

        assert!(v["battery"].is_null());

        let vols = v["volumes"].as_array().unwrap();
        assert!(vols[0]["mount"].is_string());
        assert!(vols[0]["usedGB"].is_number());
        assert!(vols[0]["totalGB"].is_number());

        let procs = v["processes"].as_array().unwrap();
        assert!(procs[0]["pid"].is_i64());
        assert!(procs[0]["name"].is_string());
        assert!(procs[0]["cpuPercent"].is_number());
        assert!(procs[0]["memoryMB"].is_number());
    }

    #[test]
    fn timestamp_is_rfc3339_z_suffixed() {
        let ts = now_rfc3339();
        // e.g. 2026-06-04T22:00:00Z — ends with Z, contains a 'T' separator.
        assert!(ts.ends_with('Z'), "timestamp must end with Z: {ts}");
        assert!(ts.contains('T'), "timestamp must contain T: {ts}");
        assert!(
            !ts.contains("+00:00"),
            "offset must be normalized to Z: {ts}"
        );
    }
}
