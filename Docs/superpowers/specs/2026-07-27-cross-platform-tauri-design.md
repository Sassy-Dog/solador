# Cross-platform port — Rust core + Tauri shell

Date: 2026-07-27
Status: proposed

## Problem

Solador is macOS-only: SwiftUI views, SwiftData models, IOKit metrics, Keychain.
The goal is to ship it to other developers, and most developer desktops are macOS or
Windows.

The port is also an opportunity to fix something already wrong. Solador currently
maintains **two Swift reimplementations of Rust code it already owns**:

| Swift | Duplicates | Evidence |
|---|---|---|
| `Packages/HostMetricsKit` (3,143 lines, IOKit) | `agent/src/metrics.rs` (1,042 lines, `sysinfo`) | Same metric set; the agent has zero `#[cfg(target_os)]` branches, so it is already platform-neutral |
| `Services/OpenClaw/` (6 files, ~42 KB) | `cpmadrid/periclaw` `src/net` (4,002 lines) | `AgentRuntimeModels.swift:10` says so: *"Mirrors periclaw's `domain::AgentStatus`"* |

A third of the Swift is a translation layer over Rust that already exists.

## Goal

One codebase serving macOS and Windows, shipped to other developers, with the
duplicated subsystems collapsed into shared Rust crates.

### Deployment profile

Several decisions below only make sense against this, so it is stated once:

| | |
|---|---|
| Primary author use | Directly on macOS, on real hardware |
| Distribution | Other developers, on macOS and Windows desktops |
| Explicitly not targeted | GPU-less sessions (VDI, RDP, VMs, locked-down drivers), Linux desktops |

Both halves matter. "Runs on my Mac" is why VDI-class constraints are excluded;
"shipped to other developers" is why signing, a Windows CI runner, and the security
controls in *Security* are non-negotiable rather than nice-to-have.

## Decisions

| Decision | Choice | Why |
|---|---|---|
| Target platforms | macOS + Windows | Where developer desktops are. Linux dropped — see *Scope* |
| Shell | Tauri v2 | Smallest binary, fastest build, least code, free responsive layout — all measured |
| Frontend | Plain HTML/CSS/JS, no npm | No `package.json`, no bundler, no framework. 134 lines because the core does the thinking |
| Core | Rust workspace, `card-core` pattern generalised | Collapses both duplications; makes presentation logic unit-testable without a renderer |
| Layout policy | Computed in Rust, applied by CSS | Divisor ladders and block heights are testable values; distribution is the engine's job |
| Charts | Inline SVG, x in real pixels | Zero dependencies; pixel-x is what makes a wide chart show more time |
| Storage | SQLite (`rusqlite`) | SwiftData has no cross-platform equivalent |
| Secrets | `keyring` crate | Keychain / Windows Credential Manager behind one API; PeriClaw already proves it |
| Icons | Bundled icon font | SF Symbols do not exist off-Apple; the card uses six |
| Trust boundary | Escape-at-render + strict CSP | A webview parses markup and remote hosts supply strings — see *Security* |

## Evidence

Three complete prototypes of `HostMetricsPanel.swift`, all driving one shared
`card-core` with identical data. Measured, not estimated.

| Measure | egui 0.35 | Slint 1.17 | **Tauri 2** |
|---|---|---|---|
| Release binary | 5.0 MB | 6.6 MB | **3.2 MB** |
| Cold release build | 69 s | 78 s | **66 s** |
| Dependency crates | **160** | 268 | 217 |
| Card source | 714 | 654 | **562** |
| JavaScript | 0 | 0 | 134 |
| npm / lockfile / node_modules | none | none | none |
| `cargo check` → macOS | pass | pass | pass |
| `cargo check` → Windows | pass | pass | pass |
| `cargo check` → Linux | pass | fontconfig | 9 GTK libs |
| Resident (idle) | 70.5 MB | 68.9 MB | 118.7 MB |
| Idle CPU | 0.0 % | 0.0 % | 0.0 % |
| Responsive reflow | hand-rolled | hand-rolled | **CSS** |
| Markup parser in trust boundary | no | no | **yes** |

