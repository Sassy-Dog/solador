# Solador brand

Source files for the name and mark. Everything here is **authored** — the icon
sets the apps actually ship are generated from `icon.svg` and live where their
toolchains require them (see *Derived files*). Nothing here is built, imported,
or read at runtime.

## Terms

The code in this repository is Apache-2.0. **The Solador name and mark are
not** — Apache-2.0 §6 grants no trademark rights, and this section is that
clause in plain words.

You may:

- use the name and mark to *refer* to Solador — a blog post, a comparison, a
  talk, a link, a package that integrates with it;
- redistribute an unmodified build carrying its own mark.

Please don't:

- put the mark on a fork, a modified build, or a product of your own;
- alter the mark's proportions, colours, or geometry and still call it the
  Solador mark;
- use it in a way that implies endorsement or affiliation.

Forks are welcome and the license permits them — just ship them under your own
name. If you want to do something not covered here, ask.

## The mark

A tiler's 3×3 grid with one tile out of true.

The name is Spanish — *solar* (to floor, to pave, to tile) plus the agent
suffix *-dor*: the tradesperson who lays tiles. The known weakness of the name,
recorded when it was chosen, is that it describes the **rendering** and not the
**watching**. The out-of-true tile answers that: the anomaly is the entire
product, and a grid where everything is square says nothing.

Two details are load-bearing. The odd tile is **larger** as well as rotated, so
it survives being small — at 16px the rotation is gone but the size and colour
break still read. And the grid is **polychrome** rather than one colour with an
accent, which is what real azulejo work looks like and what makes the mark
legible on any ground (see below).

## Palette

| Role | Hex | Count | Source |
|---|---|---|---|
| Terracotta — includes the out-of-true tile | `#E0614F` | 2 | `color.rs` `RED` |
| Dark terracotta | `#984236` | 2 | derived † |
| Cobalt | `#1E4FA0` | 2 | brand cobalt ‡ |
| Deep cobalt | `#14346A` | 2 | derived † |
| Amber — the centre tile | `#E0A03A` | 1 | `color.rs` `AMBER` |
| Sand — full-bleed ground, `icon.svg` only | `#F5E6CD` | — | `color.rs` `INK` ※ |

**※ This one value travelled the other way.** Everything else in the table is
the mark adopting the app. `INK` was `#E8E2D4`; it is now the mark's own sand,
because ink is the one colour in `color.rs` that carries no meaning — no
threshold reads it, so nothing about *good/warning/error* moves when it does.
It also reads slightly better: 15.4:1 on `PANEL` against the 14.7:1 it
replaced. `icon.svg`'s ground is `INK` by definition and follows it.

**† The two derived values exist because the app palette has no deep tones to
lend.** `color.rs` is a *UI* palette — semantic good/warning/error plus a few
series hues — and carries no deep blue and no dark terracotta, which a
five-value mosaic needs. Both are their neighbour darkened at constant hue:
`#14346A` is `#1E4FA0` at hue 218°, `#984236` is `#E0614F` at hue 7°. They are
**brand-only** and deliberately not added to `color.rs`; nothing in the UI
needs them.

**‡ The cobalt is derived too, and not arbitrary.** `LINE` (`#1c2b4a`) and
`MUTED` (`#8090ac`) both sit at hue 217°, because every cockpit surface is one
cobalt shaded to a different lightness. `#1E4FA0` is that hue at full
saturation: the colour the app implies everywhere and paints nowhere.

**This file is not the source of truth for UI colour.** `color.rs` is, and two
tests (`the_css_mirror_matches_the_rust_constants`,
`the_crons_spec_mirror_matches_the_rust_constants`) exist precisely because a
second copy of a palette drifts silently. The mark's own arrival demonstrated
it: `mark.svg` and `icon.svg` came from separate vectorisation runs carrying
**six different hex values for the same five colours** — `#D04116`/`#D04117`,
`#992F10`/`#9A3010`, `#024E80`/`#024E81`, `#E38F49`/`#E38F4A` — differences of
one in one channel, invisible on screen and permanent in a diff. If the values
here ever appear to disagree with `color.rs` about a UI colour, `color.rs`
wins.

### Why the mark moved and the app didn't

The mark arrived in its own palette — `#1672B0` / `#024E81` / `#D04117` /
`#9A3010` / `#E38F4A` on sand `#F5E6CD` — sharing nothing with the cockpit.
Both reconciliations were built and rendered before choosing; the app-side one
was applied to `color.rs` and `app/ui/app.css` for real and captured by
`tests/frontend/screenshots.spec.js`, then reverted. Three costs decided it:

- **It halved the separation between *warning* and *error*.** Amber and red sit
  in the same columns and the cockpit is read at a glance from across a room.
  Current: `#E0A03A` at 37° and `#E0614F` at 7°, about 30° apart. The mark's
  equivalents: `#E38F4A` at 27° and `#D04117` at 14°, about 13°.
- **The mark has no green**, and green is *good* on every threshold in
  `usage_color` / `pressure_color` / `volume_color`. One would have had to be
  invented — a poor provenance for a semantic colour.
- **The mark's mid blue would have had nowhere to live.** `color.rs` already
  records why CPU and NET are turquoise: a cobalt ground claims blue for
  itself. A bluer ground strands `#1672B0` entirely.

