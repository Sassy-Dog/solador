// The entire frontend. No npm, no bundler, no framework: `viewmodel` has
// already decided every string and colour, so this only paints.

const $ = (id) => document.getElementById(id);
// Every field hook inside a card is a class, not an id -- with N host cards on
// the page an id would be duplicated N times and `getElementById` would answer
// with whichever card rendered first, silently painting one host's numbers
// onto another's card.
const f = (card, name) => card.querySelector("." + name);

// Host names, CPU models, mount paths and process names arrive from a REMOTE
// agent. A webview parses markup, and in Tauri the DOM can call `invoke`, so an
// unescaped `<img onerror=...>` would reach the Rust command surface.
const esc = (v) =>
  String(v).replace(/[&<>"']/g, (ch) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[ch]));

/**
 * The one way this frontend talks to Rust.
 *
 * `fixture` is the offline fallback (`cargo run -p solador-app -- --dump*`):
 * the same payload shape the command returns, so that path can't diverge from
 * the real one. See app/README.md's smoke test for why every fixture must be
 * deleted before exercising the IPC boundary by hand. A command with no
 * fixture (every mutation) simply does nothing outside Tauri -- a settings
 * edit has nowhere to go in a plain browser, and pretending otherwise would
 * show a saved-looking form that saved nothing.
 */
const callRust = async (command, args, fixture) => {
  if (window.__TAURI__) return await window.__TAURI__.core.invoke(command, args);
  if (!fixture) return null;
  return await (await fetch(fixture)).json();
};

// Set by settings.js. While the Settings view is up the cockpit is off-screen,
// so re-rendering it would only measure a zero width; Rust keeps polling
// regardless, so nothing is missed. settings.js calls `refreshCockpit()` on
// close to repaint at the real width immediately rather than up to a tick late.
let settingsOpen = false;
let refreshCockpit = async () => {};

// Panels poll on their own timers and skip the work while Settings is up, so
// closing it used to leave every one of them stale for a full slow-cadence
// tick. `refreshCockpit` only repaints the host cards -- the panels are
// separate. Each registers here; `closeSettings` brings them all current at
// once, which is what makes a setting you just saved visible immediately
// rather than up to ten seconds later still showing its own setup
// instruction.
const PANEL_REFRESHERS = [];
const registerPanelRefresh = (fn) => PANEL_REFRESHERS.push(fn);
// allSettled, not all: one panel whose provider is down must not stop the rest
// from repainting.
const refreshPanels = async () => {
  await Promise.allSettled(PANEL_REFRESHERS.map((fn) => fn()));
};

const CHARTS = new Map();

// Per-paint unique gradient ids: `url(#id)` resolves document-wide, so every
// svg must reference only its own defs or a repaint of one chart could orphan
// another's fill.
let gradSeq = 0;

// A `style-src 'self'` CSP (no 'unsafe-inline') blocks `<style>` elements
// outright, even an empty one created before its content is set — Tauri's
// nonce injection only stamps <style> tags already present in the built
// HTML, and this one is built at runtime. A *constructable* stylesheet is
// exempt: it's reachable only via script, the same trust boundary CSP's
// script-src already governs, so style-src doesn't re-gate it.
const coreLadderSheet = new CSSStyleSheet();
document.adoptedStyleSheets = [...document.adoptedStyleSheets, coreLadderSheet];

function paint(el) {
  const spec = CHARTS.get(el);
  if (!spec) return;
  const w = Math.max(1, Math.round(el.clientWidth));
  const h = Math.max(1, Math.round(el.clientHeight));
  const { series, lo, hi, grid, pxPerSample, retained } = spec;
  const visible = Math.min(retained, Math.max(2, Math.floor(w / pxPerSample)));
  const parts = [];
  const defs = [];
  if (grid) {
    for (const fr of [0, 0.5, 1]) {
      const y = (fr * 100).toFixed(2);
      parts.push(`<line x1="0" y1="${y}" x2="${w}" y2="${y}" stroke="var(--line)" stroke-width="0.5" vector-effect="non-scaling-stroke"/>`);
    }
  }
  for (const sr of series) {
    const all = sr.values || [];
    if (all.length < 2) continue;
    const win = all.slice(Math.max(0, all.length - visible));
    const span = Math.max(hi - lo, 1e-4);
    // Parity with the original (Sparkline): the newest sample is pinned at the
    // right edge and points step left by a FIXED step derived from the
    // window this width can show — a filling history grows in from the
    // right at constant density instead of stretching across the full
    // width and then snapping to a slide once the buffer fills. When
    // win.length === visible the two formulas agree exactly.
    const step = visible > 1 ? w / (visible - 1) : 0;
    const pts = win.map((v, i) => {
      const y = 100 - Math.min(Math.max((v - lo) / span, 0), 1) * 100;
      const x = w - (win.length - 1 - i) * step;
      return `${x.toFixed(2)},${y.toFixed(2)}`;
    }).join(" ");
    // Parity with the original (Sparkline): a 0.28 -> 0 fade of the series colour
    // under the line. userSpaceOnUse over the full 0..100 viewBox height,
    // because the original's LinearGradient spans the whole chart frame — the
    // default objectBoundingBox would compress the fade into a flat line's
    // own few-pixel extent and paint it as a solid band.
    const gid = `spark-fade-${gradSeq++}`;
    defs.push(`<linearGradient id="${gid}" gradientUnits="userSpaceOnUse" x1="0" y1="0" x2="0" y2="100"><stop offset="0" stop-color="${esc(sr.color)}" stop-opacity="0.28"/><stop offset="1" stop-color="${esc(sr.color)}" stop-opacity="0"/></linearGradient>`);
    const x0 = (w - (win.length - 1) * step).toFixed(2);
    parts.push(`<polygon points="${x0},100 ${pts} ${w},100" fill="url(#${gid})"/>`);
    parts.push(`<polyline points="${pts}" fill="none" stroke="${esc(sr.color)}" stroke-width="1.5" vector-effect="non-scaling-stroke" stroke-linejoin="round"/>`);
  }
  // x in real pixels, y normalised: a wider chart shows MORE TIME, not a
  // stretched line. A symmetric viewBox is what causes stretching.
  el.innerHTML = `<svg viewBox="0 0 ${w} 100" preserveAspectRatio="none" width="${w}" height="${h}" role="img" aria-label="metric history, ${visible} samples"><defs>${defs.join("")}</defs>${parts.join("")}</svg>`;
}

const chartObserver = new ResizeObserver((es) => { for (const e of es) paint(e.target); });

function spark(el, series, lo, hi, capacity, grid = true) {
  if (el.dataset.h) el.style.height = Number(el.dataset.h) + "px";
  CHARTS.set(el, { series, lo, hi, grid, pxPerSample: window.__PX || 4, retained: capacity });
  paint(el);
  chartObserver.observe(el);
}

/** Drops every chart inside `root` from the bookkeeping the observer holds. */
function forgetCharts(root) {
  for (const el of root.querySelectorAll(".plot")) {
    CHARTS.delete(el);
    chartObserver.unobserve(el);
  }
}

/**
 * Rust computes which column counts leave a full last row; CSS distributes.
 *
 * One sheet for the whole page, so the rules are keyed by core count: two
 * hosts with different core counts need different ladders, and the container
 * query alone can't tell their `.cores` grids apart. `core_column_ladder` is a
 * pure function of the core count, so one rung set per distinct count is
 * exactly right — no per-card sheet needed.
 */
function installCoreLadders(hosts) {
  const rules = [];
  const seen = new Set();
  for (const h of hosts) {
    const n = (h.cores || []).length;
    if (!h.coreLadder || seen.has(n)) continue;
    seen.add(n);
    // Rust-generated today, not user text, but this is still the one
    // string->CSS sink with no `esc()` between it and the source: coerce to
    // integers rather than trust the shape of the JSON.
    for (const r of h.coreLadder) {
      // Guarded, not coerced: Number(undefined)|0 is 0, and a rung from a
      // stale payload without `height` must fall back to the --core-block-h
      // base, not collapse the grid to 0px.
      const rungH = Number(r.height);
      const height = Number.isFinite(rungH) && rungH > 0 ? `;height:${rungH | 0}px` : "";
      rules.push(`@container cores (min-width: ${Number(r.minWidth) | 0}px){.cores[data-n="${n | 0}"]{grid-template-columns:repeat(${Number(r.cols) | 0},1fr)${height}}}`);
    }
  }
  coreLadderSheet.replaceSync(rules.join("\n"));
}

/** `volumeSlots` is the payload's cross-card number, not this host's — a card
 *  cannot see its neighbours, so the count of tiles it reserves has to arrive
 *  from above. `0` (stacked, or tabs) means reserve nothing. */
function render(card, d, volumeSlots) {
  const r = document.documentElement.style;
  for (const [k, v] of Object.entries(d.theme)) {
    r.setProperty("--" + (k === "netUp" ? "netup" : k), v);
  }
  window.__PX = d.pxPerSample;
  r.setProperty("--core-block-h", d.coreBlockHeight + "px");

  f(card, "hostName").textContent = d.hostName;
  f(card, "cpuModel").textContent = d.cpuModel;
  f(card, "cpuValue").textContent = d.cpuValue;
  f(card, "cpuValue").style.color = d.cpuValueColor;
  const th = f(card, "thermal");
  th.textContent = d.thermalText;
  th.style.color = d.thermalColor;
  th.style.background = d.thermalColor + "22";

  spark(f(card, "cpuChart"), [{ values: d.cpuHistory, color: d.theme.cpu }], 0, 100, d.capacity);

  // No `style=""` here: a `style-src 'self'` CSP blocks inline style
  // attributes just as it blocks `<style>` elements. The value's colour is
  // data-driven (usage-dependent), so it's set below via `.style.color` on
  // the created node — the CSSOM setter, unlike the attribute, is exempt.
  //
  // Rebuild the cell markup only when the core count itself changes (a real
  // host doesn't grow/shrink cores between polls, so in practice this means
  // "once per card"). Every OTHER poll used to replace all 16 cells'
  // innerHTML unconditionally, discarding the old `.plot` nodes while CHARTS
  // (a strong Map) and chartObserver kept holding onto them — ~32 detached
  // nodes leaked per poll, indefinitely, in an app meant to run full-screen
  // forever. The count is tracked per card (`data-n`), not in one module-level
  // variable: with N cards a shared counter matches on the first card and
  // then skips the rebuild every other card needs.
  const coresEl = card.querySelector(".cores");
  if (String(d.cores.length) !== coresEl.dataset.n) {
    forgetCharts(coresEl);
    coresEl.innerHTML = d.cores.map((c) =>
      `<div class="core"><div class="cap">${esc(c.label)}<b></b></div><div class="plot"></div></div>`
    ).join("");
    coresEl.dataset.n = d.cores.length;
  }
  coresEl.querySelectorAll(".core").forEach((el, i) => {
    const core = d.cores[i];
    const val = el.querySelector(".cap b");
    val.textContent = core.value;
    val.style.color = core.valueColor;
    spark(el.querySelector(".plot"), [{ values: core.history, color: core.hue }], 0, 100, d.capacity, false);
  });

  f(card, "memValue").textContent = d.memValue;
  spark(f(card, "memChart"), [{ values: d.memHistory, color: d.theme.mem }], 0, 100, d.capacity);
  f(card, "swapText").textContent = d.swapText;
  f(card, "pressureText").textContent = d.pressureText;
  f(card, "pressureText").style.color = d.pressureColor;

  f(card, "gpuValue").textContent = d.gpuValue;
  f(card, "gpuValue").style.color = d.gpuValueColor;
  spark(f(card, "gpuChart"), [{ values: d.gpuHistory, color: d.theme.gpu }], 0, 100, d.capacity);
  f(card, "vramText").textContent = d.vramText;

  f(card, "diskRead").textContent = d.diskRead;
  f(card, "diskWrite").textContent = d.diskWrite;
  f(card, "diskAxis").textContent = d.diskAxis;
  spark(f(card, "diskChart"), [
    { values: d.diskReadHistory, color: d.theme.read },
    { values: d.diskWriteHistory, color: d.theme.write },
  ], 0, d.diskMax, d.capacity);

  f(card, "netDown").textContent = d.netDown;
  f(card, "netUp").textContent = d.netUp;
  f(card, "netAxis").textContent = d.netAxis;
  spark(f(card, "netChart"), [
    { values: d.netDownHistory, color: d.theme.net },
    { values: d.netUpHistory, color: d.theme.netUp },
  ], 0, d.netMax, d.capacity);

  f(card, "volumeCount").textContent = d.volumeCount;
  // Same CSP constraint as the core cells above: fraction-driven width and
  // tint are set via CSSOM after creation, never as a `style=""` attribute.
  const volsEl = f(card, "volumes");
  // Reserved slots keep this block the same height as its neighbours': the
  // cards in a row are equal width, so an equal tile *count* gives them the
  // same auto-fit column count and therefore the same number of rows. The
  // padding tiles carry the real tile's markup — a non-breaking space in
  // `.mount` so the line box exists — which is what makes the two measure
  // identically without a height constant to keep in step with the CSS.
  const pad = Math.max(0, (Number(volumeSlots) | 0) - d.volumes.length);
  volsEl.innerHTML = d.volumes.map((v) =>
    `<div class="vol"><div class="top"><span class="mount">${esc(v.mount)}</span><span class="detail"></span></div><div class="bar"><span></span></div></div>`
  ).join("") + `<div class="vol pad" aria-hidden="true"><div class="top"><span class="mount">&nbsp;</span><span class="detail"></span></div><div class="bar"><span></span></div></div>`.repeat(pad);
  // `:not(.pad)` is load-bearing: the reserved tiles have no entry in
  // `d.volumes`, so a bare `.vol` walk would index past its end and throw.
  volsEl.querySelectorAll(".vol:not(.pad)").forEach((el, i) => {
    const v = d.volumes[i];
    const detail = el.querySelector(".detail");
    detail.textContent = v.detail;
    detail.style.color = v.tint;
    const fill = el.querySelector(".bar > span");
    fill.style.width = (v.fraction * 100).toFixed(1) + "%";
    fill.style.background = v.tint;
  });

  const procs = (list) => list.map((p) =>
    `<div class="proc"><span>${esc(p.name)}</span><span class="v">${esc(p.value)}</span></div>`
  ).join("");
  f(card, "topCpu").innerHTML = procs(d.topCpu);
  f(card, "topRam").innerHTML = procs(d.topRam);
}

/** One host's card: the connection badge, then either the error or the data. */
function drawCard(card, d, volumeSlots) {
  // The dot's colour and the "connecting"/"live"/"stale"/"failed" state
  // both come from Rust (`viewmodel::color`), never chosen here — same
  // discipline as every other colour in the card. `data-state` (not a
  // `style=""` attribute) is what the connection-state Playwright test
  // reads to confirm the transition happened.
  const dot = f(card, "connDot");
  const stale = f(card, "staleMsg");
  if (d.connection) {
    dot.style.background = d.connection.color;
    dot.dataset.state = d.connection.state;
    // Mirrored onto the card so a per-host state is addressable from the
    // outside without reaching into the header.
    card.dataset.state = d.connection.state;
  }

  const down = f(card, "card-down");
  if (d.error) {
    // Nothing worth plotting: still connecting, every attempt has failed, or
    // the host answered once and can no longer be reached. The cause, never
    // fabricated numbers — and never the *last* numbers either, which is what
    // the `.card[data-state]` rule below the header hides. Cards are reused
    // across polls to keep their chart history, so without that rule a host
    // that went down would sit there wearing the readings it had when it did.
    f(card, "hostName").textContent = d.error.hostName;
    f(card, "cpuValue").textContent = "—";
    f(card, "cpuModel").textContent = "";
    down.textContent = d.error.message;
    down.style.color = d.connection ? d.connection.color : "var(--red)";
    down.hidden = false;
    stale.textContent = "";
    return;
  }
  down.textContent = "";
  down.hidden = true;
  f(card, "cpuModel").style.color = "";

  // A snapshot exists but the latest poll failed: this is real, still
  // rendered, data -- just not current. Say so plainly rather than let a
  // stale reading sit there looking exactly like a live one.
  if (d.connection && d.connection.state === "stale") {
    stale.textContent = d.connection.message;
    stale.style.color = d.connection.color;
  } else {
    stale.textContent = "";
    stale.style.color = "";
  }
  render(card, d, volumeSlots);
}

// Host id -> its card element. Cards are reused across polls so each card
// keeps its chart nodes (and their history) instead of being rebuilt from
// scratch every tick.
const CARDS = new Map();

/**
 * Which `<section>` each panel id owns.
 *
 * `hosts` is absent on purpose — it is the grid above this block, not a section.
 * Rust still sends its row; an id with no section here contributes nothing, and
 * a row that ends up empty is skipped rather than rendered as a gap.
 */
const PANEL_SECTIONS = {
  containers: "containersPanel",
  ghWorkflows: "reposPanel",
  ghRunners: "runnersPanel",
  claudeUsage: "usagePanel",
  azureCost: "azurePanel",
  services: "servicesPanel",
  sentryCrons: "cronsPanel",
  openclawAgents: "openclawPanel",
};

// The row shape currently on screen. Rebuilding the containers moves the
// sections, which restarts CSS animations (the needs-approval dot pulses) and
// throws away scroll position — so it happens on a real reflow and not on every
// one-second poll.
let panelRowShape = "";

/**
 * Arranges the panel sections into rows.
 *
 * The arrangement is `viewmodel::cockpit`'s `hosts_forward` layout reflowed for
 * the measured width (`panelRows` in the payload) — applied here, never decided
 * here. A CSS `repeat(auto-fit, minmax(…))` over these panels would be a second
 * implementation of every panel's own `min_width`, and it would get the case
 * that matters wrong: OpenClaw + Usage stay paired at a width where Repos +
 * Runners must split, which no single global breakpoint can express.
 *
 * Every row is the same four quarter tracks (`.panel-row` in the stylesheet),
 * and a panel claims its `weight` of them with `grid-column`. The weights are
 * Rust's for the same reason the rows are: `panel_widths` derived every `width`
 * and `columns` in the payload from this exact grid, so a track re-derived here
 * would be free to disagree with the width the panel was told it has.
 *
 * The track list used to be per-row — `2fr 1fr 1fr` for a half beside two
 * quarters — which looks equivalent and is not. `fr` splits what is left after
 * the row's *own* gutters, so a two-panel row gave its halves `(W-g)/2` and a
 * three-panel row gave its half `(W-2g)/2`. Same span, 8pt apart on the shipped
 * cockpit, and the edge between Repos and Runners missed the edge between
 * Containers and OpenClaw directly below it.
 *
 * The start line is explicit rather than left to auto-placement. A panel with no
 * section here (`hosts`, rendered in the grid above) or one still `hidden` on a
 * cold start is skipped by auto-placement entirely, and its neighbours would
 * slide up into tracks Rust never gave them — painting them at a width that
 * contradicts the `columns` their contents were laid out for. A gap is the
 * honest rendering of a slot this container is not filling.
 */
function applyPanelRows(rows) {
  if (!Array.isArray(rows) || !rows.length) return;
  applyPanelColumns(rows);
  const shape = JSON.stringify(
    rows.map((row) => (row || []).map((p) => p && [p.id, p.weight]))
  );
  if (shape === panelRowShape) return;
  panelRowShape = shape;

  const built = [];
  for (const row of rows) {
    // Sections and their tracks are paired off before either is used, and the
    // start line advances across the whole row — including the panels this
    // container has no section for, whose quarters are still spoken for.
    const placed = [];
    let start = 1;
    for (const panel of row || []) {
      const weight = Math.min(4, Math.max(1, Number(panel && panel.weight) | 0));
      const section = panel && PANEL_SECTIONS[panel.id] ? $(PANEL_SECTIONS[panel.id]) : null;
      if (section) placed.push([section, `${start} / span ${weight}`]);
      start += weight;
    }
    if (!placed.length) continue;
    const container = document.createElement("div");
    container.className = "panel-row";
    for (const [section, column] of placed) {
      // CSSOM, not a `style=""` attribute: a `style-src 'self'` CSP blocks the
      // attribute outright. Same setter the host grid's column count uses.
      section.style.gridColumn = column;
    }
    // `append` MOVES the existing sections, so each panel keeps whatever its
    // own script last painted into it — nothing is rebuilt, only re-parented.
    container.append(...placed.map(([section]) => section));
    built.push(container);
  }
  // Safe after the moves above: the old containers are empty by now.
  $("panelRows").replaceChildren(...built);
}

/**
 * Hands each panel the content-column count Rust derived for its width.
 *
 * Deliberately OUTSIDE `applyPanelRows`'s shape memo: the memo skips the
 * rebuild when the same panels still share the same rows, but a window resize
 * changes their *width* without changing that shape — so a count applied
 * inside it would freeze at whatever the last reflow happened to see.
 *
 * The count lands as a custom property, which is enough on its own for the
 * panels whose split is pure CSS (Containers' section grid, Runners'
 * multi-column list). A panel that has to rebuild its DOM to re-split (Repos,
 * whose columns each carry their own header) also gets an event, so it
 * re-renders now rather than at the far end of its own 10s poll.
 */
function applyPanelColumns(rows) {
  for (const row of rows) {
    for (const panel of row || []) {
      const section = panel && PANEL_SECTIONS[panel.id] ? $(PANEL_SECTIONS[panel.id]) : null;
      if (!section) continue;
      const cols = Math.max(1, Number(panel.columns) | 0);
      if (Number(section.dataset.cols) === cols) continue;
      section.dataset.cols = String(cols);
      // CSSOM, never a `style=""` attribute -- `style-src 'self'`.
      section.style.setProperty("--panel-cols", String(cols));
      section.dispatchEvent(new CustomEvent("panelcolumns", { detail: { columns: cols } }));
    }
  }
}

// Which host the tab bar has selected, by id. Module-level so it survives every
// poll: the payload is rebuilt once a second, and a selection stored on the DOM
// would be reset by the next render.
let selectedHost = null;

/**
 * The host tab bar — the "Show as tabs" overflow mode.
 *
 * Applied, never decided: `hostTabs` is null unless
 * `viewmodel::cockpit::host_tabs` says the cards collapse (below the
 * side-by-side breakpoint, more than one host, and the preference set), and a
 * JS `columns <= 1 && hosts > 1` here would be that rule restated where no Rust
 * test can see it. The labels are the cards' own host names, from the same
 * payload.
 *
 * Returns the id of the one card that stays visible, or null when every card
 * does.
 */
function applyHostTabs(spec, grid) {
  const bar = $("hostTabs");
  if (!spec) {
    // Back to the stacked/gridded case: every card visible, no floor.
    bar.replaceChildren();
    bar.hidden = true;
    grid.style.minHeight = "";
    return null;
  }

  const tabs = spec.tabs || [];
  // A selection whose host has left the payload would hide every card. Fall
  // back to the first tab, which is this machine — always present.
  if (!tabs.some((t) => t.id === selectedHost)) {
    selectedHost = tabs.length ? tabs[0].id : null;
  }

  bar.replaceChildren();
  for (const t of tabs) {
    // createElement + textContent, not innerHTML: a host name comes from user
    // input and reaches the DOM as text, never as markup. Same rule as
    // settings.js.
    const b = document.createElement("button");
    b.type = "button";
    b.className = "btn tab";
    b.textContent = t.label;
    b.dataset.host = t.id;
    if (t.id === selectedHost) b.dataset.active = "true";
    // A tab bar shows one card and hides the rest, so a host that goes down
    // while you are looking at another one has nothing on screen but this
    // button. `alert` is Rust's verdict, not a state string re-read here, and
    // the colour comes with it — CSSOM, never a `style=""` attribute.
    if (t.alert) {
      b.dataset.alert = "true";
      if (t.color) b.style.color = t.color;
    }
    b.addEventListener("click", () => {
      selectedHost = t.id;
      // Repaint now rather than on the next poll: a tab that takes up to a
      // second to switch reads as a dead button.
      refreshCockpit();
    });
    bar.appendChild(b);
  }
  bar.hidden = tabs.length === 0;
  // The container's floor is Rust's (`HOST_TABS_MIN_HEIGHT`, mirroring
  // HostsPanel's `.frame(minHeight: 780)`): one card is on screen at a time, so
  // without it the grid is only ever as tall as whichever host you picked, and
  // every panel below jumps when you pick another. CSSOM, not a `style=""`
  // attribute — `style-src 'self'` blocks the attribute outright.
  grid.style.minHeight = Number(spec.minHeight) + "px";
  return selectedHost;
}

/**
 * The whole page, from one `cockpit` payload.
 *
 * The grid's column count is applied, never computed: `hostColumns` is
 * `viewmodel::cockpit::host_columns` — the same breakpoint math the original
 * cockpit uses — and re-deriving it here from `hostCardMinWidth` would be a
 * second implementation free to disagree with the tested one.
 */
function renderCockpit(p) {
  const grid = $("cockpit");
  document.documentElement.style.setProperty("--cockpit-gap", Number(p.spacing) + "px");
  // The operator's row spacing, over the stylesheet's pre-payload value. Guarded
  // because an absent key would set the property to the string "NaNpx" — a legal
  // custom-property token, so `var(--row-gap)` resolves to it and only fails at
  // substitution time, leaving every list at `gap:normal`. That renders 0 and
  // looks deliberate. Saying nothing leaves app.css's value standing, which is
  // the honest answer to "we were not told".
  if (Number.isFinite(p.rowGapPx)) {
    document.documentElement.style.setProperty("--row-gap", p.rowGapPx + "px");
  }
  const cols = Math.max(1, Number(p.hostColumns) | 0);
  grid.style.gridTemplateColumns = `repeat(${cols}, minmax(0, 1fr))`;

  const hostsPanel = (p.panels || []).find((panel) => panel.id === "hosts");
  $("hostsTitle").textContent = hostsPanel ? hostsPanel.title : "";

  // The Settings button's label is Rust's too, and it arrives here because the
  // button has to exist before anything has asked for the settings payload.
  const toggle = $("settingsToggle");
  toggle.textContent = p.settingsLabel || "";
  toggle.hidden = !p.settingsLabel;

  const emptyEl = $("emptyMsg");
  emptyEl.textContent = p.empty ? p.empty.message : "";
  emptyEl.hidden = !p.empty;

  applyPanelRows(p.panelRows);

  installCoreLadders(p.hosts);

  // Cards for hosts that are gone must take their chart bookkeeping with
  // them: CHARTS is a strong Map and chartObserver holds its targets, so a
  // removed card that isn't pruned leaks every plot node it owned.
  const live = new Set(p.hosts.map((h) => h.id));
  for (const [id, el] of CARDS) {
    if (live.has(id)) continue;
    forgetCharts(el);
    el.remove();
    CARDS.delete(id);
  }

  // Null unless the cards collapsed into tabs, in which case exactly one of
  // them stays on the page.
  const shown = applyHostTabs(p.hostTabs, grid);

  p.hosts.forEach((h, i) => {
    let card = CARDS.get(h.id);
    if (!card) {
      card = $("cardTpl").content.firstElementChild.cloneNode(true);
      CARDS.set(h.id, card);
    }
    // Cheap and idempotent: a no-op while the order is stable, and a real
    // move the moment the payload reorders (a rename re-sorts the list).
    if (grid.children[i] !== card) grid.insertBefore(card, grid.children[i] || null);
    // Hidden, not removed: a card torn down on every tab switch would lose its
    // chart nodes and therefore its sparkline history, which is the one thing
    // the cockpit is worth looking at for. `drawCard` still runs, so the
    // off-screen cards keep recording and are current the moment they are
    // shown -- and their charts repaint from the ResizeObserver.
    card.hidden = shown !== null && h.id !== shown;
    drawCard(card, h, p.volumeSlots);
  });
}

// Test-only introspection (tests/frontend/layout.spec.js's leak-regression
// check). `renderCockpit` and `CHARTS` are otherwise unreachable from outside
// this closure -- this is what lets a test drive many renders directly and
// assert chart bookkeeping stays flat, instead of waiting on real poll ticks.
// Read-only, no production behaviour depends on it.
window.__SOLADOR_TEST__ = { render: renderCockpit, chartCount: () => CHARTS.size };

(async () => {
  // The width the host grid actually has. Rust turns it into a column count;
  // an unmeasured 0 stacks there rather than assuming wide.
  const gridWidth = () => $("cockpit").clientWidth;

  const poll = async () =>
    renderCockpit(await callRust("cockpit", { width: gridWidth() }, "sample.json"));

  refreshCockpit = async () => { try { await poll(); } catch {} };

  try { await poll(); } catch (e) {
    // CSSOM setters, not a `style=""` attribute: this string embeds `esc(e)`
    // and runs under the same `style-src 'self'` CSP as everywhere else in
    // this file (see the top-of-file note and `render()`'s volume bars).
    document.body.innerHTML = `<pre>failed to load cockpit: ${esc(e)}</pre>`;
    const pre = document.body.querySelector("pre");
    pre.style.color = "var(--red)";
    pre.style.padding = "20px";
  }
  if (window.__TAURI__) {
    setInterval(async () => { if (!settingsOpen) await refreshCockpit(); }, 1000);
    // A resize changes the width Rust decides the column count from, so ask
    // again rather than waiting up to a full poll interval to reflow.
    let pending = false;
    window.addEventListener("resize", () => {
      if (pending || settingsOpen) return;
      pending = true;
      requestAnimationFrame(async () => { pending = false; await refreshCockpit(); });
    });
  }
})();