Both Slint's and Tauri's failures are Linux-only, so dropping Linux un-eliminates
both. On macOS + Windows no measurement separates egui from Tauri decisively; Tauri
leads on binary, build, and code volume, and gets layout free. egui's counter-argument
is the trust boundary, which is real and is mitigated rather than eliminated below.

Slint is rejected on charting: it has no chart primitive, and a host card draws
**23 sparklines** (CPU + 16 cores + memory + GPU + 2 overlaid I/O pairs). Each becomes
an SVG command string assembled in Rust. That cost is platform-independent.

Slint's distinguishing advantage is a true software renderer — it produced its
prototype render with no GPU, no winit and no display server, which neither egui nor
Tauri can do. Two distinct capabilities hide behind that, and they resolve differently:

| Capability | What it means | Verdict |
|---|---|---|
| **GPU-less session** | A screen and a human, but no usable GPU acceleration — VDI, RDP, VMs, old iGPUs, locked-down drivers. A compatibility requirement | Out of the deployment profile. **This is the one case that would re-open the toolkit choice** |
| **No display session** | No window server at all; not running the app for a human but *rendering an image of it* — a scheduled cockpit-to-PNG for Slack, a wall display, a CI visual baseline | Does **not** re-open anything — see below |

The second case is additive, not exclusive. Because `crates/viewmodel` already computes
every string, colour and layout value, a headless image renderer is a **second binary
over the same crates**, not a replacement shell. That is the same pattern the agent and
the app already use over `crates/metrics`. If cockpit-to-PNG is ever wanted, add a
small Slint or `resvg` renderer beside the Tauri app and leave the app alone.

So on real desktop hardware Slint's advantage buys nothing the architecture can't
supply later, and the charting cost decides.

## Scope

**In:** macOS, Windows. The cockpit and Settings.

**Out:** Linux. Not because it can't work, but because a Tauri Linux target needs the
GTK/WebKitGTK stack present at build time (`glib-sys`, `gobject-sys`, `gtk-sys`,
`webkit2gtk-sys`, `soup3-sys`, +4), which is a standing CI and packaging cost for a
platform few target users are on. **If Linux is ever added, this decision must be
revisited — egui is the only one of the three that checks clean there.**

## Architecture

```
devcanopy/
├── crates/
│   ├── metrics/        # was HostMetricsKit + agent/src/metrics.rs
│   ├── gateway/        # was Services/OpenClaw/ + periclaw src/net
│   ├── sources/        # was Services/{GitHub,AzureCost,ClaudeUsage,GitMonitor,Containers}
│   ├── viewmodel/      # formatters, colour rules, layout policy (the card-core pattern)
│   ├── store/          # was Models/ (SwiftData -> SQLite)
│   └── secrets/        # was KeychainHelper (-> keyring)
├── agent/              # thin bin over crates/metrics (unchanged behaviour)
└── app/
    ├── src-tauri/      # thin bin over the same crates; #[tauri::command] surface
    └── ui/             # index.html, app.css, app.js — static, no build step
```

The agent and the app become two entry points over one core. `crates/metrics` serves
both remote hosts (over Tailscale, as today) and localhost — which is what deletes
`HostMetricsKit` rather than porting it.

### Core / shell contract

The shell receives a **finished view-model** and paints. Every formatted string and
every colour decision happens in Rust:

```rust
json!({
  "cpuValue":      "34%",
  "cpuValueColor": "#33d17a",     // usage_color(34.0)
  "gpuValue":      "—",           // None -> em dash, never 0%
  "coreBlockHeight": 220.0,
  "coreLadder":    [{"minWidth":104,"cols":1}, …],
})
```

This is why the frontend is 134 lines. It also means the "no fake numbers" rule is
enforced in Rust: `gpu_usage: Option<f64>` renders `—` at the source, and the frontend
has no branch that could invent a zero.

## Layout model

Three pieces of policy move into `crates/viewmodel`, all unit-tested without a
renderer, all ported from the existing Swift arithmetic.

### Divisor ladder (generalises `coreColumns`)

