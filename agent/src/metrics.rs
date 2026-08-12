//! Host metric sampling.
//!
//! The wire types are NOT defined here — they live in the `solador-wire`
//! crate (imported as `wire`) and are re-exported below, so the agent that
//! produces the JSON and the app that consumes it cannot drift. This module
//! owns only the sampling: how each value is measured, and the `MetricsState`
//! handle the server reads from.
//!
//! # Measured, or absent
//!
//! Every optional field in the contract is here for one reason: this agent
//! either measures it or says nothing. `Some(0.0)` is a **reading** of zero —
//! an idle disk, a host with no memory stalls — and an omitted key means
//! nobody measured it, which a consumer renders as an em dash rather than a
//! reassuring green zero.
//!
//! What that means field by field (#183):
//!
//! - `memory.pressure` — **measured** on Linux from `/proc/pressure/memory`
//!   (PSI, see [`parse_psi_some_avg10`]); omitted where that file does not
//!   exist (macOS, or a kernel built without `CONFIG_PSI`).
//! - `cpu.thermalState` — **always omitted**. The contract's 0–3 ladder is
//!   macOS's `ProcessInfo.ThermalState`. Linux exposes thermal *zones* in
//!   millidegrees, and collapsing those into the ladder needs per-machine trip
//!   points nothing here knows — a different fabrication, not a fix for this
//!   one.
//! - `gpu` — **measured on hosts with an NVIDIA card** (#217), out of
//!   `nvidia-smi` rather than `sysinfo`, which reports no GPU on any platform.
//!   See [`crate::gpu`] for the probe and why it never runs on this module's 1s
//!   path. Omitted ([`Gpu::unknown`], serialising as `{}`) everywhere that
//!   probe measures nothing: no NVIDIA driver, a failed or hung invocation, or
//!   output it does not recognise. AMD and Intel are not read yet, so they are
//!   part of that "omitted" set.
//! - `disk` / `network` rates — **measured** from the sampler's byte deltas,
//!   so they are present (and often a legitimate `0.0`) in every real sample.
//!   Absent only from [`empty_snapshot`], which predates the first delta.
//! - `battery` — still `null` rather than omitted: it is the one optional the
//!   contract deliberately keeps emitting (see `wire::Snapshot::battery`), and
//!   `null` already reads as "no battery" on both consumers.

// Re-exported so `crate::metrics::Snapshot` keeps resolving for server.rs,
// main.rs and this module's tests. `allow(unused_imports)`: this is a binary
// crate, so a re-export nothing references internally still warns even though
// it is the module's documented surface.
#[allow(unused_imports)]
pub use wire::{Battery, Cpu, Disk, Gpu, Memory, Network, Process, Snapshot, Volume};

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sysinfo::{
    Disks, Networks, ProcessRefreshKind, ProcessesToUpdate, System, ThreadKind, UpdateKind,
};
use tokio::sync::RwLock;

// `pub(crate)` so the GPU probe converts its MiB figures through the same pair
// — the contract's `…GB` is 1024-base everywhere or it is inconsistent.
pub(crate) const BYTES_PER_GIB: f64 = 1024.0 * 1024.0 * 1024.0;
pub(crate) const BYTES_PER_MIB: f64 = 1024.0 * 1024.0;

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
// Wire contract types: see the `solador-wire` crate, re-exported at the top
// of this module. They used to be defined here, duplicated field-for-field
// against the Swift decoder — which is exactly how
// `HostMetricsError.decodeFailed` ("agent/app version skew") came to exist. One
// definition now; a rename here is a rename everywhere, and
// `crates/wire/tests/wire.rs` pins the JSON shape.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Volume filtering.
// ---------------------------------------------------------------------------

/// Env var holding a comma-separated fstype skip list. When set it REPLACES the
/// default list entirely (an operator who sets it owns the whole policy); an
/// empty value disables fstype filtering.
pub const SKIP_FSTYPES_ENV: &str = "SOLADOR_AGENT_SKIP_FSTYPES";

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

