# Cross-platform walking skeleton — Rust core + Tauri shell

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A running Tauri app on macOS that renders the real DevCanopy host card from live metrics served by the existing agent, with all presentation logic in unit-tested Rust and the Swift app untouched.

**Architecture:** Three new Rust crates under a root workspace — `metrics` (wire types shared by agent and app), `agentclient` (HTTP polling), `viewmodel` (formatters, colour rules, layout policy, view-model assembly) — plus a Tauri v2 shell whose frontend is static HTML/CSS/JS with no build step. The frontend receives a finished view-model and only paints.

**Tech Stack:** Rust 2021, Tauri 2, `reqwest`, `serde`, `keyring`, plain HTML/CSS/JS, Playwright for frontend layout assertions.

**Spec:** `Docs/superpowers/specs/2026-07-27-cross-platform-tauri-design.md`

## Global Constraints

Every task's requirements implicitly include these.

- **Platforms:** macOS and Windows only. Do not add Linux-conditional code or dependencies.
- **No npm.** No `package.json`, no `node_modules`, no lockfile, no bundler, no framework. `app/ui/` is served as static files.
- **CSP is strict.** `app/src-tauri/tauri.conf.json` must set a real `csp`; `"csp": null` must never be committed.
- **Escape all remote-origin strings.** Host names, CPU models, mount paths and process names come from a remote agent. Every interpolation into markup passes through `esc()`, or uses `textContent`.
- **Unknown renders `—`.** Never `0`, never a default. `Option<f64>` at the Rust boundary; the frontend has no branch that could invent a value.
- **CockpitTheme colours are verbatim** from `DevCanopy/Views/Cockpit/CockpitTheme.swift`: panel `#050805`, panelAlt `#0a0f0c`, line `#13301f`, green `#33d17a`, amber `#e09a26`, red `#e05a4f`, muted `#5a6b60`, ink `#cfe9d8`.
- **Chart hues verbatim** from `HostMetricsPanel.swift`: cpu `#5b8def`, mem `#b066f0`, gpu `#33c7c7`, read `#3fb950`, write `#e0922a`, net `#5b8def`, netUp `#9bd34a`. Core hues cycle through the 10 in `CORE_COLORS`.
- **Never compute a version.** `Scripts/get-version-info.sh` and `Scripts/get-build-number.sh` remain the only sources (`Docs/VERSIONING.md`).
- **Do not modify the Swift app.** It keeps shipping unchanged throughout this plan.
- **Do not modify `agent/`.** Task 3 verifies compatibility against it by fixture; changing the agent is a later plan.

## Two corrections to the spec

Both were found by checking the spec against the code; apply them to the spec when this plan lands.

1. **The spec's crate list omits an agent client.** `crates/metrics` covers collection and `crates/sources` covers third-party APIs, but nothing owned "poll a DevCanopy agent over HTTP" — which is `Services/HostMetrics/RemoteHostMetricsService.swift` today. This plan adds `crates/agentclient`.

2. **The spec claimed phases 1–3 delete the duplication while the Swift app still ships. They don't.** Swift cannot consume a Rust crate without FFI, so `HostMetricsKit` survives until either the Swift app retires or the local machine is served by a localhost agent. The honest framing: phases 1–3 *build the replacement*; deletion lands later. This plan does not claim otherwise.

## File Structure

| File | Responsibility |
|---|---|
| `Cargo.toml` (root) | Workspace: `crates/*`, `app/src-tauri`. `agent/` stays independent this plan |
| `crates/metrics/src/lib.rs` | Wire types (`Snapshot`, `Cpu`, `Memory`, …) with `Serialize + Deserialize`. Owns the agent's JSON contract |
| `crates/agentclient/src/lib.rs` | `AgentClient::snapshot()` — bearer auth, timeout, typed errors |
| `crates/viewmodel/src/format.rs` | `fmt`, `fmt_rate`, `fmt_axis`, `memory_label` |
| `crates/viewmodel/src/color.rs` | Palette constants, `usage_color`, `pressure_color`, `volume_color`, `thermal_badge` |
| `crates/viewmodel/src/layout.rs` | `core_column_ladder`, `core_block_height`, `core_cell_height`, `core_visual_rows`, `visible_samples` |
| `crates/viewmodel/src/card.rs` | `host_card(snapshot, history) -> HostCardVm` — assembles the finished view-model |
| `crates/viewmodel/src/history.rs` | `History` ring buffer, capacity 600 |
| `app/src-tauri/src/main.rs` | Tauri bin: state, poll loop, `#[tauri::command] snapshot()` |
| `app/ui/index.html` | Card structure |
| `app/ui/app.css` | CockpitTheme, container queries, fixed core block |
| `app/ui/app.js` | Paint the view-model; `esc()`; `ResizeObserver` chart windowing |
| `.github/workflows/ci.yml` | Add a `rust-workspace` job |

---

### Task 1: Workspace + formatters and colour rules

**Files:**
- Create: `Cargo.toml`
- Create: `crates/viewmodel/Cargo.toml`
- Create: `crates/viewmodel/src/lib.rs`
- Create: `crates/viewmodel/src/format.rs`
- Create: `crates/viewmodel/src/color.rs`

**Interfaces:**
- Consumes: nothing
- Produces: `viewmodel::format::{fmt, fmt_rate, fmt_axis, memory_label}` all `fn(f64) -> String`; `viewmodel::color::{PANEL, PANEL_ALT, LINE, GREEN, AMBER, RED, MUTED, INK, CPU, MEM, GPU, READ, WRITE, NET, NET_UP, CORE_COLORS, hex, usage_color, pressure_color, volume_color}` — colours are `u32`, `hex(u32) -> String` renders `#rrggbb`, the `*_color` fns are `fn(f64) -> u32`; `viewmodel::color::{ThermalState, thermal_badge}` where `ThermalState` has variants `Nominal | Fair | Serious | Critical` plus `ThermalState::from_wire(i64) -> ThermalState`, and `thermal_badge(ThermalState) -> (&'static str, u32)`

- [ ] **Step 1: Create the workspace manifest**

`Cargo.toml`:

```toml
[workspace]
resolver = "2"
members = ["crates/metrics", "crates/agentclient", "crates/viewmodel", "app/src-tauri"]

[workspace.package]
edition = "2021"
rust-version = "1.90"

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
strip = true
```

`crates/viewmodel/Cargo.toml`:

```toml
[package]
name = "viewmodel"
version = "0.1.0"
edition.workspace = true

[dependencies]
serde = { workspace = true }
metrics = { path = "../metrics" }
```

`crates/viewmodel/src/lib.rs`:

```rust
pub mod card;
pub mod color;
pub mod format;
pub mod history;
pub mod layout;
```

Create empty placeholder modules so the crate compiles as tasks land:

```bash
mkdir -p crates/viewmodel/src
for m in card color format history layout; do touch crates/viewmodel/src/$m.rs; done
```

- [ ] **Step 2: Write the failing formatter tests**

`crates/viewmodel/src/format.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_decimal_below_100_integral_above() {
        assert_eq!(fmt(18.24), "18.2");
        assert_eq!(fmt(412.0), "412");
    }

    #[test]
    fn rate_switches_to_gb_at_1000() {
        assert_eq!(fmt_rate(88.1), "88.1 MB/s");
        assert_eq!(fmt_rate(1024.0), "1.0 GB/s");
    }

    #[test]
    fn axis_collapses_thousands_so_the_column_never_widens() {
        assert_eq!(fmt_axis(17151.0), "17k");
        assert_eq!(fmt_axis(88.1), "88.1");
    }

    #[test]
    fn memory_label_switches_unit_at_1024_mb() {
        assert_eq!(memory_label(612.0), "612 MB");
        assert_eq!(memory_label(2150.0), "2.1 GB");
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p viewmodel format`
Expected: FAIL — `cannot find function 'fmt' in this scope`

- [ ] **Step 4: Implement the formatters**

Prepend to `crates/viewmodel/src/format.rs`:

```rust
//! Ported verbatim from the private methods on `HostMetricsPanel` (Swift).
//! They were unreachable from tests there; here they are free functions.

/// One decimal below 100, integral above.
pub fn fmt(v: f64) -> String {
    if v >= 100.0 { format!("{}", v as i64) } else { format!("{v:.1}") }
}

/// MB/s up to 1000, then GB/s, so a burst can't widen the anchored legend.
pub fn fmt_rate(mbps: f64) -> String {
    if mbps >= 1000.0 {
        format!("{:.1} GB/s", mbps / 1024.0)
    } else {
        format!("{} MB/s", fmt(mbps))
    }
}

/// Collapses thousands to `k` so an auto-scaled axis never shifts the plot.
pub fn fmt_axis(v: f64) -> String {
    if v >= 1000.0 { format!("{:.0}k", v / 1000.0) } else { fmt(v) }
}

pub fn memory_label(mb: f64) -> String {
    if mb >= 1024.0 { format!("{} GB", fmt(mb / 1024.0)) } else { format!("{} MB", mb.round() as i64) }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p viewmodel format`
Expected: PASS, 4 tests

- [ ] **Step 6: Write the failing colour tests**

