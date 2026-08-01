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
//! `wire::Snapshot` cannot carry that distinction. `wire::Network`, `wire::Disk`
//! and `wire::Memory::pressure` are bare `f64`s and `wire::Cpu::thermal_state`
//! is a bare `i64`, so [`LocalSnapshot::to_wire`] is lossy by construction and
//! documented as such. The one field that *can* express absence is `wire::Gpu`,
//! whose `is_present()` reads capacity — not usage — as the discriminator; an
//! unknown GPU lowers to `Gpu::zeros()`, which the view-model already renders as
//! "—".
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
mod process;
mod rate;
mod thermal;
mod volume;

use std::time::{Duration, Instant};

use sysinfo::{Disks, Networks, ProcessesToUpdate, System, MINIMUM_CPU_UPDATE_INTERVAL};

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
    /// Disk I/O rates. `None` on the first sample — see [`LocalSampler`].
    pub disk: Option<wire::Disk>,
    /// Network rates. `None` on the first sample — see [`LocalSampler`].
    pub network: Option<wire::Network>,
    /// `None` on every platform today; see [`LocalSampler::sample`].
    pub gpu: Option<wire::Gpu>,
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
    /// **Lossy, deliberately.** Every `None` above becomes the wire contract's
    /// placeholder, and for the bare-`f64` fields that placeholder is `0.0` —
    /// indistinguishable from a measured zero once it lands. `wire::Gpu` is the
    /// exception that proves the rule: `Gpu::zeros()` is what `Gpu::is_present()`
    /// reads as "this host has no GPU", so an unknown GPU survives the trip.
    ///
    /// Use this where a `wire::Snapshot` is genuinely required — feeding a
    /// view-model built for agent payloads, or a serialisation boundary. A
    /// caller that can render "—" should read the `Option` fields directly.
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
                thermal_state: self.cpu.thermal_state.map_or(0, ThermalState::to_wire),
            },
            memory: wire::Memory {
                used_gb: self.memory.used_gb,
                total_gb: self.memory.total_gb,
                swap_used_gb: self.memory.swap_used_gb,
                pressure: self.memory.pressure.unwrap_or(0.0),
            },
            disk: self.disk.clone().unwrap_or(wire::Disk {
                read_mbps: 0.0,
                write_mbps: 0.0,
            }),
            network: self.network.clone().unwrap_or(wire::Network {
                download_mbps: 0.0,
                upload_mbps: 0.0,
            }),
            gpu: self.gpu.clone().unwrap_or_else(wire::Gpu::zeros),
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
    /// GPU is `None` on every platform today: neither macOS nor Windows exposes
    /// usage and VRAM through a portable, dependency-free read (macOS wants
    /// IOKit's `IOAccelerator` registry, Windows wants DXGI or a vendor library
    /// like NVML), and half a GPU reading is worse than an honest absence. The
    /// wire type can say so — `Gpu::zeros()` is `is_present() == false` — so the
    /// card renders "—" rather than a 0% GPU nobody looked at.
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
            gpu: None,
            battery: battery::read(),
            volumes: self.collect_volumes(),
            processes: self.processes.clone(),
        }
    }

    /// Network rates from the summed per-interface cumulative counters.
    fn network_rates(&mut self, now: Instant) -> Option<wire::Network> {
        let mut counters = Counters::default();
        for (_name, data) in self.networks.iter() {
            counters.inbound = counters.inbound.saturating_add(data.total_received());
            counters.outbound = counters.outbound.saturating_add(data.total_transmitted());
        }
        let rates = self.network_rate.update(counters, now)?;
        Some(wire::Network {
            download_mbps: rates.inbound_mib_s,
            upload_mbps: rates.outbound_mib_s,
        })
    }

    /// Disk I/O rates from the summed per-disk cumulative counters.
    fn disk_rates(&mut self, now: Instant) -> Option<wire::Disk> {
        let mut counters = Counters::default();
        for disk in self.disks.list() {
            let usage = disk.usage();
            counters.inbound = counters.inbound.saturating_add(usage.total_read_bytes);
            counters.outbound = counters.outbound.saturating_add(usage.total_written_bytes);
        }
        let rates = self.disk_rate.update(counters, now)?;
        Some(wire::Disk {
            read_mbps: rates.inbound_mib_s,
            write_mbps: rates.outbound_mib_s,
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
    fn refresh_processes_if_due(&mut self, now: Instant, deltas_are_meaningful: bool) {
        let due = self
            .processes_refreshed_at
            .is_none_or(|at| now.saturating_duration_since(at) >= PROCESS_SAMPLE_INTERVAL);
        if !due || !deltas_are_meaningful {
            return;
        }

        self.system.refresh_processes(ProcessesToUpdate::All, true);
        let all: Vec<wire::Process> = self
            .system
            .processes()
            .iter()
            .map(|(pid, process)| wire::Process {
                pid: i64::from(pid.as_u32()),
                // Prefer the executable's file name ("node", "chrome"):
                // sysinfo's `name()` is the thread/comm name for many
                // processes. Same preference the agent applies.
                name: process
                    .exe()
                    .and_then(|path| path.file_name())
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| process.name().to_string_lossy().to_string()),
                cpu_percent: f64::from(process.cpu_usage()),
                memory_mb: process.memory() as f64 / BYTES_PER_MIB,
            })
            .collect();

        self.processes = process::top_union(all, process::TOP_LIMIT);
        self.processes_refreshed_at = Some(now);
    }
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
            disk: None,
            network: None,
            gpu: None,
            battery: None,
            volumes: Vec::new(),
            processes: Vec::new(),
        }
    }

    /// The acceptance criterion in one test: a first sample (rates unknown) and
    /// an idle sample (rates measured at zero) lower to *identical* wire values,
    /// so the wire type demonstrably cannot tell them apart — and
    /// [`LocalSnapshot`] demonstrably can.
    #[test]
    fn unknown_and_measured_zero_are_indistinguishable_on_the_wire_but_not_before_it() {
        let unmeasured = unknown_everything();
        let mut idle = unknown_everything();
        idle.network = Some(wire::Network {
            download_mbps: 0.0,
            upload_mbps: 0.0,
        });
        idle.disk = Some(wire::Disk {
            read_mbps: 0.0,
            write_mbps: 0.0,
        });

        assert_eq!(unmeasured.to_wire(), idle.to_wire());
        assert_ne!(unmeasured, idle);
        assert!(unmeasured.network.is_none());
        assert!(idle.network.is_some());
    }

    /// The one unknown the wire contract *can* carry: `is_present()` keys on
    /// VRAM capacity, so an unmeasured GPU survives the lowering as an absent
    /// one and the view-model renders "—".
    #[test]
    fn an_unknown_gpu_lowers_to_an_absent_one_not_an_idle_one() {
        let wired = unknown_everything().to_wire();

        assert!(!wired.gpu.is_present());
    }

    #[test]
    fn a_present_but_idle_gpu_survives_the_lowering_as_present() {
        let mut snapshot = unknown_everything();
        snapshot.gpu = Some(wire::Gpu {
            usage: 0.0,
            vram_used_gb: 1.0,
            vram_total_gb: 24.0,
        });

        assert!(snapshot.to_wire().gpu.is_present());
    }

    /// Lowering an unknown thermal state hands the wire contract its `0`, which
    /// decodes as "Nominal". That is the loss `to_wire`'s doc comment warns
    /// about, pinned here so nobody mistakes it for a measurement.
    #[test]
    fn an_unknown_thermal_state_lowers_to_the_contracts_nominal() {
        let snapshot = unknown_everything();

        assert_eq!(snapshot.cpu.thermal_state, None);
        assert_eq!(snapshot.to_wire().cpu.thermal_state, 0);
    }

    #[test]
    fn a_measured_thermal_state_lowers_to_its_own_encoding() {
        let mut snapshot = unknown_everything();
        snapshot.cpu.thermal_state = Some(ThermalState::Serious);

        assert_eq!(snapshot.to_wire().cpu.thermal_state, 2);
    }

    #[test]
    fn unknown_cpu_usage_lowers_to_an_empty_core_list_not_a_row_of_zeros() {
        let wired = unknown_everything().to_wire();

        assert!(wired.cpu.core_usages.is_empty());
        assert_eq!(wired.cpu.total_usage, 0.0);
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

    #[test]
    fn the_timestamp_is_rfc3339_with_a_z_suffix_like_the_agents() {
        let stamp = now_rfc3339();

        assert!(stamp.ends_with('Z'), "got {stamp:?}");
        assert!(!stamp.contains('+'), "got {stamp:?}");
        assert_eq!(stamp.len(), "2026-06-04T22:00:00Z".len(), "got {stamp:?}");
    }
}
