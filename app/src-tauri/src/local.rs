//! The local machine's host card — the one that leads the Hosts grid.
//!
//! Port of `DevCanopy/Services/HostMetrics/LocalHostMetricsService.swift` plus
//! the slice of `HostMetricsPanel` that renders it. The sampling itself is
//! [`localhost::LocalSampler`]; this module is the shell around it: the poll
//! loop's state, the display name, and the honest-unknown rendering.
//!
//! # Why this is very nearly just `host_card(&snapshot.to_wire(), …)`
//!
//! Because it must be: the local card has to be the *same* card a remote host
//! gets, or the two drift. Since #191 the wire contract can say "unknown", so
//! [`localhost::LocalSnapshot::to_wire`] carries the sampler's `Option`s across
//! intact and `viewmodel::card` blanks them — memory pressure, the thermal
//! state, the disk and network rates and the GPU all render "—" through the one
//! shared path, for a local card and an agent's alike.
//!
//! **One residue is left here.** `wire::Cpu::total_usage` is still a bare
//! `f64`, so an unmeasured CPU lowers to `0.0`; [`lower_unknowns`] blanks that
//! single field, driven by matching on the `Option` itself rather than by
//! testing the lowered number for zero — `0.0` is a legitimate reading (an idle
//! disk, a cool CPU) and treating it as absent is the mirror-image bug.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use viewmodel::card::{host_card, pending_card, Connection, HostHistories, Pending, UNKNOWN};
use viewmodel::color;

/// The card's `id` in the cockpit payload.
///
/// Remote cards are keyed by their store uuid; this one cannot collide with any
/// of them, which is what lets the frontend reuse DOM nodes across polls without
/// ever matching the local card to a remote one.
pub const CARD_ID: &str = "local";

/// What the card is called when the platform will not say what this machine is
/// called. Swift never hits this case (`ProcessInfo.hostName` always answers),
/// but a nameless card is worse than a generic one.
///
/// Taken from the store rather than spelled again here: it is the same phrase
/// the Containers panel already labels this machine's section with, and two
/// copies of one name are two things that can drift apart.
pub const FALLBACK_NAME: &str = store::LOCAL_HOST_SCOPE;

/// This machine's display name: the platform host name with macOS's cosmetic
/// `.local` suffix removed.
///
/// Byte-for-byte Swift's
/// `ProcessInfo.processInfo.hostName.replacingOccurrences(of: ".local", with: "")`
/// — `str::replace` has the same all-occurrences semantics — so the card is
/// titled the same in both apps.
#[must_use]
pub fn display_name(raw: Option<String>) -> String {
    raw.map(|name| name.replace(".local", ""))
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| FALLBACK_NAME.to_owned())
}

/// The local machine's poll state: the sampler (which must outlive one sample —
/// every rate is a delta between two of them), the histories the charts plot,
/// and the most recent sample.
pub struct LocalHostState {
    name: String,
    sampler: localhost::LocalSampler,
    histories: HostHistories,
    latest: Option<localhost::LocalSnapshot>,
}

impl LocalHostState {
    #[must_use]
    pub fn new() -> Self {
        LocalHostState {
            name: display_name(localhost::host_name()),
            sampler: localhost::LocalSampler::new(),
            histories: HostHistories::new(),
            latest: None,
        }
    }

    /// Takes one sample and folds it into the state.
    pub fn sample(&mut self) {
        let snapshot = self.sampler.sample();
        self.record(snapshot);
    }

    /// Folds an already-taken sample into the state.
    ///
    /// **An unmeasured value is shown but not plotted.** The rates take care of
    /// themselves — `HostHistories::record` skips any `None` field since #191,
    /// so a first sample's unknown disk rate simply does not enter that series.
    /// CPU usage is the exception, because it is the one field the wire lowering
    /// still flattens to `0.0`: pushing that would draw a dip to a floor nobody
    /// measured, so a sample with no CPU reading is not plotted at all.
    ///
    /// Separate from [`sample`](Self::sample) so the rule above is reachable
    /// from a test: a real sampler cannot be made to report unknown rates on
    /// demand, and a test that re-implemented this guard inline would pass
    /// against a version that had dropped it.
    pub fn record(&mut self, snapshot: localhost::LocalSnapshot) {
        if snapshot.cpu.usage.is_some() {
            self.histories.record(&snapshot.to_wire());
        }
        self.latest = Some(snapshot);
    }