`crates/viewmodel/src/color.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_renders_six_digit_lowercase() {
        assert_eq!(hex(GREEN), "#33d17a");
        assert_eq!(hex(PANEL), "#050805");
    }

    #[test]
    fn usage_thresholds_are_70_and_90() {
        assert_eq!(usage_color(69.9), GREEN);
        assert_eq!(usage_color(70.0), AMBER);
        assert_eq!(usage_color(89.9), AMBER);
        assert_eq!(usage_color(90.0), RED);
    }

    #[test]
    fn pressure_thresholds_are_60_and_85() {
        assert_eq!(pressure_color(59.9), GREEN);
        assert_eq!(pressure_color(60.0), AMBER);
        assert_eq!(pressure_color(85.0), RED);
    }

    #[test]
    fn volumes_warn_earlier_than_cpu_because_a_full_volume_fails() {
        assert_eq!(volume_color(84.9), GREEN);
        assert_eq!(volume_color(85.0), AMBER);
        assert_eq!(volume_color(95.0), RED);
        // 88% is amber for both, but a volume at 90% is amber while a CPU is red
        assert_eq!(volume_color(90.0), AMBER);
        assert_eq!(usage_color(90.0), RED);
    }

    #[test]
    fn nominal_and_fair_both_render_green() {
        assert_eq!(thermal_badge(ThermalState::Nominal), ("Normal", GREEN));
        assert_eq!(thermal_badge(ThermalState::Fair), ("Fair", GREEN));
        assert_eq!(thermal_badge(ThermalState::Serious), ("Hot", AMBER));
        assert_eq!(thermal_badge(ThermalState::Critical), ("Critical", RED));
    }

    #[test]
    fn ten_core_hues_cycle() {
        assert_eq!(CORE_COLORS.len(), 10);
        assert_eq!(CORE_COLORS[0], CORE_COLORS[10 % CORE_COLORS.len()]);
    }
}
```

- [ ] **Step 7: Run the tests to verify they fail**

Run: `cargo test -p viewmodel color`
Expected: FAIL — `cannot find value 'GREEN' in this scope`

- [ ] **Step 8: Implement the palette and rules**

Prepend to `crates/viewmodel/src/color.rs`:

```rust
//! CockpitTheme, verbatim from `DevCanopy/Views/Cockpit/CockpitTheme.swift`,
//! plus the chart hues from `HostMetricsPanel.swift`.

pub const PANEL: u32 = 0x0005_0805;
pub const PANEL_ALT: u32 = 0x000A_0F0C;
pub const LINE: u32 = 0x0013_301F;
pub const GREEN: u32 = 0x0033_D17A;
pub const AMBER: u32 = 0x00E0_9A26;
pub const RED: u32 = 0x00E0_5A4F;
pub const MUTED: u32 = 0x005A_6B60;
pub const INK: u32 = 0x00CF_E9D8;

pub const CPU: u32 = 0x005B_8DEF;
pub const MEM: u32 = 0x00B0_66F0;
pub const GPU: u32 = 0x0033_C7C7;
pub const READ: u32 = 0x003F_B950;
pub const WRITE: u32 = 0x00E0_922A;
pub const NET: u32 = 0x005B_8DEF;
pub const NET_UP: u32 = 0x009B_D34A;

/// The 10 cycling per-core hues.
pub const CORE_COLORS: [u32; 10] = [
    0x005B_8DEF, 0x003F_B950, 0x00E0_922A, 0x00B0_66F0, 0x00E0_584F,
    0x0033_C7C7, 0x00E0_6AB0, 0x009B_D34A, 0x004F_B0E0, 0x00D0_C24A,
];

pub fn hex(c: u32) -> String {
    format!("#{:02x}{:02x}{:02x}", (c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF)
}

pub fn usage_color(v: f64) -> u32 {
    if v < 70.0 { GREEN } else if v < 90.0 { AMBER } else { RED }
}

pub fn pressure_color(v: f64) -> u32 {
    if v < 60.0 { GREEN } else if v < 85.0 { AMBER } else { RED }
}

/// Volumes warn earlier than CPU — a full volume fails outright.
pub fn volume_color(pct: f64) -> u32 {
    if pct < 85.0 { GREEN } else if pct < 95.0 { AMBER } else { RED }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThermalState { Nominal, Fair, Serious, Critical }

impl ThermalState {
    /// The agent sends `thermalState` as an integer.
    pub fn from_wire(v: i64) -> Self {
        match v {
            0 => ThermalState::Nominal,
            1 => ThermalState::Fair,
            2 => ThermalState::Serious,
            _ => ThermalState::Critical,
        }
    }
}

/// Mirrors `thermalBadge`. Nominal and Fair are both green by design.
pub fn thermal_badge(s: ThermalState) -> (&'static str, u32) {
    match s {
        ThermalState::Nominal => ("Normal", GREEN),
        ThermalState::Fair => ("Fair", GREEN),
        ThermalState::Serious => ("Hot", AMBER),
        ThermalState::Critical => ("Critical", RED),
    }
}
```

- [ ] **Step 9: Run the tests to verify they pass**

Run: `cargo test -p viewmodel`
Expected: PASS, 10 tests

- [ ] **Step 10: Commit**

```bash
git add Cargo.toml crates/viewmodel
git commit -m "feat(viewmodel): extract formatters and colour rules from HostMetricsPanel

These were private methods on a SwiftUI View, reachable only through the
view. As free functions they are unit-testable with no renderer linked."
```

---

### Task 2: Layout policy

**Files:**
- Modify: `crates/viewmodel/src/layout.rs`

**Interfaces:**
- Consumes: nothing
- Produces: `viewmodel::layout::{CORE_ROW_UNIT, CORE_ROW_SPAN_DEFAULT, CORE_GAP, CORE_MIN_CELL, HISTORY_CAPACITY, PX_PER_SAMPLE}` (all `f64` except `CORE_ROW_SPAN_DEFAULT: usize` and `HISTORY_CAPACITY: usize`); `core_column_ladder(count: usize, min_cell: f64, gap: f64) -> Vec<(f64, usize)>`; `core_block_height(row_span: usize) -> f64`; `core_cell_height(block: f64, rows: usize, gap: f64) -> f64`; `core_visual_rows(count: usize, cols: usize) -> usize`; `visible_samples(width_px: f64, px_per_sample: f64, retained: usize) -> usize`

- [ ] **Step 1: Write the failing tests**

`crates/viewmodel/src/layout.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_only_yields_column_counts_that_leave_a_full_last_row() {
        for count in [4usize, 8, 10, 12, 16, 20, 24, 32, 64] {
            for (_, cols) in core_column_ladder(count, CORE_MIN_CELL, CORE_GAP) {
                assert_eq!(count % cols, 0, "count {count} cols {cols}");
            }
        }
    }

    #[test]
    fn ladder_for_16_cores_matches_the_documented_rungs() {
        let l = core_column_ladder(16, 104.0, 8.0);
        assert_eq!(l, vec![(104.0, 1), (216.0, 2), (440.0, 4), (888.0, 8), (1784.0, 16)]);
    }

    #[test]
    fn block_height_is_fixed_regardless_of_core_count() {
        let h = core_block_height(CORE_ROW_SPAN_DEFAULT);
        assert_eq!(h, 220.0);
        for count in [4usize, 8, 16, 32, 64] {
            let cols = core_column_ladder(count, CORE_MIN_CELL, CORE_GAP)
                .into_iter().filter(|(w, _)| *w <= 940.0).map(|(_, c)| c).max().unwrap_or(1);
            let rows = core_visual_rows(count, cols);
            let cell = core_cell_height(h, rows, CORE_GAP);
            let total = cell * rows as f64 + CORE_GAP * (rows - 1) as f64;
            assert!((total - h).abs() < 1e-9, "count {count}: block drifted to {total}");
        }
    }

    #[test]
    fn cell_height_matches_the_swift_arithmetic() {
        let h = core_block_height(2);
        assert_eq!(core_cell_height(h, 1, CORE_GAP), 220.0);
        assert_eq!(core_cell_height(h, 2, CORE_GAP), 106.0);
        assert_eq!(core_cell_height(h, 4, CORE_GAP), 49.0);
    }

    #[test]
    fn wider_charts_show_more_time_not_stretched_pixels() {
        let narrow = visible_samples(400.0, PX_PER_SAMPLE, HISTORY_CAPACITY);
        let wide = visible_samples(1600.0, PX_PER_SAMPLE, HISTORY_CAPACITY);
        assert_eq!(narrow, 100);
        assert_eq!(wide, 400);
        assert!((400.0 / narrow as f64 - 1600.0 / wide as f64).abs() < 1e-9);
    }

    #[test]
    fn visible_window_is_clamped_to_the_buffer() {
        assert_eq!(visible_samples(999_999.0, PX_PER_SAMPLE, HISTORY_CAPACITY), HISTORY_CAPACITY);
        assert_eq!(visible_samples(0.0, PX_PER_SAMPLE, HISTORY_CAPACITY), 2);
    }

    #[test]
    fn zero_cores_does_not_panic() {
        assert!(core_column_ladder(0, CORE_MIN_CELL, CORE_GAP).is_empty());
        assert_eq!(core_visual_rows(0, 0), 1);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p viewmodel layout`
