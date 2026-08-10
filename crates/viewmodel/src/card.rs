//! Assembles the finished host-card view-model.
//!
//! Every string and every colour is decided here so the shell only paints.
//! That is what keeps the frontend small and the logic testable.
//!
//! # Unknown renders as an em dash — for every card
//!
//! `wire::Snapshot` carries an `Option` wherever a producer may have been
//! unable to measure something (see the `wire` crate's module docs), and
//! [`host_card`] is the **one** place those are lowered: `Some` formats,
//! `None` becomes [`UNKNOWN`] in the muted colour, and nothing that was never
//! measured reaches a chart. Remote cards and the local one go through it
//! alike — a second lowering beside it is how the two would start disagreeing
//! about what "we don't know" looks like.
//!
//! The rule is driven by the `Option`, never by testing a lowered number for
//! zero: `0.0` is a legitimate reading (an idle disk, a cool CPU) and hiding it
//! is the mirror-image bug.

use crate::color::{self, ThermalState};
use crate::format::{fmt, fmt_axis, fmt_rate, memory_label, relative_age};
use crate::history::History;
use crate::layout::{
    core_block_height, core_column_ladder, core_rung_height, core_visual_rows, CORE_GAP,
    CORE_MAX_COLUMNS, CORE_MIN_CELL, CORE_ROW_SPAN_DEFAULT, HISTORY_CAPACITY, PX_PER_SAMPLE,
};
use serde_json::{json, Value};

/// What the card says when the link is healthy and the *agent* is the thing
/// that stopped.
///
/// It deliberately says the agent is up: the operator's next move for a
/// stalled sampler (restart the agent) is not the one an unreachable host
/// asks for (check the tailnet, check the box is on), and a shared "not
/// current" phrasing would send them to the wrong layer — the same argument
/// `AgentError::user_message` makes one level down.
pub const SAMPLER_STALLED_MESSAGE: &str =
    "Agent is up but its sampler has stalled; these numbers are frozen";

/// What a card shows for a value nobody measured.
///
/// One character, one meaning, everywhere on every card — the shell reads it
/// back rather than spelling its own, so "we didn't measure it" cannot come to
/// look like two different things depending on which module blanked the field.
pub const UNKNOWN: &str = "—";

/// One `Option<f64>` as the card renders it: the formatter's output, or the em
/// dash. The entire unknown-lowering rule, in one place, so no field can quietly
/// pick a different fallback.
fn or_unknown(value: Option<f64>, format: impl FnOnce(f64) -> String) -> String {
    value.map_or_else(|| UNKNOWN.to_string(), format)
}

/// Connection health for a host that has at least one sample. `host_card`
/// doesn't poll — the shell decides which case applies — but the colour for
/// each case still comes from here, same as every other field this module
/// decides.
#[derive(Debug, Clone, PartialEq)]
pub enum Connection {
    /// The most recent poll succeeded; the data below is current.
    Live,
    // There is deliberately no `Stale` variant. A poll failure over a host
    // that *had* data used to render here — the last snapshot behind a red
    // badge — and it is now [`Pending::Unreachable`], a blanked card. Every
    // figure on a host card is a present-tense claim, and a badge is what a
    // glance does not read. Reintroducing it would put four-minute-old numbers
    // back under a green-looking grid.
    /// Every poll is *succeeding* and the data is stale anyway: the agent's
    /// own `/v1/health` reports `samplerStale`, so what it is serving is
    /// whatever its sampler last produced — or, before its first sample,
    /// `empty_snapshot()`'s zeros.
    ///
    /// The age is therefore the **agent's**, not this side's: `sampleAgeSeconds`
    /// from the same health payload, measured against the sampler's own clock.
    /// A coordinator-side elapsed would be the age of our last successful
    /// *request*, which for a stalled sampler is about a second — the exact
    /// number that makes frozen data read as current. `None` when the agent
    /// declines to report one (pre-#35 agents don't), and then the badge says
    /// "unknown" rather than inventing a figure.
    SamplerStale { sample_age_secs: Option<u64> },
}

fn connection_badge(c: &Connection) -> Value {
    match c {
        Connection::Live => json!({
            "state": "live",
            "color": color::hex(color::GREEN),
            "message": Value::Null,
        }),
        // The same `"state"` and the same red as a failed poll, on purpose: the
        // frontend keys its dot on that string, and a third state would mean a
        // new rendering to design, style and test for a card that must simply
        // stop looking live. What differs is the *message*, which is the part
        // that tells an operator which layer to go and look at.
        Connection::SamplerStale { sample_age_secs } => {
            stale_badge(SAMPLER_STALLED_MESSAGE, *sample_age_secs)
        }
    }
}

