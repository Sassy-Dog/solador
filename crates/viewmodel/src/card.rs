//! Assembles the finished host-card view-model.
//!
//! Every string and every colour is decided here so the shell only paints.
//! That is what keeps the frontend small and the logic testable.

use crate::color::{self, ThermalState};
use crate::format::{fmt, fmt_axis, fmt_rate, memory_label, relative_age};
use crate::history::History;
use crate::layout::{
    core_block_height, core_column_ladder, CORE_GAP, CORE_MIN_CELL, CORE_ROW_SPAN_DEFAULT,
    HISTORY_CAPACITY, PX_PER_SAMPLE,
};
use serde_json::{json, Value};

/// Connection health for a host that has at least one sample. `host_card`
/// doesn't poll — the shell decides which case applies — but the colour for
/// each case still comes from here, same as every other field this module
/// decides.
#[derive(Debug, Clone, PartialEq)]
pub enum Connection {
    /// The most recent poll succeeded; the data below is current.
    Live,
    /// A poll failed, but a prior sample exists — the data below is real,
    /// just not current. `stale_secs` is how long ago the last successful
    /// poll landed; surfaced so "last thing we heard" can never be mistaken
    /// for "this is live". `None` when that age itself is unknown — renders
    /// as "unknown", never a fabricated `0s ago`.
    Stale {
        message: String,
        stale_secs: Option<u64>,
    },
}

fn connection_badge(c: &Connection) -> Value {
    match c {
        Connection::Live => json!({
            "state": "live",
            "color": color::hex(color::GREEN),
            "message": Value::Null,
        }),
        Connection::Stale {
            message,
            stale_secs,
        } => {
            let age = stale_secs.map_or_else(|| "unknown".to_string(), relative_age);
            json!({
                "state": "stale",
                "color": color::hex(color::RED),
                "message": format!("{message} — last update {age}"),
            })
        }
    }
}

/// A host with no sample yet: still waiting on its first poll, or every poll
/// so far has failed before one ever landed. There's no data to show, so
/// there's no `host_card` to build — but the colour vocabulary is the same.
#[derive(Debug, Clone, PartialEq)]
pub enum Pending {
    Connecting,
    Failed(String),
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

    pub fn record(&mut self, s: &metrics::Snapshot) {
        self.cpu.push(s.cpu.total_usage);
        let mem_pct = if s.memory.total_gb > 0.0 {
            s.memory.used_gb / s.memory.total_gb * 100.0
        } else {
            0.0
        };
        self.mem.push(mem_pct);
        self.gpu.push(s.gpu.usage);
        self.disk_read.push(s.disk.read_mbps);
        self.disk_write.push(s.disk.write_mbps);
        self.net_down.push(s.network.download_mbps);
        self.net_up.push(s.network.upload_mbps);

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

/// A host reports no discrete adapter as zero VRAM. Render `—`, never `0%`.
fn has_gpu(s: &metrics::Snapshot) -> bool {
    s.gpu.vram_total_gb > 0.0
}

pub fn host_card(
    host_name: &str,
    s: &metrics::Snapshot,
    h: &HostHistories,
    connection: &Connection,
) -> Value {
    let (badge, badge_col) = color::thermal_badge(ThermalState::from_wire(s.cpu.thermal_state));

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

    let mut vols: Vec<&metrics::Volume> = s.volumes.iter().collect();
    vols.sort_by(|a, b| b.percent_used().partial_cmp(&a.percent_used()).unwrap());
    let volumes: Vec<Value> = vols
        .iter()
        .map(|v| {
            let pct = v.percent_used();
            json!({
                "mount": v.mount,
                "detail": format!("{} / {} GB · {}%", fmt(v.used_gb), fmt(v.total_gb), pct.round() as i64),
                "tint": color::hex(color::volume_color(pct)),
                "fraction": pct.clamp(0.0, 100.0) / 100.0,
            })
        })
        .collect();

    let mut by_cpu: Vec<&metrics::Process> = s.processes.iter().collect();
    by_cpu.sort_by(|a, b| b.cpu_percent.partial_cmp(&a.cpu_percent).unwrap());
    let mut by_mem: Vec<&metrics::Process> = s.processes.iter().collect();
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
            format!("{}%", s.gpu.usage.round() as i64),
            color::hex(color::GPU),
            format!(
                "VRAM: {} / {} GB",
                fmt(s.gpu.vram_used_gb),
                fmt(s.gpu.vram_total_gb)
            ),
        )
    } else {
        (
            "—".to_string(),
            color::hex(color::MUTED),
            "VRAM: —".to_string(),
        )
    };

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
        "coreLadder": core_column_ladder(s.cpu.core_usages.len(), CORE_MIN_CELL, CORE_GAP)
            .into_iter().map(|(w, c)| json!({"minWidth": w, "cols": c})).collect::<Vec<_>>(),
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
        "pressureText": format!("Pressure: {}%", s.memory.pressure.round() as i64),
        "pressureColor": color::hex(color::pressure_color(s.memory.pressure)),
        "gpuValue": gpu_value,
        "gpuValueColor": gpu_color,
        "gpuHistory": h.gpu.values(),
        "vramText": vram,
        "diskRead": fmt_rate(s.disk.read_mbps),
        "diskWrite": fmt_rate(s.disk.write_mbps),
        "diskAxis": fmt_axis(disk_max),
        "diskMax": disk_max,
        "diskReadHistory": h.disk_read.values(),
        "diskWriteHistory": h.disk_write.values(),
        "netDown": fmt_rate(s.network.download_mbps),
        "netUp": fmt_rate(s.network.upload_mbps),
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

