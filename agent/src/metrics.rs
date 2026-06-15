//! Host metrics sampling via `sysinfo`.
//!
//! A background task refreshes `sysinfo` roughly once per second and stores the
//! latest computed [`Snapshot`] in shared state. Disk-I/O and network values are
//! *rates* (bytes/s), which require a delta between two refreshes — hence the
//! background sampler rather than computing on-demand inside the HTTP handler.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// How many sample intervals may elapse before the sampler is considered stale.
/// A healthy sampler writes every [`SAMPLE_INTERVAL`]; if no fresh sample has
/// landed in this many intervals (e.g. the task panicked), `/v1/health` reports
/// `degraded`. Three intervals tolerates a missed tick or two without flapping.
pub const STALE_INTERVALS: u32 = 3;

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
    /// Filesystem type, lowercased (e.g. "ext4"). Absent from the wire when
    /// unknown — the Swift decoder treats the key as optional.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fstype: Option<String>,
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

// ---------------------------------------------------------------------------
// Volume filtering.
// ---------------------------------------------------------------------------

/// Env var holding a comma-separated fstype skip list. When set it REPLACES the
/// default list entirely (an operator who sets it owns the whole policy); an
/// empty value disables fstype filtering.
pub const SKIP_FSTYPES_ENV: &str = "DEVCANOPY_AGENT_SKIP_FSTYPES";

/// Filesystem types that are transient, remote, or pseudo — mounts that flap
/// (autofs/NFS automounts) or aren't host storage. Filtering them at the source
/// keeps the dashboard's volume list stable.
const DEFAULT_SKIP_FSTYPES: &[&str] = &[
    "autofs",
    "nfs",
    "nfs4",
    "cifs",
    "smb",
    "smb2",
    "smb3",
    "smbfs",
    "9p",
    "afs",
    "afpfs",
    "ceph",
    "glusterfs",
    "lustre",
    "davfs",
    "davfs2",
    "sshfs",
    "curlftpfs",
    "tmpfs",
    "devtmpfs",
    "ramfs",
    "squashfs",
    "overlay",
    "overlayfs",
    "iso9660",
];

/// Resolve the fstype skip set from the env value (`None` = unset → defaults).
fn skip_fstypes(env_val: Option<&str>) -> std::collections::HashSet<String> {
    match env_val {
        None => DEFAULT_SKIP_FSTYPES.iter().map(|s| s.to_string()).collect(),
        Some(raw) => raw
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect(),
    }
}

/// Whether a (lowercased) fstype should be skipped. With the default policy
/// (any non-empty skip set containing "autofs"), `fuse.*` subtypes are also
/// skipped — they're FUSE network/userspace mounts (sshfs, rclone, gvfs).
/// `fuseblk` (NTFS via FUSE, a real local disk) deliberately does not match.
fn should_skip_fstype(fstype: &str, skip: &std::collections::HashSet<String>) -> bool {
    skip.contains(fstype) || (!skip.is_empty() && fstype.starts_with("fuse."))
}

/// A mounted filesystem as plain data, so volume filtering is testable
/// (sysinfo's `Disk` cannot be constructed in tests).
struct DiskEntry {
    mount: String,
    /// Backing device (e.g. "/dev/mapper/vg-root"); may be empty.
    device: String,
    total: u64,
    available: u64,
    fstype: String,
}