/// The one stale badge both stale cases render, so "not current" can never
/// arrive in two shapes the frontend has to tell apart.
fn stale_badge(message: &str, age_secs: Option<u64>) -> Value {
    let age = age_secs.map_or_else(|| "unknown".to_string(), relative_age);
    json!({
        "state": "stale",
        "color": color::hex(color::RED),
        "message": format!("{message} — last update {age}"),
    })
}

/// A host with nothing worth plotting: still waiting on its first poll, every
/// poll so far having failed before one landed, or — [`Unreachable`](Self::Unreachable)
/// — a host that *had* data and can no longer be contacted. There's nothing to
/// show, so there's no `host_card` to build, but the colour vocabulary is the
/// same.
#[derive(Debug, Clone, PartialEq)]
pub enum Pending {
    Connecting,
    Failed(String),
    /// The link is down on a host we used to reach.
    ///
    /// The card is blanked rather than left showing the last snapshot behind a
    /// badge. A CPU percentage is a present-tense claim, and a machine that has
    /// not answered in four minutes is not at 12% — the reading is as
    /// unmeasured as any the em dash covers, and a full card of them is a far
    /// louder lie than one number. What survives is `age_secs`: *when* it went
    /// quiet is the one fact still worth reporting, and the histories are kept
    /// in state, so the sparklines return intact with the host.
    Unreachable {
        message: String,
        /// Seconds since the last successful poll. `None` is rendered
        /// "unknown", never a fabricated `0s`.
        age_secs: Option<u64>,
    },
}

/// The `{"error": …}` shape the shell returns when it has nothing to show
/// yet, paired with the same `connection` badge `host_card` embeds once
/// there's data — so the frontend never special-cases where the colour
/// comes from.
pub fn pending_card(host_name: &str, p: &Pending) -> Value {
    let (state, col, message) = match p {
        Pending::Connecting => (
            "connecting",
            color::AMBER,
            "waiting for first sample…".to_string(),
        ),
        Pending::Failed(msg) => ("failed", color::RED, msg.clone()),
        // Same wording as `stale_badge`'s suffix, because it answers the same
        // question — only here it is the whole card rather than a badge on top
        // of numbers that are no longer true.
        Pending::Unreachable { message, age_secs } => (
            "unreachable",
            color::RED,
            format!(
                "{message} — last update {}",
                age_secs.map_or_else(|| "unknown".to_string(), relative_age)
            ),
        ),
    };
    json!({
        "error": { "hostName": host_name, "message": message },
        "connection": { "state": state, "color": color::hex(col), "message": Value::Null },
    })
}

/// Every series a host card plots.
#[derive(Debug, Clone, Default)]
pub struct HostHistories {
    pub cpu: History,
    pub mem: History,
    pub gpu: History,
    pub disk_read: History,
    pub disk_write: History,
    pub net_down: History,
    pub net_up: History,
    pub cores: Vec<History>,
}

impl HostHistories {
    pub fn new() -> Self {
        Self::default()
    }

    /// Folds one sample into every series.
    ///
    /// **Only measurements enter a chart.** A field the producer reported as
    /// `None` is skipped rather than pushed as `0.0`: a plotted point is a claim
    /// that somebody read that number at that moment, and a floor of fabricated
    /// zeros under an em dash would have the card saying two opposite things at
    /// once. Skipping costs one pixel of a 1s series.
    ///
    /// Series therefore advance independently, which they already did — the
    /// per-core buffers reset whenever the core count changes.
    pub fn record(&mut self, s: &wire::Snapshot) {
        self.cpu.push(s.cpu.total_usage);
        let mem_pct = if s.memory.total_gb > 0.0 {
            s.memory.used_gb / s.memory.total_gb * 100.0
        } else {
            0.0
        };
        self.mem.push(mem_pct);
        for (series, value) in [
            (&mut self.gpu, s.gpu.usage),
            (&mut self.disk_read, s.disk.read_mbps),
            (&mut self.disk_write, s.disk.write_mbps),
            (&mut self.net_down, s.network.download_mbps),
            (&mut self.net_up, s.network.upload_mbps),
        ] {
            if let Some(v) = value {
                series.push(v);
            }
        }

        if self.cores.len() != s.cpu.core_usages.len() {
            self.cores = (0..s.cpu.core_usages.len())
                .map(|_| History::new(HISTORY_CAPACITY))
                .collect();
        }
        for (h, v) in self.cores.iter_mut().zip(&s.cpu.core_usages) {
            h.push(*v);
        }
    }
}

/// Delegates to the wire crate so "does this host have a GPU" has exactly one
/// definition, shared with the agent that produces the data. Re-deriving the
/// `vram_total_gb > 0.0` test here is how the two sides drift.
fn has_gpu(s: &wire::Snapshot) -> bool {
    s.gpu.is_present()
}