    fn fixture() -> metrics::Snapshot {
        serde_json::from_str(include_str!("../../metrics/tests/fixtures/snapshot.json")).unwrap()
    }

    #[test]
    fn a_host_with_no_gpu_renders_an_em_dash_never_zero() {
        let mut s = fixture();
        s.gpu.vram_total_gb = 0.0;
        s.gpu.usage = 0.0;
        let h = HostHistories::new();
        let vm = host_card("ubu-3xdv", &s, &h, &Connection::Live);
        assert_eq!(vm["gpuValue"], "—");
        assert_eq!(vm["vramText"], "VRAM: —");
    }

    #[test]
    fn a_host_with_a_gpu_renders_the_percentage() {
        let mut s = fixture();
        s.gpu.usage = 41.0;
        s.gpu.vram_used_gb = 3.5;
        s.gpu.vram_total_gb = 24.0;
        let vm = host_card("m4", &s, &HostHistories::new(), &Connection::Live);
        assert_eq!(vm["gpuValue"], "41%");
        assert_eq!(vm["vramText"], "VRAM: 3.5 / 24.0 GB");
    }

    #[test]
    fn volumes_are_ordered_fullest_first() {
        let vm = host_card(
            "ubu-3xdv",
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
            metrics::Process {
                pid: 1,
                name: "burst".to_string(),
                cpu_percent: 195.0,
                memory_mb: 800.0,
            },
            metrics::Process {
                pid: 2,
                name: "hog".to_string(),
                cpu_percent: 12.0,
                memory_mb: 6144.0,
            },
        ];
        let vm = host_card("ubu-3xdv", &s, &HostHistories::new(), &Connection::Live);
        assert_ne!(vm["topCpu"][0]["name"], vm["topRam"][0]["name"]);
        assert_eq!(vm["topCpu"][0]["name"], "burst");
        assert_eq!(vm["topCpu"][0]["value"], "195%");
        assert_eq!(vm["topRam"][0]["name"], "hog");
        assert_eq!(vm["topRam"][0]["value"], "6.0 GB");
    }

    #[test]
    fn the_ladder_and_block_height_travel_with_the_view_model() {
        let vm = host_card(
            "ubu-3xdv",
            &fixture(),
            &HostHistories::new(),
            &Connection::Live,
        );
        assert_eq!(vm["coreBlockHeight"], 220.0);
        let rungs = vm["coreLadder"].as_array().unwrap();
        assert_eq!(rungs.len(), 5);
        assert_eq!(rungs[3]["cols"], 8);
        assert_eq!(rungs[3]["minWidth"], 888.0);
    }