/// A row of the OS process table as plain data, so the "is this a process?"
/// rule below is testable (sysinfo's `Process`, like its `Disk`, cannot be
/// constructed outside that crate).
struct ProcEntry {
    pid: i64,
    name: String,
    /// What sysinfo makes of this row: `None` for a real process (a userland
    /// thread-group leader), `Some(Userland)` for one of that leader's sibling
    /// threads, `Some(Kernel)` for a kernel thread. Always `None` off Linux,
    /// where the table has no thread rows to classify.
    thread_kind: Option<ThreadKind>,
    cpu_percent: f64,
    memory_mb: f64,
}

/// Read the refreshed process table into [`ProcEntry`] rows.
fn process_entries(sys: &System) -> Vec<ProcEntry> {
    sys.processes()
        .iter()
        .map(|(pid, p)| ProcEntry {
            pid: pid.as_u32() as i64,
            // Prefer the executable's file name (e.g. "node", "chrome"); sysinfo's
            // name() is the Linux comm/thread name ("MainThread") for many procs.
            name: p
                .exe()
                .and_then(|e| e.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| p.name().to_string_lossy().to_string()),
            thread_kind: p.thread_kind(),
            cpu_percent: p.cpu_usage() as f64,
            memory_mb: p.memory() as f64 / BYTES_PER_MIB,
        })
        .collect()
}

/// Whether a process-table row is a **process** — the only thing `processes[]`
/// is allowed to describe (#211).
///
/// On Linux the table sysinfo hands back is really a *task* table, and both
/// kinds of non-process task were reaching the wire:
///
/// - **Sibling threads.** Every thread of a multi-threaded process is its own
///   task with its own TID, and each one carries its whole process's RSS
///   (threads share one address space). One SQL Server engine therefore painted
///   five identical 1.9 GB rows in TOP RAM and several partial rows in TOP CPU
///   — fabricated processes, and pid-dedupe cannot collapse them because the
///   TIDs genuinely differ.
/// - **Kernel threads.** `txg_sync` and friends are kernel tasks, not programs
///   anyone can point at; they arrive as top-level `/proc` entries no matter how
///   the table is enumerated, so a "leaders only" rule alone would keep them.
///
/// `thread_kind()` answers both: it is `Some` for exactly those two cases and
/// `None` for a userland thread-group leader, so a single test is the whole
/// policy.
fn is_process(entry: &ProcEntry) -> bool {
    entry.thread_kind.is_none()
}

