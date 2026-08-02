//! Sampling the machine this process is running on.
//!
//! The Swift app renders the local machine as the first host card, collected
//! in-process by HostMetricsKit through IOKit and mach — macOS-only. This crate
//! is the cross-platform counterpart: the same *rendered* metrics on macOS and
//! Windows, built on `sysinfo` rather than the same syscalls. Parity is what the
//! card shows, not how it was read.
//!
//! # Unknown is not zero
//!
//! Most of what a host card shows is a rate or a level that the platform can
//! decline to answer, and this repo's rule is that unknown renders as "—", never
//! as a fabricated `0`. So [`LocalSnapshot`] — not `wire::Snapshot` — is what
//! [`LocalSampler::sample`] returns: every field the platform can fail to
//! measure is an `Option`, and `Some(0.0)` therefore means *measured zero* while
//! `None` means *nobody measured it*.
//!
//! Since #191 the wire contract carries that same distinction, so
//! [`LocalSnapshot::to_wire`] is very nearly a rename — the rates, memory
//! pressure, the thermal state and the GPU all cross intact. `wire::Disk`,
//! `wire::Network` and `wire::Gpu` are therefore held *directly* here rather
//! than behind another `Option`: two encodings of "unknown" stacked on one field
//! is how the two sides start disagreeing about which one means it.
//!
//! One loss survives: `wire::Cpu::total_usage` is still a bare `f64`, so an
//! unmeasured CPU lowers to `0.0` (with an empty core list beside it). The card
//! blanks that field from the `Option` here — see
//! `app/src-tauri/src/local.rs::lower_unknowns`, which is now the only rule left
//! in that module.
//!
//! # Cadence
//!
//! [`LocalSampler`] holds the previous reading of every cumulative counter, so
//! it must be kept alive across samples — a fresh sampler per poll would report
//! every rate as unknown, forever. Poll it about once a second: anything faster
//! than `sysinfo::MINIMUM_CPU_UPDATE_INTERVAL` leaves CPU usage unknown rather
//! than publishing a number measured over an interval too short to mean
//! anything.

mod battery;
mod gpu;
mod process;
mod rate;
mod thermal;
mod volume;

use std::time::{Duration, Instant};

use sysinfo::{Disks, Networks, ProcessesToUpdate, System, MINIMUM_CPU_UPDATE_INTERVAL};

use process::ProcEntry;
use rate::{Counters, RateTracker};
use volume::{MountEntry, MountPolicy};

pub use thermal::ThermalState;

const BYTES_PER_GIB: f64 = 1024.0 * 1024.0 * 1024.0;
const BYTES_PER_MIB: f64 = 1024.0 * 1024.0;

/// How long between full process enumerations. Matches the Swift collector's
/// `processSampleInterval`: enumerating every process is expensive, and the
/// question it answers ("what has been hogging this machine?") is a
/// ~minute-scale one, not a per-second one.
const PROCESS_SAMPLE_INTERVAL: Duration = Duration::from_secs(55);

/// One sample of the machine this process is running on.
///
/// See the module docs for why this is not a `wire::Snapshot`: every `Option`
/// here is a field the platform may decline to answer, and `None` must reach the
/// UI as "—" rather than as a number.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalSnapshot {
    /// RFC3339 UTC, formatted exactly as the agent's `now_rfc3339` does so both
    /// sources of a host card stamp time the same way.
    pub timestamp: String,
    pub cpu: LocalCpu,
    pub memory: LocalMemory,
    /// Disk I/O rates, each `None` until there are two cumulative readings to
    /// diff — see [`LocalSampler`]. The wire type carries its own unknown since
    /// #191, so there is no second `Option` wrapped around it.
    pub disk: wire::Disk,
    /// Network rates. `None` on the first sample, same as [`Self::disk`].
    pub network: wire::Network,
    /// Utilisation and VRAM on macOS, [`wire::Gpu::unknown`] elsewhere and on
    /// any machine with no `IOAccelerator` to read; see [`LocalSampler::sample`].
    pub gpu: wire::Gpu,
    /// `None` on machines with no battery, which is also what
    /// `wire::Snapshot::battery` being an `Option` means.
    pub battery: Option<wire::Battery>,
    pub volumes: Vec<wire::Volume>,
    /// The union of the top processes by CPU and by memory. Empty until the
    /// first enumeration lands — the wire contract's own way of saying "no
    /// processes to show", since it has no third state.
    pub processes: Vec<wire::Process>,
}

