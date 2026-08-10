# Cockpit breakpoints — per-panel minimum widths

Date: 2026-07-26
Status: implemented

## Problem

The cockpit rendered a fixed two-column `Grid`, and the Hosts panel packed host cards
side-by-side whenever `width >= hosts.count * 760`. On a portrait display (~1568pt of
content) two host cards landed at ≈752pt each — just barely passing that check — so
both rendered squeezed: the 36-core grid on `ubu-01` collapsed to `···` labels and
volume mounts truncated.

When the check *did* fail, the fallback was a `TabView`, which hides a host behind a
click. That defeats the purpose of an always-on, glanceable cockpit.

## Goal

Give the cockpit a real responsive concept so panels reflow instead of squeezing, and
so host cards stack vertically at the widths the cockpit is actually used at.

## Decisions

| Decision | Choice | Why |
|---|---|---|
| Responsive model | Per-panel minimum widths (CSS `repeat(auto-fit, minmax(…, 1fr))`) | Global `sm/md/lg` tiers can't express "Claude Usage + Azure Cost still fit 2-up at a width where Hosts must stack" |
| Measurement | One `GeometryReader` at the root; panel widths derived and injected | Panels measuring themselves via preferences did not work at all (see below); deriving is also loop-free and deterministic |
| Unknown width | Fall back to the layout that can't be unreadable | A dead measurement should look wrong, not deliberate |
| Host card minimum | 900pt | Below this the per-core grid loses its `Core N xx%` labels; 2-up needs ≥1816pt, 3-up ≥2732pt |
| Tabs fallback | Kept, demoted to a setting, stacking default | Vertical space is cheap on a portrait cockpit; hiding a host should be opt-in |
| Row merging | Reflow only ever *splits* rows | Authored pairings are intent, not a packing suggestion |

## Design

### Per-widget declaration

`CockpitPanelKind.minWidth` sits beside the existing `title`/`systemImage` metadata —
the enum is already the per-panel metadata surface.

| Panel | minWidth | Basis |
|---|---|---|
| `.hosts` | 900 | one host card |
| `.ghWorkflows` (Repos) | 560 | 312pt of fixed numeric columns + gaps + status dot + repo name + 28pt card padding |
| `.openclawAgents` | 440 | agent rows + wrapping channel dots |
| `.ghRunners` | 400 | runner name + 48pt state column |
| `.containers` | 400 | container name + status |
| `.azureCost` | 400 | total + two-column service breakdown |
| `.claudeUsage` | 360 | 42pt label column + values |

Resulting row break points (window width = available + 40pt page padding):

| Row | Breaks below |
|---|---|
| Repos + Runners | 1016pt |
| Containers + OpenClaw | 896pt |
| Claude Usage + Azure Cost | 816pt |

At ~1568pt no panel row breaks — only the host cards inside the Hosts panel stack,
which is precisely the requested behaviour.

### Layout math (`CockpitBreakpoints`)

Pure values, no SwiftUI, mirroring `CoreGridLayout` / `VolumeGridLayout`:

- `reflow(rows:available:spacing:)` — greedy, order-preserving repack. Never merges
  across authored rows; a lone panel keeps its row even when wider than the window.
  `available == 0` (first render) passes rows through untouched.
- `hostColumns(available:hostCount:minCardWidth:spacing:)` — the `auto-fit` formula
  `Int((available + spacing) / (minCardWidth + spacing))`, clamped to `1...hostCount`.
- `rows(_:columns:)` — chunking helper; `columns <= 1` gives one item per row.
- `HostOverflowMode` — `.stack` (default) / `.tabs`.

### Measurement (`CockpitPanelWidth`)

The first attempt reused the pattern already in `HostsPanel` —
`.background(GeometryReader { Color.clear.preference(…) })` plus `onPreferenceChange`
— extracted into a shared `.readingWidth(into:)` modifier. Instrumenting it showed it
never fires:

```
[BP] cockpit contentWidth=0.0 …
[BP] hosts availableWidth=0.0 count=2 columns=2
```

Both readers sat at `defaultValue` forever, at two independent sites. Preferences set
inside a `.background` do not reach `onPreferenceChange` in this SwiftUI version.

**This was a pre-existing bug**, not a regression: `HostsPanel` had always read 0, so
its `fits` check was always true and the `TabView` branch was unreachable. It went
unnoticed because the failure mode — assume wide — is what a wide window looks like.

The fix removes measurement from panels entirely. `CockpitView` wraps its `ScrollView`
in one `GeometryReader`, and since it knows each row's composition after reflow it
derives every panel's width (`available` for a lone panel, `(available - gap) / 2` for
a pair) and injects it as `\.cockpitPanelWidth`. No preferences, no measure→relayout
feedback loop.

### Failing safe

`hostColumns(available: 0, …)` now returns **1**, not `hostCount`. The old fallback
made a dead measurement indistinguishable from a genuinely wide window, which is how
the bug survived. Stacking is never unreadable and is obviously wrong on a big
display, so the next such failure gets reported. `volumeColumns` follows the same rule.
`reflow` keeps passing authored rows through at unknown width — panel minimums are
small, so an authored pair is never the unreadable option.

### Rendering

- `CockpitView` renders `CockpitBreakpoints.reflow(layout.rows, available:)` at the
  width from its root `GeometryReader`, minus page padding. Column span comes from
  `PanelSpan`, except that a panel alone in a rendered row always spans the full width
  — otherwise a reflow-split row leaves a hole.
- `HostsPanel` lays cards out in a `Grid` of `hostColumns` per row (`Grid`, not
  `HStack`, so cards in a row share a height). One column is the stacked case. Tabs
  only when `columns == 1`, more than one host, and the user chose `.tabs`.
- Stacked cards receive the panel's full width, which clears the existing 560pt
  two-column threshold in `volumeColumns` — so stacking also un-truncates volumes.

## Files

| File | Change |
|---|---|
| `DevCanopy/Views/Cockpit/CockpitBreakpoints.swift` | new — `reflow`, `hostColumns`, `rows`, `HostOverflowMode` |
| `DevCanopy/Views/Cockpit/CockpitPanelWidth.swift` | new — `\.cockpitPanelWidth` environment value |
| `DevCanopy/Views/Cockpit/CockpitPanel.swift` | `CockpitPanelKind.minWidth` |
| `DevCanopy/Views/Cockpit/CockpitView.swift` | measure width, reflow rows, span from row count |
| `DevCanopy/Views/Cockpit/Panels/HostsPanel.swift` | card grid/stack, 900pt minimum, overflow setting |
| `DevCanopy/Views/Settings/SettingsView.swift` | host-overflow picker |
| `DevCanopyTests/CockpitBreakpointsTests.swift` | new — 15 tests |
| `DevCanopyTests/CockpitLayoutTests.swift` | every panel kind must declare a positive `minWidth` |

## Testing

`CockpitBreakpointsTests` covers reflow (portrait width leaves rows paired; the 880pt
window floor breaks only the rows that don't fit; exact-requirement boundaries;
unmeasured width; conservation of order and membership across 100–3000pt; wide widths
never merge authored rows) and `hostColumns` (the 1528pt/2-host regression, the
1815/1816 and 2731/2732 boundaries, clamping, unmeasured width).

`CockpitLayoutTests` gains a guard that a new panel kind can't ship without a
`minWidth`.

## Out of scope

- Reordering panels at narrow widths — reflow preserves order; a different arrangement
  is a different `CockpitLayout` value.
- User-configurable `minWidth`.
- Height-based breakpoints.