Expected: FAIL — `cannot find function 'core_column_ladder' in this scope`

- [ ] **Step 3: Implement the layout policy**

Prepend to `crates/viewmodel/src/layout.rs`:

```rust
//! Layout policy. Values, not rendering — the shell applies these via CSS.
//!
//! Ports `coreColumns` and the `coreRowSpan * coreRowUnit` block arithmetic
//! from `HostMetricsPanel.swift`, and generalises the first: instead of one
//! column count for all widths, a ladder of every count that divides the core
//! count evenly, so the last row is never an orphan at any width.

/// Height of one cockpit "section row" (`HostMetricsPanel.coreRowUnit`).
pub const CORE_ROW_UNIT: f64 = 110.0;
/// Default `@AppStorage("coreRowSpan")`.
pub const CORE_ROW_SPAN_DEFAULT: usize = 2;
pub const CORE_GAP: f64 = 8.0;
/// Narrowest legible core cell; below this the `Core N xx%` label truncates.
pub const CORE_MIN_CELL: f64 = 104.0;

/// Samples retained. Deliberately larger than any chart can show, so widening
/// a chart reveals more history rather than stretching the same samples.
pub const HISTORY_CAPACITY: usize = 600;
/// On-screen width of one sample, held constant at every chart width.
pub const PX_PER_SAMPLE: f64 = 4.0;

/// Every column count that divides `count` evenly, with the container width
/// each needs. Ascending by width.
pub fn core_column_ladder(count: usize, min_cell: f64, gap: f64) -> Vec<(f64, usize)> {
    if count == 0 {
        return vec![];
    }
    let mut l: Vec<(f64, usize)> = (1..=count)
        .filter(|d| count % d == 0)
        .map(|d| (d as f64 * min_cell + (d.saturating_sub(1)) as f64 * gap, d))
        .collect();
    l.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    l
}

/// The cores block reserves a fixed height regardless of core count, so host
/// cards line up in the cockpit grid. Cells divide it; the block never grows.
pub fn core_block_height(row_span: usize) -> f64 {
    row_span.max(1) as f64 * CORE_ROW_UNIT
}

pub fn core_cell_height(block_height: f64, rows: usize, gap: f64) -> f64 {
    let rows = rows.max(1);
    (block_height - gap * (rows - 1) as f64) / rows as f64
}

pub fn core_visual_rows(count: usize, cols: usize) -> usize {
    if cols == 0 { return 1; }
    count.div_ceil(cols).max(1)
}

/// How many retained samples fit `width_px` at fixed density.
pub fn visible_samples(width_px: f64, px_per_sample: f64, retained: usize) -> usize {
    if px_per_sample <= 0.0 || width_px <= 0.0 {
        return retained.min(2);
    }
    ((width_px / px_per_sample).floor().max(2.0) as usize).min(retained)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p viewmodel`
Expected: PASS, 17 tests

- [ ] **Step 5: Commit**

```bash
git add crates/viewmodel/src/layout.rs
git commit -m "feat(viewmodel): port core-grid and chart layout policy

core_column_ladder generalises coreColumns: every rung divides the core count
evenly, so the last row is never an orphan at any width. core_block_height
ports coreRowSpan * coreRowUnit so cards keep a fixed cores section."
```

---

### Task 3: Agent wire types

**Files:**
- Create: `crates/metrics/Cargo.toml`
- Create: `crates/metrics/src/lib.rs`
- Create: `crates/metrics/tests/fixtures/snapshot.json`
- Create: `crates/metrics/tests/wire.rs`

**Interfaces:**
- Consumes: nothing
- Produces: `metrics::{Snapshot, Cpu, Memory, Disk, Network, Gpu, Battery, Volume, Process}`, all `Deserialize + Serialize + Clone + Debug`. Field names are Rust snake_case with `#[serde(rename)]` to the agent's camelCase. `Snapshot { timestamp: String, cpu, memory, disk, network, gpu, battery: Option<Battery>, volumes: Vec<Volume>, processes: Vec<Process> }`. `Volume::percent_used(&self) -> f64`.

- [ ] **Step 1: Create the crate and capture a fixture**

`crates/metrics/Cargo.toml`:

```toml
[package]
name = "metrics"
version = "0.1.0"
edition.workspace = true

[dependencies]
serde = { workspace = true }

[dev-dependencies]
serde_json = { workspace = true }
```

`crates/metrics/tests/fixtures/snapshot.json` — the agent's exact wire format. Field names are copied from the `#[serde(rename)]` attributes in `agent/src/metrics.rs`; do not invent names:

```json
{
  "timestamp": "2026-07-27T14:03:12Z",
  "cpu": {
    "totalUsage": 34.2,
    "coreUsages": [12.0, 88.0, 9.0, 94.0, 21.0, 17.0, 73.0, 11.0,
                   6.0, 19.0, 91.0, 14.0, 8.0, 62.0, 13.0, 7.0],
    "model": "AMD Ryzen 9 7950X 16-Core Processor",
    "thermalState": 0
  },
  "memory": { "usedGB": 18.2, "totalGB": 62.7, "swapUsedGB": 0.4, "pressure": 22.0 },
  "disk": { "readMBps": 12.4, "writeMBps": 88.1 },
  "network": { "downloadMBps": 2.1, "uploadMBps": 0.4 },
  "gpu": { "usage": 0.0, "vramUsedGB": 0.0, "vramTotalGB": 0.0 },
  "battery": null,
  "volumes": [
    { "mount": "/", "usedGB": 412.0, "totalGB": 916.0, "fstype": "ext4" },
    { "mount": "/mnt/data", "usedGB": 1843.0, "totalGB": 3686.0, "fstype": "xfs" },
    { "mount": "/boot", "usedGB": 1.1, "totalGB": 1.2, "fstype": "ext2" }
  ],
  "processes": [
    { "pid": 4411, "name": "cargo", "cpuPercent": 184.0, "memoryMB": 2150.0 },
    { "pid": 4418, "name": "rustc", "cpuPercent": 97.0, "memoryMB": 1430.0 },
    { "pid": 991,  "name": "podman", "cpuPercent": 31.0, "memoryMB": 612.0 }
  ]
}
```

- [ ] **Step 2: Write the failing tests**

`crates/metrics/tests/wire.rs`:

```rust
use metrics::Snapshot;

const FIXTURE: &str = include_str!("fixtures/snapshot.json");

#[test]
fn deserialises_the_agents_wire_format() {
    let s: Snapshot = serde_json::from_str(FIXTURE).expect("agent JSON must deserialise");
    assert_eq!(s.cpu.core_usages.len(), 16);
    assert_eq!(s.cpu.total_usage, 34.2);
    assert_eq!(s.memory.total_gb, 62.7);
    assert_eq!(s.disk.write_mbps, 88.1);
    assert_eq!(s.network.download_mbps, 2.1);
    assert_eq!(s.volumes.len(), 3);
    assert_eq!(s.processes[0].name, "cargo");
}

#[test]
fn absent_battery_is_none_not_a_default() {
    let s: Snapshot = serde_json::from_str(FIXTURE).unwrap();
    assert!(s.battery.is_none(), "a host with no battery must be None, never a zeroed Battery");
}

#[test]
fn percent_used_guards_against_a_zero_total() {
    let s: Snapshot = serde_json::from_str(FIXTURE).unwrap();
    let root = s.volumes.iter().find(|v| v.mount == "/").unwrap();
    assert!((root.percent_used() - 44.978).abs() < 0.01);

    let empty = metrics::Volume {
        mount: "/x".into(), used_gb: 1.0, total_gb: 0.0, fstype: None,
    };
    assert_eq!(empty.percent_used(), 0.0, "must not divide by zero");
}

#[test]
fn round_trips_so_the_app_and_agent_cannot_drift() {
    let s: Snapshot = serde_json::from_str(FIXTURE).unwrap();
    let out = serde_json::to_string(&s).unwrap();
    let again: Snapshot = serde_json::from_str(&out).unwrap();
    assert_eq!(s.cpu.core_usages, again.cpu.core_usages);
    assert_eq!(s.volumes.len(), again.volumes.len());
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p metrics`
Expected: FAIL — `unresolved import 'metrics::Snapshot'`

- [ ] **Step 4: Implement the wire types**

`crates/metrics/src/lib.rs`:

```rust
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
    #[serde(default)]
    pub fstype: Option<String>,
}

impl Volume {
    pub fn percent_used(&self) -> f64 {
        if self.total_gb <= 0.0 { 0.0 } else { self.used_gb / self.total_gb * 100.0 }
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
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p metrics`
Expected: PASS, 4 tests

- [ ] **Step 6: Verify the fixture against the real agent**

The fixture is only useful if it matches production. Fetch a live snapshot and confirm the same types deserialise it:

```bash
curl -s -H "Authorization: Bearer $DEVCANOPY_AGENT_TOKEN" \
  http://100.87.202.125:7878/v1/snapshot > /tmp/live-snapshot.json
cp /tmp/live-snapshot.json crates/metrics/tests/fixtures/snapshot-live.json
```

Add to `crates/metrics/tests/wire.rs`:

```rust
/// Guards against the committed fixture drifting from what the agent sends.
/// Skipped when the live capture is absent so CI stays hermetic.
#[test]
fn live_capture_deserialises_when_present() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/snapshot-live.json");
    let Ok(raw) = std::fs::read_to_string(path) else { return };
    let s: Snapshot = serde_json::from_str(&raw).expect("live agent JSON must deserialise");
    assert!(!s.cpu.core_usages.is_empty());
}
```

Run: `cargo test -p metrics`
Expected: PASS, 5 tests. If `live_capture_deserialises_when_present` fails, the committed fixture is wrong — fix the types, not the test.

- [ ] **Step 7: Commit**

```bash
git add crates/metrics
git commit -m "feat(metrics): own the agent's JSON contract in one place

The Swift app defines these types a second time, which is why
HostMetricsError.decodeFailed exists. One Serialize+Deserialize definition
removes the drift by construction. Fixture verified against the live agent."
```

---

### Task 4: History buffer and host-card view-model

**Files:**
- Modify: `crates/viewmodel/src/history.rs`
- Modify: `crates/viewmodel/src/card.rs`

**Interfaces:**
- Consumes: `metrics::Snapshot`; `viewmodel::{format::*, color::*, layout::*}`
- Produces: `viewmodel::history::History` with `History::new(capacity: usize)`, `push(&mut self, v: f64)`, `values(&self) -> &[f64]`, `len(&self)`; `viewmodel::card::HostHistories` with `HostHistories::new()`, `record(&mut self, s: &metrics::Snapshot)`; `viewmodel::card::host_card(host_name: &str, s: &metrics::Snapshot, h: &HostHistories) -> serde_json::Value`

- [ ] **Step 1: Write the failing history tests**

`crates/viewmodel/src/history.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_most_recent_samples_and_drops_the_oldest() {
        let mut h = History::new(3);
        for v in [1.0, 2.0, 3.0, 4.0] { h.push(v); }
        assert_eq!(h.values(), &[2.0, 3.0, 4.0]);
    }

    #[test]
    fn starts_empty_and_grows_to_capacity() {
        let mut h = History::new(4);
        assert_eq!(h.len(), 0);
        h.push(1.0);
        assert_eq!(h.len(), 1);
        for v in [2.0, 3.0, 4.0, 5.0] { h.push(v); }
        assert_eq!(h.len(), 4);
    }

    #[test]
    fn zero_capacity_never_panics() {
        let mut h = History::new(0);
        h.push(1.0);
        assert_eq!(h.len(), 0);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p viewmodel history`
Expected: FAIL — `cannot find type 'History' in this scope`

- [ ] **Step 3: Implement the ring buffer**

Prepend to `crates/viewmodel/src/history.rs`:

```rust
//! Fixed-capacity sample history. Ordered oldest to newest so a renderer can
//! take the tail without reversing.

#[derive(Debug, Clone)]
pub struct History {
    capacity: usize,
    values: Vec<f64>,
}

impl History {
    pub fn new(capacity: usize) -> Self {
        Self { capacity, values: Vec::with_capacity(capacity) }
    }

    pub fn push(&mut self, v: f64) {
        if self.capacity == 0 { return; }
        if self.values.len() == self.capacity {
            self.values.remove(0);
        }
        self.values.push(v);
    }

    pub fn values(&self) -> &[f64] { &self.values }
    pub fn len(&self) -> usize { self.values.len() }
    pub fn is_empty(&self) -> bool { self.values.is_empty() }
}

impl Default for History {
    fn default() -> Self { History::new(crate::layout::HISTORY_CAPACITY) }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p viewmodel history`
Expected: PASS, 3 tests

- [ ] **Step 5: Write the failing card tests**

`crates/viewmodel/src/card.rs`:

```rust
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
        let vm = host_card("ubu-3xdv", &s, &h);
        assert_eq!(vm["gpuValue"], "—");
        assert_eq!(vm["vramText"], "VRAM: —");
    }

    #[test]
    fn a_host_with_a_gpu_renders_the_percentage() {
        let mut s = fixture();
        s.gpu.usage = 41.0;
        s.gpu.vram_used_gb = 3.5;
        s.gpu.vram_total_gb = 24.0;
        let vm = host_card("m4", &s, &HostHistories::new());
        assert_eq!(vm["gpuValue"], "41%");
        assert_eq!(vm["vramText"], "VRAM: 3.5 / 24.0 GB");
    }

    #[test]
    fn volumes_are_ordered_fullest_first() {
        let vm = host_card("ubu-3xdv", &fixture(), &HostHistories::new());
        let mounts: Vec<&str> = vm["volumes"].as_array().unwrap()
            .iter().map(|v| v["mount"].as_str().unwrap()).collect();
        assert_eq!(mounts, vec!["/boot", "/mnt/data", "/"]);
    }

    #[test]
    fn processes_are_ranked_separately_for_cpu_and_ram() {
        let vm = host_card("ubu-3xdv", &fixture(), &HostHistories::new());
        assert_eq!(vm["topCpu"][0]["name"], "cargo");
        assert_eq!(vm["topCpu"][0]["value"], "184%");
        assert_eq!(vm["topRam"][0]["value"], "2.1 GB");
    }

    #[test]
    fn the_ladder_and_block_height_travel_with_the_view_model() {
        let vm = host_card("ubu-3xdv", &fixture(), &HostHistories::new());
        assert_eq!(vm["coreBlockHeight"], 220.0);
        let rungs = vm["coreLadder"].as_array().unwrap();
        assert_eq!(rungs.len(), 5);
        assert_eq!(rungs[3]["cols"], 8);
        assert_eq!(rungs[3]["minWidth"], 888.0);
    }

    #[test]
    fn every_core_gets_a_hue_and_a_usage_coloured_value() {
        let vm = host_card("ubu-3xdv", &fixture(), &HostHistories::new());
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
}
```

- [ ] **Step 6: Run to verify failure**

Run: `cargo test -p viewmodel card`
Expected: FAIL — `cannot find function 'host_card' in this scope`

- [ ] **Step 7: Implement the view-model assembly**

Prepend to `crates/viewmodel/src/card.rs`:

```rust
//! Assembles the finished host-card view-model.
//!
//! Every string and every colour is decided here so the shell only paints.
//! That is what keeps the frontend small and the logic testable.

use crate::color::{self, ThermalState};
use crate::format::{fmt, fmt_axis, fmt_rate, memory_label};
use crate::history::History;
use crate::layout::{
    core_block_height, core_column_ladder, CORE_GAP, CORE_MIN_CELL, CORE_ROW_SPAN_DEFAULT,
    HISTORY_CAPACITY, PX_PER_SAMPLE,
};
use serde_json::{json, Value};

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
    pub fn new() -> Self { Self::default() }

    pub fn record(&mut self, s: &metrics::Snapshot) {
        self.cpu.push(s.cpu.total_usage);
        let mem_pct = if s.memory.total_gb > 0.0 {
            s.memory.used_gb / s.memory.total_gb * 100.0
        } else { 0.0 };
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

pub fn host_card(host_name: &str, s: &metrics::Snapshot, h: &HostHistories) -> Value {
    let (badge, badge_col) = color::thermal_badge(ThermalState::from_wire(s.cpu.thermal_state));

    let cores: Vec<Value> = s.cpu.core_usages.iter().enumerate().map(|(i, v)| {
        json!({
            "label": format!("Core {i}"),
            "value": format!("{}%", v.round() as i64),
            "valueColor": color::hex(color::usage_color(*v)),
            "hue": color::hex(color::CORE_COLORS[i % color::CORE_COLORS.len()]),
            "history": h.cores.get(i).map(|c| c.values()).unwrap_or(&[]),
        })
    }).collect();

    let mut vols: Vec<&metrics::Volume> = s.volumes.iter().collect();
    vols.sort_by(|a, b| b.percent_used().partial_cmp(&a.percent_used()).unwrap());
    let volumes: Vec<Value> = vols.iter().map(|v| {
        let pct = v.percent_used();
        json!({
            "mount": v.mount,
            "detail": format!("{} / {} GB · {}%", fmt(v.used_gb), fmt(v.total_gb), pct.round() as i64),
            "tint": color::hex(color::volume_color(pct)),
            "fraction": pct.clamp(0.0, 100.0) / 100.0,
        })
    }).collect();

    let mut by_cpu: Vec<&metrics::Process> = s.processes.iter().collect();
    by_cpu.sort_by(|a, b| b.cpu_percent.partial_cmp(&a.cpu_percent).unwrap());
    let mut by_mem: Vec<&metrics::Process> = s.processes.iter().collect();
    by_mem.sort_by(|a, b| b.memory_mb.partial_cmp(&a.memory_mb).unwrap());

    let disk_max = h.disk_read.values().iter().chain(h.disk_write.values())
        .cloned().fold(0.1f64, f64::max);
    let net_max = h.net_down.values().iter().chain(h.net_up.values())
        .cloned().fold(0.1f64, f64::max);

    let (gpu_value, gpu_color, vram) = if has_gpu(s) {
        (format!("{}%", s.gpu.usage.round() as i64),
         color::hex(color::GPU),
         format!("VRAM: {} / {} GB", fmt(s.gpu.vram_used_gb), fmt(s.gpu.vram_total_gb)))
    } else {
        ("—".to_string(), color::hex(color::MUTED), "VRAM: —".to_string())
    };

    json!({
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
    })
}
```