pub fn host_card(
    host_name: &str,
    s: &wire::Snapshot,
    h: &HostHistories,
    connection: &Connection,
) -> Value {
    // An unread thermal state renders no badge at all — the same quiet the
    // "Normal" badge produces, but reached deliberately rather than by the
    // encoding coincidence that `0` happens to mean Nominal. The badge must
    // never claim a state nobody measured.
    let (badge, badge_col) = match s.cpu.thermal_state {
        Some(state) => color::thermal_badge(ThermalState::from_wire(state)),
        None => ("", color::MUTED),
    };

    let cores: Vec<Value> = s
        .cpu
        .core_usages
        .iter()
        .enumerate()
        .map(|(i, v)| {
            json!({
                "label": format!("Core {i}"),
                "value": format!("{}%", v.round() as i64),
                "valueColor": color::hex(color::usage_color(*v)),
                "hue": color::hex(color::CORE_COLORS[i % color::CORE_COLORS.len()]),
                "history": h.cores.get(i).map(|c| c.values()).unwrap_or(&[]),
            })
        })
        .collect();

    let mut vols: Vec<&wire::Volume> = s.volumes.iter().collect();
    // Fullest first, and a volume with no capacity to divide by sorts last:
    // `None` is not "0% full", it is "we can't say", and a card that led with
    // the unknowns would bury the one that is actually about to fill up.
    vols.sort_by(|a, b| {
        b.percent_used()
            .partial_cmp(&a.percent_used())
            .expect("percent_used is never NaN")
    });
    let volumes: Vec<Value> = vols
        .iter()
        .map(|v| {
            let pct = v.percent_used();
            json!({
                "mount": v.mount,
                "detail": format!(
                    "{} / {} GB · {}",
                    fmt(v.used_gb),
                    fmt(v.total_gb),
                    or_unknown(pct, |p| format!("{}%", p.round() as i64)),
                ),
                "tint": color::hex(pct.map_or(color::MUTED, color::volume_color)),
                // A bar length, not a reading: an unmeasurable volume draws an
                // empty muted track, and the "—" beside it is what says why.
                "fraction": pct.unwrap_or(0.0).clamp(0.0, 100.0) / 100.0,
            })
        })
        .collect();

    let mut by_cpu: Vec<&wire::Process> = s.processes.iter().collect();
    by_cpu.sort_by(|a, b| b.cpu_percent.partial_cmp(&a.cpu_percent).unwrap());
    let mut by_mem: Vec<&wire::Process> = s.processes.iter().collect();
    by_mem.sort_by(|a, b| b.memory_mb.partial_cmp(&a.memory_mb).unwrap());

    let disk_max = h
        .disk_read
        .values()
        .iter()
        .chain(h.disk_write.values())
        .cloned()
        .fold(0.1f64, f64::max);
    let net_max = h
        .net_down
        .values()
        .iter()
        .chain(h.net_up.values())
        .cloned()
        .fold(0.1f64, f64::max);

    let (gpu_value, gpu_color, vram) = if has_gpu(s) {
        (
            or_unknown(s.gpu.usage, |u| format!("{}%", u.round() as i64)),
            color::hex(color::GPU),
            format!(
                "VRAM: {} / {} GB",
                or_unknown(s.gpu.vram_used_gb, fmt),
                or_unknown(s.gpu.vram_total_gb, fmt)
            ),
        )
    } else {
        (
            UNKNOWN.to_string(),
            color::hex(color::MUTED),
            format!("VRAM: {UNKNOWN}"),
        )
    };
    // …and no series either. A pre-#183 agent reports an absent GPU as a literal
    // `usage: 0.0` on every sample — a measured zero as far as the contract can
    // tell — so the buffer really does fill, and the frontend would draw a flat
    // 0% line *underneath* the em dash above. A chart is a claim about
    // measurements; there are none to plot here, and an empty series draws the
    // grid and nothing else.
    //
    // The rates need no equivalent: `HostHistories::record` never pushes an
    // unmeasured one, so those buffers hold only real readings and stay
    // plottable even on the sample where the latest is unknown.
    let gpu_history: &[f64] = if has_gpu(s) { h.gpu.values() } else { &[] };

    let mut card = json!({
        "theme": {
            "panel": color::hex(color::PANEL), "panelAlt": color::hex(color::PANEL_ALT),
            "line": color::hex(color::LINE), "green": color::hex(color::GREEN),
            "muted": color::hex(color::MUTED), "ink": color::hex(color::INK),
            "cpu": color::hex(color::CPU), "mem": color::hex(color::MEM),
            "gpu": color::hex(color::GPU), "read": color::hex(color::READ),
            "write": color::hex(color::WRITE), "net": color::hex(color::NET),
            "netUp": color::hex(color::NET_UP),
        },
        "capacity": HISTORY_CAPACITY,
        "pxPerSample": PX_PER_SAMPLE,
        "coreBlockHeight": core_block_height(CORE_ROW_SPAN_DEFAULT),
        "coreLadder": core_column_ladder(
            s.cpu.core_usages.len(), CORE_MIN_CELL, CORE_GAP, CORE_MAX_COLUMNS,
        )
            .into_iter().map(|(w, c)| json!({
                "minWidth": w, "cols": c,
                // Per-rung block height: fixed while cells clear the Swift
                // squeeze floor, grown past it so plots never flex to 0.
                "height": core_rung_height(
                    CORE_ROW_SPAN_DEFAULT,
                    core_visual_rows(s.cpu.core_usages.len(), c),
                ),
            })).collect::<Vec<_>>(),
        "hostName": host_name,
        "cpuModel": s.cpu.model,
        "cpuValue": format!("{}%", s.cpu.total_usage.round() as i64),
        "cpuValueColor": color::hex(color::usage_color(s.cpu.total_usage)),
        "thermalText": badge,
        "thermalColor": color::hex(badge_col),
        "cpuHistory": h.cpu.values(),
        "cores": cores,
        "memValue": format!("{} / {} GB", fmt(s.memory.used_gb), s.memory.total_gb as i64),
        "memHistory": h.mem.values(),
        "swapText": format!("Swap: {} GB", fmt(s.memory.swap_used_gb)),
        "pressureText": format!(
            "Pressure: {}",
            or_unknown(s.memory.pressure, |p| format!("{}%", p.round() as i64)),
        ),
        "pressureColor": color::hex(
            s.memory.pressure.map_or(color::MUTED, color::pressure_color),
        ),
        "gpuValue": gpu_value,
        "gpuValueColor": gpu_color,
        "gpuHistory": gpu_history,
        "vramText": vram,
        "diskRead": or_unknown(s.disk.read_mbps, fmt_rate),
        "diskWrite": or_unknown(s.disk.write_mbps, fmt_rate),
        "diskAxis": fmt_axis(disk_max),
        "diskMax": disk_max,
        "diskReadHistory": h.disk_read.values(),
        "diskWriteHistory": h.disk_write.values(),
        "netDown": or_unknown(s.network.download_mbps, fmt_rate),
        "netUp": or_unknown(s.network.upload_mbps, fmt_rate),
        "netAxis": fmt_axis(net_max),
        "netMax": net_max,
        "netDownHistory": h.net_down.values(),
        "netUpHistory": h.net_up.values(),
        "volumeCount": s.volumes.len().to_string(),
        "volumes": volumes,
        "topCpu": by_cpu.iter().take(5).map(|p| json!({
            "name": p.name, "value": format!("{}%", p.cpu_percent.round() as i64)
        })).collect::<Vec<_>>(),
        "topRam": by_mem.iter().take(5).map(|p| json!({
            "name": p.name, "value": memory_label(p.memory_mb)
        })).collect::<Vec<_>>(),
    });
    // Built outside the macro above: nesting one more field inside it pushes
    // `json!`'s expansion past the default recursion limit.
    card["connection"] = connection_badge(connection);
    card
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pre-#183 agent's payload: every unmeasurable field present and
    /// fabricated. What the fleet still sends today.
    fn fixture() -> wire::Snapshot {
        serde_json::from_str(include_str!("../../wire/tests/fixtures/snapshot.json")).unwrap()
    }

    /// A #183-era agent's payload: everything it cannot measure is omitted, so
    /// every lifted field arrives as `None`. The fixture is the *contract's*,
    /// not a copy — a card test that hand-wrote the JSON could not notice the
    /// wire crate renaming a key.
    fn unknowns_fixture() -> wire::Snapshot {
        serde_json::from_str(include_str!(
            "../../wire/tests/fixtures/snapshot-unknowns.json"
        ))
        .unwrap()
    }

    fn gpu(usage: f64, used: f64, total: f64) -> wire::Gpu {
        wire::Gpu {
            usage: Some(usage),
            vram_used_gb: Some(used),
            vram_total_gb: Some(total),
        }
    }

    #[test]
    fn a_host_with_no_gpu_renders_an_em_dash_never_zero() {
        let mut s = fixture();
        s.gpu = wire::Gpu::zeros();
        let h = HostHistories::new();
        let vm = host_card("ubu-01", &s, &h, &Connection::Live);
        assert_eq!(vm["gpuValue"], "—");
        assert_eq!(vm["vramText"], "VRAM: —");
    }

    /// The em dash above and a flat 0% line under it would say opposite things.
    /// A pre-#183 agent reports an absent GPU as a literal `usage: 0.0`, which
    /// the contract cannot tell from a measurement, so the buffer really does
    /// fill — the series has to be dropped at the same place the value is, or
    /// the card renders a measurement nobody took.
    #[test]
    fn an_absent_gpu_plots_nothing_rather_than_a_flat_line_of_zeros() {
        let mut s = fixture();
        s.gpu = wire::Gpu::zeros();
        let mut h = HostHistories::new();
        for _ in 0..10 {
            h.record(&s);
        }
        assert_eq!(h.gpu.len(), 10, "the buffer really does fill with zeros");

        let vm = host_card("ubu-01", &s, &h, &Connection::Live);
        assert_eq!(vm["gpuValue"], "—");
        assert!(
            vm["gpuHistory"].as_array().expect("gpuHistory").is_empty(),
            "got {}",
            vm["gpuHistory"]
        );
    }

    /// …and a GPU that *is* present still plots, including while idle at 0%.
    #[test]
    fn a_present_gpu_plots_its_series_even_when_idle() {
        let mut s = fixture();
        s.gpu = gpu(0.0, 0.5, 24.0);
        let mut h = HostHistories::new();
        for _ in 0..10 {
            h.record(&s);
        }

        let vm = host_card("m4", &s, &h, &Connection::Live);
        assert_eq!(vm["gpuValue"], "0%");
        assert_eq!(vm["gpuHistory"].as_array().expect("gpuHistory").len(), 10);
    }

    /// The exact edge the em-dash rule pivots on. `usage == 0.0` is what an
    /// idle-but-present GPU reports, and it is indistinguishable from an
    /// absent one on that field alone — VRAM capacity is the discriminator.
    /// Deciding on `usage` instead would render a working GPU as "—" every
    /// time the host went quiet.
    #[test]
    fn a_gpu_that_is_present_but_idle_renders_zero_percent_not_an_em_dash() {
        let mut s = fixture();
        s.gpu = gpu(0.0, 0.5, 24.0);
        let vm = host_card("m4", &s, &HostHistories::new(), &Connection::Live);
        assert_eq!(vm["gpuValue"], "0%");
        assert_eq!(vm["gpuValueColor"], color::hex(color::GPU));
        assert_eq!(vm["vramText"], "VRAM: 0.5 / 24.0 GB");
    }

    #[test]
    fn a_host_with_a_gpu_renders_the_percentage() {
        let mut s = fixture();
        s.gpu = gpu(41.0, 3.5, 24.0);
        let vm = host_card("m4", &s, &HostHistories::new(), &Connection::Live);
        assert_eq!(vm["gpuValue"], "41%");
        assert_eq!(vm["vramText"], "VRAM: 3.5 / 24.0 GB");
    }

    /// A GPU whose capacity is readable but whose utilisation is not: present,
    /// so the card is not blanked wholesale, yet the one field nobody read
    /// still says so. Blanking per-field is the difference between "no GPU" and
    /// "a GPU we can't see the load on".
    #[test]
    fn a_present_gpu_with_an_unreadable_usage_blanks_only_that_field() {
        let mut s = fixture();
        s.gpu = wire::Gpu {
            usage: None,
            vram_used_gb: Some(3.5),
            vram_total_gb: Some(24.0),
        };
        let vm = host_card("m4", &s, &HostHistories::new(), &Connection::Live);
        assert_eq!(vm["gpuValue"], UNKNOWN);
        assert_eq!(vm["vramText"], "VRAM: 3.5 / 24.0 GB");
    }

    // MARK: the shared unknown-lowering (#191)

    /// The acceptance criterion for a REMOTE card, and the whole point of the
    /// issue: an agent that omits what it cannot measure gets em dashes, not
    /// the fabricated zeros the old contract forced. This is the same path the
    /// local card goes through — `app/src-tauri/src/local.rs` no longer owns a
    /// copy of these rules.
    #[test]
    fn a_remote_card_renders_every_omitted_field_as_an_em_dash() {
        let s = unknowns_fixture();
        let vm = host_card("nuc", &s, &HostHistories::new(), &Connection::Live);

        let muted = color::hex(color::MUTED);
        assert_eq!(vm["pressureText"], "Pressure: —");
        assert_eq!(vm["pressureColor"], muted);
        assert_eq!(vm["thermalText"], "");
        assert_eq!(vm["thermalColor"], muted);
        assert_eq!(vm["diskRead"], UNKNOWN);
        assert_eq!(vm["diskWrite"], UNKNOWN);
        assert_eq!(vm["gpuValue"], UNKNOWN);
        assert_eq!(vm["vramText"], "VRAM: —");

        // …while everything the agent DID measure renders as a number, so the
        // blanking is per-field rather than a whole card giving up.
        assert_eq!(vm["cpuValue"], "12%");
        assert_eq!(vm["netDown"], "0.9 MB/s");
        assert_eq!(vm["netUp"], "0.1 MB/s");
    }

    /// The mirror-image bug, guarded on the same fields: a host that is genuinely
    /// idle reports zeros and must SEE zeros. Nothing in the lowering may key on
    /// the value — only on whether the producer answered at all.
    #[test]
    fn a_measured_zero_renders_as_zero_on_a_remote_card_not_an_em_dash() {
        let mut s = unknowns_fixture();
        s.cpu.thermal_state = Some(0);
        s.memory.pressure = Some(0.0);
        s.disk = wire::Disk {
            read_mbps: Some(0.0),
            write_mbps: Some(0.0),
        };
        s.gpu = gpu(0.0, 0.0, 24.0);

        let vm = host_card("nuc", &s, &HostHistories::new(), &Connection::Live);
        assert_eq!(vm["pressureText"], "Pressure: 0%");
        assert_eq!(vm["pressureColor"], color::hex(color::GREEN));
        assert_eq!(vm["thermalText"], "Normal");
        assert_eq!(vm["diskRead"], "0.0 MB/s");
        assert_eq!(vm["gpuValue"], "0%");
    }

    /// An unmeasured rate must not enter a chart — the same rule the local
    /// card's poll loop has enforced since #176, now enforced for every card by
    /// `HostHistories::record` itself. A buffer of fabricated zeros under an
    /// em dash is the defect; here the buffer simply stays empty.
    #[test]
    fn an_unmeasured_rate_is_shown_as_unknown_and_never_plotted() {
        let s = unknowns_fixture();
        let mut h = HostHistories::new();
        for _ in 0..10 {
            h.record(&s);
        }

        assert!(h.disk_read.values().is_empty(), "no disk rate was measured");
        assert!(h.disk_write.values().is_empty());
        assert_eq!(h.net_down.values().len(), 10, "…but the network one was");

        let vm = host_card("nuc", &s, &h, &Connection::Live);
        assert_eq!(vm["diskRead"], UNKNOWN);
        assert!(vm["diskReadHistory"]
            .as_array()
            .expect("diskReadHistory")
            .is_empty());
        assert_eq!(vm["netDownHistory"].as_array().unwrap().len(), 10);
    }

    /// A volume with no capacity cannot be a percentage, and the old `0.0` made
    /// it a comfortable green bar instead. It renders "—" in the muted tint and
    /// sorts below every volume that does have a reading.
    #[test]
    fn a_volume_with_no_capacity_renders_unknown_and_sorts_last() {
        let vm = host_card(
            "nuc",
            &unknowns_fixture(),
            &HostHistories::new(),
            &Connection::Live,
        );
        let volumes = vm["volumes"].as_array().unwrap();

        assert_eq!(volumes[0]["mount"], "/");
        assert_eq!(volumes[0]["detail"], "61.0 / 234 GB · 26%");

        let unmeasurable = &volumes[1];
        assert_eq!(unmeasurable["mount"], "/media/empty");
        assert_eq!(unmeasurable["detail"], "0.0 / 0.0 GB · —");
        assert_eq!(unmeasurable["tint"], color::hex(color::MUTED));
        assert_eq!(
            unmeasurable["fraction"], 0.0,
            "an empty track, not a full one"
        );
    }

    #[test]
    fn volumes_are_ordered_fullest_first() {
        let vm = host_card(
            "ubu-01",
            &fixture(),
            &HostHistories::new(),
            &Connection::Live,
        );
        let mounts: Vec<&str> = vm["volumes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["mount"].as_str().unwrap())
            .collect();
        assert_eq!(mounts, vec!["/boot", "/mnt/data", "/"]);
    }

    #[test]
    fn processes_are_ranked_separately_for_cpu_and_ram() {
        // Deliberately divergent: the CPU leader is not the RAM leader, so a
        // copy-paste bug that sorted `topRam` on `cpu_percent` (or vice
        // versa) would be caught here — the shared fixture's processes have
        // correlated cpu/mem values and can't distinguish the two sorts.
        let mut s = fixture();
        s.processes = vec![
            wire::Process {
                pid: 1,
                name: "burst".to_string(),
                cpu_percent: 195.0,
                memory_mb: 800.0,
            },
            wire::Process {
                pid: 2,
                name: "hog".to_string(),
                cpu_percent: 12.0,
                memory_mb: 6144.0,
            },
        ];
        let vm = host_card("ubu-01", &s, &HostHistories::new(), &Connection::Live);
        assert_ne!(vm["topCpu"][0]["name"], vm["topRam"][0]["name"]);
        assert_eq!(vm["topCpu"][0]["name"], "burst");
        assert_eq!(vm["topCpu"][0]["value"], "195%");
        assert_eq!(vm["topRam"][0]["name"], "hog");
        assert_eq!(vm["topRam"][0]["value"], "6.0 GB");
    }

    #[test]
    fn the_ladder_and_block_height_travel_with_the_view_model() {
        let vm = host_card(
            "ubu-01",
            &fixture(),
            &HostHistories::new(),
            &Connection::Live,
        );
        assert_eq!(vm["coreBlockHeight"], 220.0);
        let rungs = vm["coreLadder"].as_array().unwrap();
        // Four rungs, not five: 16 cores at 16 columns is the one-row grid
        // `core_columns` would never pick, so the ladder no longer offers it.
        assert_eq!(rungs.len(), 4);
        assert_eq!(rungs[3]["cols"], 8);
        assert_eq!(rungs[3]["minWidth"], 888.0);
        // 8 cols -> 2 rows: fixed block. 2 cols -> 8 rows and 1 col -> 16
        // rows: past the squeeze floor, the rung grows so plots keep room.
        assert_eq!(rungs[3]["height"], 220.0);
        assert_eq!(rungs[1]["height"], 448.0);
        assert_eq!(rungs[0]["height"], 904.0);
    }

    #[test]
    fn every_core_gets_a_hue_and_a_usage_coloured_value() {
        let vm = host_card(
            "ubu-01",
            &fixture(),
            &HostHistories::new(),
            &Connection::Live,
        );
        let cores = vm["cores"].as_array().unwrap();
        assert_eq!(cores.len(), 16);
        assert_eq!(cores[0]["label"], "Core 0");
        // core 3 is at 94% in the fixture -> red
        assert_eq!(cores[3]["valueColor"], "#e05a4f");

        // 16 cores and 10 hues, so the fixture actually exercises the
        // `i % CORE_COLORS.len()` wraparound: core 10 returns to core 0's hue
        // and core 9 -- the last of the first cycle -- must not.
        assert_eq!(cores[10]["hue"], cores[0]["hue"]);
        assert_ne!(cores[9]["hue"], cores[0]["hue"]);
        assert_eq!(cores[11]["hue"], cores[1]["hue"]);
    }

    /// "Every series" means all eight `HostHistories` fields, each holding
    /// its OWN reading. Lengths alone can't tell the seven scalar series
    /// apart, so a copy-paste that recorded `disk.read_mbps` into
    /// `disk_write` would have gone unnoticed; the fixture's values are
    /// pairwise distinct, so asserting them pins each series to its source.
    #[test]
    fn recording_a_snapshot_populates_every_series() {
        let mut s = fixture();
        // The fixture's host has no GPU, so its usage is 0.0 -- the one value
        // that would collide with an unrecorded series.
        s.gpu.usage = Some(55.0);

        let mut h = HostHistories::new();
        h.record(&s);
        h.record(&s);

        assert_eq!(h.cpu.values(), &[34.2, 34.2]);
        assert_eq!(h.gpu.values(), &[55.0, 55.0]);
        assert_eq!(h.disk_read.values(), &[12.4, 12.4]);
        assert_eq!(h.disk_write.values(), &[88.1, 88.1]);
        assert_eq!(h.net_down.values(), &[2.1, 2.1]);
        assert_eq!(h.net_up.values(), &[0.4, 0.4]);

        // Memory is the one derived series: a percentage of total, never the
        // raw 18.2 GB.
        assert_eq!(h.mem.len(), 2);
        for v in h.mem.values() {
            assert!(
                (v - 18.2 / 62.7 * 100.0).abs() < 1e-9,
                "mem must be a percentage, got {v}"
            );
        }

        assert_eq!(h.cores.len(), 16);
        assert!(h.cores.iter().all(|c| c.len() == 2), "every core series");
        assert_eq!(h.cores[3].values(), &[94.0, 94.0]);
    }

    #[test]
    fn a_live_host_card_carries_a_green_connection_badge_with_no_message() {
        let vm = host_card(
            "ubu-01",
            &fixture(),
            &HostHistories::new(),
            &Connection::Live,
        );
        assert_eq!(vm["connection"]["state"], "live");
        assert_eq!(vm["connection"]["color"], "#33d17a");
        assert!(vm["connection"]["message"].is_null());
    }

    #[test]
    fn an_unreachable_host_renders_no_figures_and_dates_the_outage() {
        // The defect this guards against, inverted. It used to assert that a
        // failed poll changed the badge and never the numbers; the numbers were
        // the problem. A card is read at a glance, and at a glance a card is
        // its figures — so a host we cannot contact renders none of them.
        let live = host_card(
            "ubu-01",
            &fixture(),
            &HostHistories::new(),
            &Connection::Live,
        );
        assert!(!live["cpuValue"].is_null(), "the live card has figures");

        let down = pending_card(
            "ubu-01",
            &Pending::Unreachable {
                message: "Couldn't reach the agent. Check the host is up and the agent is running."
                    .to_string(),
                age_secs: Some(95),
            },
        );
        for field in ["cpuValue", "cores", "memValue", "volumes"] {
            assert!(down[field].is_null(), "{field} outlived the connection");
        }
        assert_eq!(down["connection"]["state"], "unreachable");
        assert_eq!(down["connection"]["color"], "#e05a4f");
        assert_eq!(down["error"]["hostName"], "ubu-01");
        let msg = down["error"]["message"].as_str().unwrap();
        assert!(msg.contains("Couldn't reach the agent"));
        assert!(
            msg.contains("1m ago"),
            "expected a relative age, got {msg:?}"
        );
    }

    /// `age_secs: None` is unreachable via the shell's own poll loop today
    /// (see `app/src-tauri/src/main.rs`'s `view_for`), but `pending_card` is a
    /// public API and must not fall back to a fabricated `0s ago` for an
    /// unknown age -- that would be exactly the kind of defaulted number this
    /// codebase's "unknown renders an em dash/placeholder, never zero" rule
    /// exists to catch.
    #[test]
    fn an_unreachable_card_with_an_unknown_age_says_unknown_never_zero() {
        let vm = pending_card(
            "ubu-01",
            &Pending::Unreachable {
                message: "Couldn't reach the agent.".to_string(),
                age_secs: None,
            },
        );
        let msg = vm["error"]["message"].as_str().unwrap();
        assert!(msg.contains("last update unknown"), "got {msg:?}");
        assert!(
            !msg.contains("0s ago"),
            "must not fabricate an age, got {msg:?}"
        );
    }

    /// The #182 case: every poll succeeded, so nothing on this side can tell
    /// the card is wrong — only the agent's own `samplerStale` can. It must
    /// land on the *same* badge a failed poll does (red, `"stale"`), because
    /// the frontend has exactly one not-live rendering and a card that is
    /// serving frozen numbers must never be the green one.
    #[test]
    fn a_stalled_sampler_renders_the_stale_badge_and_names_the_agent() {
        let s = fixture();
        let h = HostHistories::new();
        let live = host_card("ubu-01", &s, &h, &Connection::Live);
        let stalled = host_card(
            "ubu-01",
            &s,
            &h,
            &Connection::SamplerStale {
                sample_age_secs: Some(95),
            },
        );

        assert_eq!(stalled["connection"]["state"], "stale");
        assert_eq!(stalled["connection"]["color"], "#e05a4f");
        let msg = stalled["connection"]["message"].as_str().unwrap();
        assert!(msg.contains("sampler"), "got {msg:?}");
        // The agent's own age, not this side's: 95s is what `/v1/health`
        // reported, and it is the whole reason this variant exists.
        assert!(
            msg.contains("1m ago"),
            "expected the agent's age, got {msg:?}"
        );
        // …and it must not read like an unreachable host, or the operator
        // goes and checks the network instead of the agent.
        assert!(!msg.contains("Couldn't reach"), "got {msg:?}");

        // Same rule as the failed-poll case: staleness changes the badge,
        // never the numbers.
        assert_eq!(stalled["cpuValue"], live["cpuValue"]);
        assert_eq!(stalled["cores"], live["cores"]);
        assert_ne!(stalled["connection"], live["connection"]);
    }

    /// Reachable via the real poll loop, unlike its `Connection::Stale`
    /// counterpart: an agent older than #35 answers `/v1/health` without
    /// `sampleAgeSeconds`, and a `0s ago` invented there would say the frozen
    /// numbers are a second old.
    #[test]
    fn a_stalled_sampler_with_no_reported_age_says_unknown_never_zero() {
        let vm = host_card(
            "ubu-01",
            &fixture(),
            &HostHistories::new(),
            &Connection::SamplerStale {
                sample_age_secs: None,
            },
        );
        let msg = vm["connection"]["message"].as_str().unwrap();
        assert!(msg.contains("last update unknown"), "got {msg:?}");
        assert!(
            !msg.contains("0s ago"),
            "must not fabricate an age, got {msg:?}"
        );
    }

    #[test]
    fn a_connecting_host_has_no_data_and_an_amber_badge() {
        let vm = pending_card("ubu-01", &Pending::Connecting);
        assert_eq!(vm["error"]["hostName"], "ubu-01");
        assert_eq!(vm["error"]["message"], "waiting for first sample…");
        assert_eq!(vm["connection"]["state"], "connecting");
        assert_eq!(vm["connection"]["color"], "#e09a26");
    }

    #[test]
    fn a_host_that_never_connected_has_no_data_and_a_red_badge() {
        let vm = pending_card(
            "ubu-01",
            &Pending::Failed("Couldn't reach the agent.".to_string()),
        );
        assert_eq!(vm["error"]["message"], "Couldn't reach the agent.");
        assert_eq!(vm["connection"]["state"], "failed");
        assert_eq!(vm["connection"]["color"], "#e05a4f");
    }
}