    /// The card, ready to paint.
    ///
    /// The connection dot is **always green**: this process *is* the host, so
    /// there is no link to lose and no staleness to report. That is
    /// `ConnectionState.local` in Swift, which shares the green of `.connected`.
    #[must_use]
    pub fn card(&self) -> Value {
        match &self.latest {
            Some(snapshot) => card_from(&self.name, snapshot, &self.histories),
            // Before the first sample there is nothing to show — the same
            // amber "waiting" card a remote host gets, for the same reason.
            None => {
                let mut card = pending_card(&self.name, &Pending::Connecting);
                card["id"] = json!(CARD_ID);
                card
            }
        }
    }
}

/// One card from an already-taken sample.
///
/// Split out of [`LocalHostState::card`] so the offline fixture can build a
/// byte-stable local card from a hand-made snapshot — a fixture built through a
/// real sampler would carry whatever the dumping machine happened to be doing,
/// and no test could assert a value in it.
#[must_use]
pub fn card_from(
    name: &str,
    snapshot: &localhost::LocalSnapshot,
    histories: &HostHistories,
) -> Value {
    let mut card = host_card(name, &snapshot.to_wire(), histories, &Connection::Live);
    lower_unknowns(&mut card, snapshot);
    card["id"] = json!(CARD_ID);
    card
}

impl Default for LocalHostState {
    fn default() -> Self {
        Self::new()
    }
}

/// Blanks the one unknown the wire lowering still cannot carry.
///
/// Everything else — memory pressure, the thermal state, both rate pairs, the
/// GPU — reaches `viewmodel::card` as a `None` and is blanked there, by the same
/// code that blanks a remote agent's omitted keys. This function is what is left
/// after #191, and it should shrink to nothing the day `wire::Cpu::total_usage`
/// can say "unknown" too.
///
/// Driven by the `Option`, never by testing the lowered number for zero: an idle
/// CPU really does read 0%, and a card that hid it would be lying in the other
/// direction.
fn lower_unknowns(card: &mut Value, snapshot: &localhost::LocalSnapshot) {
    if snapshot.cpu.usage.is_none() {
        // `to_wire` already drops the per-core list to empty, so the core grid
        // simply has no cells — there is no row of fabricated 0% to blank.
        card["cpuValue"] = json!(UNKNOWN);
        card["cpuValueColor"] = json!(color::hex(color::MUTED));
    }
}