/// CPU metrics, with the two a platform can decline separated out.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalCpu {
    /// `None` when too little time has elapsed since the previous refresh for a
    /// usage delta to mean anything. Total and per-core are unknown together
    /// because they come out of the same refresh.
    pub usage: Option<CpuUsage>,
    /// The CPU brand string, or `"Unknown"` when sysinfo reports none — the same
    /// fallback the agent uses. A string can carry its own unknown, so this is
    /// not an `Option`.
    pub model: String,
    /// `None` where the platform exposes no thermal source. Windows is that
    /// platform today.
    pub thermal_state: Option<ThermalState>,
}

/// CPU usage over the interval since the previous refresh, 0–100.
#[derive(Clone, Debug, PartialEq)]
pub struct CpuUsage {
    pub total: f64,
    pub per_core: Vec<f64>,
}

/// Memory metrics. Capacity is always readable; pressure is not.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalMemory {
    pub used_gb: f64,
    pub total_gb: f64,
    pub swap_used_gb: f64,
    /// Memory pressure 0–100, as the host card's "Pressure: N%" line.
    ///
    /// `None` on every platform: macOS derives it from wired and compressed page
    /// counts that sysinfo does not expose (the Swift collector reaches into
    /// mach for them), and Windows has no equivalent figure at all. A defaulted
    /// `0` here would paint a permanently green pressure badge, which is exactly
    /// the fabricated number this crate refuses to publish.
    pub pressure: Option<f64>,
}

impl LocalSnapshot {
    /// Lowers this sample onto the agent's wire shape.
    ///
    /// Since #191 the contract can say "unknown", so every `Option` above
    /// crosses intact: an unmeasured rate, pressure, thermal state or GPU
    /// arrives on the other side as `None` and renders "—", exactly as a remote
    /// agent's omitted key does.
    ///
    /// **One field is still lossy:** `wire::Cpu::total_usage` is a bare `f64`,
    /// so an unmeasured CPU lands as `0.0` with an empty `core_usages` beside
    /// it. A caller that can render "—" must read [`LocalCpu::usage`] itself —
    /// which is precisely what `app/src-tauri/src/local.rs::lower_unknowns`
    /// does, and the only thing left in it.
    #[must_use]
    pub fn to_wire(&self) -> wire::Snapshot {
        wire::Snapshot {
            timestamp: self.timestamp.clone(),
            cpu: wire::Cpu {
                total_usage: self.cpu.usage.as_ref().map_or(0.0, |usage| usage.total),
                core_usages: self
                    .cpu
                    .usage
                    .as_ref()
                    .map(|usage| usage.per_core.clone())
                    .unwrap_or_default(),
                model: self.cpu.model.clone(),
                thermal_state: self.cpu.thermal_state.map(ThermalState::to_wire),
            },
            memory: wire::Memory {
                used_gb: self.memory.used_gb,
                total_gb: self.memory.total_gb,
                swap_used_gb: self.memory.swap_used_gb,
                pressure: self.memory.pressure,
            },
            disk: self.disk.clone(),
            network: self.network.clone(),
            gpu: self.gpu.clone(),
            battery: self.battery.clone(),
            volumes: self.volumes.clone(),
            processes: self.processes.clone(),
        }
    }
}

/// Samples the local machine, holding the state that makes rates possible.
///
/// Network and disk I/O arrive as cumulative byte totals, so a rate is only ever
/// a delta between two samples — which is why this is a long-lived struct and
/// not a free function. Construct it once, keep it, and call
/// [`sample`](Self::sample) on a timer; the first call has nothing to diff
/// against and reports both rate pairs as `None`.
pub struct LocalSampler {
    system: System,
    networks: Networks,
    disks: Disks,
    network_rate: RateTracker,
    disk_rate: RateTracker,
    /// When the CPU counters were last refreshed. `System::new_all()` primes
    /// them in [`Self::new`], so the first `sample()` already has a baseline —
    /// provided the caller did not call it in the same instant.
    cpu_refreshed_at: Instant,
    /// The most recent process enumeration, kept between samples because it is
    /// expensive and only refreshed every [`PROCESS_SAMPLE_INTERVAL`].
    processes: Vec<wire::Process>,
    processes_refreshed_at: Option<Instant>,
}

impl Default for LocalSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalSampler {
    /// Builds a sampler and primes it.
    ///
    /// `System::new_all()` takes the first reading of the CPU and process
    /// counters, so the first [`sample`](Self::sample) can already diff against
    /// something. The network and disk I/O baselines are deliberately *not*
    /// taken here: those rates are unknown until two samples exist, and pinning
    /// the baseline to construction time would quietly rate the first sample
    /// over however long the caller happened to wait before polling.
    #[must_use]
    pub fn new() -> Self {
        LocalSampler {
            system: System::new_all(),
            networks: Networks::new_with_refreshed_list(),
            disks: Disks::new_with_refreshed_list(),
            network_rate: RateTracker::default(),
            disk_rate: RateTracker::default(),
            cpu_refreshed_at: Instant::now(),
            processes: Vec::new(),
            processes_refreshed_at: None,
        }
    }

