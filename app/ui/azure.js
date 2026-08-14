// The Azure Cost panel. Same discipline as the cockpit, Settings, Containers,
// the GitHub panels and Usage: every string, every dollar figure and every
// colour arrives from Rust (`app/src-tauri/src/azure.rs`), and this file does
// layout, wiring, and nothing else.
//
// There is no currency formatting here, deliberately. `$1,234.56` is an en_US
// `NumberFormatter` in the original panel and a hand-rolled twin of it in Rust; a
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

/** How often to ask while the panel is still filling in — see `github.js`. */
const LOADING_REFRESH_MS = 1000;

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

  // The costs are one block so the breakdowns can sit BESIDE them on a
  // full-width card: `#azureBody` is a grid of `--panel-cols` tracks, and each
  // of these two wrappers is one grid item. Same DOM at one column, where they
  // simply stack — which column count applies is CSS's, from Rust's
  // `panel_columns`, never decided here.
  const main = node("div", "az-main");

  if (payload.headline) {
    const headline = node("div", "az-headline");
    const value = node("span", "value", payload.headline.value);
    value.style.color = payload.headline.valueColor;
    const caption = node("span", "caption", payload.headline.caption);
    // Amber when the figures cover a month that is NOT the one a reader would
    // assume — the whole point of the rollover-gap caption.
    caption.style.color = payload.headline.captionColor;
    headline.append(value, caption);
    main.appendChild(headline);
  }

  for (const stat of payload.stats || []) main.appendChild(statNode(stat));

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
    main.appendChild(budget);
  }

  if (main.childElementCount) children.push(main);

  const columns = payload.breakdowns || [];
  if (columns.length) {
    children.push(node("hr", "pv-divider"));
    const grid = node("div", "az-columns");
    for (const column of columns) grid.appendChild(breakdownNode(column));
    children.push(grid);
  }

  $a("azureBody").replaceChildren(...children);

  // A healthy, fresh panel renders no warning at all — the cockpit stays
  // glanceable, and a warning means something when it appears. It sits in the
  // header rather than under the body because a line below the content makes
  // the card taller, and `.panel-row` stretches its neighbours to match: the
  // panel that degraded would move the panel that did not. Ellipsised in a
  // narrow panel, so `title` keeps the whole message reachable.
  const stale = $a("azureStale");
  stale.textContent = payload.footer ? payload.footer.text : "";
  stale.title = payload.footer ? payload.footer.text : "";
  stale.style.color = payload.footer ? payload.footer.color : "";
  stale.hidden = !payload.footer;

  // How old the headline is — a DIFFERENT question from the warning above, and
  // deliberately a different element. `payload.footer` says the poller failed
  // or is late; `payload.freshness` says the dollars on screen were measured
  // 23h ago. Rust classifies the age against this panel's cadence
  // (`Freshness::classify`) and hands over the finished line, so nothing here
  // compares an age to a threshold — a second rule in this file could disagree
  // with the one the Rust tests guard.
  //
  // `live` and `unknown` carry no text and paint nothing: a current reading
  // reads as it always did, and a panel that has never read has no figure to
  // date. Dimming the stale headline is Rust's too, arriving as the
  // `headline.valueColor` applied above.
  const asOf = payload.freshness && payload.freshness.text;
  const fresh = $a("azureFreshness");
  fresh.textContent = asOf || "";
  fresh.title = asOf || "";
  fresh.style.color = (payload.freshness && payload.freshness.color) || "";
  fresh.hidden = !asOf;

  $a("azurePanel").hidden = false;
}

async function refresh() {
  try {
    const payload = await callRust("azure_cost", {}, "sample-azure.json");
    if (payload) renderAzure(payload);
    return Boolean(payload && payload.loading);
  } catch {
    // A failed poll leaves the last good panel on screen: Rust already carries
    // the last summary forward with the reason in the footer.
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

// Test-only introspection, matching app.js's `window.__SOLADOR_TEST__`.
window.__SOLADOR_AZURE_TEST__ = { render: renderAzure, refresh };

})();
