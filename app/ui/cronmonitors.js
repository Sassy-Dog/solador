/** The Sentry Crons panel: cron monitors that are not ok, and for how long.
 *
 *  Same contract as every other panel script — Rust decides every label, state
 *  word, duration and colour (`app/src-tauri/src/crons.rs`), and this file paints
 *  them. It does not know what `missed_checkin` means, which monitors exist, how
 *  `7d 22h` is spelled, or that an age measured from the last check-in is a
 *  weaker claim than one measured from an incident's start — that distinction is
 *  the whole point of the panel, and it arrives as a different `age` string and a
 *  different `ageColor`. Formatting a duration here would be a second
 *  implementation of the one rule this panel exists to get right.
 *
 *  Nothing below uses `innerHTML`. Monitor slugs, project slugs and Sentry status
 *  words are all remote strings and a webview parses markup, so rows are built
 *  with createElement + textContent.
 *
 *  Its own IIFE, like the others: a `const` at file scope would collide with
 *  app.js's and every host card would stop painting. The only names crossing the
 *  boundary are the two app.js exposes — `callRust` and `settingsOpen` — plus the
 *  test hook at the bottom.
 */
(function () {

/** How often the panel asks Rust for a fresh payload.
 *
 *  Far faster than the hourly read behind it, and not to see new monitors: the
 *  footer carries a relative age that has to tick up on a clock this side never
 *  sees move, and a panel frozen at "just now" through a stuck poller is the one
 *  thing the footer exists to prevent. */
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

/** One monitor environment: dot + name + age, then the scope/state line. */
function rowNode(row) {
  const el = node("div", "cron-row");
  el.dataset.monitor = row.id;
  // Published by Rust rather than inferred from the colour: the panel's own
  // trailing count is built from the same field, and reading it off a pixel
  // would be a second definition of "suppressed".
  if (row.suppressed) el.dataset.suppressed = "true";

  const head = node("div", "cron-head");
  const dot = node("span", "dot");
  // CSSOM, never a `style=""` attribute -- `style-src 'self'` blocks it.
  dot.style.background = row.color;
  head.appendChild(dot);

  const name = node("span", "cron-name", row.label);
  name.style.color = row.color;
  head.appendChild(name);

  // Its own colour, which is NOT the row's: an age derived from a check-in is
  // amber beside a red row, because the monitor is still broken and the number
  // beside it is an approximation.
  const age = node("span", "cron-age", row.age);
  age.style.color = row.ageColor;
  head.appendChild(age);

  el.appendChild(head);
  el.appendChild(node("div", "cron-detail", row.detail));
  // The whole row as one sentence, for the hover — including why an age is not
  // the precise one.
  if (row.title) el.title = row.title;
  return el;
}

function renderCrons(payload) {
  $c("cronsTitle").textContent = payload.title;
  $c("cronsTrailing").textContent = payload.trailing || "";

  const children = [];
  if (payload.message) {
    // One sentence, and its colour is Rust's: muted reads as setup, red as a
    // failure or a read that cannot be trusted, green as a measured all-clear.
    // Choosing between them here would put that judgement where no Rust test can
    // see it — and "no rows" is exactly the state that must not default to calm.
    const message = node("p", "cron-message", payload.message.text);
    message.style.color = payload.message.color;
    children.push(message);
  }
  for (const row of payload.rows || []) children.push(rowNode(row));
  $c("cronsBody").replaceChildren(...children);

  // A healthy, fresh panel renders no warning at all. It sits in the header
  // rather than under the body because a line below the content makes the card
  // taller, and `.panel-row` stretches its neighbours to match.
  const stale = $c("cronsStale");
  stale.textContent = payload.footer ? payload.footer.text : "";
  stale.title = payload.footer ? payload.footer.text : "";
  stale.style.color = payload.footer ? payload.footer.color : "";
  stale.hidden = !payload.footer;

  // How old the ages above are — a DIFFERENT question from the warning beside
  // it, and deliberately a different element. `payload.footer` says the poller
  // failed or is late; `payload.freshness` says the durations on screen were
  // measured 23h ago, which on this panel means every one of them is 23h short.
  // Rust classifies the age against this panel's cadence (`Freshness::classify`)
  // and hands over the finished line, so nothing here compares an age to a
  // threshold — a second rule in this file could disagree with the one the Rust
  // tests guard, and this panel is *about* not misreporting an age.
  //
  // `live` and `unknown` carry no text and paint nothing: a current reading
  // reads as it always did, and a panel that has never read has no rows to date.
  const asOf = payload.freshness && payload.freshness.text;
  const fresh = $c("cronsFreshness");
  fresh.textContent = asOf || "";
  fresh.title = asOf || "";
  fresh.style.color = (payload.freshness && payload.freshness.color) || "";
  fresh.hidden = !asOf;

  $c("cronsPanel").hidden = false;
}

async function refresh() {
  try {
    // Offline (no Tauri), the dumped-fixture path every other panel uses, so this
    // one opens in a plain browser and in the Playwright suite.
    const payload = await callRust("crons", {}, "sample-crons.json");
    if (payload) renderCrons(payload);
    return Boolean(payload && payload.loading);
  } catch {
    // A failed poll leaves the last good panel on screen: Rust already carries
    // the last monitor list forward with the reason in the footer.
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
  // While Settings is up the cockpit is off-screen, so skip the work — but still
  // re-arm, or closing Settings would find a dead timer.
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
window.__SOLADOR_CRONS_TEST__ = { render: renderCrons, refresh };

})();