/// The local machine's poll loop, on the host cadence.
///
/// Same 1s as the remote hosts', and for the same reason: one history sample is
/// one fixed time slice (`PX_PER_SAMPLE`), so the cadence is part of the charts'
/// time axis rather than a preference. Sampling blocks (sysinfo reads
/// `/proc`-equivalents and enumerates mounts), so it runs on the blocking pool.
pub async fn poll_loop(state: Arc<Mutex<LocalHostState>>, interval: std::time::Duration) {
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let handle = Arc::clone(&state);
        // The lock lives entirely inside the blocking task, so it is never held
        // across the await and the `cockpit` command never waits on a sample.
        if let Err(e) = tokio::task::spawn_blocking(move || {
            handle.lock().expect("local state poisoned").sample()
        })
        .await
        {
            eprintln!("local metrics sampling failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> localhost::LocalSnapshot {
        localhost::LocalSnapshot {
            timestamp: "2026-07-31T00:00:00Z".to_owned(),
            cpu: localhost::LocalCpu {
                usage: Some(localhost::CpuUsage {
                    total: 37.5,
                    per_core: vec![40.0, 35.0],
                }),
                model: "Apple M4 Pro".to_owned(),
                thermal_state: Some(localhost::ThermalState::Nominal),
            },
            memory: localhost::LocalMemory {
                used_gb: 18.2,
                total_gb: 64.0,
                swap_used_gb: 0.5,
                pressure: Some(41.0),
            },
            disk: wire::Disk {
                read_mbps: Some(12.4),
                write_mbps: Some(88.1),
            },
            network: wire::Network {
                download_mbps: Some(2.1),
                upload_mbps: Some(0.4),
            },
            gpu: wire::Gpu::unknown(),
            battery: None,
            volumes: Vec::new(),
            processes: Vec::new(),
        }
    }

    fn card_for(snapshot: localhost::LocalSnapshot) -> Value {
        let mut card = host_card(
            "mac-studio",
            &snapshot.to_wire(),
            &HostHistories::new(),
            &Connection::Live,
        );
        lower_unknowns(&mut card, &snapshot);
        card
    }

    // MARK: display_name

    #[test]
    fn the_local_card_is_named_after_this_machine_without_the_local_suffix() {
        assert_eq!(display_name(Some("mac-studio.local".into())), "mac-studio");
        assert_eq!(display_name(Some("ubu-3xdv".into())), "ubu-3xdv");
    }

    #[test]
    fn an_unknown_or_blank_host_name_falls_back_rather_than_rendering_nothing() {
        assert_eq!(display_name(None), FALLBACK_NAME);
        assert_eq!(display_name(Some("   ".into())), FALLBACK_NAME);
        // A machine literally named ".local" leaves nothing behind.
        assert_eq!(display_name(Some(".local".into())), FALLBACK_NAME);
    }

    // MARK: the honest-unknown rules

    /// The acceptance criterion, in one test: a first sample (rates and CPU
    /// unknown, no thermal source, no pressure) renders em dashes, and the
    /// *identical* sample with those fields measured at exactly zero renders
    /// zeros. Nothing on this card can turn one into the other.
    ///
    /// Most of what it asserts is now decided in `viewmodel::card` rather than
    /// here — which is the point of #191, and why it is still asserted through
    /// the real local card: the local rendering is what must not regress,
    /// wherever the rule happens to live.
    #[test]
    fn unmeasured_fields_render_an_em_dash_and_measured_zeros_render_zero() {
        let mut unmeasured = snapshot();
        unmeasured.cpu.usage = None;
        unmeasured.cpu.thermal_state = None;
        unmeasured.memory.pressure = None;
        unmeasured.disk = wire::Disk::default();
        unmeasured.network = wire::Network::default();

        let mut idle = snapshot();
        idle.cpu.usage = Some(localhost::CpuUsage {
            total: 0.0,
            per_core: vec![0.0, 0.0],
        });
        idle.memory.pressure = Some(0.0);
        idle.disk = wire::Disk {
            read_mbps: Some(0.0),
            write_mbps: Some(0.0),
        };
        idle.network = wire::Network {
            download_mbps: Some(0.0),
            upload_mbps: Some(0.0),
        };

        let blank = card_for(unmeasured);
        assert_eq!(blank["cpuValue"], UNKNOWN);
        assert_eq!(blank["pressureText"], "Pressure: —");
        assert_eq!(blank["diskRead"], UNKNOWN);
        assert_eq!(blank["diskWrite"], UNKNOWN);
        assert_eq!(blank["netDown"], UNKNOWN);
        assert_eq!(blank["netUp"], UNKNOWN);
        assert_eq!(blank["thermalText"], "");

        let quiet = card_for(idle);
        assert_eq!(quiet["cpuValue"], "0%");
        assert_eq!(quiet["pressureText"], "Pressure: 0%");
        assert_eq!(quiet["diskRead"], "0.0 MB/s");
        assert_eq!(quiet["netDown"], "0.0 MB/s");
    }

    /// Every blanked field is muted, so "unknown" reads the same everywhere on
    /// the card — including the GPU, which `viewmodel::card` blanks on its own.
    #[test]
    fn every_unknown_field_is_muted_including_the_gpu_the_wire_type_can_carry() {
        let mut unmeasured = snapshot();
        unmeasured.cpu.usage = None;
        unmeasured.memory.pressure = None;
        let card = card_for(unmeasured);

        let muted = color::hex(color::MUTED);
        assert_eq!(card["cpuValueColor"], muted);
        assert_eq!(card["pressureColor"], muted);
        assert_eq!(card["gpuValue"], UNKNOWN);
        assert_eq!(card["gpuValueColor"], muted);
        assert_eq!(card["vramText"], "VRAM: —");
    }

    /// A measured card is untouched: `lower_unknowns` must only ever subtract,
    /// never rewrite a real reading.
    #[test]
    fn a_fully_measured_sample_renders_exactly_what_the_shared_card_builds() {
        let s = snapshot();
        let plain = host_card(
            "mac-studio",
            &s.to_wire(),
            &HostHistories::new(),
            &Connection::Live,
        );
        assert_eq!(card_for(s), plain);
    }

    /// The local machine cannot be unreachable from inside itself, so the dot
    /// is green and there is no stale message to render.
    #[test]
    fn the_local_connection_dot_is_always_green() {
        let card = card_for(snapshot());
        assert_eq!(card["connection"]["state"], "live");
        assert_eq!(card["connection"]["color"], color::hex(color::GREEN));
        assert!(card["connection"]["message"].is_null());
    }

    // MARK: the state machine

    #[test]
    fn before_the_first_sample_the_card_waits_rather_than_showing_zeros() {
        let state = LocalHostState {
            name: "mac-studio".to_owned(),
            sampler: localhost::LocalSampler::new(),
            histories: HostHistories::new(),
            latest: None,
        };
        let card = state.card();
        assert_eq!(card["id"], CARD_ID);
        assert_eq!(card["error"]["message"], "waiting for first sample…");
        assert_eq!(card["connection"]["state"], "connecting");
    }

    /// The card carries the fixed id whichever branch built it — a card whose
    /// id changed between the pending and the live state would be rebuilt from
    /// scratch (losing its chart nodes) on the very first successful sample.
    #[test]
    fn the_card_id_is_stable_across_the_pending_and_live_states() {
        let mut state = LocalHostState {
            name: "mac-studio".to_owned(),
            sampler: localhost::LocalSampler::new(),
            histories: HostHistories::new(),
            latest: None,
        };
        let pending = state.card();
        state.latest = Some(snapshot());
        let live = state.card();
        assert_eq!(pending["id"], live["id"]);
        assert_eq!(live["hostName"], "mac-studio");
    }

    /// A half-measured sample must not push a fabricated floor into the charts —
    /// driven through the real `record`, so deleting the guard fails this.
    #[test]
    fn an_unmeasured_sample_is_shown_but_not_plotted() {
        let mut state = LocalHostState {
            name: "mac-studio".to_owned(),
            sampler: localhost::LocalSampler::new(),
            histories: HostHistories::new(),
            latest: None,
        };

        let mut first = snapshot();
        first.disk = wire::Disk::default();
        first.network = wire::Network::default();
        state.record(first);
        assert!(
            state.histories.disk_read.values().is_empty(),
            "an unmeasured rate must not enter the series"
        );
        // …but it IS on screen: the card shows what was measured, with em dashes
        // for the rest, rather than waiting for a fully-measured sample.
        assert_eq!(state.card()["diskRead"], UNKNOWN);
        assert_eq!(state.card()["cpuValue"], "38%");
        // …and the CPU it *did* measure is plotted, rather than the whole sample
        // being dropped because one unrelated rate was unknown.
        assert_eq!(state.histories.cpu.values(), &[37.5]);

        state.record(snapshot());
        assert_eq!(state.histories.disk_read.values(), &[12.4]);
        assert_eq!(state.card()["diskRead"], "12.4 MB/s");
    }

    /// The other half of that guard, and the one the rates no longer need: a
    /// sample with no CPU reading is shown but not plotted, because
    /// `total_usage` is the field the wire lowering still flattens to `0.0`.
    #[test]
    fn a_sample_with_no_cpu_reading_is_shown_but_never_plotted_as_zero() {
        let mut state = LocalHostState {
            name: "mac-studio".to_owned(),
            sampler: localhost::LocalSampler::new(),
            histories: HostHistories::new(),
            latest: None,
        };

        let mut blind = snapshot();
        blind.cpu.usage = None;
        state.record(blind);

        assert!(
            state.histories.cpu.values().is_empty(),
            "a fabricated 0% must not enter the CPU series"
        );
        assert_eq!(state.card()["cpuValue"], UNKNOWN);
    }
}
