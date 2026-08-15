// The Usage panel: Claude Code token rollups, plus a Neon and a Sentry section
// when those providers are configured. Same discipline as every other panel
// here: each string, colour and figure arrives from Rust
// (`app/src-tauri/src/usage.rs`), and this file does layout, wiring, and
// nothing else.
//
// In particular there is no formatting and no thresholding here. The muted "—"
// that means "configured, and we could not find out" is a Rust decision
// arriving as a `{value, valueColor}` pair; a `0` is a different Rust decision.
// Deriving either from a number in JS would put a second implementation of that
// distinction on the far side of the IPC boundary, where no Rust test can see
// it. The same goes for the quota bar's green/amber/red: `bar` is either an
// object with a colour or `null`, and `null` means "do not draw one".
//
// Nothing below uses `innerHTML`. Project names come from directory paths on
// this machine and a webview parses markup, so rows are built with createElement
// + textContent — the names reach the DOM as text and cannot reach it as markup
// at all, which is stronger than escaping them.
//
// Wrapped in an IIFE, and that is load-bearing: classic scripts share one global
// scope, so a top-level `function render()` here would silently REPLACE app.js's
// and every host card would stop painting (settings.js learned this the hard
// way). The only names crossing the boundary are the two app.js exposes —
// `callRust` and `settingsOpen` — plus the test hook at the bottom.
(function () {

/** How often the panel asks Rust for a fresh payload.
 *
 *  Faster than either Rust-side cadence (Claude on the store's refresh
 *  interval, Neon and Sentry hourly) because the payload is not static between
 *  fetches: every footer carries a relative age, and "updated 4m ago" has to
 *  tick up on a clock this side never sees move. Asking every 10s costs one
 *  lock and one JSON build. */
const REFRESH_MS = 10000;

/** How often to ask while the panel is still filling in — see `github.js`. */
const LOADING_REFRESH_MS = 1000;

const $u = (id) => document.getElementById(id);

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

/**
 * One progress bar, or nothing.
 *
 * `null` is a decision, not a missing field: Rust suppresses the bar when the
 * quota is unset OR the count is unknown, because a bar drawn at a defaulted
 * zero would read "comfortably under quota" when the truth is "nobody
 * measured". Substituting a 0 here would undo exactly that.
 */
function barNode(bar) {
  if (!bar) return null;
  const el = node("div", "pbar");
  const fill = node("span");
  fill.style.width = (Number(bar.fraction) * 100).toFixed(1) + "%";
  fill.style.background = bar.color;
  el.appendChild(fill);
  return el;
}

/** A footer line, or nothing — a healthy, fresh section renders neither. */
function footerNode(footer, cls) {
  if (!footer) return null;
  const el = node("p", cls, footer.text);
  el.style.color = footer.color;
  return el;
}

/**
 * How old this section's figures are, or nothing.
 *
 * A DIFFERENT question from the footer beside it, and deliberately a different
 * element: `provider.footer` says the last attempt failed or the poller is
 * late, `provider.freshness` says the dollars and counts on screen were
 * measured 23h ago. These providers are read hourly, so the first speaks a full
 * half hour before the second does — the window where a figure is no longer
 * current and the poller is not yet worth warning about.
 *
 * `.panel-asof` is the Azure Cost panel's class, reused rather than reinvented:
 * same job, same typography. `live` and `unknown` carry no text and paint
 * nothing — a current reading reads as it always did, and a section that has
 * never answered has no figure to date. Rust classifies the age against the
 * providers' cadence and hands over the finished line, so nothing here compares
 * an age to a threshold.
 */
function asOfNode(freshness) {
  const text = freshness && freshness.text;
  if (!text) return null;
  const el = node("div", "panel-asof", text);
  el.title = text;
  el.style.color = freshness.color;
  return el;
}

/**
 * One provider section: a divider, its rows, an optional bar, how old they are,
 * and its own footer.
 */
function providerNode(provider) {
  const el = node("div", "pv-section");
  el.dataset.provider = provider.id;
  el.appendChild(node("hr", "pv-divider"));
  for (const row of provider.rows || []) el.appendChild(valueRow(row));
  const bar = barNode(provider.bar);
  if (bar) el.appendChild(bar);
  // Above the footer, matching the Azure header's order: how old the figure is,
  // then the warning about the poller.
  const asOf = asOfNode(provider.freshness);
  if (asOf) el.appendChild(asOf);
  const footer = footerNode(provider.footer, "pv-footer");
  if (footer) el.appendChild(footer);
  return el;
}

function renderUsage(payload) {
  $u("usageTitle").textContent = payload.title;
  $u("usageTrailing").textContent = payload.trailing || "";

  const children = [];
  // "reading logs…" / "no usage data" / "no Claude usage in the last 7 days"
  // are three different Rust sentences, in a colour Rust also chose; this file
  // knows none of them.
  if (payload.message) {
    const message = node("p", "pv-message", payload.message.text);
    message.style.color = payload.message.color;
    children.push(message);
  }
  // Claude's own rollups are one block so the provider sections can sit BESIDE
  // them on a full-width card: `#usageBody` is a grid of `--panel-cols` tracks,
  // and each of these two wrappers is one grid item. Same DOM at one column,
  // where they simply stack — which count applies is CSS's, from Rust's
  // `panel_columns`, never decided here.
  const main = node("div", "usage-main");

  // Not `window` — that name is the global object inside this loop body.
  for (const row of payload.windows || []) main.appendChild(valueRow(row, "window"));

  if (payload.projects) {
    const section = node("div", "pv-section");
    section.dataset.section = "projects";
    section.appendChild(node("hr", "pv-divider"));
    section.appendChild(node("div", "lbl", payload.projects.label));
    for (const item of payload.projects.rows || []) section.appendChild(itemRow(item));
    main.appendChild(section);
  }

  if (main.childElementCount) children.push(main);

  // An unconfigured provider contributes no element at all, so the panel is
  // pixel-identical to its Claude-only self. That is a Rust decision too: the
  // section is simply absent from `providers`. The wrapper follows the same
  // rule: no providers, no second grid item, and `.usage-main` takes the whole
  // body rather than leaving an empty track beside itself.
  const providers = node("div", "usage-providers");
  for (const provider of payload.providers || []) providers.appendChild(providerNode(provider));
  if (providers.childElementCount) children.push(providers);

  $u("usageBody").replaceChildren(...children);

  // Claude's own warning goes in the header, not below the provider sections
  // where the original panel puts it: a line under the body makes the card taller
  // and `.panel-row` stretches its neighbours to match, so a stale rollup would
  // move OpenClaw beside it. The per-provider `.pv-footer` lines stay in the
  // body — they belong to a section, and a section has no header to move to.
  // Ellipsised in a narrow panel, so `title` keeps the whole message reachable.
  const stale = $u("usageStale");
  stale.textContent = payload.footer ? payload.footer.text : "";
  stale.title = payload.footer ? payload.footer.text : "";
  stale.style.color = payload.footer ? payload.footer.color : "";
  stale.hidden = !payload.footer;

  $u("usagePanel").hidden = false;
}

async function refresh() {
  try {
    // Offline (no Tauri), the dumped-fixture path every other panel uses, so
    // this one opens in a plain browser and in the Playwright suite.
    const payload = await callRust("usage", {}, "sample-usage.json");
    if (payload) renderUsage(payload);
    return Boolean(payload && payload.loading);
  } catch {
    // A failed poll leaves the last good panel on screen rather than blanking
    // it: Rust already retains last-known figures through a bad fetch, and
    // wiping the DOM here would undo that.
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
window.__SOLADOR_USAGE_TEST__ = { render: renderUsage, refresh };

})();
