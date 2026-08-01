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
 * `fixture` is the offline fallback (`cargo run -p devcanopy-app -- --dump*`):
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

const CHARTS = new Map();

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
      rules.push(`@container cores (min-width: ${Number(r.minWidth) | 0}px){.cores[data-n="${n | 0}"]{grid-template-columns:repeat(${Number(r.cols) | 0},1fr)}}`);
    }
  }
  coreLadderSheet.replaceSync(rules.join("\n"));
}

function render(card, d) {
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
  volsEl.innerHTML = d.volumes.map((v) =>
    `<div class="vol"><div class="top"><span class="mount">${esc(v.mount)}</span><span class="detail"></span></div><div class="bar"><span></span></div></div>`
  ).join("");
  volsEl.querySelectorAll(".vol").forEach((el, i) => {
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
function drawCard(card, d) {
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

  if (d.error) {
    // No prior sample exists yet (still connecting, or every attempt has
    // failed): the cause, never fabricated numbers.
    f(card, "hostName").textContent = d.error.hostName;
    f(card, "cpuValue").textContent = "—";
    f(card, "cpuModel").textContent = d.error.message;
    f(card, "cpuModel").style.color = d.connection ? d.connection.color : "#e05a4f";
    stale.textContent = "";
    return;
  }
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
  render(card, d);
}

// Host id -> its card element. Cards are reused across polls so each card
// keeps its chart nodes (and their history) instead of being rebuilt from
// scratch every tick.
const CARDS = new Map();

/**
 * The whole page, from one `cockpit` payload.
 *
 * The grid's column count is applied, never computed: `hostColumns` is
 * `viewmodel::cockpit::host_columns` — the same breakpoint math the SwiftUI
 * cockpit uses — and re-deriving it here from `hostCardMinWidth` would be a
 * second implementation free to disagree with the tested one.
 */
function renderCockpit(p) {
  const grid = $("cockpit");
  document.documentElement.style.setProperty("--cockpit-gap", Number(p.spacing) + "px");
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

  p.hosts.forEach((h, i) => {
    let card = CARDS.get(h.id);
    if (!card) {
      card = $("cardTpl").content.firstElementChild.cloneNode(true);
      CARDS.set(h.id, card);
    }
    // Cheap and idempotent: a no-op while the order is stable, and a real
    // move the moment the payload reorders (a rename re-sorts the list).
    if (grid.children[i] !== card) grid.insertBefore(card, grid.children[i] || null);
    drawCard(card, h);
  });
}

// Test-only introspection (tests/frontend/layout.spec.js's leak-regression
// check). `renderCockpit` and `CHARTS` are otherwise unreachable from outside
// this closure -- this is what lets a test drive many renders directly and
// assert chart bookkeeping stays flat, instead of waiting on real poll ticks.
// Read-only, no production behaviour depends on it.
window.__DEVCANOPY_TEST__ = { render: renderCockpit, chartCount: () => CHARTS.size };

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
    pre.style.color = "#e05a4f";
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
