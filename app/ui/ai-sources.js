// The Anthropic and OpenAI panels: each vendor's plan limits, drawn with the
// Machines tile's meters (`meter.js`), and on Anthropic this machine's Claude
// Code token rollups. Same discipline as every other panel here: each string,
// colour and figure arrives from Rust (`usage::anthropic_view`,
// `usage::openai_view`), and this file does layout, wiring, and nothing else.
//
// One renderer, because the two panels are one shape: OpenAI's payload simply
// carries no rollups. Each has its own section, command, timer and `loading`,
// so one still filling in never speeds up the other.
//
// Nothing below uses `innerHTML`. Project names come from directory paths on
// this machine and a webview parses markup, so rows are built with createElement
// + textContent — the names reach the DOM as text and cannot reach it as markup
// at all, which is stronger than escaping them.
//
// Wrapped in an IIFE, and that is load-bearing: classic scripts share one global
// scope, so a top-level `function render()` here would silently REPLACE app.js's
// and every host card would stop painting. The only names crossing the boundary
// are the two app.js exposes — `callRust` and `settingsOpen` — meter.js's
// `soladorMeter`, and the test hook at the bottom.
(function () {

/** How often a panel asks Rust for a fresh payload: every footer carries a
 *  relative age that has to tick up on a clock this side never sees move, and
 *  asking costs one lock and one JSON build. */
const REFRESH_MS = 10000;

/** How often to ask while the panel is still filling in — see `github.js`. */
const LOADING_REFRESH_MS = 1000;

const $a = (id) => document.getElementById(id);

function node(tag, cls, text) {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined && text !== null) n.textContent = text;
  return n;
}

/** A `LABEL … value` row. Both the text and the value's colour are Rust's. */
function valueRow(row, extraClass) {
  const el = node("div", extraClass ? "pv-row " + extraClass : "pv-row");
  el.appendChild(node("span", "lbl", row.label));
  const value = node("span", "val", row.value);
  value.style.color = row.valueColor;
  el.appendChild(value);
  return el;
}

/** A dot-plus-name breakdown line (one project). */
function itemRow(item) {
  const el = node("div", "pv-item");
  const dot = node("span", "dot");
  dot.style.background = item.dotColor;
  el.append(dot, node("span", "name", item.name), node("span", "val", item.value));
  return el;
}

/** A muted sentence in Rust's words and Rust's colour. */
function messageNode(message) {
  const line = node("p", "pv-message", message.text);
  line.style.color = message.color;
  return line;
}

/**
 * One of the panel's two header lines, the warning or the freshness clock, or
 * no line at all. `null` means "render nothing", not "render an empty one": a
 * blank element still occupies the header and still reads as a claim. Ellipsised
 * in a narrow panel, so `title` keeps the whole message reachable.
 */
function headerLine(id, line) {
  const el = $a(id);
  el.textContent = line ? line.text : "";
  el.title = line ? line.text : "";
  el.style.color = line ? line.color : "";
  el.hidden = !line;
}

/**
 * One plan section: a divider, the plan's header row (`Claude · Ultra` and
 * Rust's verdict), then a meter per limit.
 *
 * The meters are the Machines tile's (`meter.js`), so a plan's Week reads the
 * way a machine's RAM does, and each one's reset-and-pace sentence is shown
 * under it: on the panel there is room for the words the tile keeps in a
 * tooltip. A plan with nothing to show yet has no meters at all, not empty
 * ones.
 */
function planNode(plan) {
  const el = node("div", "pv-section");
  el.dataset.plan = plan.id;
  el.appendChild(node("hr", "pv-divider"));
  el.appendChild(valueRow(plan, "plan"));
  if (plan.metrics?.length) {
    const stats = node("div", "db-host-stats");
    for (const m of plan.metrics) {
      stats.appendChild(window.soladorMeter.metricNode(m, plan.label, { showNote: true }).metric);
    }
    el.appendChild(stats);
  }
  return el;
}

/** This machine's own token counts, under Rust's heading, or nothing. */
function rollupsNode(payload) {
  const el = node("div", "ai-rollups");
  // `null` when there are no rows to head.
  if (payload.machineHeading) el.appendChild(node("div", "lbl pv-heading", payload.machineHeading));
  // Not `window` — that name is the global object inside this loop body.
  for (const row of payload.windows || []) el.appendChild(valueRow(row, "window"));
  if (payload.projects) {
    const section = node("div", "pv-section");
    section.dataset.section = "projects";
    section.appendChild(node("hr", "pv-divider"));
    section.appendChild(node("div", "lbl", payload.projects.label));
    for (const item of payload.projects.rows || []) section.appendChild(itemRow(item));
    el.appendChild(section);
  }
  return el.childElementCount ? el : null;
}

/**
 * The organization's spend from an Admin key. The note says how far the figure
 * reaches, which is not when it was read: Anthropic's lag by hours.
 */
function apiNode(api) {
  const el = node("div", "pv-section");
  el.dataset.section = "api";
  el.appendChild(node("hr", "pv-divider"));
  el.appendChild(valueRow(api));
  if (api.note) el.appendChild(node("div", "pv-note", api.note));
  return el;
}

/** What the source costs: the plan and the API spend, whichever are set up. A
 *  part that is not set up says nothing. */
function plansNode(payload) {
  const el = node("div", "ai-plans");
  for (const plan of payload.plans || []) el.appendChild(planNode(plan));
  if (payload.api) el.appendChild(apiNode(payload.api));
  return el.childElementCount ? el : null;
}

/**
 * Mounts one panel. `id` is the source's, and names the command, the section's
 * element ids and the fixture alike, so the two panels cannot drift apart.
 */
function mount(id) {
  function render(payload) {
    $a(`${id}Title`).textContent = window.soladorDashboard?.tileHeading?.(id) || payload.title;
    $a(`${id}Trailing`).textContent = payload.trailing || "";

    // A state of the whole panel, in Rust's words: this file knows none of them.
    const children = payload.message ? [messageNode(payload.message)] : [];
    // The plan leads: a share of a subscription's limit is the figure an
    // operator acts on. The two blocks are `#<id>Body`'s grid items, so the
    // rollups sit beside the plan on a full-width card. Which count applies is
    // CSS's, from Rust's `panel_columns`, never decided here.
    const plans = plansNode(payload);
    if (plans) children.push(plans);
    const rollups = rollupsNode(payload);
    if (rollups) children.push(rollups);
    $a(`${id}Body`).replaceChildren(...children);

    // Header lines, never body lines: a line under the rows would make the card
    // taller the moment it fired. Both are Rust's, already attributed.
    headerLine(`${id}Stale`, payload.footer);
    headerLine(`${id}Freshness`, payload.freshness);

    $a(`${id}Panel`).hidden = false;
  }

  async function refresh() {
    if (!panelWanted(id)) return false;
    try {
      // Offline (no Tauri), the dumped-fixture path every other panel uses.
      const payload = await callRust(id, window.soladorDashboard?.tileArgs?.(id) ?? {}, `sample-${id}.json`);
      if (payload) render(payload);
      return Boolean(payload && payload.loading);
    } catch {
      // A failed poll leaves the last good panel on screen rather than blanking
      // it: Rust already retains last-known figures through a bad fetch.
    }
    // A poll that threw is not "loading" — it is a failure, and retrying it
    // every second would be a busy loop against whatever just broke.
    return false;
  }

  let refreshTimer = null;

  /** Re-arms the poll at the cadence the last payload asked for. */
  function scheduleRefresh(loading) {
    clearTimeout(refreshTimer);
    refreshTimer = setTimeout(tick, loading ? LOADING_REFRESH_MS : REFRESH_MS);
  }

  async function tick() {
    // While Settings is up the cockpit is off-screen, so skip the work — but
    // still re-arm, or closing Settings would find a dead timer.
    scheduleRefresh(settingsOpen ? false : await refresh());
  }

  refresh().then((loading) => {
    if (window.__TAURI__) scheduleRefresh(loading);
  });

  // Brought current when Settings closes, rather than waiting out this panel's
  // own timer.
  registerPanelRefresh(async () => scheduleRefresh(await refresh()));

  return { render, refresh };
}

// Test-only introspection, matching app.js's `window.__SOLADOR_TEST__`:
// read-only, and no production behaviour depends on it.
window.__SOLADOR_AI_TEST__ = { anthropic: mount("anthropic"), openai: mount("openai") };

})();
