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
  // Rust-generated today, not user text, but this is still the one
  // string->CSS sink with no `esc()` between it and the source: coerce to
  // integers rather than trust the shape of the JSON.
  const rules = ladder.map((r) =>
    `@container cores (min-width: ${Number(r.minWidth) | 0}px){.cores{grid-template-columns:repeat(${Number(r.cols) | 0},1fr)}}`
  ).join("\n");
  coreLadderSheet.replaceSync(rules);
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

  // No `style=""` here: a `style-src 'self'` CSP blocks inline style
  // attributes just as it blocks `<style>` elements. The value's colour is
  // data-driven (usage-dependent), so it's set below via `.style.color` on
  // the created node — the CSSOM setter, unlike the attribute, is exempt.
  $("cores").innerHTML = d.cores.map((c) =>
    `<div class="core"><div class="cap">${esc(c.label)}<b></b></div><div class="plot"></div></div>`
  ).join("");
  document.querySelectorAll("#cores .core").forEach((el, i) => {
    const core = d.cores[i];
    const val = el.querySelector(".cap b");
    val.textContent = core.value;
    val.style.color = core.valueColor;
    spark(el.querySelector(".plot"), [{ values: core.history, color: core.hue }], 0, 100, d.capacity, false);
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
  // Same CSP constraint as the core cells above: fraction-driven width and
  // tint are set via CSSOM after creation, never as a `style=""` attribute.
  $("volumes").innerHTML = d.volumes.map((v) =>
    `<div class="vol"><div class="top"><span class="mount">${esc(v.mount)}</span><span class="detail"></span></div><div class="bar"><span></span></div></div>`
  ).join("");
  document.querySelectorAll("#volumes .vol").forEach((el, i) => {
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
  $("topCpu").innerHTML = procs(d.topCpu);
  $("topRam").innerHTML = procs(d.topRam);
}

(async () => {
  const load = async () =>
    window.__TAURI__
      ? await window.__TAURI__.core.invoke("snapshot")
      : await (await fetch("sample.json")).json();

  const draw = (d) => {
    // The dot's colour and the "connecting"/"live"/"stale"/"failed" state
    // both come from Rust (`viewmodel::color`), never chosen here — same
    // discipline as every other colour in the card. `data-state` (not a
    // `style=""` attribute) is what the connection-state Playwright test
    // reads to confirm the transition happened.
    const dot = $("connDot");
    const stale = $("staleMsg");
    if (d.connection) {
      dot.style.background = d.connection.color;
      dot.dataset.state = d.connection.state;
    }

    if (d.error) {
      // No prior sample exists yet (still connecting, or every attempt has
      // failed): the cause, never fabricated numbers.
      $("hostName").textContent = d.error.hostName;
      $("cpuValue").textContent = "—";
      $("cpuModel").textContent = d.error.message;
      $("cpuModel").style.color = d.connection ? d.connection.color : "#e05a4f";
      stale.textContent = "";
      return;
    }
    $("cpuModel").style.color = "";

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
    render(d);
  };

  try { draw(await load()); } catch (e) {
    // CSSOM setters, not a `style=""` attribute: this string embeds `esc(e)`
    // and runs under the same `style-src 'self'` CSP as everywhere else in
    // this file (see the top-of-file note and `render()`'s volume bars).
    document.body.innerHTML = `<pre>failed to load snapshot: ${esc(e)}</pre>`;
    const pre = document.body.querySelector("pre");
    pre.style.color = "#e05a4f";
    pre.style.padding = "20px";
  }
  if (window.__TAURI__) setInterval(async () => { try { draw(await load()); } catch {} }, 2000);
})();