Add `serde_json` to `crates/viewmodel/Cargo.toml`:

```toml
serde_json = { workspace = true }
```

- [ ] **Step 8: Run to verify pass**

Run: `cargo test -p viewmodel`
Expected: PASS, 27 tests

- [ ] **Step 9: Commit**

```bash
git add crates/viewmodel
git commit -m "feat(viewmodel): assemble the host-card view-model

Every string and colour is decided in Rust so the shell only paints. A host
with no discrete adapter renders an em dash, asserted by test — never a
fabricated 0%."
```

---

### Task 5: Agent client

**Files:**
- Create: `crates/agentclient/Cargo.toml`
- Create: `crates/agentclient/src/lib.rs`

**Interfaces:**
- Consumes: `metrics::Snapshot`
- Produces: `agentclient::{AgentClient, AgentError}`. `AgentClient::new(base_url: impl Into<String>, token: impl Into<String>) -> Self`; `async fn snapshot(&self) -> Result<metrics::Snapshot, AgentError>`. `AgentError` variants: `Unreachable(String)`, `AuthFailed`, `HttpStatus(u16)`, `DecodeFailed(String)`, each with a `user_message(&self) -> &'static str`.

- [ ] **Step 1: Write the failing tests**

`crates/agentclient/Cargo.toml`:

```toml
[package]
name = "agentclient"
version = "0.1.0"
edition.workspace = true

[dependencies]
metrics = { path = "../metrics" }
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
thiserror = "2"

[dev-dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
wiremock = "0.6"
```

`crates/agentclient/src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const FIXTURE: &str = include_str!("../../metrics/tests/fixtures/snapshot.json");

    #[tokio::test]
    async fn sends_a_bearer_token_and_decodes_the_snapshot() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/snapshot"))
            .and(header("authorization", "Bearer s3cret"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(FIXTURE, "application/json"))
            .mount(&server).await;

        let c = AgentClient::new(server.uri(), "s3cret");
        let snap = c.snapshot().await.expect("should decode");
        assert_eq!(snap.cpu.core_usages.len(), 16);
    }

    #[tokio::test]
    async fn a_401_is_auth_failed_not_a_generic_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/v1/snapshot"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server).await;

        let err = AgentClient::new(server.uri(), "wrong").snapshot().await.unwrap_err();
        assert!(matches!(err, AgentError::AuthFailed));
        assert!(err.user_message().contains("token"));
    }

    #[tokio::test]
    async fn a_500_is_reported_with_its_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/v1/snapshot"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server).await;

        let err = AgentClient::new(server.uri(), "t").snapshot().await.unwrap_err();
        assert!(matches!(err, AgentError::HttpStatus(503)));
    }

    #[tokio::test]
    async fn malformed_json_is_decode_failed_so_skew_is_diagnosable() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/v1/snapshot"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("{\"cpu\":1}", "application/json"))
            .mount(&server).await;

        let err = AgentClient::new(server.uri(), "t").snapshot().await.unwrap_err();
        assert!(matches!(err, AgentError::DecodeFailed(_)));
        assert!(err.user_message().contains("version skew"));
    }

    #[tokio::test]
    async fn an_unroutable_host_is_unreachable() {
        let c = AgentClient::new("http://127.0.0.1:1", "t");
        let err = c.snapshot().await.unwrap_err();
        assert!(matches!(err, AgentError::Unreachable(_)));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p agentclient`
Expected: FAIL — `cannot find type 'AgentClient' in this scope`

- [ ] **Step 3: Implement the client**

Prepend to `crates/agentclient/src/lib.rs`:

```rust
//! Polls a DevCanopy agent over HTTP. Replaces
//! `Services/HostMetrics/RemoteHostMetricsService.swift`.
//!
//! The error variants mirror the Swift `failureTooltip` cases so the shell can
//! keep giving cause-specific guidance instead of a generic failure.

use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("unreachable: {0}")]
    Unreachable(String),
    #[error("agent rejected the token")]
    AuthFailed,
    #[error("agent returned HTTP {0}")]
    HttpStatus(u16),
    #[error("could not decode the agent payload: {0}")]
    DecodeFailed(String),
}

impl AgentError {
    /// Cause-specific guidance, so the operator chases the right layer.
    pub fn user_message(&self) -> &'static str {
        match self {
            AgentError::Unreachable(_) =>
                "Couldn't reach the agent. Check the host is up and the agent is running.",
            AgentError::AuthFailed =>
                "Agent rejected the bearer token (401). Check the host's token in Settings.",
            AgentError::HttpStatus(_) =>
                "The agent responded with an error status.",
            AgentError::DecodeFailed(_) =>
                "Agent responded but the payload didn't decode — likely agent/app version skew after a redeploy.",
        }
    }
}

pub struct AgentClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl AgentClient {
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token: token.into(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("reqwest client"),
        }
    }

    pub async fn snapshot(&self) -> Result<metrics::Snapshot, AgentError> {
        let url = format!("{}/v1/snapshot", self.base_url);
        let resp = self.http.get(&url)
            .bearer_auth(&self.token)
            .send().await
            .map_err(|e| AgentError::Unreachable(e.to_string()))?;

        match resp.status().as_u16() {
            200 => {}
            401 | 403 => return Err(AgentError::AuthFailed),
            other => return Err(AgentError::HttpStatus(other)),
        }

        let body = resp.text().await.map_err(|e| AgentError::Unreachable(e.to_string()))?;
        serde_json::from_str(&body).map_err(|e| AgentError::DecodeFailed(e.to_string()))
    }
}
```

Add `serde_json = { workspace = true }` to `crates/agentclient/Cargo.toml` dependencies.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p agentclient`
Expected: PASS, 5 tests

- [ ] **Step 5: Commit**

```bash
git add crates/agentclient
git commit -m "feat(agentclient): typed agent polling with cause-specific errors

Error variants mirror the Swift failureTooltip cases so the shell keeps
telling the operator which layer to chase, not just that something failed."
```

---

### Task 6: Tauri shell rendering a fixture

**Files:**
- Create: `app/src-tauri/Cargo.toml`
- Create: `app/src-tauri/build.rs`
- Create: `app/src-tauri/tauri.conf.json`
- Create: `app/src-tauri/capabilities/default.json`
- Create: `app/src-tauri/src/main.rs`
- Create: `app/src-tauri/icons/icon.png`
- Create: `app/ui/index.html`
- Create: `app/ui/app.css`
- Create: `app/ui/app.js`

**Interfaces:**
- Consumes: `viewmodel::card::{host_card, HostHistories}`, `metrics::Snapshot`
- Produces: `#[tauri::command] snapshot() -> serde_json::Value`; binary flag `--dump <path>` writing the same view-model for browser-based tests

- [ ] **Step 1: Scaffold the Tauri crate**

`app/src-tauri/Cargo.toml`:

```toml
[package]
name = "devcanopy-app"
version = "0.1.0"
edition.workspace = true

[build-dependencies]
tauri-build = "2"

[dependencies]
metrics = { path = "../../crates/metrics" }
viewmodel = { path = "../../crates/viewmodel" }
tauri = "2"
serde_json = { workspace = true }
```

`app/src-tauri/build.rs`:

```rust
fn main() { tauri_build::build() }
```

`app/src-tauri/tauri.conf.json` — note the real CSP; `null` must never be committed:

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "DevCanopy",
  "version": "0.1.0",
  "identifier": "com.sassydog.devcanopy.app",
  "build": { "frontendDist": "../ui" },
  "app": {
    "withGlobalTauri": true,
    "windows": [
      { "title": "DevCanopy", "width": 1000, "height": 1120, "resizable": true, "minWidth": 200, "minHeight": 400 }
    ],
    "security": {
      "csp": "default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'"
    }
  },
  "bundle": { "active": false }
}
```

Generate the required RGBA icon:

```bash
mkdir -p app/src-tauri/icons
python3 - <<'PY'
import zlib, struct
W = H = 512
fg, bg = (0x33,0xD1,0x7A,0xFF), (0x05,0x08,0x05,0xFF)
rows = b""
for y in range(H):
    row = b"\x00"
    for x in range(W):
        ring = (96 <= x < 416 and 96 <= y < 416) and not (128 <= x < 384 and 128 <= y < 384)
        row += bytes(fg if ring else bg)
    rows += row
def chunk(t, d):
    c = t + d
    return struct.pack(">I", len(d)) + c + struct.pack(">I", zlib.crc32(c) & 0xFFFFFFFF)
png = (b"\x89PNG\r\n\x1a\n"
       + chunk(b"IHDR", struct.pack(">IIBBBBB", W, H, 8, 6, 0, 0, 0))
       + chunk(b"IDAT", zlib.compress(rows, 9)) + chunk(b"IEND", b""))
open("app/src-tauri/icons/icon.png","wb").write(png)
PY
```

- [ ] **Step 2: Restrict the capability allowlist**

The spec requires the window to reach only the commands it actually uses, and no
`fs`, `shell` or `http` plugin. Tauri v2 expresses that as a capability file.

`app/src-tauri/capabilities/default.json`:

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "description": "The cockpit window may call the read-only snapshot command and nothing else.",
  "windows": ["main"],
  "permissions": ["core:default"]
}
```

