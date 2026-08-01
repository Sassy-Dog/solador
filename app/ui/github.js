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

const $g = (id) => document.getElementById(id);

function node(tag, cls, text) {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined && text !== null) n.textContent = text;
  return n;
}

/** A right-aligned fixed-width cell. Rust owns the width, in points. */
function cellNode(cls, cell) {
  const el = node("span", cls, cell.text);
  el.style.color = cell.color;
  if (cell.width !== null && cell.width !== undefined) {
    el.style.width = cell.width + "px";
  }
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
  el.appendChild(node("span", "gh-repo-name", repo.label));
  el.appendChild(node("span", "grow"));
  for (const column of fixed) {
    const cell = node("span", "gh-cell", column.label);
    if (column.width !== null) cell.style.width = column.width + "px";
    el.appendChild(cell);
  }
  return el;
}

function repoRowNode(row) {
  const el = node("div", "gh-row");
  const dot = node("span", "dot");
  dot.style.background = row.dotColor;
  // The pulse means "a human must approve this" — Rust decides which rows get
  // it, so a colour alone never has to carry two meanings.
  if (row.blinking) dot.classList.add("blink");
  el.append(dot, node("span", "gh-repo-name", row.name), node("span", "grow"));
  for (const cell of row.cells) el.appendChild(cellNode("gh-cell", cell));
  return el;
}

function renderRepos(payload) {
  $g("reposTitle").textContent = payload.title;
  $g("reposTrailing").textContent = payload.trailing || "";

  const children = [];
  if (payload.message) {
    children.push(messageNode(payload.message));
  } else {
    children.push(repoHeader(payload.columns));
    for (const row of payload.rows || []) children.push(repoRowNode(row));
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

  const status = node("span", "gh-runner-status", row.status);
  status.style.color = row.statusColor;
  el.appendChild(status);
  return el;
}

function renderRunners(payload) {
  $g("runnersTitle").textContent = payload.title;
  $g("runnersTrailing").textContent = payload.trailing || "";

  const children = [];
  if (payload.message) children.push(messageNode(payload.message));
  if ((payload.stats || []).length) {
    const stats = node("div", "gh-stats");
    for (const stat of payload.stats) stats.appendChild(statNode(stat));
    children.push(stats);
  }
  if ((payload.chips || []).length) {
    const chips = node("div", "gh-chips");
    for (const chip of payload.chips) chips.appendChild(node("span", "gh-chip", chip));
    children.push(chips);
  }
  for (const row of payload.rows || []) children.push(runnerRowNode(row));
  $g("runnersBody").replaceChildren(...children);

  // A healthy, fresh panel renders no footer at all — the cockpit stays
  // glanceable, and a warning line means something when it appears.
  const footer = $g("runnersFooter");
  footer.textContent = payload.footer ? payload.footer.text : "";
  footer.style.color = payload.footer ? payload.footer.color : "";
  footer.hidden = !payload.footer;

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
}

refresh();
if (window.__TAURI__) {
  setInterval(() => { if (!settingsOpen) refresh(); }, REFRESH_MS);
}

// Test-only introspection, matching app.js's `window.__DEVCANOPY_TEST__`:
// read-only, and no production behaviour depends on it.
window.__DEVCANOPY_GITHUB_TEST__ = { renderRepos, renderRunners, refresh };

})();
