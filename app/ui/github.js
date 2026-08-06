// The Repos and GitHub Runners panels. Same discipline as the cockpit,
// Settings and the Containers panel: every string, colour, column width and
// count arrives from Rust (`app/src-tauri/src/github/`), and this file does
// layout, wiring, and nothing else.
//
// In particular there is no formatting here. The "—" that means "we could not
// find out" and the dimmed "0" that means "there are none" are two different
// Rust decisions arriving as two different `{text, color}` pairs; deriving
// either one from a number in JS would put a second implementation of that
// distinction on the other side of the IPC boundary, where no Rust test can
// see it.
//
// Nothing below uses `innerHTML`. Repo names, runner names and status words
// come from the GitHub API, and a webview parses markup — building rows with
// createElement + textContent means they reach the DOM as text and cannot
// reach it as markup at all, which is stronger than escaping them.
//
// Wrapped in an IIFE, and that is load-bearing: classic scripts share one
// global scope, so a top-level `function render()` here would silently REPLACE
// app.js's and every host card would stop painting (settings.js learned this
// the hard way). The only names crossing the boundary are the two app.js
// exposes — `callRust` and `settingsOpen` — plus the test hook at the bottom.
(function () {

/** How often both panels ask Rust for a fresh payload.
 *
 *  Deliberately faster than the Rust-side poll (the store's
 *  `refresh_interval_secs`, 30s–5m): the payloads are not static between
 *  fetches. The Repos panel's LONGEST column is elapsed-since-start, so a
 *  running job has to tick up, and the Runners footer goes stale on a clock
 *  this side never sees move. Asking every 10s costs one lock and one JSON
 *  build; not asking makes a running job look frozen. */
const REFRESH_MS = 10000;

/** How often to ask while a panel is still filling in.
 *
 *  Rust answers from whatever its poll loop has so far, and its first pass lands
 *  seconds after launch — so on the settled cadence a correct "loading…" still
 *  sat on the cockpit for up to ten seconds after the data was already there.
 *  The panels say when they are waiting (`payload.loading`), and this backs off
 *  the moment they stop. Costs a handful of extra no-op calls, at startup only. */
const LOADING_REFRESH_MS = 1000;

const $g = (id) => document.getElementById(id);

function node(tag, cls, text) {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined && text !== null) n.textContent = text;
  return n;
}

/**
 * Reserves a column's footprint, in points, from Rust's figure.
 *
 * A *width*, never a minimum: these columns are the panel's whole legibility,
 * and one that grows to fit its own text drags every column right of it out of
 * line on exactly the rows that are interesting (#206). A payload with no
 * figure leaves the element to the stylesheet rather than to a number invented
 * here.
 */
function reserveWidth(el, width) {
  if (width !== null && width !== undefined) el.style.width = width + "px";
}

/** A right-aligned fixed-width cell. Rust owns the width, in points. */
function cellNode(cls, cell) {
  const el = node("span", cls, cell.text);
  el.style.color = cell.color;
  reserveWidth(el, cell.width);
  return el;
}

/**
 * The single "this panel has nothing to show yet" line — no token, or no
 * fetch has landed. Rust decides which sentence it is; there is no copy of
 * either one here.
 */
function messageNode(message) {
  return node("p", "gh-message", message.text);
}

// MARK: - Repos

function repoHeader(columns) {
  const el = node("div", "gh-row gh-head");
  // Aligns the header labels with the rows' status dots, which the header
  // has no equivalent of.
  el.appendChild(node("span", "dot gh-dot-spacer"));
  const [repo, ...fixed] = columns;
  const name = node("span", "gh-repo-name", repo.label);
  reserveWidth(name, repo.width);
  el.appendChild(name);
  for (const column of fixed) {
    const cell = node("span", "gh-cell", column.label);
    reserveWidth(cell, column.width);
    el.appendChild(cell);
  }
  // Trailing, not between the name and the cells: the block reads as one unit
  // at the row's start, and the slack collects at the panel's edge — which is
  // where a second column goes when the panel is wide enough for one.
  el.appendChild(node("span", "grow"));
  return el;
}

/**
 * Opens a repo row's GitHub Actions page in the user's real browser.
 *
 * `row.url` is Rust's (`github::actions_url`) and is never built here: the
 * granted ACL scope in `src-tauri/capabilities/default.json` admits exactly
 * that one URL shape, so a URL composed in this file would be a second author
 * of the only string the webview is trusted with. It is passed straight
 * through, unmodified.
 *
 * `plugin:opener|open_url` is the raw IPC spelling of the opener plugin's
 * command. Using it rather than `window.__TAURI__.opener.openUrl` keeps this
 * file on the same single seam as every other call the app makes — `invoke`,
 * which the Playwright suite already stubs and records — instead of a second
 * injected global that offline (and under test) is not there at all.
 */
function openRepo(url) {
  if (!window.__TAURI__ || !url) return;
  // A rejected scope surfaces here as a rejected promise. Swallowed on
  // purpose: `NSWorkspace.open` is a discard in Swift too, and a cockpit panel
  // is the wrong place to report that a click went nowhere.
  window.__TAURI__.core.invoke("plugin:opener|open_url", { url }).catch(() => {});
}

function repoRowNode(row, nameWidth) {
  const el = node("div", "gh-row");
  const dot = node("span", "dot");
  dot.style.background = row.dotColor;
  // The pulse means "a human must approve this" — Rust decides which rows get
  // it, so a colour alone never has to carry two meanings.
  if (row.blinking) dot.classList.add("blink");
  const name = node("span", "gh-repo-name", row.name);
  // The same reservation the header uses. Without it the name is the only
  // shrinkable thing in the row, so in a narrow column the seven fixed cells
  // squeeze it to nothing.
  reserveWidth(name, nameWidth);
  el.append(dot, name);
  for (const cell of row.cells) el.appendChild(cellNode("gh-cell", cell));
  el.appendChild(node("span", "grow"));

  // The Swift panel's `onTapGesture` + `NSWorkspace.open`. A `div` is not a
  // link, so the affordances a real one would carry are spelled out: a role
  // and a Rust-authored accessible name (the row's own text is seven numbers),
  // a tab stop, and Enter — a click-only target is a target a keyboard cannot
  // reach.
  if (row.url) {
    el.classList.add("gh-row-link");
    el.setAttribute("role", "link");
    el.setAttribute("aria-label", row.linkLabel);
    el.tabIndex = 0;
    el.addEventListener("click", () => openRepo(row.url));
    el.addEventListener("keydown", (e) => {
      if (e.key === "Enter") openRepo(row.url);
    });
  }
  return el;
}

/** The last payload rendered, so a column-count change can re-split without
 *  waiting for the next poll. */
let lastRepos = null;

/** Column-major chunks: `[1..6]` over 2 columns reads 1,2,3 | 4,5,6 — down one
 *  column and on to the next, the order the list is already sorted in. */
function chunk(items, columns) {
  const perColumn = Math.ceil(items.length / columns);
  const out = [];
  for (let i = 0; i < items.length; i += perColumn) out.push(items.slice(i, i + perColumn));
  return out;
}

function renderRepos(payload) {
  lastRepos = payload;
  $g("reposTitle").textContent = payload.title;
  $g("reposTrailing").textContent = payload.trailing || "";

  const children = [];
  if (payload.message) {
    children.push(messageNode(payload.message));
  } else {
    // Each column is a table in its own right — its own header, its own
    // right-flush numeric block. Unlike the runner list this cannot be CSS
    // multi-column: balancing has no way to repeat a header per column.
    const cols = Math.max(1, Number($g("reposPanel").dataset.cols) | 0);
    const rows = payload.rows || [];
    const nameWidth = (payload.columns[0] || {}).width;
    for (const group of chunk(rows, Math.min(cols, Math.max(1, rows.length)))) {
      const column = node("div", "gh-col");
      column.appendChild(repoHeader(payload.columns));
      for (const row of group) column.appendChild(repoRowNode(row, nameWidth));
      children.push(column);
    }
  }
  $g("reposBody").replaceChildren(...children);

  // The reassurance line, and only once there is something to be reassured
  // about: no token means no claim about anyone's health.
  const health = $g("reposHealth");
  health.textContent = payload.health ? payload.health.text : "";
  health.style.color = payload.health ? payload.health.color : "";
  health.hidden = !payload.health;

  $g("reposPanel").hidden = false;
}

// MARK: - GitHub Runners

function statNode(stat) {
  const el = node("div", "gh-stat");
  el.appendChild(node("span", "lbl", stat.label));
  const value = node("span", "gh-stat-value", stat.value);
  value.style.color = stat.color;
  el.appendChild(value);
  return el;
}

function runnerRowNode(row) {
  const el = node("div", "gh-row");
  // `data-kind` is what makes a registered row and a remembered-absent one
  // addressable from a test without reading their colours back.
  el.dataset.kind = row.kind;

  const dot = node("span", "dot");
  dot.style.background = row.dotColor;
  el.append(
    dot,
    node("span", "gh-runner-name", row.name),
    node("span", "grow"),
    node("span", "gh-runner-os", row.os)
  );

  // The status is the widest thing in the row that changes — "idle" one poll,
  // "recycling 40s" the next — so it is the one column that must be reserved
  // rather than measured. Sized by Rust for the longest word it can produce,
  // which is what holds the OS chips above in one line while a runner
  // recycles.
  const status = node("span", "gh-runner-status", row.status);
  status.style.color = row.statusColor;
  reserveWidth(status, row.statusWidth);
  el.appendChild(status);
  return el;
}

function renderRunners(payload) {
  $g("runnersTitle").textContent = payload.title;
  $g("runnersTrailing").textContent = payload.trailing || "";

  const children = [];
  if (payload.message) children.push(messageNode(payload.message));
  // Stats and chips share one row, so they share one wrapper — the line they
  // sit on is `.gh-header`'s, not two siblings' worth of block flow.
  const stats = payload.stats || [];
  const chips = payload.chips || [];
  if (stats.length || chips.length) {
    const header = node("div", "gh-header");
    if (stats.length) {
      const statsRow = node("div", "gh-stats");
      for (const stat of stats) statsRow.appendChild(statNode(stat));
      header.appendChild(statsRow);
    }
    if (chips.length) {
      const chipsRow = node("div", "gh-chips");
      for (const chip of chips) chipsRow.appendChild(node("span", "gh-chip", chip));
      header.appendChild(chipsRow);
    }
    children.push(header);
  }
  // The rows go in their own wrapper so the header above them stays
  // full-width: `--panel-cols` splits the LIST, not the whole panel body.
  const list = node("div", "gh-list");
  for (const row of payload.rows || []) list.appendChild(runnerRowNode(row));
  children.push(list);
  $g("runnersBody").replaceChildren(...children);

  // A healthy, fresh panel renders no warning at all — the cockpit stays
  // glanceable, and a warning means something when it appears. It sits in the
  // header rather than under the body because a line below the content makes
  // the card taller, and `.panel-row` stretches its neighbours to match: the
  // panel that degraded would move the panel that did not. Ellipsised in a
  // narrow panel, so `title` keeps the whole message reachable.
  const stale = $g("runnersStale");
  stale.textContent = payload.footer ? payload.footer.text : "";
  stale.title = payload.footer ? payload.footer.text : "";
  stale.style.color = payload.footer ? payload.footer.color : "";
  stale.hidden = !payload.footer;

  $g("runnersPanel").hidden = false;
}

async function refresh() {
  // Offline (no Tauri), the dumped-fixture path the cockpit, Settings and the
  // Containers panel all use, so these panels open in a plain browser and in
  // the Playwright suite.
  const [repos, runners] = await Promise.all([
    callRust("repos", {}, "sample-repos.json").catch(() => null),
    callRust("runners", {}, "sample-runners.json").catch(() => null),
  ]);
  // A failed poll leaves the last good panel on screen rather than blanking
  // it: Rust already retains last-known rows through a bad fetch, and wiping
  // the DOM here would undo that.
  if (repos) renderRepos(repos);
  if (runners) renderRunners(runners);
  // A poll that threw is not "loading" — it is a failure, and retrying it every
  // second would be a busy loop against whatever just broke.
  return Boolean((repos && repos.loading) || (runners && runners.loading));
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

// Repos rebuilds its DOM to re-split, so it cannot ride the custom property
// alone the way the runner list does — app.js fires this the moment Rust's
// count changes, which is a window resize away and 10s sooner than the poll.
$g("reposPanel").addEventListener("panelcolumns", () => {
  if (lastRepos) renderRepos(lastRepos);
});

refresh().then((loading) => {
  if (window.__TAURI__) scheduleRefresh(loading);
});

// Test-only introspection, matching app.js's `window.__DEVCANOPY_TEST__`:
// read-only, and no production behaviour depends on it.
window.__DEVCANOPY_GITHUB_TEST__ = { renderRepos, renderRunners, refresh };

})();