Set the window's label so the capability binds to it — in `tauri.conf.json`, add
`"label": "main"` to the window object:

```json
{ "label": "main", "title": "DevCanopy", "width": 1000, "height": 1120, "resizable": true, "minWidth": 200, "minHeight": 400 }
```

No `fs`, `shell`, `http` or `dialog` plugin is added anywhere in this plan. Adding one
later is a security review checkpoint, not a routine dependency bump.

- [ ] **Step 3: Write the Tauri bin**

`app/src-tauri/src/main.rs`:

```rust
//! DevCanopy shell. The frontend receives a finished view-model and paints;
//! all logic lives in `viewmodel`.

use serde_json::Value;
use viewmodel::card::{host_card, HostHistories};

const FIXTURE: &str = include_str!("../../../crates/metrics/tests/fixtures/snapshot.json");

/// Task 7 replaces this with live polling.
fn current_view_model() -> Value {
    let snap: metrics::Snapshot = serde_json::from_str(FIXTURE).expect("fixture");
    let mut h = HostHistories::new();
    // seed enough history that the charts have something to draw
    for _ in 0..120 { h.record(&snap); }
    host_card("ubu-3xdv", &snap, &h)
}

#[tauri::command]
fn snapshot() -> Value { current_view_model() }

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--dump") {
        let path = args.get(i + 1).cloned().unwrap_or_else(|| "sample.json".into());
        std::fs::write(&path, serde_json::to_string_pretty(&current_view_model()).unwrap())
            .expect("write view-model");
        println!("wrote {path}");
        return;
    }

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![snapshot])
        .run(tauri::generate_context!())
        .expect("failed to start");
}
```

- [ ] **Step 4: Write the frontend**

`app/ui/index.html`:

```html
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<title>DevCanopy</title>
<link rel="stylesheet" href="app.css" />
</head>
<body>
  <div class="card">
    <header class="hdr"><span class="dot"></span><span class="host" id="hostName"></span></header>
    <div class="sec-hdr">
      <i class="glyph"></i><span class="title" id="cpuModel"></span>
      <span class="badge" id="thermal"></span><span class="grow"></span>
      <span class="big" id="cpuValue"></span>
    </div>
    <div class="chart">
      <div class="axis"><span>100%</span><span>50%</span><span>0%</span></div>
      <div class="plot" id="cpuChart" data-h="110"></div>
    </div>
    <div class="cores-wrap"><div class="cores" id="cores"></div></div>
    <hr />
    <div class="cols2">
      <section>
        <div class="sec-hdr"><i class="glyph"></i><span class="title">Memory</span>
          <span class="grow"></span><span class="big" id="memValue"></span></div>
        <div class="chart"><div class="axis"><span>100%</span><span>50%</span><span>0%</span></div>
          <div class="plot" id="memChart" data-h="90"></div></div>
        <div class="foot"><span id="swapText"></span><span class="grow"></span><span id="pressureText"></span></div>
      </section>
      <section>
        <div class="sec-hdr"><i class="glyph"></i><span class="title">Graphics</span>
          <span class="grow"></span><span class="big" id="gpuValue"></span></div>
        <div class="chart"><div class="axis"><span>100%</span><span>50%</span><span>0%</span></div>
          <div class="plot" id="gpuChart" data-h="90"></div></div>
        <div class="foot"><span id="vramText"></span></div>
      </section>
    </div>
    <hr />
    <div class="cols2">
      <section>
        <div class="sec-hdr"><i class="glyph"></i><span class="title">Disk I/O</span><span class="grow"></span>
          <div class="legends">
            <span class="lg"><i style="background:var(--read)"></i>Read:<b id="diskRead"></b></span>
            <span class="lg"><i style="background:var(--write)"></i>Write:<b id="diskWrite"></b></span>
          </div></div>
        <div class="chart"><div class="axis two"><span id="diskAxis"></span><span>0</span></div>
          <div class="plot" id="diskChart" data-h="90"></div></div>
      </section>
      <section>
        <div class="sec-hdr"><i class="glyph"></i><span class="title">Network I/O</span><span class="grow"></span>
          <div class="legends">
            <span class="lg"><i style="background:var(--net)"></i>Down:<b id="netDown"></b></span>
            <span class="lg"><i style="background:var(--netup)"></i>Up:<b id="netUp"></b></span>
          </div></div>
        <div class="chart"><div class="axis two"><span id="netAxis"></span><span>0</span></div>
          <div class="plot" id="netChart" data-h="90"></div></div>
      </section>
    </div>
    <hr />
    <div class="sec-hdr"><i class="glyph"></i><span class="title">Volumes</span>
      <span class="grow"></span><span class="big muted" id="volumeCount"></span></div>
    <div class="vols" id="volumes"></div>
    <hr />
    <div class="cols2">
      <section><div class="lbl">TOP CPU</div><div class="procs" id="topCpu"></div></section>
      <section><div class="lbl">TOP RAM</div><div class="procs" id="topRam"></div></section>
    </div>
  </div>
  <script src="app.js"></script>
</body>
</html>
```

`app/ui/app.css`:

```css
:root {
  --panel:#050805; --panelAlt:#0a0f0c; --line:#13301f; --green:#33d17a;
  --muted:#5a6b60; --ink:#cfe9d8; --read:#3fb950; --write:#e0922a;
  --net:#5b8def; --netup:#9bd34a;
  --mono: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
}
* { box-sizing: border-box; }
html, body {
  margin:0; background:#000; color:var(--ink); font-family:var(--mono);
  font-variant-numeric: tabular-nums; font-size:12px; line-height:1.45;
  -webkit-font-smoothing: antialiased;
}
.card { margin:12px; padding:18px; background:var(--panel); border:1px solid var(--line);
  border-radius:12px; display:flex; flex-direction:column; gap:10px; }
.hdr { display:flex; align-items:center; gap:8px; }
.dot { width:8px; height:8px; border-radius:50%; background:var(--green); }
.host { font-size:14px; font-weight:700; }
.grow { flex:1; } .muted { color:var(--muted); }
/* SF Symbols have no web equivalent; a filled square stands in until an icon
   font is bundled (tracked separately). */
.glyph { width:7px; height:7px; border-radius:1px; background:var(--green);
  display:inline-block; flex:none; }
.sec-hdr { display:flex; align-items:center; gap:8px; min-width:0; }
.sec-hdr .title { font-size:13px; font-weight:700; white-space:nowrap;
  overflow:hidden; text-overflow:ellipsis; }
.big { font-size:18px; font-weight:700; white-space:nowrap; }
.badge { font-size:10px; font-weight:700; padding:2px 7px; border-radius:8px;
  white-space:nowrap; flex:none; }
hr { border:0; border-top:1px solid var(--line); margin:4px 0; width:100%; }
.chart { display:flex; gap:6px; align-items:stretch; }
.axis { width:30px; flex:none; display:flex; flex-direction:column;
  justify-content:space-between; align-items:flex-end; font-size:8px; color:var(--muted); }
.plot { flex:1; min-width:0; position:relative; }
.plot svg { display:block; width:100%; height:100%; }
.foot { display:flex; font-size:10px; color:var(--muted); }

/* The grid's own wrapper is the query container, so each card reflows against
   ITS width, not the viewport's — what CockpitBreakpoints.reflow() does by hand. */
.cores-wrap {
  container-type: inline-size; container-name: cores;
  /* fixed block from core_block_height(); cells divide it, it never grows */
  height: var(--core-block-h, 220px);
}
.cores { display:grid; height:100%; grid-auto-rows:1fr;
  grid-template-columns: repeat(1, 1fr); gap:8px; }
.core { background:var(--panelAlt); border-radius:8px; padding:8px; display:flex;
  flex-direction:column; gap:4px; min-width:0; min-height:0; overflow:hidden; }
.core .cap { display:flex; font-size:10px; color:var(--muted); }
.core .cap b { font-weight:700; }
.core .plot { flex:1; min-height:0; }

.cols2 { display:grid; grid-template-columns: repeat(auto-fit, minmax(320px, 1fr)); gap:24px; }
.cols2 > section { display:flex; flex-direction:column; gap:8px; min-width:0; }
.legends { display:flex; flex-direction:column; gap:1px; align-items:flex-end; }
.lg { display:flex; align-items:center; gap:4px; font-size:9px; color:var(--muted); }
.lg i { width:6px; height:6px; border-radius:50%; flex:none; }
.lg b { color:var(--ink); font-weight:700; min-width:62px; text-align:right; }
.vols { display:grid; grid-template-columns: repeat(auto-fit, minmax(300px, 1fr)); gap:6px 16px; }
.vol { display:flex; flex-direction:column; gap:3px; height:28px; }
.vol .top { display:flex; font-size:10px; }
.vol .top .mount { font-weight:700; }
.vol .top .detail { margin-left:auto; font-size:9px; }
.vol .bar { height:4px; border-radius:2px; background:var(--panelAlt); overflow:hidden; }
.vol .bar > span { display:block; height:100%; border-radius:2px; }
.lbl { font-size:10px; font-weight:700; color:var(--muted); }
.procs { display:flex; flex-direction:column; gap:6px; }
.proc { display:flex; font-size:10px; }
.proc .v { margin-left:auto; font-weight:700; color:var(--muted); }
```