    /// Takes one sample of the local machine.
    ///
    /// GPU is read from IOKit's `IOAccelerator` registry on macOS, the same
    /// source `GPUMonitor.swift` reads (#205). Windows has no equally cheap
    /// equivalent — DXGI reports adapter memory but not utilisation — and stays
    /// a named non-goal, as does any Mac that matches no accelerator at all
    /// (a VM, which is what CI's macOS runners are). Both report
    /// `Gpu::unknown()`, whose `is_present() == false` renders "—" rather than
    /// a 0% GPU nobody looked at. See [`gpu`] for what the port deliberately
    /// leaves behind.
    pub fn sample(&mut self) -> LocalSnapshot {
        let now = Instant::now();
        let cpu_interval = now.saturating_duration_since(self.cpu_refreshed_at);
        // Below sysinfo's minimum, a usage delta is measured over too short an
        // interval to mean anything. Unknown beats a number nobody can trust.
        let deltas_are_meaningful =
            !cpu_interval.is_zero() && cpu_interval >= MINIMUM_CPU_UPDATE_INTERVAL;

        self.system.refresh_cpu_all();
        self.system.refresh_memory();
        self.networks.refresh(true);
        self.disks.refresh(true);
        self.cpu_refreshed_at = now;

        self.refresh_processes_if_due(now, deltas_are_meaningful);

        LocalSnapshot {
            timestamp: now_rfc3339(),
            cpu: LocalCpu {
                usage: deltas_are_meaningful.then(|| CpuUsage {
                    total: f64::from(self.system.global_cpu_usage()),
                    per_core: self
                        .system
                        .cpus()
                        .iter()
                        .map(|cpu| f64::from(cpu.cpu_usage()))
                        .collect(),
                }),
                model: self
                    .system
                    .cpus()
                    .first()
                    .map(|cpu| cpu.brand().trim().to_string())
                    .filter(|brand| !brand.is_empty())
                    .unwrap_or_else(|| "Unknown".to_string()),
                thermal_state: thermal::read(),
            },
            memory: LocalMemory {
                used_gb: self.system.used_memory() as f64 / BYTES_PER_GIB,
                total_gb: self.system.total_memory() as f64 / BYTES_PER_GIB,
                swap_used_gb: self.system.used_swap() as f64 / BYTES_PER_GIB,
                pressure: None,
            },
            disk: self.disk_rates(now),
            network: self.network_rates(now),
            // Physical memory is only ever used as the pool a *unified*-memory
            // GPU's occupancy is measured against; a discrete adapter is
            // measured against its own VRAM. See `gpu`'s module docs.
            gpu: gpu::read(self.system.total_memory()),
            battery: battery::read(),
            volumes: self.collect_volumes(),
            processes: self.processes.clone(),
        }
    }

    /// Network rates from the summed per-interface cumulative counters.
    ///
    /// Both fields `None` until there is a baseline to diff against — the wire
    /// type's own way of saying the rate is unknown, not a pair of zeros.
    fn network_rates(&mut self, now: Instant) -> wire::Network {
        let mut counters = Counters::default();
        for (_name, data) in self.networks.iter() {
            counters.inbound = counters.inbound.saturating_add(data.total_received());
            counters.outbound = counters.outbound.saturating_add(data.total_transmitted());
        }
        self.network_rate
            .update(counters, now)
            .map_or_else(wire::Network::default, |rates| wire::Network {
                download_mbps: Some(rates.inbound_mib_s),
                upload_mbps: Some(rates.outbound_mib_s),
            })
    }

    /// Disk I/O rates from the summed per-disk cumulative counters. Unknown
    /// before a baseline exists, same as [`Self::network_rates`].
    fn disk_rates(&mut self, now: Instant) -> wire::Disk {
        let mut counters = Counters::default();
        for disk in self.disks.list() {
            let usage = disk.usage();
            counters.inbound = counters.inbound.saturating_add(usage.total_read_bytes);
            counters.outbound = counters.outbound.saturating_add(usage.total_written_bytes);
        }
        self.disk_rate
            .update(counters, now)
            .map_or_else(wire::Disk::default, |rates| wire::Disk {
                read_mbps: Some(rates.inbound_mib_s),
                write_mbps: Some(rates.outbound_mib_s),
            })
    }