/// Filter and reduce mounted filesystems to the reported volume list:
/// skip zero-capacity pseudo-filesystems and skipped fstypes, collapse the same
/// filesystem mounted in several places (bind mounts) to the shortest mount
/// path, and sort by mount.
///
/// Dedupe keys on the backing device when known — NOT on (total, available):
/// a bind mount reads statvfs at a different instant than its source, so on a
/// busy host the sizes momentarily disagree and a size-based key flaps. Size is
/// only the fallback when the device name is empty.
fn build_volumes(entries: Vec<DiskEntry>, skip: &std::collections::HashSet<String>) -> Vec<Volume> {
    let mut by_fs: std::collections::HashMap<String, Volume> = std::collections::HashMap::new();
    for entry in entries {
        if entry.total == 0 || should_skip_fstype(&entry.fstype, skip) {
            continue;
        }
        let key = if entry.device.is_empty() {
            format!("size:{}:{}", entry.total, entry.available)
        } else {
            format!("dev:{}", entry.device)
        };
        let vol = Volume {
            mount: entry.mount.clone(),
            used_gb: entry.total.saturating_sub(entry.available) as f64 / BYTES_PER_GIB,
            total_gb: entry.total as f64 / BYTES_PER_GIB,
            fstype: Some(entry.fstype),
        };
        by_fs
            .entry(key)
            .and_modify(|existing| {
                if entry.mount.len() < existing.mount.len() {
                    *existing = vol.clone();
                }
            })
            .or_insert(vol);
    }
    let mut volumes: Vec<Volume> = by_fs.into_values().collect();
    volumes.sort_by(|a, b| a.mount.cmp(&b.mount));
    volumes
}