`app/ui/app.js`:

```js
// The entire frontend. No npm, no bundler, no framework: `viewmodel` has
// already decided every string and colour, so this only paints.

const $ = (id) => document.getElementById(id);

// Host names, CPU models, mount paths and process names arrive from a REMOTE
// agent. A webview parses markup, and in Tauri the DOM can call `invoke`, so an
// unescaped `<img onerror=...>` would reach the Rust command surface.
const esc = (v) =>
  String(v).replace(/[&<>"']/g, (ch) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[ch]));

const CHARTS = new Map();

function paint(el) {
  const spec = CHARTS.get(el);
  if (!spec) return;
  const w = Math.max(1, Math.round(el.clientWidth));
  const h = Math.max(1, Math.round(el.clientHeight));
  const { series, lo, hi, grid, pxPerSample, retained } = spec;
  const visible = Math.min(retained, Math.max(2, Math.floor(w / pxPerSample)));
  const parts = [];
  if (grid) {
    for (const f of [0, 0.5, 1]) {
      const y = (f * 100).toFixed(2);
      parts.push(`<line x1="0" y1="${y}" x2="${w}" y2="${y}" stroke="var(--line)" stroke-width="0.5" vector-effect="non-scaling-stroke"/>`);
    }
  }
  for (const sr of series) {
    const all = sr.values || [];
    if (all.length < 2) continue;
    const win = all.slice(Math.max(0, all.length - visible));
    const span = Math.max(hi - lo, 1e-4);
    const step = win.length > 1 ? w / (win.length - 1) : 0;
    const pts = win.map((v, i) => {
      const y = 100 - Math.min(Math.max((v - lo) / span, 0), 1) * 100;
      return `${(i * step).toFixed(2)},${y.toFixed(2)}`;
    }).join(" ");
    parts.push(`<polyline points="${pts}" fill="none" stroke="${esc(sr.color)}" stroke-width="1.5" vector-effect="non-scaling-stroke" stroke-linejoin="round"/>`);
  }
  // x in real pixels, y normalised: a wider chart shows MORE TIME, not a
  // stretched line. A symmetric viewBox is what causes stretching.
  el.innerHTML = `<svg viewBox="0 0 ${w} 100" preserveAspectRatio="none" width="${w}" height="${h}" role="img" aria-label="metric history, ${visible} samples">${parts.join("")}</svg>`;
}

const chartObserver = new ResizeObserver((es) => { for (const e of es) paint(e.target); });

function spark(el, series, lo, hi, capacity, grid = true) {
  if (el.dataset.h) el.style.height = Number(el.dataset.h) + "px";
  CHARTS.set(el, { series, lo, hi, grid, pxPerSample: window.__PX || 4, retained: capacity });
  paint(el);
  chartObserver.observe(el);
}

/** Rust computes which column counts leave a full last row; CSS distributes. */
function installCoreLadder(ladder) {
  const rules = ladder.map((r) =>
    `@container cores (min-width: ${r.minWidth}px){.cores{grid-template-columns:repeat(${r.cols},1fr)}}`
  ).join("\n");
  let el = document.getElementById("coreLadder");
  if (!el) { el = document.createElement("style"); el.id = "coreLadder"; document.head.appendChild(el); }
  el.textContent = rules;
}

function render(d) {
  const r = document.documentElement.style;
  for (const [k, v] of Object.entries(d.theme)) {
    r.setProperty("--" + (k === "netUp" ? "netup" : k), v);
  }
  window.__PX = d.pxPerSample;
  r.setProperty("--core-block-h", d.coreBlockHeight + "px");
  installCoreLadder(d.coreLadder);

  $("hostName").textContent = d.hostName;
  $("cpuModel").textContent = d.cpuModel;
  $("cpuValue").textContent = d.cpuValue;
  $("cpuValue").style.color = d.cpuValueColor;
  const th = $("thermal");
  th.textContent = d.thermalText;
  th.style.color = d.thermalColor;
  th.style.background = d.thermalColor + "22";

  spark($("cpuChart"), [{ values: d.cpuHistory, color: d.theme.cpu }], 0, 100, d.capacity);

  $("cores").innerHTML = d.cores.map((c) =>
    `<div class="core"><div class="cap">${esc(c.label)}<b style="margin-left:auto;color:${esc(c.valueColor)}">${esc(c.value)}</b></div><div class="plot"></div></div>`
  ).join("");
  document.querySelectorAll("#cores .plot").forEach((el, i) => {
    spark(el, [{ values: d.cores[i].history, color: d.cores[i].hue }], 0, 100, d.capacity, false);
  });

  $("memValue").textContent = d.memValue;
  spark($("memChart"), [{ values: d.memHistory, color: d.theme.mem }], 0, 100, d.capacity);
  $("swapText").textContent = d.swapText;
  $("pressureText").textContent = d.pressureText;
  $("pressureText").style.color = d.pressureColor;

  $("gpuValue").textContent = d.gpuValue;
  $("gpuValue").style.color = d.gpuValueColor;
  spark($("gpuChart"), [{ values: d.gpuHistory, color: d.theme.gpu }], 0, 100, d.capacity);
  $("vramText").textContent = d.vramText;

  $("diskRead").textContent = d.diskRead;
  $("diskWrite").textContent = d.diskWrite;
  $("diskAxis").textContent = d.diskAxis;
  spark($("diskChart"), [
    { values: d.diskReadHistory, color: d.theme.read },
    { values: d.diskWriteHistory, color: d.theme.write },
  ], 0, d.diskMax, d.capacity);

  $("netDown").textContent = d.netDown;
  $("netUp").textContent = d.netUp;
  $("netAxis").textContent = d.netAxis;
  spark($("netChart"), [
    { values: d.netDownHistory, color: d.theme.net },
    { values: d.netUpHistory, color: d.theme.netUp },
  ], 0, d.netMax, d.capacity);

  $("volumeCount").textContent = d.volumeCount;
  $("volumes").innerHTML = d.volumes.map((v) =>
    `<div class="vol"><div class="top"><span class="mount">${esc(v.mount)}</span><span class="detail" style="color:${esc(v.tint)}">${esc(v.detail)}</span></div><div class="bar"><span style="width:${(v.fraction * 100).toFixed(1)}%;background:${esc(v.tint)}"></span></div></div>`
  ).join("");

  const procs = (list) => list.map((p) =>
    `<div class="proc"><span>${esc(p.name)}</span><span class="v">${esc(p.value)}</span></div>`
  ).join("");
  $("topCpu").innerHTML = procs(d.topCpu);
  $("topRam").innerHTML = procs(d.topRam);
}

(async () => {
  try {
    const data = window.__TAURI__
      ? await window.__TAURI__.core.invoke("snapshot")
      : await (await fetch("sample.json")).json();
    render(data);
  } catch (e) {
    document.body.innerHTML =
      `<pre style="color:#e05a4f;padding:20px">failed to load snapshot: ${esc(e)}</pre>`;
  }
})();
```

- [ ] **Step 5: Build and verify it runs**

Run: `cargo build -p devcanopy-app --release`
Expected: builds clean.

Run: `./target/release/devcanopy-app`
Expected: a window titled "DevCanopy" showing the host card with 16 core cells and populated charts. Close it.

- [ ] **Step 6: Assert the layout, not a screenshot**

```bash
mkdir -p /tmp/dc-preview && cp app/ui/* /tmp/dc-preview/
./target/release/devcanopy-app --dump /tmp/dc-preview/sample.json
(cd /tmp/dc-preview && python3 -m http.server 3799 &) && sleep 2
```

Verify in any browser at `http://127.0.0.1:3799/index.html`, then in its console:

```js
const wrap = document.querySelector('.cores-wrap');
const grid = document.querySelector('.cores');
for (const w of [1900, 890, 440, 216]) {
  wrap.style.width = w + 'px'; void wrap.offsetWidth;
  const cols = getComputedStyle(grid).gridTemplateColumns.trim().split(/\s+/).length;
  const cells = grid.children.length;
  console.log(w, 'cols', cols, 'fullLastRow', cells % cols === 0,
              'blockPx', Math.round(wrap.getBoundingClientRect().height));
}
wrap.style.width = '';
```

Expected: `1900 cols 16 fullLastRow true blockPx 220`, `890 cols 8 … 220`, `440 cols 4 … 220`, `216 cols 2 … 220`. Every row full, block fixed at 220 throughout.

Stop the server: `pkill -f 'http.server 3799'`

- [ ] **Step 7: Commit**

```bash
git add app
git commit -m "feat(app): Tauri shell rendering the host card from viewmodel

Frontend is static HTML/CSS/JS with no npm. The core grid reflows via
container queries emitted from the Rust divisor ladder, and the cores block
holds a fixed 220pt exactly as coreRowSpan * coreRowUnit does in Swift."
```

---

### Task 7: Live polling against the real agent

**Files:**
- Modify: `app/src-tauri/Cargo.toml`
- Modify: `app/src-tauri/src/main.rs`