    fn collect_volumes(&self) -> Vec<wire::Volume> {
        let entries: Vec<MountEntry> = self
            .disks
            .list()
            .iter()
            .map(|disk| MountEntry {
                mount: disk.mount_point().to_string_lossy().to_string(),
                device: disk.name().to_string_lossy().to_string(),
                total: disk.total_space(),
                available: disk.available_space(),
                fstype: disk.file_system().to_string_lossy().to_lowercase(),
            })
            .collect();
        volume::build_volumes(entries, MountPolicy::host())
    }

    /// Re-enumerates processes on the slow cadence, keeping the previous list in
    /// between.
    ///
    /// The first enumeration also waits for `deltas_are_meaningful`: sysinfo
    /// derives per-process CPU from the interval since that process was last
    /// refreshed, and the very first interval — construction to first sample —
    /// can be arbitrarily short. Later enumerations are
    /// [`PROCESS_SAMPLE_INTERVAL`] apart, so the gate only ever bites once.
    ///
    /// What comes back is a *task* table on Linux, not a process table, so the
    /// rows are handed to [`process::top_union`] as-is and it is that function —
    /// with [`process::refresh_kind`] above it — that decides which of them are
    /// processes (#211).
    fn refresh_processes_if_due(&mut self, now: Instant, deltas_are_meaningful: bool) {
        let due = self
            .processes_refreshed_at
            .is_none_or(|at| now.saturating_duration_since(at) >= PROCESS_SAMPLE_INTERVAL);
        if !due || !deltas_are_meaningful {
            return;
        }

        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            process::refresh_kind(),
        );
        let all: Vec<ProcEntry> = self
            .system
            .processes()
            .iter()
            .map(|(pid, process)| ProcEntry {
                pid: i64::from(pid.as_u32()),
                // Prefer the executable's file name ("node", "chrome"):
                // sysinfo's `name()` is the thread/comm name for many
                // processes. Same preference the agent applies.
                name: process
                    .exe()
                    .and_then(|path| path.file_name())
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| process.name().to_string_lossy().to_string()),
                thread_kind: process.thread_kind(),
                cpu_percent: f64::from(process.cpu_usage()),
                memory_mb: process.memory() as f64 / BYTES_PER_MIB,
            })
            .collect();

        self.processes = process::top_union(all, process::TOP_LIMIT);
        self.processes_refreshed_at = Some(now);
    }
}