`core_column_ladder(count, min_cell, gap)` returns every column count that divides
`count` evenly, paired with the container width it needs. For 16 cores at 104pt cells:

| Container ≥ | Columns | Rows |
|---|---|---|
| 104pt | 1 | 16 |
| 216pt | 2 | 8 |
| 440pt | 4 | 4 |
| 888pt | 8 | 2 |
| 1784pt | 16 | 1 |

Emitted as `@container` rules. The last row is structurally never an orphan, and `1fr`
consumes the remainder so the grid is always flush with the card edge. Verified at
eight widths: `fullLastRow: true, fillsContainer: true` at every one.

Today's Swift picks **one** column count for all widths — which is where the wasted
space comes from. The ladder picks per-width with the same divisor guarantee.

### Fixed block height (ports `coreRowSpan × coreRowUnit`)

The cores section reserves a fixed height regardless of core count, so host cards line
up in the cockpit grid. `CORE_ROW_UNIT = 110`, `CORE_ROW_SPAN_DEFAULT = 2` → 220pt.
Cells divide the block; the block never grows:

| Rows | Cell height | Swift equivalent |
|---|---|---|
| 1 | 220pt | `(220 − 0) / 1` |
| 2 | 106pt | `(220 − 8) / 2` |
| 4 | 49pt | `(220 − 24) / 4` |

CSS: `grid-auto-rows: 1fr` inside a fixed-height container. Measured at 220pt across
every width tested.

### Chart windowing

A wider chart must show **more time**, not the same samples stretched. The buffer
retains 600 samples; a chart draws the most recent `width / PX_PER_SAMPLE` of them at
a constant 4pt per sample.

| Chart width | Samples drawn | pt/sample |
|---|---|---|
| 420pt | 80 | 4.08 |
| 900pt | 200 | 4.03 |
| 1800pt | 425 | 4.01 |

This is the one place CSS alone is insufficient: sample count is a function of pixel
width, so it needs a `ResizeObserver` (~12 lines) and an SVG whose `viewBox` tracks
element width (`0 0 ${w} 100`) — y normalised, **x in real pixels**. A symmetric
`viewBox="0 0 100 100"` with `preserveAspectRatio="none"` is what produces stretching.

## Security

This is the cost of choosing a webview and it is not optional discipline.

Host names, CPU model strings, volume mount paths and process names all arrive **from
a remote agent over the network**. In a webview those strings enter an HTML parser that
egui and Slint do not have — and Tauri's DOM can call `invoke`, so injection in the UI
layer reaches the Rust command surface.

Mitigations, all required:

| Control | Rule |
|---|---|
| Escaping | Every remote-origin interpolation passes through `esc()`; prefer `textContent` where no markup is needed |
| CSP | Strict `default-src 'self'`; no `unsafe-inline`, no remote origins. (The prototype sets `csp: null` — that must not ship) |
| Command surface | `#[tauri::command]` functions are read-only projections of the view-model. No command takes a path, URL, or shell fragment from the frontend |
| Capabilities | Tauri v2 capability allowlist limited to the commands actually used; no `fs`, `shell`, or `http` plugins |
| Review | Any new `innerHTML` site is a review checkpoint |

Accepted residual risk: this is a maintained discipline, not a structural guarantee.
egui and Slint would eliminate the class entirely. That trade is made deliberately in
exchange for the layout engine, native text entry, and the smallest binary.

## Storage and secrets

| Today | After | Note |
|---|---|---|
| `MonitoredHost`, `AppSettings`, `WorkflowRunModels` (SwiftData) | SQLite via `rusqlite`, one migration module | No ORM; the schema is small |
| `KeychainHelper` (Keychain) | `keyring` crate | macOS Keychain / Windows Credential Manager; PeriClaw ships this already |
| GitHub PAT, per-host bearer tokens, OpenClaw token, Azure SAS | unchanged semantics | Still never persisted in the database |

The existing rule holds: credentials live in the OS credential store, never in the
app's own storage.

## Distribution

