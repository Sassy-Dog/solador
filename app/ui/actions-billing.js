// The Actions billing panel. Same discipline as the Azure Cost panel it is
// modelled on: every string, every dollar figure, every minute count and every
// colour arrives from Rust (`app/src-tauri/src/actions_billing.rs`), and this
// file does layout, wiring, and nothing else.
//
// There is no threshold here and no money formatting, deliberately. Whether the
// budget bar is amber or red is Rust's `progress_bar` over the 0.9 and 1.0
// constants, and `$1,234.56` is Rust's `usd`: a comparison or a `toLocaleString`
// in this file would be a second rule free to disagree with the one the Rust
// tests guard.
//
// The states this panel must not conflate are also Rust's, arriving as
// `{text, color}` pairs: no token or org is a muted setup sentence, a failed
// read is red, a month with no Actions usage is muted words (never `$0.00`), and
// an org with no Actions budget is `budgetNote` with NO `budget` and so no bar.
// Choosing between them here, from "is there a bar", would put that judgement
// where no Rust test can see it.
//
// The Billing alerts rows (`payload.alerts`, #770) are Rust's verdicts, painted
// as given: a tripped spike or macOS rule is red, a rule that could not be
// judged is muted. Which days were checked and every threshold stay in Rust.
//
// The report's own lag, `dataThrough` ("usage through Nov 14 19:00 UTC"), is a
// plain line in the body. It is not the status footer and not the freshness
// line: those are about the poller, and are both absent while it is healthy.
//
// Nothing below uses `innerHTML`. Repository names come from GitHub and a
// webview parses markup, so rows are built with createElement + textContent.
//
// Wrapped in an IIFE for the reason every panel script here is: classic scripts
// share one global scope, and a top-level `render()` would silently replace
// app.js's.
(function () {

/** How often the panel asks Rust for a fresh payload. Far faster than the
 *  hourly read behind it, because the header's relative ages have to tick up on
 *  a clock this side never sees move. */
const REFRESH_MS = 10000;

/** How often to ask while the panel is still filling in — see `github.js`. */
const LOADING_REFRESH_MS = 1000;

const $b = (id) => document.getElementById(id);

function node(tag, cls, text) {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined && text !== null) n.textContent = text;
  return n;
}

/** A `LABEL … value` row. */
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

/** One dot-plus-name line with its minutes and dollars. */
function rowNode(row) {
  const el = node("div", "pv-item");
  const dot = node("span", "dot");
  dot.style.background = row.dotColor;
  el.append(dot, node("span", "name", row.name), node("span", "val", row.value));
  return el;
}

/** One titled column. */
function breakdownNode(column) {
  const el = node("div", "pv-section");
  el.dataset.section = column.title;
  el.appendChild(node("div", "lbl", column.title));
  for (const row of column.rows || []) el.appendChild(rowNode(row));
  return el;
}

/** One Billing alerts rule row: a tripped rule is red, one that could not be
 *  judged is muted and says so. Rust made the label, the figures, the detail
 *  and the colour; nothing here decides whether a rule tripped. */
function alertNode(alert) {
  const el = node("div", "pv-section");
  el.dataset.section = "alert";
  el.dataset.alert = alert.id;
  const header = node("div", "pv-row");
  header.appendChild(node("span", "lbl", alert.label));
  const value = node("span", "val", alert.value);
  value.style.color = alert.color;
  header.appendChild(value);
  el.appendChild(header);
  if (alert.detail) el.appendChild(node("p", "pv-note", alert.detail));
  return el;
}

/** The daily hosted-minutes trend's section: Rust's label and caption around an
 *  empty `.plot` that `spark()` fills once the section is in the page (it
 *  measures the node). The series, its range and its dates are Rust's; the
 *  length of the series is the painter's capacity. */
function trendNode(trend) {
  const el = node("div", "pv-section az-trend");
  el.dataset.section = "trend";
  el.appendChild(node("div", "lbl", trend.label));
  const plot = node("div", "plot");
  plot.dataset.h = "56";
  el.appendChild(plot);
  const caption = node("p", "pv-note", trend.caption);
  caption.style.color = trend.captionColor;
  el.appendChild(caption);
  return el;
}

/** A coloured one-line sentence: the budget note and the report's lag. */
function noteNode(note, section) {
  const el = node("p", "pv-note", note.text);
  el.style.color = note.color;
  if (section) el.dataset.section = section;
  return el;
}

function renderActionsBilling(payload) {
  $b("actionsBillingTitle").textContent =
    window.soladorDashboard?.tileHeading?.("actionsBilling") || payload.title;
  $b("actionsBillingTrailing").textContent = payload.trailing || "";

  const children = [];
  if (payload.message) {
    const message = node("p", "pv-message", payload.message.text);
    message.style.color = payload.message.color;
    children.push(message);
  }

  // The figures are one block so the breakdowns can sit BESIDE them on a
  // full-width card, as Azure's do: which column count applies is CSS's, from
  // Rust's `panel_columns`, never decided here.
  const main = node("div", "az-main");

  if (payload.headline) {
    const headline = node("div", "az-headline");
    const value = node("span", "value", payload.headline.value);
    value.style.color = payload.headline.valueColor;
    const caption = node("span", "caption", payload.headline.caption);
    caption.style.color = payload.headline.captionColor;
    headline.append(value, caption);
    main.appendChild(headline);
  }

  for (const stat of payload.stats || []) main.appendChild(statNode(stat));

  if (payload.dataThrough) main.appendChild(noteNode(payload.dataThrough, "data-through"));

  if (payload.budget) {
    const budget = node("div", "pv-section");
    budget.dataset.section = "budget";
    const header = node("div", "pv-row");
    header.appendChild(node("span", "lbl", payload.budget.label));
    const value = node("span", "val", payload.budget.value);
    value.style.color = payload.budget.valueColor;
    header.appendChild(value);
    budget.appendChild(header);
    // No bar when Rust sent none: a budget with no measured fraction is a label
    // and its figures, never an empty track that reads as 0%.
    const bar = barNode(payload.budget.bar);
    if (bar) budget.appendChild(bar);
    main.appendChild(budget);
  } else if (payload.budgetNote) {
    main.appendChild(noteNode(payload.budgetNote, "budget-note"));
  }

  for (const alert of payload.alerts || []) main.appendChild(alertNode(alert));

  // The daily trend (#771): the Full view's alone. This panel is only on screen
  // there, and Rust leaves `trend` and `trendNote` null for a failed read, a
  // month with no usage and a tile that picked rows.
  if (payload.trend) main.appendChild(trendNode(payload.trend));
  else if (payload.trendNote) main.appendChild(noteNode(payload.trendNote, "trend-note"));

  if (main.childElementCount) children.push(main);

  const columns = payload.breakdowns || [];
  if (columns.length) {
    children.push(node("hr", "pv-divider"));
    const grid = node("div", "az-columns");
    for (const column of columns) grid.appendChild(breakdownNode(column));
    children.push(grid);
  }

  const body = $b("actionsBillingBody");
  // The painter keeps each chart it was given; drop the ones this repaint is
  // about to discard.
  forgetCharts(body);
  body.replaceChildren(...children);
  if (payload.trend) {
    const t = payload.trend;
    spark(body.querySelector('[data-section="trend"] .plot'), [{ values: t.values, color: t.color }], t.lo, t.hi, t.values.length);
  }

  // The poller's warning, in the header so a degrading tile does not grow. A
  // healthy, current panel renders none.
  const stale = $b("actionsBillingStale");
  stale.textContent = payload.footer ? payload.footer.text : "";
  stale.title = payload.footer ? payload.footer.text : "";
  stale.style.color = payload.footer ? payload.footer.color : "";
  stale.hidden = !payload.footer;

  // How old the figure is: a different question from the warning above, and
  // Rust hands over the finished line, so nothing here compares an age to a
  // cadence.
  const asOf = payload.freshness && payload.freshness.text;
  const fresh = $b("actionsBillingFreshness");
  fresh.textContent = asOf || "";
  fresh.title = asOf || "";
  fresh.style.color = (payload.freshness && payload.freshness.color) || "";
  fresh.hidden = !asOf;

  $b("actionsBillingPanel").hidden = false;
}

async function refresh() {
  if (!panelWanted("actionsBilling")) return false;
  try {
    const payload = await callRust(
      "actions_billing",
      window.soladorDashboard?.tileArgs?.("actionsBilling") ?? {},
      "sample-actions-billing.json"
    );
    if (payload) renderActionsBilling(payload);
    return Boolean(payload && payload.loading);
  } catch {
    // A failed poll leaves the last good panel on screen: Rust already carries
    // the last summary forward with the reason in the footer.
  }
  // A poll that threw is not "loading"; retrying it every second would be a
  // busy loop against whatever just broke.
  return false;
}

let refreshTimer = null;

/** Re-arms the poll at the cadence the last payload asked for. */
function scheduleRefresh(loading) {
  clearTimeout(refreshTimer);
  refreshTimer = setTimeout(tick, loading ? LOADING_REFRESH_MS : REFRESH_MS);
}

async function tick() {
  // While Settings is up the cockpit is off-screen, so skip the work, but still
  // re-arm, or closing Settings would find a dead timer.
  scheduleRefresh(settingsOpen ? false : await refresh());
}

refresh().then((loading) => {
  if (window.__TAURI__) scheduleRefresh(loading);
});

// Brought current when Settings closes, rather than waiting out this panel's
// own timer.
registerPanelRefresh(async () => scheduleRefresh(await refresh()));

// Test-only introspection, matching azure.js's.
window.__SOLADOR_ACTIONS_BILLING_TEST__ = { render: renderActionsBilling, refresh };

})();