/// Reduce all processes to the union of the top-`limit` by CPU and by memory
/// (deduped by pid), sorted by CPU descending.
fn top_processes(sys: &System, limit: usize) -> Vec<Process> {
    let all: Vec<Process> = sys
        .processes()
        .iter()
        .map(|(pid, p)| Process {
            pid: pid.as_u32() as i64,
            // Prefer the executable's file name (e.g. "node", "chrome"); sysinfo's
            // name() is the Linux comm/thread name ("MainThread") for many procs.
            name: p
                .exe()
                .and_then(|e| e.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| p.name().to_string_lossy().to_string()),
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
    /// Instant at which the most recent sample was written. The sampler stamps
    /// this on every write; `sample_age` reads it to detect a dead sampler.
    last_sample: Arc<Mutex<Instant>>,
}

impl MetricsState {
    /// Returns the latest sampled snapshot.
    ///
    /// The `timestamp` is the one the sampler stamped when it wrote the sample —
    /// *not* the request time. A frozen sampler therefore returns a frozen
    /// timestamp, so the dashboard can see the sample is stale instead of being
    /// shown old numbers behind a fresh "now".
    pub async fn latest(&self) -> Snapshot {
        self.inner.read().await.clone()
    }

    /// How long since the sampler last wrote a sample.
    pub fn sample_age(&self) -> Duration {
        self.last_sample
            .lock()
            .expect("last_sample mutex poisoned")
            .elapsed()
    }

    /// Whether the sampler has gone stale (no fresh sample within
    /// [`STALE_INTERVALS`] sample intervals) — the signal that the background
    /// task has died and the served numbers are frozen.
    pub fn is_stale(&self) -> bool {
        self.sample_age() > SAMPLE_INTERVAL * STALE_INTERVALS
    }

    /// Record that a fresh sample landed now.
    fn mark_sampled(&self) {
        *self.last_sample.lock().expect("last_sample mutex poisoned") = Instant::now();
    }

    /// Build a state around a fixed snapshot whose sample is marked fresh now —
    /// for tests that need a `MetricsState` without a running sampler.
    #[cfg(test)]
    pub fn fresh_for_test(snapshot: Snapshot) -> Self {
        MetricsState {
            inner: Arc::new(RwLock::new(snapshot)),
            last_sample: Arc::new(Mutex::new(Instant::now())),
        }
    }

    /// Force the recorded last-sample time `age` into the past — for tests that
    /// exercise the stale-sampler path without sleeping a real interval.
    #[cfg(test)]
    pub fn set_sample_age_for_test(&self, age: Duration) {
        let backdated = Instant::now()
            .checked_sub(age)
            .expect("backdate within Instant range");
        *self.last_sample.lock().expect("last_sample mutex poisoned") = backdated;
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
///
/// The loop is run under a supervisor: if it panics, the supervisor logs a
/// `tracing::error!` and restarts it after a short backoff. Until a restart
/// produces a fresh sample, [`MetricsState::is_stale`] reports `true`, so a
/// crashed sampler surfaces as `degraded` on `/v1/health` rather than serving
/// frozen numbers behind a green health check.
pub fn spawn_sampler() -> MetricsState {
    let state = MetricsState {
        inner: Arc::new(RwLock::new(empty_snapshot())),
        last_sample: Arc::new(Mutex::new(Instant::now())),
    };
    let handle = state.clone();

    tokio::spawn(async move {
        loop {
            let task = handle.clone();
            // Isolate the loop so a panic inside it doesn't take down the agent;
            // a `JoinHandle` carries the panic back here as an `Err`.
            let join = tokio::spawn(async move { sampler_loop(task).await });
            match join.await {
                Ok(()) => {
                    // The loop is infinite; a clean return is unexpected.
                    tracing::error!("metrics sampler exited unexpectedly; restarting");
                }
                Err(e) => {
                    tracing::error!(error = %e, "metrics sampler panicked; restarting");
                }
            }
            // Back off so a tight panic loop doesn't spin the CPU.
            tokio::time::sleep(SAMPLE_INTERVAL).await;
        }
    });

    state
}

/// The sampler's inner loop: prime once, then sample forever.
async fn sampler_loop(state: MetricsState) {
    let mut sys = System::new_all();
    let mut networks = Networks::new_with_refreshed_list();
    let mut disks = Disks::new_with_refreshed_list();

    // Read once at startup; the skip policy doesn't change while running.
    let skip_env = std::env::var(SKIP_FSTYPES_ENV).ok();
    let skip = skip_fstypes(skip_env.as_deref());

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
            &skip,
        );
        *state.inner.write().await = snap;
        state.mark_sampled();

        tick = tick.wrapping_add(1);
        tokio::time::sleep(SAMPLE_INTERVAL).await;
    }
}

/// A zero-valued snapshot used before the first sample lands.
pub(crate) fn empty_snapshot() -> Snapshot {
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
    skip_fstypes: &std::collections::HashSet<String>,
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

    // Per-volume usage; see `build_volumes` for the filtering policy.
    let entries: Vec<DiskEntry> = disks
        .list()
        .iter()
        .map(|disk| DiskEntry {
            mount: disk.mount_point().to_string_lossy().to_string(),
            device: disk.name().to_string_lossy().to_string(),
            total: disk.total_space(),
            available: disk.available_space(),
            fstype: disk.file_system().to_string_lossy().to_lowercase(),
        })
        .collect();
    let volumes = build_volumes(entries, skip_fstypes);

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
            "volumes": [{ "mount": "/", "usedGB": 10.0, "totalGB": 100.0, "fstype": "ext4" }],
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
                fstype: Some("ext4".to_string()),
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
        assert!(vols[0]["fstype"].is_string());

        let procs = v["processes"].as_array().unwrap();
        assert!(procs[0]["pid"].is_i64());
        assert!(procs[0]["name"].is_string());
        assert!(procs[0]["cpuPercent"].is_number());
        assert!(procs[0]["memoryMB"].is_number());
    }

    /// `fstype` is optional on the wire: absent (not null) when unknown, so old
    /// consumers and the canonical contract stay byte-compatible.
    #[test]
    fn volume_without_fstype_omits_the_key() {
        let v = serde_json::to_value(Volume {
            mount: "/".to_string(),
            used_gb: 1.0,
            total_gb: 2.0,
            fstype: None,
        })
        .unwrap();
        assert!(!v.as_object().unwrap().contains_key("fstype"));
    }

    fn entry(mount: &str, total: u64, available: u64, fstype: &str) -> DiskEntry {
        entry_dev(mount, "", total, available, fstype)
    }

    fn entry_dev(mount: &str, device: &str, total: u64, available: u64, fstype: &str) -> DiskEntry {
        DiskEntry {
            mount: mount.to_string(),
            device: device.to_string(),
            total,
            available,
            fstype: fstype.to_string(),
        }
    }

    fn mounts(volumes: &[Volume]) -> Vec<&str> {
        volumes.iter().map(|v| v.mount.as_str()).collect()
    }

    const GIB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn build_volumes_skips_transient_and_pseudo_fstypes() {
        let skip = skip_fstypes(None);
        let vols = build_volumes(
            vec![
                entry("/", 100 * GIB, 90 * GIB, "ext4"),
                entry("/shared", 50 * GIB, 40 * GIB, "autofs"),
                entry("/mnt/nas", 50 * GIB, 30 * GIB, "nfs4"),
                entry("/mnt/win", 50 * GIB, 20 * GIB, "cifs"),
                entry("/mnt/ssh", 50 * GIB, 10 * GIB, "fuse.sshfs"),
                entry("/run", 8 * GIB, 7 * GIB, "tmpfs"),
            ],
            &skip,
        );
        assert_eq!(mounts(&vols), vec!["/"]);
    }

    #[test]
    fn build_volumes_keeps_local_filesystems() {
        let skip = skip_fstypes(None);
        let vols = build_volumes(
            vec![
                entry("/", 100 * GIB, 90 * GIB, "ext4"),
                entry("/data", 101 * GIB, 90 * GIB, "xfs"),
                entry("/pool", 102 * GIB, 90 * GIB, "btrfs"),
                entry("/tank", 103 * GIB, 90 * GIB, "zfs"),
                entry("/boot/efi", GIB, 0, "vfat"),
                // NTFS via FUSE is a real local disk — the `fuse.` prefix rule
                // must not catch it.
                entry("/mnt/ntfs", 104 * GIB, 90 * GIB, "fuseblk"),
            ],
            &skip,
        );
        assert_eq!(
            mounts(&vols),
            vec!["/", "/boot/efi", "/data", "/mnt/ntfs", "/pool", "/tank"]
        );
    }

    #[test]
    fn build_volumes_skips_zero_capacity() {
        let skip = skip_fstypes(None);
        let vols = build_volumes(vec![entry("/proc", 0, 0, "ext4")], &skip);
        assert!(vols.is_empty());
    }

    #[test]
    fn build_volumes_dedupes_bind_mounts_to_shortest_path() {
        let skip = skip_fstypes(None);
        let vols = build_volumes(
            vec![
                entry("/var/lib/docker/btrfs", 100 * GIB, 60 * GIB, "btrfs"),
                entry("/data", 100 * GIB, 60 * GIB, "btrfs"),
            ],
            &skip,
        );
        assert_eq!(mounts(&vols), vec!["/data"]);
    }

    /// THE ubu-3xdv `/shared` bug: a bind mount of the root filesystem reads
    /// statvfs at a different instant than `/`, so on a busy host the available
    /// bytes differ between the two entries and size-based dedupe randomly
    /// misses — the duplicate flaps in and out every sample. Same backing
    /// device MUST collapse regardless of momentary size disagreement.
    #[test]
    fn build_volumes_dedupes_same_device_even_when_sizes_disagree() {
        let skip = skip_fstypes(None);
        let dev = "/dev/mapper/ubuntu--vg-ubuntu--lv";
        let vols = build_volumes(
            vec![
                entry_dev("/", dev, 436 * GIB, 283 * GIB, "ext4"),
                entry_dev(
                    "/home/chris/.openclaw/workspace/shared",
                    dev,
                    436 * GIB,
                    283 * GIB + 12345,
                    "ext4",
                ),
            ],
            &skip,
        );
        assert_eq!(mounts(&vols), vec!["/"]);
    }

    /// Distinct devices that happen to report identical sizes (e.g. two freshly
    /// formatted identical disks) are different filesystems — both must show.
    #[test]
    fn build_volumes_keeps_distinct_devices_with_identical_sizes() {
        let skip = skip_fstypes(None);
        let vols = build_volumes(
            vec![
                entry_dev("/data1", "/dev/sda1", 100 * GIB, 60 * GIB, "ext4"),
                entry_dev("/data2", "/dev/sdb1", 100 * GIB, 60 * GIB, "ext4"),
            ],
            &skip,
        );
        assert_eq!(mounts(&vols), vec!["/data1", "/data2"]);
    }

    #[test]
    fn build_volumes_populates_fstype_and_converts_to_gib() {
        let skip = skip_fstypes(None);
        let vols = build_volumes(vec![entry("/", 100 * GIB, 75 * GIB, "ext4")], &skip);
        assert_eq!(vols.len(), 1);
        assert_eq!(vols[0].fstype.as_deref(), Some("ext4"));
        assert!((vols[0].total_gb - 100.0).abs() < 1e-9);
        assert!((vols[0].used_gb - 25.0).abs() < 1e-9);
    }

    #[test]
    fn skip_fstypes_defaults_when_env_unset() {
        let skip = skip_fstypes(None);
        for fs in ["autofs", "nfs", "nfs4", "cifs", "tmpfs", "overlay"] {
            assert!(skip.contains(fs), "default skip list must contain {fs}");
        }
        assert!(!skip.contains("ext4"));
    }

    #[test]
    fn skip_fstypes_env_replaces_default_entirely() {
        let skip = skip_fstypes(Some(" NFS , ext4 "));
        let mut got: Vec<&str> = skip.iter().map(String::as_str).collect();
        got.sort_unstable();
        assert_eq!(got, vec!["ext4", "nfs"]);
    }

    #[test]
    fn skip_fstypes_empty_env_disables_filtering() {
        let skip = skip_fstypes(Some(""));
        assert!(skip.is_empty());
        let vols = build_volumes(vec![entry("/shared", GIB, 0, "autofs")], &skip);
        assert_eq!(mounts(&vols), vec!["/shared"]);
    }

    #[test]
    fn fuse_prefixed_fstypes_are_always_skipped_with_default_list() {
        let skip = skip_fstypes(None);
        let vols = build_volumes(
            vec![entry("/mnt/r", 10 * GIB, 5 * GIB, "fuse.rclone")],
            &skip,
        );
        assert!(vols.is_empty());
    }

    #[tokio::test]
    async fn latest_preserves_sampler_timestamp() {
        // `latest()` must return the sampler's stamped timestamp verbatim, not a
        // request-time "now" — that masking is the bug this guards against.
        let snap = canonical_snapshot();
        let state = MetricsState::fresh_for_test(snap.clone());
        let got = state.latest().await;
        assert_eq!(
            got.timestamp, snap.timestamp,
            "latest() must not rewrite the timestamp at request time"
        );
    }

    #[test]
    fn fresh_state_is_not_stale() {
        let state = MetricsState::fresh_for_test(empty_snapshot());
        assert!(!state.is_stale(), "a just-marked sample must be fresh");
        assert!(state.sample_age() < SAMPLE_INTERVAL * STALE_INTERVALS);
    }

    #[test]
    fn sampler_goes_stale_once_intervals_elapse() {
        // Backdating the last sample past STALE_INTERVALS must flip is_stale() —
        // the dead-sampler signal /v1/health reports.
        let state = MetricsState::fresh_for_test(empty_snapshot());
        assert!(!state.is_stale());
        state.set_sample_age_for_test(
            SAMPLE_INTERVAL * STALE_INTERVALS + Duration::from_millis(500),
        );
        assert!(
            state.is_stale(),
            "no fresh sample past {STALE_INTERVALS} intervals must read stale"
        );
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