| Platform | Signing | Runtime dependency |
|---|---|---|
| macOS | Developer ID + notarization (team `52YMXC3348`) | none |
| Windows | Code-signing certificate — Azure Trusted Signing fits the existing Azure estate | WebView2; evergreen on Win11, bootstrapper bundled for older Win10 |

CI becomes `macos` (already self-hosted, #118) + `windows-latest`. Dropping Linux
removes the AppImage/deb/Flatpak matrix entirely.

Versioning is unchanged: CalVer marketing version and commit-count build number, both
derived from git per `Docs/VERSIONING.md`. `Scripts/build.sh` gains a Tauri path; no
version is ever computed anywhere else.

## Testing

| Layer | Approach |
|---|---|
| `crates/viewmodel` | Unit tests, no renderer linked. The prototype already has 12 covering formatters, colour thresholds, the divisor ladder, fixed-block arithmetic, and chart windowing |
| `crates/metrics`, `gateway`, `sources` | Unit tests over recorded fixtures; the agent's existing tests carry over |
| `crates/store` | Migration round-trip tests |
| Frontend | Playwright against the static `ui/` with a dumped view-model — the `--dump` flag already exists for this. Assertions are on computed layout (column counts, block height, samples drawn), not screenshots |

The frontend is deliberately thin enough that its own logic barely warrants tests; what
needs testing is the layout policy, and that lives in Rust.

## Migration sequencing

Each phase leaves a working product.

| Phase | Work | Ships |
|---|---|---|
| 1 | Extract `crates/viewmodel` + `crates/metrics`; agent switches to the shared crate | Nothing user-visible; deletes the metrics duplication |
| 2 | `crates/gateway` from periclaw's `src/net`; agent-farm parity | Nothing user-visible; deletes the OpenClaw duplication |
| 3 | `crates/sources`, `store`, `secrets` | Nothing user-visible; Swift app still shipping |
| 4 | Tauri shell: cockpit panels, then Settings | macOS beta alongside the Swift app |
| 5 | Windows build, signing, WebView2 bundling | Windows beta |
| 6 | Retire the Swift app | Single codebase |

Phases 1–3 are worth doing **regardless of whether phases 4–6 happen** — they remove
duplication and make presentation logic testable in the existing app.

## Risks

| Risk | Mitigation |
|---|---|
| XSS via remote-host strings | The controls above; treat as a permanent review obligation |
| Cockpit framerate with 4 cards (92 sparklines) | Unmeasured. Measure before phase 4 commits; SVG node count is the likely limit, `<canvas>` is the fallback |
| Windows text rendering divergence | Test on real hardware in phase 5; the cockpit is monospaced and dark, which is the forgiving case |
| Settings feels non-native | This is Tauri's advantage over egui, not a risk — native fields, paste menus, autofill |
| Narrow cards give 21pt core cells | Same property as today's Swift. Decide whether narrow widths should raise `coreRowSpan` automatically |
| Scope creep into a rewrite | Phases 1–3 are refactors of the Swift app; only phase 4 starts a new shell |

## Out of scope

- Linux support (see *Scope*; revisiting it reopens the toolkit decision).
- Mobile or web targets.
- Redesigning the host card. This port reproduces today's card; layout changes are
  separate work.
- Replacing the agent's transport or auth model.
- Multiple host-card variants by preference — a later feature, not a port concern.

## Prototypes

Three working builds — `card-core`, `egui-card`, `slint-card`, `tauri-card` — each
rendering the card from the same data. `--dump` writes the view-model, `--show` opens
a window.

**They live in a session scratchpad and are not committed.** Every measurement in this
document was taken from them, so if the evidence needs to be reproducible, the Tauri
prototype and `card-core` should be committed (as `crates/viewmodel` seed work under
phase 1) before the scratchpad is lost. The egui and Slint builds have served their
purpose and can be discarded.

Rendered comparison of all three, with the measurement tables:
<https://claude.ai/code/artifact/c5e76404-7b11-4c6f-85d6-32762b38e457>

Note: the egui and Slint renders in that artifact predate the fixed-block and
chart-windowing fixes, which were applied to the Tauri build only.