/// This machine's network host name, exactly as the platform reports it.
///
/// `None` when the platform declines to answer — an unknown name, not a guess
/// at one, the same rule the rest of this crate follows. Cosmetic suffixes
/// (macOS's `.local`) are left on: stripping them is a display decision, and
/// the caller that paints the card is the one that owns it.
#[must_use]
pub fn host_name() -> Option<String> {
    System::host_name()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

/// The current instant as RFC3339/ISO-8601 UTC, e.g. `2026-06-04T22:00:00Z`.
///
/// Byte-for-byte the agent's `now_rfc3339` (`agent/src/metrics.rs`), down to
/// dropping sub-second precision and normalising the offset to `Z`: a local
/// sample and an agent sample land in the same fields of the same card, and a
/// timestamp that formats two ways is a bug waiting for a staleness check.
fn now_rfc3339() -> String {
    use time::format_description::well_known::Rfc3339;
    use time::OffsetDateTime;
    OffsetDateTime::now_utc()
        .replace_nanosecond(0)
        .unwrap_or_else(|_| OffsetDateTime::now_utc())
        .format(&Rfc3339)
        .map(|formatted| formatted.replace("+00:00", "Z"))
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unknown_everything() -> LocalSnapshot {
        LocalSnapshot {
            timestamp: "2026-07-31T00:00:00Z".to_string(),
            cpu: LocalCpu {
                usage: None,
                model: "Unknown".to_string(),
                thermal_state: None,
            },
            memory: LocalMemory {
                used_gb: 8.0,
                total_gb: 16.0,
                swap_used_gb: 0.0,
                pressure: None,
            },
            disk: wire::Disk::default(),
            network: wire::Network::default(),
            gpu: wire::Gpu::unknown(),
            battery: None,
            volumes: Vec::new(),
            processes: Vec::new(),
        }
    }

    /// The acceptance criterion in one test, and the half of it #191 flipped: a
    /// first sample (rates unknown) and an idle sample (rates measured at zero)
    /// used to lower to *identical* wire values, because the contract had no
    /// way to say "unknown". Now they stay distinct all the way across, so the
    /// card can render "—" for one and "0.0 MB/s" for the other.
    #[test]
    fn unknown_and_measured_zero_stay_distinct_across_the_lowering() {
        let unmeasured = unknown_everything();
        let mut idle = unknown_everything();
        idle.network = wire::Network {
            download_mbps: Some(0.0),
            upload_mbps: Some(0.0),
        };
        idle.disk = wire::Disk {
            read_mbps: Some(0.0),
            write_mbps: Some(0.0),
        };

        assert_ne!(unmeasured, idle);
        assert_ne!(
            unmeasured.to_wire(),
            idle.to_wire(),
            "the wire contract must keep the two apart, not flatten them"
        );
        assert_eq!(unmeasured.to_wire().network.download_mbps, None);
        assert_eq!(idle.to_wire().network.download_mbps, Some(0.0));
    }

    /// `is_present()` keys on VRAM capacity, so an unmeasured GPU survives the
    /// lowering as an absent one and the view-model renders "—".
    #[test]
    fn an_unknown_gpu_lowers_to_an_absent_one_not_an_idle_one() {
        let wired = unknown_everything().to_wire();

        assert!(!wired.gpu.is_present());
        assert_eq!(wired.gpu, wire::Gpu::unknown());
    }

    #[test]
    fn a_present_but_idle_gpu_survives_the_lowering_as_present() {
        let mut snapshot = unknown_everything();
        snapshot.gpu = wire::Gpu {
            usage: Some(0.0),
            vram_used_gb: Some(1.0),
            vram_total_gb: Some(24.0),
        };

        assert!(snapshot.to_wire().gpu.is_present());
    }

    /// Before #191 an unknown thermal state lowered to the contract's `0`,
    /// which decodes as "Nominal" — a state nobody read, rendered as a badge.
    /// The contract can now say nothing at all, and does.
    #[test]
    fn an_unknown_thermal_state_lowers_to_unknown_not_to_nominal() {
        let snapshot = unknown_everything();

        assert_eq!(snapshot.cpu.thermal_state, None);
        assert_eq!(snapshot.to_wire().cpu.thermal_state, None);
    }

    #[test]
    fn a_measured_thermal_state_lowers_to_its_own_encoding() {
        let mut snapshot = unknown_everything();
        snapshot.cpu.thermal_state = Some(ThermalState::Serious);

        assert_eq!(snapshot.to_wire().cpu.thermal_state, Some(2));
    }

    /// Memory pressure crosses intact too, so the local card's green
    /// `Pressure: 0%` — the whole reason `lower_unknowns` had a branch for it —
    /// can no longer be produced by the lowering at all.
    #[test]
    fn unknown_memory_pressure_lowers_to_unknown_not_to_zero() {
        assert_eq!(unknown_everything().to_wire().memory.pressure, None);

        let mut measured = unknown_everything();
        measured.memory.pressure = Some(0.0);
        assert_eq!(measured.to_wire().memory.pressure, Some(0.0));
    }

    /// The one loss left in `to_wire`, pinned so it stays visible: `total_usage`
    /// is a bare `f64` on the wire, so an unmeasured CPU lands as `0.0` and only
    /// the card's own `lower_unknowns` can blank it.
    #[test]
    fn unknown_cpu_usage_lowers_to_an_empty_core_list_not_a_row_of_zeros() {
        let wired = unknown_everything().to_wire();

        assert!(wired.cpu.core_usages.is_empty());
        assert_eq!(wired.cpu.total_usage, 0.0, "the residual fabrication");
    }

    #[test]
    fn measured_cpu_usage_lowers_field_for_field() {
        let mut snapshot = unknown_everything();
        snapshot.cpu.usage = Some(CpuUsage {
            total: 37.5,
            per_core: vec![40.0, 35.0],
        });

        let wired = snapshot.to_wire();

        assert_eq!(wired.cpu.total_usage, 37.5);
        assert_eq!(wired.cpu.core_usages, vec![40.0, 35.0]);
    }

    /// Whatever this machine is called, the answer is never an empty or
    /// whitespace-only string masquerading as a name — that would paint a
    /// nameless card rather than admitting the name is unknown.
    #[test]
    fn the_host_name_is_either_absent_or_non_blank() {
        assert!(host_name().is_none_or(|name| !name.is_empty() && name.trim() == name));
    }

    #[test]
    fn the_timestamp_is_rfc3339_with_a_z_suffix_like_the_agents() {
        let stamp = now_rfc3339();

        assert!(stamp.ends_with('Z'), "got {stamp:?}");
        assert!(!stamp.contains('+'), "got {stamp:?}");
        assert_eq!(stamp.len(), "2026-06-04T22:00:00Z".len(), "got {stamp:?}");
    }
}