**Interfaces:**
- Consumes: `agentclient::{AgentClient, AgentError}`, `viewmodel::card::{host_card, HostHistories}`
- Produces: `#[tauri::command] snapshot() -> serde_json::Value` returning either the card view-model or `{"error": {"message": …, "hostName": …}}`

- [ ] **Step 1: Add dependencies**

Add to `app/src-tauri/Cargo.toml`:

```toml
agentclient = { path = "../../crates/agentclient" }
keyring = { version = "3", features = ["apple-native", "windows-native"] }
tokio = { version = "1", features = ["rt-multi-thread", "time", "sync", "macros"] }
```

- [ ] **Step 2: Replace the fixture source with live state**

The client is immutable and lives *outside* the mutex; only the mutable state is
locked. Holding a `std::sync::Mutex` guard across an `.await` does not compile and
would deadlock if it did.

Replace everything in `app/src-tauri/src/main.rs` above `fn main`:

```rust
//! DevCanopy shell. The frontend receives a finished view-model and paints;
//! all logic lives in `viewmodel`.

use agentclient::AgentClient;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use viewmodel::card::{host_card, HostHistories};

/// The mutable half of one watched host. The `AgentClient` is immutable and is
/// held separately so the poll loop never locks across an await.
struct HostState {
    name: String,
    histories: HostHistories,
    latest: Option<metrics::Snapshot>,
    error: Option<String>,
}

type Shared = Arc<Mutex<HostState>>;

/// Tokens live in the OS credential store, never in app storage. The service
/// name matches the Swift `KeychainHelper` so an existing entry is reused.
fn load_token(host_id: &str) -> Option<String> {
    keyring::Entry::new("com.sassydog.devcanopy", &format!("host-{host_id}"))
        .ok()?
        .get_password()
        .ok()
}

#[tauri::command]
fn snapshot(state: tauri::State<'_, Shared>) -> Value {
    let s = state.lock().unwrap();
    match (&s.latest, &s.error) {
        (Some(snap), _) => host_card(&s.name, snap, &s.histories),
        (None, Some(msg)) => json!({ "error": { "message": msg, "hostName": s.name } }),
        (None, None) => json!({
            "error": { "message": "waiting for first sample…", "hostName": s.name }
        }),
    }
}
```

- [ ] **Step 3: Wire the runtime and the refresh loop into `main`**

Replace `fn main` in `app/src-tauri/src/main.rs`:

```rust
fn main() {
    // Configuration is env-driven for the skeleton; Settings arrives with the
    // store crate in a later plan.
    let host_id = std::env::var("DEVCANOPY_HOST_ID").unwrap_or_else(|_| "default".into());
    let name = std::env::var("DEVCANOPY_HOST_NAME").unwrap_or_else(|_| "ubu-3xdv".into());
    let url = std::env::var("DEVCANOPY_HOST_URL")
        .unwrap_or_else(|_| "http://100.87.202.125:7878".into());
    let token = std::env::var("DEVCANOPY_AGENT_TOKEN")
        .ok()
        .or_else(|| load_token(&host_id))
        .unwrap_or_default();

    let shared: Shared = Arc::new(Mutex::new(HostState {
        name,
        histories: HostHistories::new(),
        latest: None,
        error: None,
    }));

    // Immutable, so it is owned by the poll task rather than the mutex.
    let client = AgentClient::new(url, token);

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let poll_target = shared.clone();
    rt.spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
        loop {
            tick.tick().await;
            // No lock is held across this await.
            let result = client.snapshot().await;
            let mut s = poll_target.lock().unwrap();
            match result {
                Ok(snap) => {
                    s.histories.record(&snap);
                    s.latest = Some(snap);
                    s.error = None;
                }
                Err(e) => s.error = Some(e.user_message().to_string()),
            }
        }
    });

    tauri::Builder::default()
        .manage(shared)
        .invoke_handler(tauri::generate_handler![snapshot])
        .run(tauri::generate_context!())
        .expect("failed to start");

    // Keep the runtime alive for the lifetime of the app.
    drop(rt);
}
```

- [ ] **Step 4: Render the error state in the frontend**

In `app/ui/app.js`, replace the IIFE at the bottom:

```js
(async () => {
  const load = async () =>
    window.__TAURI__
      ? await window.__TAURI__.core.invoke("snapshot")
      : await (await fetch("sample.json")).json();

  const draw = (d) => {
    if (d.error) {
      // A failed host shows its cause, never fabricated numbers.
      $("hostName").textContent = d.error.hostName;
      $("cpuValue").textContent = "—";
      $("cpuModel").textContent = d.error.message;
      $("cpuModel").style.color = "#e05a4f";
      return;
    }
    $("cpuModel").style.color = "";
    render(d);
  };

  try { draw(await load()); } catch (e) {
    document.body.innerHTML =
      `<pre style="color:#e05a4f;padding:20px">failed to load snapshot: ${esc(e)}</pre>`;
  }
  if (window.__TAURI__) setInterval(async () => { try { draw(await load()); } catch {} }, 2000);
})();
```

- [ ] **Step 5: Run against the live agent**

```bash
cargo build -p devcanopy-app --release
DEVCANOPY_HOST_URL=http://100.87.202.125:7878 \
DEVCANOPY_AGENT_TOKEN=<token> \
DEVCANOPY_HOST_NAME=ubu-3xdv \
  ./target/release/devcanopy-app
```

Expected: the card shows live values from `ubu-3xdv`, charts fill in over ~30s as history accumulates, and the core count matches the real host.

Then verify the failure path — run with a deliberately wrong token:

```bash
DEVCANOPY_HOST_URL=http://100.87.202.125:7878 \
DEVCANOPY_AGENT_TOKEN=wrong \
  ./target/release/devcanopy-app
```

Expected: the card shows `—` for CPU and the message "Agent rejected the bearer token (401). Check the host's token in Settings." — never a zero.

- [ ] **Step 6: Commit**

```bash
git add app crates/agentclient
git commit -m "feat(app): poll a live agent and surface cause-specific failures

A host that can't be reached renders an em dash and the reason, never a
fabricated zero. Tokens come from the OS credential store under the same
service name the Swift KeychainHelper uses."
```

---

### Task 8: CI for the Rust workspace

**Files:**
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: the whole workspace
- Produces: a required `rust-workspace` check

- [ ] **Step 1: Read the existing workflow**

Run: `sed -n '1,40p' .github/workflows/ci.yml`

The existing jobs are `swift-tests` and `lint` on `[self-hosted, macOS, sassy-dog]`,
and `agent-tests` on `[self-hosted, linux, sassy-dog]`. The new job matches the macOS
label, since the Tauri app is built there.

- [ ] **Step 2: Add the job**

Append to the `jobs:` map in `.github/workflows/ci.yml`:

```yaml
  rust-workspace:
    name: rust fmt + clippy + test
    runs-on: [self-hosted, macOS, sassy-dog]
    steps:
      - uses: actions/checkout@v4
      - name: Install Rust toolchain
        run: rustup toolchain install stable --profile minimal --component rustfmt,clippy
      - name: Cache cargo
        uses: actions/cache@v4
        with:
          path: |
            ~/.cargo/registry
            ~/.cargo/git
            target
          key: ${{ runner.os }}-cargo-${{ hashFiles('Cargo.lock') }}
      - name: Format
        run: cargo fmt --all -- --check
      - name: Clippy
        run: cargo clippy --workspace --all-targets -- -D warnings
      - name: Test
        run: cargo test --workspace
      - name: Windows target still compiles
        run: |
          rustup target add x86_64-pc-windows-msvc
          cargo check -p metrics -p viewmodel -p agentclient --target x86_64-pc-windows-msvc
```

- [ ] **Step 3: Verify locally before pushing**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
rustup target add x86_64-pc-windows-msvc
cargo check -p metrics -p viewmodel -p agentclient --target x86_64-pc-windows-msvc
```

Expected: all clean. Fix any clippy findings rather than allowing them.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: gate the Rust workspace on fmt, clippy, tests and a Windows check

The Windows cross-check is cheap insurance: the port's whole premise is that
these crates stay portable, and a check job catches drift the moment it lands."
```

---

## Definition of done

- `cargo test --workspace` passes: 5 (metrics) + 27 (viewmodel) + 5 (agentclient) = 37 tests
- `./target/release/devcanopy-app` shows live metrics from a real agent
- A wrong token produces `—` and a cause-specific message, never a zero
- The core grid reflows 16/8/4/2/1 columns with a full last row at every width, and the cores block measures 220px throughout
- CI gates fmt, clippy, tests, and a Windows cross-check
- The Swift app is unmodified and still builds (`./dev build`)

## Not in this plan

Each gets its own plan.

- Multi-host, the remaining six cockpit panels, and Settings
- `crates/store` (SQLite) and `crates/secrets` — configuration is env-driven here
- `crates/gateway` (OpenClaw) and `crates/sources` (GitHub, Azure, Claude usage)
- Making `agent/` a workspace member and switching it to `crates/metrics` types
- Deleting `HostMetricsKit` — blocked until the local machine is served by an agent
- Windows packaging, signing, notarization, WebView2 bundling
- An icon font to replace the SF Symbols stand-ins
