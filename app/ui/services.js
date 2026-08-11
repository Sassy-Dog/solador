/** The Services panel: third-party availability, one row per watched vendor.
 *
 *  Same contract as every other panel script — Rust decides every label, state
 *  word and colour (`services::view`), and this file paints them. It does not
 *  know what "Partial Outage" means, which vendors exist, or that Azure's
 *  healthy row is worded differently from the rest.
 *
 *  Its own IIFE, like the others: a `const` at file scope would collide with
 *  app.js's and every host card would stop painting. The only names crossing
 *  the boundary are the two app.js exposes — `callRust` and `settingsOpen` —
 *  plus the test hook at the bottom.
 */
(function () {

/** How often the panel asks Rust for a fresh payload.
 *
 *  Matches the panels around it rather than the status poll behind it (the
 *  store's `refresh_interval_secs`, 30s–5m): Rust answers from whatever the
 *  last pass stored, and asking on the panel cadence is what makes a recovery
 *  appear promptly rather than up to five minutes late. */
const REFRESH_MS = 10000;

/** How often to ask while the panel is still filling in — see `github.js`. */
const LOADING_REFRESH_MS = 1000;

const $s = (id) => document.getElementById(id);

function node(tag, cls, text) {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined && text !== null) n.textContent = text;
  return n;
}

/** One vendor's row: dot, name, state word. */
function rowNode(row) {
  const el = node("div", "svc-row");
  el.dataset.service = row.id;

  const dot = node("span", "dot");
  // CSSOM, never a `style=""` attribute -- `style-src 'self'` blocks it.
  dot.style.background = row.color;
  el.appendChild(dot);

  // createElement + textContent, never innerHTML: `detail` can quote an
  // incident title straight out of a vendor's CMS, and Azure's arrive as
  // CDATA with entities already in them.
  el.appendChild(node("span", "svc-name", row.label));
  el.appendChild(node("span", "grow"));

  const state = node("span", "svc-state", row.state);
  state.style.color = row.color;
  el.appendChild(state);

  // The whole reason a row is the colour it is, on hover — including why a
  // reading is not newer when a refresh failed.
  if (row.detail) el.title = row.detail;
  return el;
}

function render(payload) {
  $s("servicesTitle").textContent = payload.title;
  $s("servicesTrailing").textContent = payload.trailing || "";
  $s("servicesBody").replaceChildren(
    ...(payload.rows || []).map(rowNode)
  );
  $s("servicesPanel").hidden = false;
}

async function refresh() {
  try {
    // Offline (no Tauri), the dumped-fixture path every other panel uses, so
    // this one opens in a plain browser and in the Playwright suite.
    const payload = await callRust("services", {}, "sample-services.json");
    if (payload) render(payload);
    // Every vendor still unknown means no pass has landed yet. Rust does not
    // publish a `loading` flag here because the panel has no other empty
    // state: five unknown rows *is* the pre-first-pass rendering.
    return Boolean(
      payload && (payload.rows || []).length && (payload.rows || []).every((r) => r.state === "Unknown")
    );
  } catch {
    // A failed poll leaves the last good rows on screen: Rust already retains
    // each vendor's last reading through a bad fetch.
  }
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
window.__SOLADOR_SERVICES_TEST__ = { render, refresh };

})();
