// The Containers / VMs panel. Same discipline as the cockpit and Settings:
// every string, every colour and every count arrives from Rust
// (`app/src-tauri/src/containers/`), and this file does layout, wiring, and
// nothing else. A status word or a threshold typed in here is one that can
// drift from the original panel without a test noticing.
//
// Nothing below uses `innerHTML`. Container names, VM names and status text
// come from a REMOTE agent, and a webview parses markup — building the rows
// with createElement + textContent means they reach the DOM as text and cannot
// reach it as markup at all, which is stronger than escaping them.
//
// Wrapped in an IIFE, and that is load-bearing: classic scripts share one
// global scope, so a top-level `function render()` here would silently REPLACE
// app.js's and every host card would stop painting (settings.js learned this
// the hard way). The only names crossing the boundary are the two app.js
// exposes — `callRust` and `settingsOpen` — plus the test hook at the bottom.
(function () {

/** How often the panel asks Rust for a fresh payload, matching the Rust-side
 *  poll cadence (`containers::POLL_INTERVAL_SECS`). Asking faster would only
 *  re-render the same numbers. */
const REFRESH_MS = 10000;

/** How often to ask while the panel is still filling in — see `github.js`. */
const LOADING_REFRESH_MS = 1000;

const $c = (id) => document.getElementById(id);

function node(tag, cls, text) {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined && text !== null) n.textContent = text;
  return n;
}

/**
 * One row: present container, standing absent row, or collapsed aggregate.
 *
 * The dot's colour and the status text's colour are Rust's — green/amber/red
 * carry meaning here (running, recycling, missing) and re-deriving them from
 * `kind` in JS would be a second implementation of the panel's semantics.
 * `data-kind` is what makes a row addressable from a test without reading its
 * colours back.
 */
function rowNode(row) {
  const el = node("div", "cont-row");
  el.dataset.kind = row.kind;

  const dot = node("span", "dot");
  dot.style.background = row.dotColor;
  el.append(dot, node("span", "cont-name", row.name));
  // Absent-and-never-observed entities have no runtime, and none is invented.
  if (row.runtime) el.appendChild(node("span", "cont-runtime", row.runtime));
  el.appendChild(node("span", "grow"));

  const status = node("span", "cont-status", row.status);
  status.style.color = row.statusColor;
  el.appendChild(status);
  return el;
}

function sectionNode(section) {
  const el = node("div", "cont-section");
  el.dataset.host = section.host;
  el.appendChild(node("div", "lbl", section.label));
  // "no container runtimes" and "no containers" are different sentences and
  // Rust decides which one this is; the aggregates below still render, because
  // a configured collapse rule is a standing row even at zero.
  if (section.empty) el.appendChild(node("div", "cont-empty", section.empty.message));
  for (const row of section.rows || []) el.appendChild(rowNode(row));
  return el;
}

function renderPanel(payload) {
  $c("containersTitle").textContent = payload.title;
  $c("containersTrailing").textContent = payload.trailing;

  const body = $c("containersBody");
  const children = [];
  if (payload.empty) children.push(node("p", "cont-empty", payload.empty.message));
  for (const section of payload.sections || []) children.push(sectionNode(section));
  body.replaceChildren(...children);

  // A healthy, fresh panel renders no warning at all — the cockpit stays
  // glanceable, and a warning means something when it appears. It sits in the
  // header rather than under the body because a line below the content makes
  // the card taller, and `.panel-row` stretches its neighbours to match: the
  // panel that degraded would move the panel that did not. Ellipsised in a
  // narrow panel, so `title` keeps the whole message reachable.
  const stale = $c("containersStale");
  stale.textContent = payload.footer ? payload.footer.text : "";
  stale.title = payload.footer ? payload.footer.text : "";
  stale.style.color = payload.footer ? payload.footer.color : "";
  stale.hidden = !payload.footer;

  $c("containersPanel").hidden = false;
}

async function refresh() {
  try {
    // Offline (no Tauri), the dumped-fixture path the cockpit and Settings
    // both use, so the panel can be opened in a plain browser and by the
    // Playwright suite.
    const payload = await callRust("containers", {}, "sample-containers.json");
    if (payload) renderPanel(payload);
    return Boolean(payload && payload.loading);
  } catch {
    // A failed poll leaves the last good panel on screen rather than blanking
    // it: Rust already retains last-known rows through a bad poll, and wiping
    // the DOM here would undo that.
  }
  // A poll that threw is not "loading" — it is a failure, and retrying it every
  // second would be a busy loop against whatever just broke.
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
// own timer. Re-arms too, so a panel that turns out to be loading switches to
// the fast cadence immediately.
registerPanelRefresh(async () => scheduleRefresh(await refresh()));

// Test-only introspection, matching app.js's `window.__SOLADOR_TEST__`:
// read-only, and no production behaviour depends on it.
window.__SOLADOR_CONTAINERS_TEST__ = { render: renderPanel, refresh };

})();