/// Reduce the process table to the union of the top-`limit` by CPU and by
/// memory (deduped by pid), sorted by CPU descending.
///
/// Thread and kernel-thread rows are dropped first — see [`is_process`].
///
/// **The surviving row's CPU is already the whole process's.** sysinfo derives
/// per-process CPU from the `utime`/`stime` it reads out of that entry's own
/// `stat` file, and for a leader that file is `/proc/<pid>/stat`, whose times
/// the kernel reports thread-group-wide (`do_task_stat` with `whole`, i.e.
/// `thread_group_cputime_adjusted`) — per-thread times live in the separate
/// `/proc/<pid>/task/<tid>/stat`. So dropping the sibling rows loses no CPU
/// time and summing them back in would double-count it: the engine that showed
/// 201% + 90% + 26% across three thread rows shows one row at the full figure.
/// Memory is the same story from the other side — the leader's RSS is the
/// process's RSS, which is why it appeared N times to begin with.
fn top_processes(entries: Vec<ProcEntry>, limit: usize) -> Vec<Process> {
    let all: Vec<Process> = entries
        .into_iter()
        .filter(is_process)
        .map(|e| Process {
            pid: e.pid,
            name: e.name,
            cpu_percent: e.cpu_percent,
            memory_mb: e.memory_mb,
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

/// What the sampler asks sysinfo to refresh: exactly what `refresh_processes`
/// asks for, **minus the tasks**.
///
/// `refresh_processes` is documented as `…with_tasks()`, which on Linux walks
/// `/proc/<pid>/task/` for every process and files each thread it finds as
/// another process. Those rows are the fabricated `processes[]` entries of #211
/// — [`is_process`] rejects them, but not asking for them is cheaper (sysinfo
/// calls the task walk "quite expensive", and this agent watches hosts running
/// things like SQL Server with hundreds of threads) and means the reduction
/// never sees a row it has to reject.
///
/// [`is_process`] still runs, and still matters: kernel threads are top-level
/// `/proc` entries that arrive with or without this flag.
fn process_refresh_kind() -> ProcessRefreshKind {
    // `nothing()` is documented as everything-off EXCEPT `tasks`, which sysinfo
    // hard-codes to `true` in its `Default` — so the walk this function's doc
    // promises to skip stays on unless disabled by name (caught by
    // crates/localhost's port, PR #214).
    ProcessRefreshKind::nothing()
        .with_memory()
        .with_cpu()
        .with_disk_usage()
        .with_exe(UpdateKind::OnlyIfNotSet)
        .without_tasks()
}

// ---------------------------------------------------------------------------
// Memory pressure (Linux PSI).
// ---------------------------------------------------------------------------

/// Linux Pressure Stall Information for memory. Present since 4.20 on kernels
/// built with `CONFIG_PSI` (Ubuntu's are); absent everywhere else, including
/// macOS — which is exactly the case the `Option` exists for.
const MEMORY_PRESSURE_PATH: &str = "/proc/pressure/memory";

/// Read memory pressure, 0–100, or `None` where the host has no such figure.
///
/// One small procfs read per sample — cheap enough to do inline in the sampler,
/// and the reason `pressure` is measured rather than omitted on Linux.
fn read_memory_pressure() -> Option<f64> {
    let body = std::fs::read_to_string(MEMORY_PRESSURE_PATH).ok()?;
    parse_psi_some_avg10(&body)
}

/// Pull `some avg10` out of a PSI file body:
///
/// ```text
/// some avg10=0.00 avg60=0.12 avg300=0.03 total=8419234
/// full avg10=0.00 avg60=0.09 avg300=0.02 total=6217881
/// ```
///
/// `some` is the share of the last 10 seconds during which *at least one* task
/// stalled waiting on memory — the standard "how squeezed is this host" figure,
/// and already a percentage, so it lands in the contract's 0–100 range without
/// rescaling. (`full`, where *every* task stalled, is the thrashing signal; it
/// is near-permanently 0 and would read as "nothing to see" right up until the
/// host fell over.)
///
/// `None` for anything that isn't that: an empty body, a kernel that grew a
/// different layout, a non-finite number. Guessing is what this change exists
/// to stop.
fn parse_psi_some_avg10(body: &str) -> Option<f64> {
    let value = body
        .lines()
        .find_map(|line| line.strip_prefix("some "))?
        .split_whitespace()
        .find_map(|field| field.strip_prefix("avg10="))?
        .parse::<f64>()
        .ok()?;
    value.is_finite().then(|| value.clamp(0.0, 100.0))
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
///
/// The GPU probe is spawned here rather than inside the loop, so a restart
/// after a panic reattaches to the running probe instead of starting a second
/// one.
pub fn spawn_sampler() -> MetricsState {
    let state = MetricsState {
        inner: Arc::new(RwLock::new(empty_snapshot())),
        last_sample: Arc::new(Mutex::new(Instant::now())),
    };
    let handle = state.clone();
    let gpu_state = crate::gpu::spawn_probe();

    tokio::spawn(async move {
        loop {
            let task = handle.clone();
            let gpu = gpu_state.clone();
            // Isolate the loop so a panic inside it doesn't take down the agent;
            // a `JoinHandle` carries the panic back here as an `Err`.
            let join = tokio::spawn(async move { sampler_loop(task, gpu).await });
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
///
/// `gpu_state` is read, never driven: the probe behind it runs on its own task
/// and cadence ([`crate::gpu`]), so this loop's contact with a subprocess is a
/// mutex lock. A wedged `nvidia-smi` costs a stale GPU reading, not a stalled
/// snapshot.
async fn sampler_loop(state: MetricsState, gpu_state: crate::gpu::GpuState) {
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
            sys.refresh_processes_specifics(ProcessesToUpdate::All, true, process_refresh_kind());
            cached_processes = top_processes(process_entries(&sys), PROCESS_TOP_LIMIT);
        }

        let snap = compute_snapshot(
            &sys,
            &networks,
            &disks,
            SAMPLE_INTERVAL.as_secs_f64(),
            cached_processes.clone(),
            &skip,
            ProbedReadings {
                pressure: read_memory_pressure(),
                gpu: gpu_state.latest(),
            },
        );
        *state.inner.write().await = snap;
        state.mark_sampled();

        tick = tick.wrapping_add(1);
        tokio::time::sleep(SAMPLE_INTERVAL).await;
    }
}

/// The snapshot served before the first sample lands.
///
/// Nothing has been measured yet, so every field that *can* say so does:
/// pressure, thermal state, the GPU and both rate pairs are all absent, not
/// zero. The handful of non-optional fields (`totalUsage`, the memory sizes)
/// still read `0.0` — the contract cannot express their absence, and
/// `/v1/health` is what tells a consumer this sample is a placeholder
/// (`samplerStale`, #182).
pub(crate) fn empty_snapshot() -> Snapshot {
    Snapshot {
        timestamp: now_rfc3339(),
        cpu: Cpu {
            total_usage: 0.0,
            core_usages: Vec::new(),
            model: String::new(),
            thermal_state: None,
        },
        memory: Memory {
            used_gb: 0.0,
            total_gb: 0.0,
            swap_used_gb: 0.0,
            pressure: None,
        },
        // A rate is a delta, and no interval has elapsed to take one over.
        disk: Disk::default(),
        network: Network::default(),
        gpu: Gpu::unknown(),
        battery: None,
        volumes: Vec::new(),
        processes: Vec::new(),
    }
}

/// The readings that do not come from `sysinfo` — the two things this agent
/// measures itself, each of which may measure nothing at all.
///
/// Grouped rather than passed as loose arguments so [`compute_snapshot`] keeps
/// a readable signature as the list grows; every one of these stays an
/// argument, never a read inside the builder, so the builder remains pure.
struct ProbedReadings {
    /// Memory PSI, from [`read_memory_pressure`]. `None` off Linux.
    pressure: Option<f64>,
    /// The GPU probe's most recent reading ([`crate::gpu::GpuState::latest`]).
    /// [`Gpu::unknown`] wherever it measured nothing.
    gpu: Gpu,
}

/// Build a [`Snapshot`] from refreshed sysinfo state.
///
/// `interval_secs` is the elapsed time over which the network/disk byte deltas
/// were accumulated, used to convert byte deltas into bytes/sec.
///
/// `probed` carries the readings that do not come from `sysinfo`, passed in
/// rather than read here so this stays a pure function of its inputs: the
/// sampler owns the procfs read ([`read_memory_pressure`]) and the cached GPU
/// reading ([`crate::gpu::GpuState::latest`]), and a test can hand this both a
/// measured value and an unmeasured one.
fn compute_snapshot(
    sys: &System,
    networks: &Networks,
    disks: &Disks,
    interval_secs: f64,
    processes: Vec<Process>,
    skip_fstypes: &std::collections::HashSet<String>,
    probed: ProbedReadings,
) -> Snapshot {
    let ProbedReadings { pressure, gpu } = probed;
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
            // Omitted, never guessed: there is no Linux source for the
            // contract's macOS 0–3 thermal ladder. See the module docs.
            thermal_state: None,
        },
        memory: Memory {
            used_gb,
            total_gb,
            swap_used_gb,
            // Measured from PSI where the host has it, absent where it doesn't.
            pressure,
        },
        disk: Disk {
            read_mbps: Some(read_mbps),
            write_mbps: Some(write_mbps),
        },
        network: Network {
            download_mbps: Some(download_mbps),
            upload_mbps: Some(upload_mbps),
        },
        // Whatever the GPU probe last measured — `Gpu::unknown()` on every
        // host where it measured nothing, which claims nothing about one.
        gpu,
        battery: None,
        volumes,
        processes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    /// The canonical wire sample: every optional key present. Decoded
    /// byte-for-key by the Swift `Codable` type, and the lock on how a *present*
    /// value serialises.
    ///
    /// It is deliberately NOT what this agent emits any more — since #183 it
    /// omits `thermalState` and the GPU, and `pressure` only appears where PSI
    /// exists. This shape is still on the wire (every agent deployed before
    /// #183 sends it) and both consumers must keep decoding it, which is what
    /// this pins. What the agent emits *today* is pinned by
    /// `linux_snapshot_*`/`empty_snapshot_*` below.
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
                thermal_state: Some(0),
            },
            memory: Memory {
                used_gb: 12.3,
                total_gb: 32.0,
                swap_used_gb: 0.5,
                pressure: Some(0.0),
            },
            disk: Disk {
                read_mbps: Some(1.2),
                write_mbps: Some(0.3),
            },
            network: Network {
                download_mbps: Some(0.2),
                upload_mbps: Some(0.1),
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

    /// A snapshot from the live sampling path, over deliberately empty sysinfo
    /// state: the numbers are uninteresting (and deterministic), the *keys* are
    /// the point.
    fn sampled_snapshot(pressure: Option<f64>) -> Snapshot {
        sampled_snapshot_with(pressure, Gpu::unknown())
    }

    fn sampled_snapshot_with(pressure: Option<f64>, gpu: Gpu) -> Snapshot {
        compute_snapshot(
            &System::new(),
            &Networks::new(),
            &Disks::new(),
            1.0,
            Vec::new(),
            &skip_fstypes(None),
            ProbedReadings { pressure, gpu },
        )
    }

    /// CONTRACT LOCK (#183): the keys this agent never measures are ABSENT from
    /// what it serves, not zero. A hardcoded `"thermalState": 0` /
    /// `"gpu": {…0.0}` is indistinguishable from a reading once it is on the
    /// wire — which is how every remote card came to paint a green
    /// `Pressure: 0%` for a figure Linux never produced.
    #[test]
    fn sampled_snapshot_omits_the_thermal_state_and_gpu_it_cannot_measure() {
        let v = serde_json::to_value(sampled_snapshot(Some(4.5))).unwrap();

        let cpu = v["cpu"].as_object().unwrap();
        assert!(
            !cpu.contains_key("thermalState"),
            "no Linux source for the 0–3 ladder; the key must be absent, got {cpu:?}"
        );
        assert_eq!(
            v["gpu"],
            json!({}),
            "a host whose GPU probe measured nothing must omit every GPU key"
        );
    }

    /// The other half of #217: where the probe DID measure a card, the snapshot
    /// carries its reading through to the wire under the contract's keys —
    /// including the idle `0`s an RTX 3060 genuinely reports, which are now
    /// numbers because something read them.
    #[test]
    fn sampled_snapshot_carries_a_measured_gpu_onto_the_wire() {
        let measured = Gpu {
            usage: Some(0.0),
            vram_used_gb: Some(0.0),
            vram_total_gb: Some(12.0),
        };
        let v = serde_json::to_value(sampled_snapshot_with(Some(4.5), measured)).unwrap();

        assert_eq!(
            v["gpu"],
            json!({ "usage": 0.0, "vramUsedGB": 0.0, "vramTotalGB": 12.0 }),
            "a measured GPU must reach the wire under the contract's keys"
        );
    }

    /// The measured half of the same rule: a value this agent *does* sample is
    /// present, including when it legitimately reads zero.
    #[test]
    fn sampled_snapshot_keeps_the_rates_and_pressure_it_does_measure() {
        let v = serde_json::to_value(sampled_snapshot(Some(4.5))).unwrap();

        assert_eq!(v["memory"]["pressure"], json!(4.5));
        // Empty disk/network lists are a real reading of "no traffic" — a
        // measured 0.0 stays a number, never an omission.
        assert_eq!(v["disk"], json!({ "readMBps": 0.0, "writeMBps": 0.0 }));
        assert_eq!(
            v["network"],
            json!({ "downloadMBps": 0.0, "uploadMBps": 0.0 })
        );
    }

    /// On a host with no PSI (macOS, or a kernel without `CONFIG_PSI`) the
    /// pressure key disappears rather than defaulting to a comfortable zero.
    #[test]
    fn an_unmeasurable_pressure_omits_its_key_rather_than_reading_zero() {
        let v = serde_json::to_value(sampled_snapshot(None)).unwrap();
        let mem = v["memory"].as_object().unwrap();
        assert!(
            !mem.contains_key("pressure"),
            "unmeasured pressure must be absent, got {mem:?}"
        );
        // The sizes around it are measured and still present.
        assert!(mem.contains_key("usedGB") && mem.contains_key("totalGB"));
    }

    /// The pre-first-sample placeholder claims nothing it has not sampled: no
    /// rates (no interval has elapsed to take a delta over), no pressure, no
    /// thermal state, no GPU.
    #[test]
    fn empty_snapshot_claims_nothing_it_has_not_sampled() {
        let v = serde_json::to_value(empty_snapshot()).unwrap();
        assert_eq!(v["disk"], json!({}));
        assert_eq!(v["network"], json!({}));
        assert_eq!(v["gpu"], json!({}));
        assert!(!v["memory"].as_object().unwrap().contains_key("pressure"));
        assert!(!v["cpu"].as_object().unwrap().contains_key("thermalState"));
    }

    #[test]
    fn psi_some_avg10_is_the_measured_pressure() {
        let body = "some avg10=1.25 avg60=0.12 avg300=0.03 total=8419234\n\
                    full avg10=99.00 avg60=0.09 avg300=0.02 total=6217881\n";
        assert_eq!(
            parse_psi_some_avg10(body),
            Some(1.25),
            "the `some` line is the pressure figure, never `full`"
        );
    }

    #[test]
    fn psi_parses_a_genuine_zero_as_a_reading() {
        // The distinction the whole change rests on: an idle host measures 0.0,
        // and that is a number — not the absence this agent used to fake.
        let body = "some avg10=0.00 avg60=0.00 avg300=0.00 total=0\n";
        assert_eq!(parse_psi_some_avg10(body), Some(0.0));
    }

    #[test]
    fn psi_pressure_is_clamped_to_the_contract_range() {
        let body = "some avg10=142.50 avg60=0.00 avg300=0.00 total=0\n";
        assert_eq!(parse_psi_some_avg10(body), Some(100.0));
    }

    #[test]
    fn unparseable_psi_is_unknown_rather_than_a_guess() {
        for body in [
            "",
            "full avg10=0.30 avg60=0.00 avg300=0.00 total=0\n", // no `some` line
            "some avg60=0.30 avg300=0.00 total=0\n",            // no avg10 field
            "some avg10=NaN avg60=0.00 total=0\n",
            "some avg10=banana avg60=0.00 total=0\n",
            "somewhere avg10=0.30\n", // prefix match must not be a substring one
        ] {
            assert_eq!(
                parse_psi_some_avg10(body),
                None,
                "unrecognised PSI body must read unknown: {body:?}"
            );
        }
    }

    /// Load the shared cross-language battery fixture
    /// (`tests/fixtures/battery_contract.json`), decoded byte-for-key by both this
    /// crate and the Swift wire-contract suite.
    fn shared_battery_fixture() -> String {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../tests/fixtures/battery_contract.json"
        );
        std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read shared battery fixture {path}: {e}"))
    }

    /// CONTRACT LOCK (shared fixture): the non-null battery payload both sides
    /// agree on must round-trip through the agent's `Battery` type — deserialize
    /// from the shared JSON, then re-serialize to the identical value. If the
    /// agent's `Battery` keys drift from the fixture, this fails; the Swift
    /// `testSharedBatteryContractFixture` fails on the same file.
    #[test]
    fn battery_round_trips_shared_contract_fixture() {
        let json = shared_battery_fixture();
        let battery: Battery =
            serde_json::from_str(&json).expect("shared fixture decodes into Battery");
        assert_eq!(battery.level, 82.5);
        assert!(battery.is_charging);

        let reserialized = serde_json::to_value(&battery).unwrap();
        let original: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            reserialized, original,
            "Battery diverged from the shared wire contract fixture"
        );
    }

    /// The serialized battery has exactly the two contract keys — `level` and
    /// `isCharging` (camelCase) — and nothing else.
    #[test]
    fn battery_has_exact_contract_keys() {
        let v = serde_json::to_value(Battery {
            level: 82.5,
            is_charging: true,
        })
        .unwrap();
        let obj = v.as_object().expect("battery is an object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["isCharging", "level"]);
        assert!(v["level"].is_number());
        assert!(v["isCharging"].is_boolean());
    }

    /// A snapshot carrying a non-null battery serializes the nested object with
    /// the canonical keys — the path that today's `battery: None` agent never
    /// exercises, but the first battery-bearing host will.
    #[test]
    fn snapshot_with_battery_serializes_canonical_shape() {
        let mut snap = canonical_snapshot();
        snap.battery = Some(Battery {
            level: 82.5,
            is_charging: true,
        });
        let v = serde_json::to_value(&snap).unwrap();
        assert_eq!(v["battery"]["level"], json!(82.5));
        assert_eq!(v["battery"]["isCharging"], json!(true));
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

    // -----------------------------------------------------------------------
    // Process table (#211).
    // -----------------------------------------------------------------------

    fn proc_entry(
        pid: i64,
        name: &str,
        thread_kind: Option<ThreadKind>,
        cpu_percent: f64,
        memory_mb: f64,
    ) -> ProcEntry {
        ProcEntry {
            pid,
            name: name.to_string(),
            thread_kind,
            cpu_percent,
            memory_mb,
        }
    }

    /// The ubu-01 process table, as sysinfo hands it over on Linux: ONE SQL
    /// Server engine (pid 8723) whose sibling threads are each their own row
    /// carrying the whole process's 1.9 GB RSS and a slice of its CPU, the
    /// watchdog process beside it (pid 5371), and a ZFS kernel thread.
    fn sqlservr_table() -> Vec<ProcEntry> {
        vec![
            // The engine: a thread-group leader, so its CPU is already the
            // group's total and its RSS the group's RSS.
            proc_entry(8723, "sqlservr", None, 341.0, 1900.0),
            // Its threads. Distinct TIDs, so pid-dedupe never touched them.
            proc_entry(8724, "sqlservr", Some(ThreadKind::Userland), 201.0, 1900.0),
            proc_entry(8725, "sqlservr", Some(ThreadKind::Userland), 90.0, 1900.0),
            proc_entry(8726, "sqlservr", Some(ThreadKind::Userland), 26.0, 1900.0),
            proc_entry(8727, "sqlservr", Some(ThreadKind::Userland), 24.0, 1900.0),
            // The watchdog: a second, genuinely separate process.
            proc_entry(5371, "sqlservr", None, 0.1, 3.6),
            // A ZFS kernel thread — not a program, never a `processes[]` row.
            // Busy enough to place third by CPU, which is exactly how it came
            // to be painted into TOP CPU on the real host.
            proc_entry(412, "txg_sync", Some(ThreadKind::Kernel), 95.0, 0.0),
        ]
    }

    /// `ProcessRefreshKind::nothing()` leaves `tasks` ON — sysinfo's `Default`
    /// hard-codes it `true`, so the "expensive walk skipped" promise in
    /// [`process_refresh_kind`]'s docs was a no-op until `.without_tasks()`
    /// was named explicitly (found by crates/localhost's port, PR #214).
    #[test]
    fn the_refresh_never_asks_for_tasks() {
        let kind = process_refresh_kind();

        assert!(!kind.tasks(), "task rows are what #211 is made of");
        assert!(kind.cpu());
        assert!(kind.memory());
        assert_eq!(kind.exe(), UpdateKind::OnlyIfNotSet);
    }

    /// THE #211 BUG: a multi-threaded process is ONE row, not one per thread.
    /// TOP RAM showed five 1.9 GB `sqlservr` rows for a single engine because
    /// every thread reports its process's RSS, and pid-dedupe cannot see it —
    /// the TIDs are genuinely different pids.
    #[test]
    fn a_multi_threaded_process_yields_exactly_one_row() {
        let out = top_processes(sqlservr_table(), PROCESS_TOP_LIMIT);

        let engine: Vec<&Process> = out.iter().filter(|p| p.pid == 8723).collect();
        assert_eq!(engine.len(), 1, "the engine must appear once, got {out:?}");

        let threads: Vec<i64> = out
            .iter()
            .map(|p| p.pid)
            .filter(|pid| (8724..=8727).contains(pid))
            .collect();
        assert!(
            threads.is_empty(),
            "thread TIDs must not reach the wire, got {threads:?}"
        );

        // The one row that is not the engine is the watchdog — a real second
        // process, which the filter must NOT swallow along with the threads.
        let mut pids: Vec<i64> = out.iter().map(|p| p.pid).collect();
        pids.sort_unstable();
        assert_eq!(pids, vec![5371, 8723]);
    }

    /// The surviving row carries the process-level figures: the CPU sysinfo
    /// already aggregated across the thread group (never the sum of the thread
    /// rows on top of it — that would double-count), and the RSS exactly once
    /// rather than once per thread.
    #[test]
    fn the_surviving_row_carries_process_level_cpu_and_a_single_rss() {
        let out = top_processes(sqlservr_table(), PROCESS_TOP_LIMIT);

        let engine = out
            .iter()
            .find(|p| p.pid == 8723)
            .expect("the engine survives");
        assert!(
            (engine.cpu_percent - 341.0).abs() < 1e-9,
            "CPU must be the leader's group-wide figure, got {}",
            engine.cpu_percent
        );

        let rss_rows = out.iter().filter(|p| p.memory_mb == 1900.0).count();
        assert_eq!(rss_rows, 1, "1.9 GB is one process's RSS, counted once");
    }

    /// Kernel threads (`txg_sync` and the rest of the `[bracketed]` crowd) are
    /// tasks, not programs. They arrive as top-level `/proc` entries whatever
    /// the refresh asks for, so the filter — not the enumeration — is what
    /// keeps them off the wire.
    ///
    /// The kernel thread here out-ranks every process on BOTH axes, so it is
    /// the first row an unfiltered reduction would emit: the assertion cannot
    /// pass by the kernel thread quietly missing the cut.
    #[test]
    fn kernel_threads_never_reach_the_process_list() {
        let table = vec![
            proc_entry(412, "txg_sync", Some(ThreadKind::Kernel), 300.0, 4096.0),
            proc_entry(8723, "sqlservr", None, 12.0, 1900.0),
        ];
        let out = top_processes(table, PROCESS_TOP_LIMIT);

        assert!(
            !out.iter().any(|p| p.name == "txg_sync" || p.pid == 412),
            "a kernel thread is not a process, got {out:?}"
        );
        // …and the real process below it still comes through.
        assert_eq!(out.iter().map(|p| p.pid).collect::<Vec<_>>(), vec![8723]);
    }

    /// Filtering happens BEFORE the top-N cut, so threads cannot crowd real
    /// processes out of the list: five thread rows ahead of it must not cost
    /// the quiet 8 GB process its place.
    #[test]
    fn threads_do_not_consume_top_n_slots() {
        let mut table = vec![proc_entry(999, "postgres", None, 0.2, 8192.0)];
        for tid in 8724..8734 {
            table.push(proc_entry(
                tid,
                "sqlservr",
                Some(ThreadKind::Userland),
                200.0,
                1900.0,
            ));
        }
        table.push(proc_entry(8723, "sqlservr", None, 341.0, 1900.0));

        let out = top_processes(table, PROCESS_TOP_LIMIT);
        let mut pids: Vec<i64> = out.iter().map(|p| p.pid).collect();
        pids.sort_unstable();
        assert_eq!(pids, vec![999, 8723]);
    }

    /// Off Linux every row reads `thread_kind() == None` — the table has no
    /// thread rows to classify — so the filter is a no-op and the union
    /// behaviour this reduction has always had is unchanged: top-`limit` by
    /// CPU **unioned with** top-`limit` by memory, deduped, CPU-sorted.
    #[test]
    fn the_union_of_top_cpu_and_top_memory_survives_the_filter() {
        let table = vec![
            proc_entry(1, "spinner", None, 99.0, 1.0),
            proc_entry(2, "idle-hog", None, 0.1, 8192.0),
            proc_entry(3, "middling", None, 50.0, 2.0),
        ];
        let out = top_processes(table, 1);
        let pids: Vec<i64> = out.iter().map(|p| p.pid).collect();
        assert_eq!(
            pids,
            vec![1, 2],
            "top-1 by CPU plus top-1 by memory, CPU-sorted"
        );
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

    /// THE ubu-01 `/shared` bug: a bind mount of the root filesystem reads
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
