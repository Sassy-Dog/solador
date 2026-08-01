// The Containers / VMs panel. Same discipline as the cockpit and Settings:
// every string, every colour and every count arrives from Rust
// (`app/src-tauri/src/containers/`), and this file does layout, wiring, and
// nothing else. A status word or a threshold typed in here is one that can
// drift from the Swift panel without a test noticing.
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

  // A healthy, fresh panel renders no footer at all — the cockpit stays
  // glanceable, and a warning line means something when it appears.
  const footer = $c("containersFooter");
  footer.textContent = payload.footer ? payload.footer.text : "";
  footer.style.color = payload.footer ? payload.footer.color : "";
  footer.hidden = !payload.footer;

  $c("containersPanel").hidden = false;
}

async function refresh() {
  try {
    // Offline (no Tauri), the dumped-fixture path the cockpit and Settings
    // both use, so the panel can be opened in a plain browser and by the
    // Playwright suite.
    const payload = await callRust("containers", {}, "sample-containers.json");
    if (payload) renderPanel(payload);
  } catch {
    // A failed poll leaves the last good panel on screen rather than blanking
    // it: Rust already retains last-known rows through a bad poll, and wiping
    // the DOM here would undo that.
  }
}

refresh();
if (window.__TAURI__) {
  setInterval(() => { if (!settingsOpen) refresh(); }, REFRESH_MS);
}

// Test-only introspection, matching app.js's `window.__DEVCANOPY_TEST__`:
// read-only, and no production behaviour depends on it.
window.__DEVCANOPY_CONTAINERS_TEST__ = { render: renderPanel, refresh };

})();