    #[test]
    fn every_core_gets_a_hue_and_a_usage_coloured_value() {
        let vm = host_card(
            "ubu-3xdv",
            &fixture(),
            &HostHistories::new(),
            &Connection::Live,
        );
        let cores = vm["cores"].as_array().unwrap();
        assert_eq!(cores.len(), 16);
        assert_eq!(cores[0]["label"], "Core 0");
        // core 3 is at 94% in the fixture -> red
        assert_eq!(cores[3]["valueColor"], "#e05a4f");
    }

    #[test]
    fn recording_a_snapshot_populates_every_series() {
        let s = fixture();
        let mut h = HostHistories::new();
        h.record(&s);
        h.record(&s);
        assert_eq!(h.cpu.len(), 2);
        assert_eq!(h.cores.len(), 16);
        assert_eq!(h.cores[0].len(), 2);
    }

    #[test]
    fn a_live_host_card_carries_a_green_connection_badge_with_no_message() {
        let vm = host_card(
            "ubu-3xdv",
            &fixture(),
            &HostHistories::new(),
            &Connection::Live,
        );
        assert_eq!(vm["connection"]["state"], "live");
        assert_eq!(vm["connection"]["color"], "#33d17a");
        assert!(vm["connection"]["message"].is_null());
    }

    #[test]
    fn a_stale_host_card_keeps_its_data_but_turns_red_and_says_how_old_it_is() {
        // The defect this guards against: a card that stops polling
        // successfully but keeps rendering as if nothing were wrong. The
        // numbers below must be the SAME numbers `Connection::Live` would
        // have produced -- staleness changes the badge, never the data.
        let s = fixture();
        let h = HostHistories::new();
        let live = host_card("ubu-3xdv", &s, &h, &Connection::Live);
        let stale = host_card(
            "ubu-3xdv",
            &s,
            &h,
            &Connection::Stale {
                message: "Couldn't reach the agent. Check the host is up and the agent is running."
                    .to_string(),
                stale_secs: Some(95),
            },
        );
        assert_eq!(stale["cpuValue"], live["cpuValue"]);
        assert_eq!(stale["cores"], live["cores"]);
        assert_eq!(stale["connection"]["state"], "stale");
        assert_eq!(stale["connection"]["color"], "#e05a4f");
        let msg = stale["connection"]["message"].as_str().unwrap();
        assert!(msg.contains("Couldn't reach the agent"));
        assert!(
            msg.contains("1m ago"),
            "expected a relative age, got {msg:?}"
        );
    }

    /// `stale_secs: None` is unreachable via the shell's own poll loop today
    /// (see `app/src-tauri/src/main.rs`'s `view_for`), but `host_card` is a
    /// public API and must not fall back to a fabricated `0s ago` for an
    /// unknown age -- that would be exactly the kind of defaulted number this
    /// codebase's "unknown renders an em dash/placeholder, never zero" rule
    /// exists to catch.
    #[test]
    fn a_stale_host_card_with_an_unknown_age_says_unknown_never_zero() {
        let vm = host_card(
            "ubu-3xdv",
            &fixture(),
            &HostHistories::new(),
            &Connection::Stale {
                message: "Couldn't reach the agent.".to_string(),
                stale_secs: None,
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
        let vm = pending_card("ubu-3xdv", &Pending::Connecting);
        assert_eq!(vm["error"]["hostName"], "ubu-3xdv");
        assert_eq!(vm["error"]["message"], "waiting for first sample…");
        assert_eq!(vm["connection"]["state"], "connecting");
        assert_eq!(vm["connection"]["color"], "#e09a26");
    }

    #[test]
    fn a_host_that_never_connected_has_no_data_and_a_red_badge() {
        let vm = pending_card(
            "ubu-3xdv",
            &Pending::Failed("Couldn't reach the agent.".to_string()),
        );
        assert_eq!(vm["error"]["message"], "Couldn't reach the agent.");
        assert_eq!(vm["connection"]["state"], "failed");
        assert_eq!(vm["connection"]["color"], "#e05a4f");
    }
}