The asymmetry underneath all three: the mark is decoration and costs six
replacements in two files; the app palette encodes meaning a user acts on, is
mirrored into two other files under test, and is load-bearing. The cheap,
reversible, meaning-free side is the one that should move.

## Files

| File | For |
|---|---|
| `mark.svg` | the mark, transparent ground, fills 94% of a 1024 square |
| `mark-mono.svg` | one colour via `currentColor`; **only** resolves when inlined, not through `<img>` |
| `icon.svg` | app-icon source: sand ground, full-bleed 1024, mark at 84%, no corner radius |

**There is no dark variant, deliberately.** Per-tile contrast against each
ground:

| Tile | white | `#0d1117` | `#0B1020` | black |
|---|---|---|---|---|
| Terracotta `#E0614F` | 3.50 | 5.40 | 5.40 | 5.99 |
| Dark terracotta `#984236` | 6.63 | 2.85 | 2.86 | 3.17 |
| Cobalt `#1E4FA0` | 7.83 | 2.42 | 2.42 | 2.68 |
| Deep cobalt `#14346A` | **12.15** | **1.56** | **1.56** | 1.73 |
| Amber `#E0A03A` | **2.27** | **8.34** | 8.35 | 9.26 |

Read as a prediction of failure, that table condemns both grounds — deep cobalt
at 1.56:1 on dark, amber at 2.27:1 on light. Rendered, neither happens: the
mark reads cleanly on white, `#0d1117`, `#0B1020` and pure black, down to 16px.

A per-tile ratio measures a tile against the **ground**, which is the right
question for a single-colour mark and the wrong one for a mosaic, where each
tile is read against its **neighbours**. The weak tile simply changes sides —
deep cobalt on dark, amber on light — and in both cases the eight tiles around
it carry the grid. Lifting the dim tile would also collapse the deep cobalt
into the cobalt, destroying the thing that makes it a mosaic.

So `mark.svg` goes on light *and* dark; a README needs no `<picture>` switch.
**Verify this by rendering, not by the table** — the table is exactly the
evidence that would talk you out of a mark that works.

`icon.svg` deliberately carries **no** corner radius. Windows wants the full
square; macOS wants a rounded rect drawn into the artwork. Applying the radius
is therefore a **requirement on the generator** described under *Open*, not
something this file does — a radius here would end up applied twice. Until that
generator exists, the shipped icons are square on macOS.

The two fill percentages differ on purpose: `mark.svg` is near-tight because
surrounding layout supplies its whitespace, while `icon.svg`'s ground *is* the
icon tile and its artwork wants inset, at roughly the macOS convention.

The mark paths are wrapped in a single `<g transform="translate(…) scale(…)">`
that does the framing. Path data is untouched — deleting the group restores
exactly what the generator emitted, and a reviewer reads two numbers instead of
9 KB of re-emitted coordinates.

## Derived files — do not edit by hand

Run **`./Scripts/generate-icons.sh`** after changing `icon.svg` or `mark.svg`.

| Path | Consumer | Why it can't live here |
|---|---|---|
| `app/src-tauri/icons/` | Tauri | the path is fixed in `tauri.conf.json` |
| `app/ui/mark.svg` | the frontend | dist root is `app/ui`, and the CSP is `img-src 'self' data:` |
| `DevCanopy/Assets.xcassets/AppIcon.appiconset/` | Xcode | asset catalogues are a required layout |
| `Docs/assets/screenshots/` | README | generated by the Playwright suite, not authored |

`app/ui/mark.svg` is guarded by `the_frontend_mark_matches_the_brand_mark`, so a
forgotten regeneration fails `./dev test` rather than shipping the previous
mark. The icon set is **not** guarded — a binary diff against a re-render is not
reproducible enough to assert on — so it is the one output where forgetting to
run the script is silent.

### The generator

It rasterises with **Chromium**, not ImageMagick, and that is not a preference.
IM's configured `rsvg-convert` delegate is absent on a stock macOS box, so it
falls back to MSVG — its own incomplete SVG renderer — which mishandles
`transform="rotate(angle cx cy)"`. The mark is built from exactly that, and MSVG
puts the out-of-true tile in the wrong grid cell. You get a plausible icon that
is not the mark.

Every size is rendered from the vector at its native size rather than downscaled
from one master, and the macOS inset (80.47%) and corner radius (22.37%) are
applied there — which is why `icon.svg` carries no radius of its own.

## Open

- **No wordmark.** One would need a typeface decision that hasn't been made,
  and a fabricated placeholder is worse than an absence — it gets used.
- **The frozen Swift app still carries the old icon.**
  `DevCanopy/Assets.xcassets/AppIcon.appiconset/` was not regenerated —
  `generate-icons.sh` targets the Tauri app only. That app is frozen and not
  built in CI, so this is deliberate rather than missed.
- **`bundle.active` is `false`,** so `./dev run --tauri` launches a bare binary
  with no `.app` around it and macOS shows a generic Dock icon in the dev loop
  regardless of the `.icns`. The icon set is correct and will be picked up the
  moment bundling is turned on for a release; it is simply not visible yet on
  the platform most of this work happens on.
- **Provenance stripped.** These files were generated and carried C2PA
  content-credential manifests; those were removed (67% and 36% of file size).
  If the repo ever wants to assert generation provenance publicly, it has to be
  re-attached at the source — it cannot be reconstructed from here.
