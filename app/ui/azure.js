// The Azure Cost panel. Same discipline as the cockpit, Settings, Containers,
// the GitHub panels and Usage: every string, every dollar figure and every
// colour arrives from Rust (`app/src-tauri/src/azure.rs`), and this file does
// layout, wiring, and nothing else.
//
// There is no currency formatting here, deliberately. `$1,234.56` is an en_US
// `NumberFormatter` in the Swift panel and a hand-rolled twin of it in Rust; a
// `toLocaleString` in this file would be a third implementation, rendering the
// bill in whatever locale the webview happened to inherit.
//
// The two states this panel must not conflate are also Rust's, arriving as two
// different `{text, color}` pairs: **no SAS URL** is a muted setup instruction
// and **a failed read** is red. Choosing the colour here from "is there a
// summary" would put that judgement where no Rust test can see it.
//
// Nothing below uses `innerHTML`. Resource-group names come from an Azure
// export and a webview parses markup, so rows are built with createElement +
// textContent.
//
// Wrapped in an IIFE for the reason every panel script here is: classic scripts
// share one global scope, and a top-level `render()` would silently replace
// app.js's.
(function () {

/** How often the panel asks Rust for a fresh payload.
 *
 *  Far faster than the 4h read behind it, and not to see new numbers: the
 *  footer carries a relative age ("updated 5h ago") that has to tick up on a
 *  clock this side never sees move, and a panel frozen at "just now" through a
 *  stuck poller is the one thing the footer exists to prevent. */
const REFRESH_MS = 10000;

const $a = (id) => document.getElementById(id);

function node(tag, cls, text) {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined && text !== null) n.textContent = text;
  return n;
}

/** A `LABEL … $value` row (PRIOR MONTH, PROJECTED). */
function statNode(stat) {
  const el = node("div", "pv-row");
  el.appendChild(node("span", "lbl", stat.label));
  const value = node("span", "val", stat.value);
  value.style.color = stat.valueColor;
  el.appendChild(value);
  return el;
}

function barNode(bar) {
  if (!bar) return null;
  const el = node("div", "pbar");
  const fill = node("span");
  fill.style.width = (Number(bar.fraction) * 100).toFixed(1) + "%";
  fill.style.background = bar.color;
  el.appendChild(fill);
  return el;
}

/** One dot-plus-name cost line. */
function resourceNode(resource) {
  const el = node("div", "pv-item");
  const dot = node("span", "dot");
  dot.style.background = resource.dotColor;
  el.append(
    dot,
    node("span", "name", resource.name),
    node("span", "val", resource.value)
  );
  return el;
}

/** One titled top-N column. */
function breakdownNode(column) {
  const el = node("div", "pv-section");
  el.appendChild(node("div", "lbl", column.title));
  for (const resource of column.rows || []) el.appendChild(resourceNode(resource));
  return el;
}

function renderAzure(payload) {
  $a("azureTitle").textContent = payload.title;
  $a("azureTrailing").textContent = payload.trailing || "";

  const children = [];
  if (payload.message) {
    // One sentence, and its colour is Rust's: muted "add a SAS URL" reads as
    // setup, red names a failure. This file does not choose between them.
    const message = node("p", "pv-message", payload.message.text);
    message.style.color = payload.message.color;
    children.push(message);
  }

  if (payload.headline) {
    const headline = node("div", "az-headline");
    const value = node("span", "value", payload.headline.value);
    value.style.color = payload.headline.valueColor;
    const caption = node("span", "caption", payload.headline.caption);
    // Amber when the figures cover a month that is NOT the one a reader would
    // assume — the whole point of the rollover-gap caption.
    caption.style.color = payload.headline.captionColor;
    headline.append(value, caption);
    children.push(headline);
  }

  for (const stat of payload.stats || []) children.push(statNode(stat));

  if (payload.budget) {
    const budget = node("div", "pv-section");
    budget.dataset.section = "budget";
    const header = node("div", "pv-row");
    header.appendChild(node("span", "lbl", payload.budget.label));
    const value = node("span", "val", payload.budget.value);
    value.style.color = payload.budget.valueColor;
    header.appendChild(value);
    budget.appendChild(header);
    const bar = barNode(payload.budget.bar);
    if (bar) budget.appendChild(bar);
    children.push(budget);
  }

  const columns = payload.breakdowns || [];
  if (columns.length) {
    children.push(node("hr", "pv-divider"));
    const grid = node("div", "az-columns");
    for (const column of columns) grid.appendChild(breakdownNode(column));
    children.push(grid);
  }

  $a("azureBody").replaceChildren(...children);

  // A healthy, fresh panel renders no footer at all — the cockpit stays
  // glanceable, and a warning line means something when it appears.
  const footer = $a("azureFooter");
  footer.textContent = payload.footer ? payload.footer.text : "";
  footer.style.color = payload.footer ? payload.footer.color : "";
  footer.hidden = !payload.footer;

  $a("azurePanel").hidden = false;
}

async function refresh() {
  try {
    const payload = await callRust("azure_cost", {}, "sample-azure.json");
    if (payload) renderAzure(payload);
  } catch {
    // A failed poll leaves the last good panel on screen: Rust already carries
    // the last summary forward with the reason in the footer.
  }
}

refresh();
if (window.__TAURI__) {
  setInterval(() => { if (!settingsOpen) refresh(); }, REFRESH_MS);
}

// Test-only introspection, matching app.js's `window.__DEVCANOPY_TEST__`.
window.__DEVCANOPY_AZURE_TEST__ = { render: renderAzure, refresh };

})();
