import { test, expect } from "@playwright/test";
import { advance, gotoTileFull, openTileFull, lastArgs, stubTileIpc, tileIdFor } from "./tile-full.js";

// Same CSP guard as the other suites: the page is served under the app's real
// policy (csp_server.py), so a blocked style surfaces as a console error rather
// than a thrown exception. Both panels set colours through CSSOM for exactly
// that reason — an inline `style=""` would be dropped under `style-src 'self'`
// and every dot, count and status word would silently render neutral.
test.beforeEach(async ({ page }) => {
  page.cspErrors = [];
  page.on("console", (msg) => {
    if (msg.type() === "error" && /content security policy|refused to (apply|load)/i.test(msg.text())) {
      page.cspErrors.push(msg.text());
    }
  });
});

test.afterEach(async ({ page }) => {
  expect(page.cspErrors, "no CSP violations while the page ran").toEqual([]);
});

const fixture = async (baseURL, name) => (await fetch(`${baseURL}/${name}`)).json();

/**
 * Enters one GitHub tile's Full view (View, then Full view) with the IPC
 * stubbed and every call recorded. `panel` is "repos" (the Repos tile) or
 * "runners" (the GitHub Runners tile): a tile's Full view shows only its own panel, so
 * a test about a panel enters that panel's tile. The other panel's command is
 * still answered, because the page asks for both on every pass.
 *
 * `cockpit` is given an answer too, so this suite never has to think about it:
 * a rejected `cockpit` no longer takes the page down (the overview says so and
 * stays up, #646), but it would add a failure line this suite does not assert on.
 */
const GITHUB_TILE = { repos: "ghWorkflows", runners: "ghRunners" };
async function gotoPanel(page, baseURL, { cockpit, repos, runners }, panel) {
  await gotoTileFull(page, baseURL, {
    source: GITHUB_TILE[panel],
    sources: Object.values(GITHUB_TILE),
    cockpit,
    answers: { repos, runners },
  });
}

/** Leaves the open Full view and enters the other GitHub tile's, as an operator would. */
async function switchPanel(page, panel) {
  await page.keyboard.press("Escape");
  await expect(page.locator("#dashboardOverview")).toBeVisible();
  await openTileFull(page, GITHUB_TILE[panel]);
}

/** Loads the app with both panels stubbed from the dumped fixtures. */
async function gotoWithFixtures(page, baseURL, overrides = {}, panel = "repos") {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  const repos = overrides.repos ?? (await fixture(baseURL, "sample-repos.json"));
  const runners = overrides.runners ?? (await fixture(baseURL, "sample-runners.json"));
  await gotoPanel(page, baseURL, { cockpit, repos, runners }, panel);
  return { repos, runners };
}

/** `#33d17a` as the browser reports a computed colour. */
const rgb = (hex) => {
  const n = parseInt(hex.slice(1), 16);
  return `rgb(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255})`;
};

const repoRows = (page) => page.locator("#reposBody .gh-row:not(.gh-head)");
const runnerRows = (page) => page.locator("#runnersBody .gh-row");

// MARK: - Repos

test("the Repos panel is headed by its tile's name and paints Rust's trailing label and column header", async ({ page, baseURL }) => {
  const titled = { ...(await fixture(baseURL, "sample-repos.json")), title: "Rust's own title" };
  const { repos } = await gotoWithFixtures(page, baseURL, { repos: titled }, "repos");
  await expect(page.locator("#reposPanel")).toBeVisible();

  // A tile's Full view is headed by the tile's own name (#640), not the
  // payload's `title`. The fixture's title is the tile's name too, so it is
  // changed here: the two must differ for the assertion to tell them apart.
  await expect(page.locator("#reposTitle")).toHaveText("GitHub Repos");
  await expect(page.locator("#reposTrailing")).toHaveText(repos.trailing);

  // The header labels AND their widths are Rust's — a column re-typed here
  // could drift from the cells beside it without any Rust test noticing.
  // `.first()`: a wide panel renders one header per column, identical by
  // construction — asserting the first proves the contract.
  const header = page.locator("#reposBody .gh-head").first();
  await expect(header.locator(".gh-repo-name")).toHaveText(repos.columns[0].label);
  await expect(header.locator(".gh-repo-name")).toHaveCSS(
    "width",
    `${repos.columns[0].width}px`
  );
  const labels = await header.locator(".gh-cell").allTextContents();
  expect(labels).toEqual(repos.columns.slice(1).map((c) => c.label));
  for (const [index, column] of repos.columns.slice(1).entries()) {
    await expect(header.locator(".gh-cell").nth(index)).toHaveCSS("width", `${column.width}px`);
  }

  const commands = await page.evaluate(() => window.__CALLS__.map((c) => c.command));
  expect(commands).toContain("repos");
});

test("every repo row carries Rust's cells, widths and dot colour", async ({ page, baseURL }) => {
  const { repos } = await gotoWithFixtures(page, baseURL, {}, "repos");
  await expect(repoRows(page)).toHaveCount(repos.rows.length);

  const names = await repoRows(page).locator(".gh-repo-name").allTextContents();
  expect(names).toEqual(repos.rows.map((r) => r.name));

  for (const [index, expected] of repos.rows.entries()) {
    const row = repoRows(page).nth(index);
    await expect(row.locator(".dot")).toHaveCSS("background-color", rgb(expected.dotColor));
    const cells = row.locator(".gh-cell");
    await expect(cells).toHaveCount(expected.cells.length);
    for (const [i, cell] of expected.cells.entries()) {
      await expect(cells.nth(i)).toHaveText(cell.text);
      await expect(cells.nth(i)).toHaveCSS("color", rgb(cell.color));
      await expect(cells.nth(i)).toHaveCSS("width", `${cell.width}px`);
      // Only a cell Rust gave words to has a tooltip (PRS: who opened them).
      if (cell.title) await expect(cells.nth(i)).toHaveAttribute("title", cell.title);
      else await expect(cells.nth(i)).not.toHaveAttribute("title");
    }
  }
});

/**
 * The load-bearing distinction, asserted through the DOM: "—" is "we could not
 * find out" and "0" is "there are none", and they are visibly different. The
 * fixture is built to contain both, and this fails if the frontend ever starts
 * deriving either from a number.
 */
test("an unknown count renders an em dash, a real zero renders a dimmed zero", async ({ page, baseURL }) => {
  const { repos } = await gotoWithFixtures(page, baseURL, {}, "repos");
  const texts = repos.rows.flatMap((r) => r.cells.map((c) => c.text));
  expect(texts, "the fixture must contain an unknown count").toContain("—");
  expect(texts, "…and a genuine zero beside it").toContain("0");

  const painted = await repoRows(page).locator(".gh-cell").allTextContents();
  expect(painted.filter((t) => t === "—").length).toBe(texts.filter((t) => t === "—").length);
  expect(painted.filter((t) => t === "0").length).toBe(texts.filter((t) => t === "0").length);

  // Neither is ever painted as the other's colour, and neither is ink.
  const dash = repos.rows.flatMap((r) => r.cells).find((c) => c.text === "—");
  const zero = repos.rows.flatMap((r) => r.cells).find((c) => c.text === "0");
  expect(dash.color).toBe(zero.color);
  const number = repos.rows.flatMap((r) => r.cells).find((c) => /^[1-9]/.test(c.text));
  expect(number.color).not.toBe(zero.color);
});

test("only a repo awaiting approval pulses its dot", async ({ page, baseURL }) => {
  const { repos } = await gotoWithFixtures(page, baseURL, {}, "repos");
  const blinking = repos.rows.filter((r) => r.blinking);
  expect(blinking.length, "the fixture must contain an approval gate").toBeGreaterThan(0);

  await expect(repoRows(page).locator(".dot.blink")).toHaveCount(blinking.length);
  for (const [index, row] of repos.rows.entries()) {
    await expect(repoRows(page).nth(index).locator(".dot")).toHaveClass(
      row.blinking ? /blink/ : /^dot$/
    );
  }
});

test("the health line is Rust's sentence and Rust's colour", async ({ page, baseURL }) => {
  const { repos } = await gotoWithFixtures(page, baseURL, {}, "repos");
  const health = page.locator("#reposHealth");
  await expect(health).toBeVisible();
  await expect(health).toHaveText(repos.health.text);
  await expect(health).toHaveCSS("color", rgb(repos.health.color));
});

test("with no token the Repos panel is one sentence, no table and no health claim", async ({ page, baseURL }) => {
  const empty = await fixture(baseURL, "sample-repos-empty.json");
  await gotoWithFixtures(page, baseURL, { repos: empty }, "repos");

  await expect(page.locator("#reposBody .gh-message")).toHaveText(empty.message.text);
  expect(empty.message.text).toBe("connect a GitHub token in Settings");
  await expect(repoRows(page)).toHaveCount(0);
  await expect(page.locator("#reposBody .gh-head")).toHaveCount(0);
  // No credential means no claim about anyone's health, and no counts.
  await expect(page.locator("#reposHealth")).toBeHidden();
  await expect(page.locator("#reposTrailing")).toHaveText("");
});

test("the loading state replaces the table rather than showing an empty one", async ({ page, baseURL }) => {
  const empty = await fixture(baseURL, "sample-repos-empty.json");
  // The authenticated-but-not-yet-fetched payload, as Rust builds it.
  const loading = { ...empty, message: { text: "loading…" } };
  await gotoWithFixtures(page, baseURL, { repos: loading }, "repos");

  await expect(page.locator("#reposBody .gh-message")).toHaveText("loading…");
  await expect(page.locator("#reposBody .gh-head")).toHaveCount(0);
});

test("a Repos panel with every account answering shows no warning", async ({ page, baseURL }) => {
  const { repos } = await gotoWithFixtures(page, baseURL, {}, "repos");
  expect(repos.footer).toBeNull();
  await expect(page.locator("#reposStale")).toBeHidden();
});

/**
 * One account failing is that account's failure (#290). Every row the other
 * accounts fetched stays on screen, the failing account's own repos hold their
 * slots as unreadable rows, and the header says which account and why — the
 * same contract, and the same header slot, as the Runners panel's warning.
 */
test("a per-account failure names the account in the header without blanking the panel", async ({ page, baseURL }) => {
  const good = await fixture(baseURL, "sample-repos.json");
  const failing = {
    ...good,
    // Differs from the tile's name, so the heading asserted below can only be the tile's.
    title: "Rust's own title",
    footer: {
      text: "⚠ work: no token saved — add one in Settings · last ok 4m ago",
      color: "#e09a26",
    },
  };
  await gotoWithFixtures(page, baseURL, { repos: { __frames: [good, failing] } }, "repos");
  // Rows are visible only once the Full view's own healthy reading has painted.
  await expect(repoRows(page).first()).toBeVisible();
  const stale = page.locator("#reposStale");
  await expect(stale).toBeHidden();
  const healthy = await page.locator("#reposPanel").boundingBox();

  // The next poll fails, as when a token expires while the Full view is open.
  await advance(page, "repos");
  await page.evaluate(() => window.__SOLADOR_GITHUB_TEST__.refresh());

  await expect(repoRows(page)).toHaveCount(good.rows.length);
  await expect(stale).toBeVisible();
  await expect(stale).toHaveText(failing.footer.text);
  await expect(stale).toHaveCSS("color", rgb(failing.footer.color));
  // Ellipsised rather than wrapped in a narrow panel, so the whole message has
  // to stay reachable somewhere.
  await expect(stale).toHaveAttribute("title", failing.footer.text);
  await expect(page.locator("#reposBody .gh-message")).toHaveCount(0);

  // In the header for the reason the Runners warning is: a line under the body
  // makes the card taller. Both readings come from ONE page load: whether a
  // boot frame painted (and so left a floor to release) varies from load to
  // load, so heights taken on two loads are not comparable. Within one load the
  // floor is already released when the Full view's rows paint, whenever
  // layout-stability.js's next frame runs (#707, tested above).
  const degraded = await page.locator("#reposPanel").boundingBox();
  expect(degraded.height).toBeCloseTo(healthy.height, 1);
  // The tile's own name heads its Full view (#640), not the payload's title.
  await expect(page.locator("#reposTitle")).toHaveText("GitHub Repos");
});

/**
 * How tall a panel is in its tile's Full view must not depend on whether a frame
 * painted while the app booted (#691). `#fullView` starts visible, every panel
 * reads once before `dashboard.js` has loaded, and the 200px list-panel floor
 * applies until that first render hides the view. `layout-stability.js` then
 * writes the tallest height it saw as an inline floor, which opening the Full
 * view used to keep. Holding `dashboard_view` guarantees the boot frame paints.
 */
test("a floor measured by the boot frame is not carried into the Repos Full view", async ({ page, baseURL }) => {
  const repos = await fixture(baseURL, "sample-repos.json");
  repos.rows = repos.rows.slice(0, 1); // sparse: natural height is well under the 200px floor
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  await stubTileIpc(page, baseURL, {
    source: "ghWorkflows", sources: Object.values(GITHUB_TILE), cockpit,
    answers: { repos, runners: await fixture(baseURL, "sample-runners.json") },
  });
  await page.addInitScript(() => {
    const inner = window.__TAURI__.core.invoke;
    let release;
    const held = new Promise((resolve) => (release = resolve));
    window.__releaseDashboard = () => release();
    window.__TAURI__.core.invoke = (command, args) =>
      command === "dashboard_view" ? held.then(() => inner(command, args)) : inner(command, args);
  });
  await page.goto("/index.html");

  // The boot frame: the unhidden Full view floors the body at the CSS 200px.
  const body = page.locator("#reposBody");
  await expect(repoRows(page).first()).toBeVisible();
  await expect.poll(() => body.evaluate((el) => el.style.minHeight)).toBe("200px");

  const bootWidth = await body.evaluate((el) => el.getBoundingClientRect().width);

  await page.evaluate(() => window.__releaseDashboard());
  await expect(page.locator(`[data-tile="${tileIdFor("ghWorkflows")}"]`)).toBeVisible();
  await openTileFull(page, "ghWorkflows");
  await expect(repoRows(page).first()).toBeVisible();

  // Opening the Full view released the boot frame's floor: the body is its
  // natural height, and keeps no inline floor taller than that.
  // Same width as the boot frame, so the release below is the key's doing and
  // not the width rule's: without the `#fullView` key added for #691 (measured
  // against main) this test fails. The tile part of that key is guarded by the
  // two-tile test below.
  const fullWidth = await body.evaluate((el) => el.getBoundingClientRect().width);
  expect(Math.abs(fullWidth - bootWidth)).toBeLessThanOrEqual(1);
  await expect.poll(() => body.evaluate((el) => el.getBoundingClientRect().height)).toBeLessThan(200);
  const { inline, height } = await body.evaluate((el) => ({
    inline: parseFloat(el.style.minHeight || "0"), height: el.getBoundingClientRect().height,
  }));
  expect(inline).not.toBe(200);
  expect(inline).toBeLessThanOrEqual(Math.ceil(height));
});

/**
 * The release in the test above must not wait for a frame (#707). The boot
 * frame leaves a 200px inline floor on the hidden Full view's body; the key
 * change that releases it used to be applied only in layout-stability.js's next
 * animation frame, so the Full view could paint its first rows (and be measured)
 * with the stale floor whenever that frame ran late on a loaded machine. Here
 * layout-stability's frames are held for the whole opening, which makes that
 * window the whole test: the floor must already be gone when the rows are.
 */
test("opening the Repos Full view releases the boot frame's floor before any frame runs", async ({ page, baseURL }) => {
  const repos = await fixture(baseURL, "sample-repos.json");
  repos.rows = repos.rows.slice(0, 1); // sparse: natural height is well under the 200px floor
  await stubTileIpc(page, baseURL, {
    source: "ghWorkflows", sources: Object.values(GITHUB_TILE),
    cockpit: await fixture(baseURL, "sample-cockpit.json"),
    answers: { repos, runners: await fixture(baseURL, "sample-runners.json") },
  });
  await page.addInitScript(() => {
    const inner = window.requestAnimationFrame.bind(window);
    const held = [];
    window.__holdFrames = false;
    window.requestAnimationFrame = (callback) => {
      if (!window.__holdFrames) return inner(callback);
      held.push(callback);
      return 0;
    };
    window.__runHeldFrames = () => held.splice(0).forEach((callback) => inner(callback));
    // Holding `dashboard_view` guarantees the boot frame paints (as in the test above).
    const invoke = window.__TAURI__.core.invoke;
    let release;
    const gate = new Promise((resolve) => (release = resolve));
    window.__releaseDashboard = () => release();
    window.__TAURI__.core.invoke = (command, args) =>
      command === "dashboard_view" ? gate.then(() => invoke(command, args)) : invoke(command, args);
  });
  await page.goto("/index.html");
  const body = page.locator("#reposBody");
  // The boot frame painted: the unhidden Full view floored the body at the CSS 200px.
  await expect(repoRows(page).first()).toBeVisible();
  await expect.poll(() => body.evaluate((el) => el.style.minHeight)).toBe("200px");
  await page.evaluate(() => window.__releaseDashboard());
  await expect(page.locator(`[data-tile="${tileIdFor("ghWorkflows")}"]`)).toBeVisible();

  await page.evaluate(() => (window.__holdFrames = true));
  await openTileFull(page, "ghWorkflows");
  await expect(repoRows(page).first()).toBeVisible();
  // No layout-stability frame has run since the Full view opened.
  const { inline, height } = await body.evaluate((el) => ({
    inline: el.style.minHeight, height: el.getBoundingClientRect().height,
  }));
  expect(inline).not.toBe("200px");
  expect(height).toBeLessThan(200);

  await page.evaluate(() => { window.__holdFrames = false; window.__runHeldFrames(); });
  await expect.poll(() => body.evaluate((el) => el.getBoundingClientRect().height)).toBeLessThan(200);
});

/**
 * Two tiles of one source paint into the same body, so a Full view is keyed on
 * its tile (#691): tile B's Full view must not inherit the floor tile A's
 * measured.
 */
test("a second Repos tile's Full view does not inherit the first tile's floor", async ({ page, baseURL }) => {
  const many = await fixture(baseURL, "sample-repos.json");
  while (many.rows.length < 40) many.rows = many.rows.concat(many.rows);
  const few = { ...many, rows: many.rows.slice(0, 1) };
  const second = "full-repos-b";
  await stubTileIpc(page, baseURL, {
    source: "ghWorkflows", sources: ["ghWorkflows", { source: "ghWorkflows", id: second }],
    cockpit: await fixture(baseURL, "sample-cockpit.json"),
    answers: { repos: { __byTile: { [tileIdFor("ghWorkflows")]: many, [second]: few } } },
  });
  await page.goto("/index.html");
  await openTileFull(page, "ghWorkflows");
  const body = page.locator("#reposBody");
  await expect(repoRows(page).first()).toBeVisible();
  await expect(repoRows(page)).toHaveCount(many.rows.length);
  await expect.poll(() => body.evaluate((el) => parseFloat(el.style.minHeight || "0"))).toBeGreaterThan(200);
  const floorA = await body.evaluate((el) => parseFloat(el.style.minHeight));

  await page.keyboard.press("Escape");
  await expect(page.locator("#dashboardOverview")).toBeVisible();
  await openTileFull(page, "ghWorkflows", second);
  await expect(repoRows(page)).toHaveCount(1);
  // The old floor is dropped as the Full view switches; B's own is measured on the
  // next animation frame, so poll for it.
  await expect.poll(() => body.evaluate((el) => parseFloat(el.style.minHeight || "0"))).not.toBe(floorA);
  await expect.poll(() => body.evaluate((el) => el.getBoundingClientRect().height)).toBeLessThan(floorA);
  // …and it is B's natural height, not some smaller stale floor.
  await expect.poll(() => body.evaluate((el) => el.getBoundingClientRect().height)).toBeLessThan(200);
});

// MARK: - Repos: tap to open

test("clicking a repo row asks the opener plugin for Rust's URL, unmodified", async ({ page, baseURL }) => {
  const { repos } = await gotoWithFixtures(page, baseURL, {}, "repos");

  // Addressed by repo, not by position: rows are sorted by short name, so
  // `.first()` silently follows any rename of any *other* fixture repo, and
  // the literal below then fails for a reason that has nothing to do with
  // what this test is about.
  const i = repos.rows.findIndex((r) => r.repo === "acme/widget");
  expect(i, "the fixture must contain the repo this test addresses").toBeGreaterThanOrEqual(0);

  await repoRows(page).nth(i).click();

  const opens = await page.evaluate(() =>
    window.__CALLS__.filter((c) => c.command === "plugin:opener|open_url")
  );
  // Exactly one call, exactly one argument, and it is the fixture's own
  // string. A URL rebuilt in JS from the repo name would pass a "it opened
  // something" assertion and still be a second author of the only string the
  // granted ACL scope accepts.
  expect(opens).toEqual([
    { command: "plugin:opener|open_url", args: { url: repos.rows[i].url } },
  ]);
  // Written out, never interpolated -- interpolating would be the very
  // rebuild the comment above rules out.
  expect(repos.rows[i].url).toBe("https://github.com/acme/widget/actions");
});

test("every repo row is its own tap target, including the unreachable one", async ({ page, baseURL }) => {
  const { repos } = await gotoWithFixtures(page, baseURL, {}, "repos");
  const rows = repoRows(page);
  await expect(rows).toHaveCount(repos.rows.length);

  for (const [i, row] of repos.rows.entries()) {
    // Rust's accessible name, not the row's eight numbers read aloud.
    await expect(rows.nth(i)).toHaveAttribute("aria-label", row.linkLabel);
    await expect(rows.nth(i)).toHaveAttribute("role", "link");
    await expect(rows.nth(i)).toHaveJSProperty("tabIndex", 0);
  }
  // `platform` is the repo whose runs could not be fetched — being unable to
  // read a repo's CI is exactly when you want to go and look at it.
  const unreachable = repos.rows.findIndex((r) => r.name === "toolkit");
  expect(unreachable).toBeGreaterThan(-1);
  await rows.nth(unreachable).click();
  expect(
    await page.evaluate(() => window.__CALLS__.filter((c) => c.command === "plugin:opener|open_url"))
  ).toEqual([
    { command: "plugin:opener|open_url", args: { url: repos.rows[unreachable].url } },
  ]);
});

test("a repo row opens from the keyboard, not only from a click", async ({ page, baseURL }) => {
  const { repos } = await gotoWithFixtures(page, baseURL, {}, "repos");

  await repoRows(page).first().focus();
  await page.keyboard.press("Enter");
  expect(
    await page.evaluate(() => window.__CALLS__.filter((c) => c.command === "plugin:opener|open_url"))
  ).toEqual([
    { command: "plugin:opener|open_url", args: { url: repos.rows[0].url } },
  ]);
});

test("a row with no URL is not a link and opens nothing", async ({ page, baseURL }) => {
  const repos = await fixture(baseURL, "sample-repos.json");
  // A payload from a build that predates tap-to-open. The row must degrade to
  // the plain, unclickable row it used to be rather than throwing on click and
  // taking the panel's rendering down with it.
  repos.rows = repos.rows.map(({ url, linkLabel, ...rest }) => rest);
  await gotoWithFixtures(page, baseURL, { repos }, "repos");

  const row = repoRows(page).first();
  await expect(row).toHaveCount(1);
  await expect(row).not.toHaveAttribute("role", "link");
  await row.click();
  expect(
    await page.evaluate(() => window.__CALLS__.filter((c) => c.command === "plugin:opener|open_url"))
  ).toEqual([]);
});

// MARK: - GitHub Runners

test("the Runners panel paints Rust's stats, chips and trailing label", async ({ page, baseURL }) => {
  const { runners } = await gotoWithFixtures(page, baseURL, {}, "runners");
  await expect(page.locator("#runnersPanel")).toBeVisible();

  // A tile's Full view is headed by the tile's own name (#640), not the
  // payload's `title`; this tile is named for its source.
  await expect(page.locator("#runnersTitle")).toHaveText("GitHub Runners");
  await expect(page.locator("#runnersTrailing")).toHaveText(runners.trailing);
  expect(runners.trailing, "the fixture must exercise the missing count").toContain("missing");

  // The rollup lives in the trailing above, so no stat repeats it — OFFLINE is
  // the count `3/4` cannot be read off at a glance.
  expect(runners.stats.map((s) => s.label)).toEqual(["BUSY", "IDLE", "OFFLINE"]);
  const stats = page.locator(".gh-stat");
  await expect(stats).toHaveCount(runners.stats.length);
  for (const [index, stat] of runners.stats.entries()) {
    await expect(stats.nth(index).locator(".lbl")).toHaveText(stat.label);
    await expect(stats.nth(index).locator(".gh-stat-value")).toHaveText(stat.value);
    await expect(stats.nth(index).locator(".gh-stat-value")).toHaveCSS("color", rgb(stat.color));
  }

  // Every chip is a string Rust built, in Rust's order — nothing the renderer
  // composed from an OS name and a pair of counts of its own. `Windows 0/0` is
  // the load-bearing entry: this org has no Windows runner, so a chip the
  // frontend only drew when non-zero would fail here.
  expect(runners.chips, "the fixture must exercise an empty tracked platform")
    .toContain("Windows 0/0");
  await expect(page.locator(".gh-chip")).toHaveText(runners.chips);
});

/**
 * The stats and the chips used to be two stacked rows, which cost a glanceable
 * panel a whole line for one short row of pills. Asserted with bounding boxes
 * and derived from the panel's own geometry rather than pixel constants: the
 * Runners panel's rendered width moves with the layout, and only "same line"
 * and "flush right" are the contract.
 */
test("the Runners header keeps the stats and the chips on one line", async ({ page, baseURL }) => {
  await gotoWithFixtures(page, baseURL, {}, "runners");
  await expect(page.locator("#runnersPanel")).toHaveAttribute("data-cols", "2");

  // Exercise a classic scrollbar even on Macs configured for overlay ones.
  await page.evaluate(selector => {
    const sheet = new CSSStyleSheet();
    sheet.replaceSync(`${selector}::-webkit-scrollbar { width:15px; height:15px; }`);
    document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
  }, "#runnersBody");

  const header = page.locator("#runnersBody .gh-header");
  await expect(header).toHaveCount(1);
  const stats = await header.locator(".gh-stats").boundingBox();
  const chips = await header.locator(".gh-chips").boundingBox();

  // Shared line: the chips' whole vertical extent falls inside the stats'.
  const overlap =
    Math.min(stats.y + stats.height, chips.y + chips.height) - Math.max(stats.y, chips.y);
  expect(
    Math.round(overlap),
    `stats ${JSON.stringify(stats)} vs chips ${JSON.stringify(chips)}`
  ).toBe(Math.round(chips.height));

  // Right-flush against usable content, excluding the reserved scrollbar gutter.
  const right = await page.locator("#runnersBody").evaluate(el =>
    el.getBoundingClientRect().left + el.clientLeft + el.clientWidth);
  expect(Math.abs(chips.x + chips.width - right)).toBeLessThan(1);
  expect(chips.x).toBeGreaterThan(stats.x);

  // And the header really is one line, not a tall box holding two.
  const box = await header.boundingBox();
  expect(Math.round(box.height)).toBe(Math.round(stats.height));
});

test("registered and absent runner rows carry Rust's state words and colours", async ({ page, baseURL }) => {
  const { runners } = await gotoWithFixtures(page, baseURL, {}, "runners");
  await expect(runnerRows(page)).toHaveCount(runners.rows.length);

  // The order is Rust's: macOS before Linux, digit-aware by name within — and
  // an absent runner holds the exact slot it occupied while registered.
  const names = await runnerRows(page).locator(".gh-runner-name").allTextContents();
  expect(names).toEqual(runners.rows.map((r) => r.name));

  for (const [index, expected] of runners.rows.entries()) {
    const row = runnerRows(page).nth(index);
    await expect(row).toHaveAttribute("data-kind", expected.kind);
    await expect(row.locator(".gh-runner-os")).toHaveText(expected.os);
    await expect(row.locator('td').nth(2)).toHaveText(expected.architecture);
    await expect(row.locator(".gh-runner-status")).toHaveText(expected.status);
    await expect(row.locator(".dot")).toHaveCSS("background-color", rgb(expected.dotColor));
    await expect(row.locator(".gh-runner-status")).toHaveCSS("color", rgb(expected.statusColor));
  }

  // The presence semantics the roster exists for: amber while recycling, red
  // once absence passes grace.
  const recycling = runners.rows.find((r) => r.status.startsWith("recycling"));
  const missing = runners.rows.find((r) => r.status.startsWith("missing"));
  expect(recycling.kind).toBe("absent");
  expect(missing.kind).toBe("absent");
  expect(recycling.dotColor).not.toBe(missing.dotColor);
});

/**
 * The right-click "Forget" — the escape hatch for a deliberately
 * decommissioned runner that should not wait out the roster's 24h age-out.
 * The label is Rust's (`forgetLabel`), the invoke carries the row's own name,
 * and the menu never survives its click.
 */
test("right-clicking an absent runner offers Rust's Forget and invokes runners_forget", async ({ page, baseURL }) => {
  const { runners } = await gotoWithFixtures(page, baseURL, {}, "runners");
  const absent = runners.rows.find((r) => r.kind === "absent");
  const row = runnerRows(page).filter({
    has: page.locator(".gh-runner-name", { hasText: absent.name }),
  }).first();

  // Escape dismisses without invoking anything.
  await row.click({ button: "right" });
  await expect(page.locator(".ctx-menu .ctx-item")).toHaveText(runners.forgetLabel);
  await page.keyboard.press("Escape");
  await expect(page.locator(".ctx-menu")).toHaveCount(0);

  await row.click({ button: "right" });
  await page.locator(".ctx-menu .ctx-item").click();
  await expect(page.locator(".ctx-menu")).toHaveCount(0);

  await expect
    .poll(() => page.evaluate(() => window.__CALLS__.map((c) => c.command)))
    .toContain("runners_forget");
  const call = await page.evaluate(
    () => window.__CALLS__.find((c) => c.command === "runners_forget")
  );
  expect(call.args).toEqual({ org: absent.org, name: absent.name });
});

/** A registered runner has nothing to forget, so it gets no menu at all. */
test("a registered runner offers no context menu", async ({ page, baseURL }) => {
  const { runners } = await gotoWithFixtures(page, baseURL, {}, "runners");
  const registered = runners.rows.find((r) => r.kind === "registered");
  await runnerRows(page)
    .filter({ has: page.locator(".gh-runner-name", { hasText: registered.name }) })
    .first()
    .click({ button: "right" });
  await expect(page.locator(".ctx-menu")).toHaveCount(0);
});

/**
 * The status column is a reservation, not a measurement (#206).
 *
 * A presence label ("recycling 40s") is nearly three times the width of a
 * state word ("idle"), so a column that sizes itself to its own text drags the
 * OS chip left on exactly the rows something is wrong with — the panel loses
 * its grid at the moment it is being read hardest. Rust hands every row the
 * same `statusWidth`, so what the roster is doing cannot move a column.
 */
test("the OS column holds one position across every runner state", async ({ page, baseURL }) => {
  const runners = await fixture(baseURL, "sample-runners.json");
  const words = [...new Set(runners.rows.map((r) => r.status.split(" ")[0]))].sort();
  expect(words, "the fixture must mix the steady states with both presence labels").toEqual([
    "busy",
    "idle",
    "missing",
    "offline",
    "recycling",
  ]);

  await gotoWithFixtures(page, baseURL, { runners }, "runners");
  // The narrowest the cockpit ever hands this panel: `PanelKind::GhRunners`'s
  // own 400pt min_width. Aligned there, aligned at every width above it.
  //
  // 400pt is also below the 816 a second column needs, which is what makes the
  // assertion below legitimate: one shared x across ALL rows only holds while
  // they share one column.
  await page.locator("#runnersPanel").evaluate((el) => {
    el.style.width = "400px";
    el.style.setProperty("--panel-cols", "1");
  });

  const rows = runnerRows(page);
  await expect(rows).toHaveCount(runners.rows.length);
  const chips = [];
  for (let i = 0; i < runners.rows.length; i++) {
    chips.push(await rows.nth(i).locator(".gh-runner-os").boundingBox());
  }
  // Every OS label is five characters of one monospace font, so a shared left
  // edge and a shared right edge are the same claim made twice. Both are
  // asserted because the reservation is what guarantees the right edge, and
  // the left edge is what the eye actually reads as a column.
  expect(new Set(chips.map((b) => b.x.toFixed(2))).size, "one x-position").toBe(1);
  expect(new Set(chips.map((b) => (b.x + b.width).toFixed(2))).size, "one right edge").toBe(1);
});

test("the widest presence label fits its table cell without truncating", async ({ page, baseURL }) => {
  const { runners } = await gotoWithFixtures(page, baseURL, {}, "runners");
  // A hidden panel measures 0 <= 0 for any label, so it must be laid out first.
  await expect(page.locator("#runnersPanel")).toBeVisible();
  await page.locator("#runnersPanel").evaluate((el) => {
    el.style.width = "400px";
  });

  const rows = runnerRows(page);
  for (const [index, expected] of runners.rows.entries()) {
    const status = rows.nth(index).locator(".gh-runner-status");
    await expect(status).toHaveText(expected.status);
    const [scroll, client] = await status.evaluate((el) => [el.scrollWidth, el.clientWidth]);
    expect(client, `"${expected.status}" has a laid-out cell`).toBeGreaterThan(0);
    expect(scroll, `"${expected.status}" is clipped by its reserved slot`).toBeLessThanOrEqual(client);
  }
});

test("a healthy Runners panel shows no warning", async ({ page, baseURL }) => {
  const { runners } = await gotoWithFixtures(page, baseURL, {}, "runners");
  expect(runners.footer).toBeNull();
  await expect(page.locator("#runnersStale")).toBeHidden();
});

/**
 * The clock-freeze contract from the DOM's side: a failed fetch keeps every
 * last-good row on screen and adds a warning. Blanking the panel here would
 * undo the retention Rust deliberately implements.
 */
test("a failed fetch keeps the last-good rows and surfaces the error in the header", async ({ page, baseURL }) => {
  const good = await fixture(baseURL, "sample-runners.json");
  const failing = {
    ...good,
    footer: {
      text: "⚠ couldn't read runners — token needs org self-hosted runners (read) · last ok 4m ago",
      color: "#e09a26",
    },
  };
  await gotoWithFixtures(page, baseURL, { runners: failing }, "runners");

  await expect(runnerRows(page)).toHaveCount(good.rows.length);
  const stale = page.locator("#runnersStale");
  await expect(stale).toBeVisible();
  await expect(stale).toHaveText(failing.footer.text);
  await expect(stale).toHaveCSS("color", rgb(failing.footer.color));
  // Ellipsised rather than wrapped in a narrow panel, so the whole message has
  // to stay reachable somewhere.
  await expect(stale).toHaveAttribute("title", failing.footer.text);
  await expect(page.locator("#runnersBody .gh-message")).toHaveCount(0);
});

/**
 * The reason the warning moved into the header at all. It used to be a `<p>`
 * after `.panel-body`, so a panel grew a line the moment it degraded.
 */
test("a Runners error does not make the card taller", async ({ page, baseURL }) => {
  const good = await fixture(baseURL, "sample-runners.json");
  const failing = {
    ...good,
    footer: {
      text: "⚠ couldn't read runners — token needs org self-hosted runners (read) · last ok 4m ago",
      color: "#e09a26",
    },
  };
  // Both readings come from ONE page load, as the Repos test does: a panel
  // body's floor depends on whether a boot frame painted, so heights taken on
  // two loads are not comparable (#684, #691).
  await gotoWithFixtures(page, baseURL, { runners: { __frames: [good, failing] } }, "runners");
  await expect(runnerRows(page).first()).toBeVisible();
  await expect(page.locator("#runnersStale")).toBeHidden();
  const healthy = await page.locator("#runnersPanel").boundingBox();

  await advance(page, "runners");
  await page.evaluate(() => window.__SOLADOR_GITHUB_TEST__.refresh());
  await expect(page.locator("#runnersStale")).toBeVisible();
  const degraded = await page.locator("#runnersPanel").boundingBox();

  expect(degraded.height).toBeCloseTo(healthy.height, 1);

  // Sharing the header means competing for it, and the first cut of this rule
  // copied the host card's `flex-shrink:100` — which is on the *title* there,
  // because a CPU model is the disposable half of that line. Here the title is
  // the panel's name, and the copy ate "GitHub Runners" down to zero the moment
  // the warning appeared. The message is what ellipsises now, not the label.
  const title = page.locator("#runnersTitle");
  // The tile's own name heads its Full view (#640), not the payload's title.
  await expect(title).toHaveText("GitHub Runners");
  const [titled, staled] = await Promise.all([title.boundingBox(), page.locator("#runnersStale").boundingBox()]);
  expect(titled.width, "the panel keeps its name when it degrades").toBeGreaterThan(0);
  expect(
    await title.evaluate((el) => el.scrollWidth <= Math.ceil(el.getBoundingClientRect().width)),
    "the title is not the one being truncated"
  ).toBe(true);
  // …and the warning sits beside the label rather than out at the far edge.
  expect(staled.x).toBeGreaterThan(titled.x);
});

/**
 * The same rule on the Repos header, against the longest warning it now
 * carries: a refused board read names the permission in full, beside a
 * trailing summary that is itself unshrinkable. The title's own
 * `flex-shrink` used to be 1, and a shortfall this size cost it a sub-pixel —
 * which is all `text-overflow:ellipsis` needs to eat "GitHub Repos" down to
 * "GitHub Rep…" while the warning kept most of its characters. The message
 * is the designated give; the title gives nothing.
 */
test("a long Repos warning ellipsises itself, never the panel's name", async ({ page, baseURL }) => {
  const good = await fixture(baseURL, "sample-repos.json");
  const refused = {
    ...good,
    footer: {
      text: "⚠ READY for 6 repos: GitHub refused the board read — grant the token Projects (read) for the org",
      color: "#e0a03a",
    },
  };
  // The panel is ~876pt wide, where the warning fits in part. In Solador's Full
  // view that was the half-width Repos card at an 1800pt window; a tile's Full
  // view gives the panel the whole window, so the window is the panel's width
  // plus the page padding.
  await page.setViewportSize({ width: 908, height: 1200 });
  await gotoWithFixtures(page, baseURL, { repos: refused }, "repos");
  expect(Math.round((await page.locator("#reposPanel").boundingBox()).width)).toBe(876);
  const stale = page.locator("#reposStale");
  await expect(stale).toBeVisible();
  await expect(stale).toHaveAttribute("title", refused.footer.text);
  const title = page.locator("#reposTitle");
  await expect(title).toHaveText("GitHub Repos");
  // Measured fractionally, on purpose: the integer `scrollWidth` the Runners
  // test compares reads 94 <= 94 on a title that is painting "GitHub Rep…",
  // because the shrink that triggered the ellipsis was under a pixel.
  expect(
    await title.evaluate((el) => {
      const range = document.createRange();
      range.selectNodeContents(el);
      return range.getBoundingClientRect().width <= el.getBoundingClientRect().width + 0.01;
    }),
    "the title is not the one being truncated"
  ).toBe(true);
  // The warning is what gave way — cut, but present — and the header itself
  // does not overflow the card.
  const staled = await stale.boundingBox();
  expect(staled.width).toBeGreaterThan(0);
  expect(await stale.evaluate((el) => el.scrollWidth > el.clientWidth), "the warning ellipsises").toBe(true);
  const header = page.locator("#reposPanel .panel-hdr");
  expect(await header.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
});

/**
 * The reported bug, from the DOM's side: at launch Rust has not read the
 * credential store yet, and both panels used to render "connect a GitHub token
 * in Settings" at an operator whose token was fine. The payload now says it is
 * loading, and the panel paints that.
 */
test("a loading payload paints the loading line, not the connect-a-token one", async ({ page, baseURL }) => {
  const repos = { ...(await fixture(baseURL, "sample-repos.json")), message: { text: "loading…" }, rows: [], loading: true };
  const runners = { ...(await fixture(baseURL, "sample-runners.json")), message: { text: "loading runners…" }, rows: [], loading: true };
  await gotoWithFixtures(page, baseURL, { repos, runners }, "repos");

  // Each panel in its own tile's Full view.
  await expect(page.locator("#reposBody .gh-message")).toHaveText("loading…");
  const repoMessages = await page.locator(".gh-message").allTextContents();
  await switchPanel(page, "runners");
  await expect(page.locator("#runnersBody .gh-message")).toHaveText("loading runners…");
  for (const text of [...repoMessages, ...(await page.locator(".gh-message").allTextContents())]) {
    expect(text, "a panel that has not looked may not send anyone to Settings").not.toContain("Settings");
  }
});

/**
 * …and having fixed the message, the cockpit must not then sit on it. Each
 * panel's own timer is 10s with its first tick at load, so a correct "loading…"
 * would still outlive the data by up to ten seconds. `payload.loading` is what
 * tightens the cadence, and dropping it is what relaxes it again.
 */
test("a loading panel is re-asked promptly, and settles once it is not", async ({ page, baseURL }) => {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  const loading = { ...(await fixture(baseURL, "sample-runners.json")), message: { text: "loading runners…" }, rows: [], loading: true };
  const settled = await fixture(baseURL, "sample-runners.json");

  // Answers `loading` until the test advances it (never by counting asks, #407)
  // and records the time of every ask so the cadence itself can be asserted
  // rather than inferred.
  await gotoPanel(page, baseURL, { cockpit, repos: null, runners: { __frames: [loading, settled] } }, "runners");
  const asks = () =>
    page.evaluate(() => window.__CALL_TIMES__.filter((_, i) => window.__CALLS__[i].command === "runners"));

  // Four asks inside a couple of seconds, with the reading still loading, is
  // only reachable on the fast cadence: at the settled 10s the fourth would be
  // half a minute away.
  await expect.poll(async () => (await asks()).length, { timeout: 6000 }).toBeGreaterThanOrEqual(4);
  await expect(page.locator("#runnersBody .gh-message")).toHaveText("loading runners…");

  const times = await asks();
  const gaps = times.slice(1).map((t, i) => t - times[i]);
  for (const gap of gaps) {
    expect(gap, `polls while loading were ${gaps.join("/")}ms apart`).toBeLessThan(5000);
  }

  // The reading settles when the test says so.
  await advance(page, "runners");
  await expect(page.locator("#runnersBody .gh-row").first()).toBeVisible();

  // …and once settled it backs off, or the cockpit would poll every second
  // forever.
  const settledCount = (await asks()).length;
  await page.waitForTimeout(2500);
  expect((await asks()).length).toBeLessThanOrEqual(settledCount + 1);
});

test("with no token the Runners panel is one sentence, no stats and no warning", async ({ page, baseURL }) => {
  const empty = await fixture(baseURL, "sample-runners-empty.json");
  await gotoWithFixtures(page, baseURL, { runners: empty }, "runners");

  await expect(page.locator("#runnersBody .gh-message")).toHaveText(empty.message.text);
  expect(empty.message.text).toBe("connect a GitHub token in Settings");
  await expect(runnerRows(page)).toHaveCount(0);
  await expect(page.locator(".gh-stat")).toHaveCount(0);
  await expect(page.locator(".gh-chip")).toHaveCount(0);
  // Nothing has been fetched, so there is nothing to be stale.
  await expect(page.locator("#runnersStale")).toBeHidden();
  await expect(page.locator("#runnersTrailing")).toHaveText("");
});

// MARK: - GitHub availability (the conjunction chip)

/** The chip as Rust builds it, for a fabricated matrix row. */
const chip = (label, color, detail = "why") => ({ label, color, detail });

/** Both panels' payloads carrying the same verdict. */
async function gotoWithVerdict(page, baseURL, availability) {
  const repos = { ...(await fixture(baseURL, "sample-repos.json")), availability };
  const runners = { ...(await fixture(baseURL, "sample-runners.json")), availability };
  return gotoWithFixtures(page, baseURL, { repos, runners }, "repos");
}

test("both panels paint the verdict beside their own title", async ({ page, baseURL }) => {
  // One shared element would be orphaned when the two panels are shown
  // apart (each in its own tile's Full view), which is why the verdict travels on both payloads.
  const { repos } = await gotoWithVerdict(page, baseURL, chip("Operational", "#1c6b41"));
  for (const [chipId, titleId, panel] of [
    ["#reposAvailability", "#reposTitle", "repos"],
    ["#runnersAvailability", "#runnersTitle", "runners"],
  ]) {
    if (panel === "runners") await switchPanel(page, "runners");
    await expect(page.locator(chipId)).toHaveText(repos.availability.label);
    await expect(page.locator(chipId)).toHaveCSS("color", rgb(repos.availability.color));
    const [c, t] = await Promise.all([
      page.locator(chipId).boundingBox(),
      page.locator(titleId).boundingBox(),
    ]);
    expect(c.x, `${chipId} sits beside ${titleId}`).toBeGreaterThan(t.x);
  }
});

test("every verdict paints Rust's label and colour", async ({ page, baseURL }) => {
  // Every string and colour here is Rust's; this asserts only that the frontend
  // applies what it is given. Amber is "GitHub is slow", red is "runs are
  // failing" or "it's ours" — the label is what tells those two apart.
  for (const [label, color] of [
    ["Operational", "#1c6b41"], // operational + fleet online
    ["Services Degraded", "#e09a26"], // degraded_performance / partial_outage
    ["Major Outage", "#e05a4f"], // major_outage — red, not amber
    ["Fleet Down", "#e05a4f"], // operational + fleet dark -> it's us
    ["Status Unknown", "#5a6b60"], // statuspage unreadable
  ]) {
    await gotoWithVerdict(page, baseURL, chip(label, color));
    await switchPanel(page, "runners");
    await expect(page.locator("#runnersAvailability")).toHaveText(label);
    await expect(page.locator("#runnersAvailability")).toHaveCSS("color", rgb(color));
  }
});

test("an unreachable status page reads as unknown and keeps the fleet rows", async ({ page, baseURL }) => {
  const good = await fixture(baseURL, "sample-runners.json");
  await gotoWithVerdict(page, baseURL, chip("Status Unknown", "#5a6b60", "Couldn't read GitHub's status page."));
  await switchPanel(page, "runners");
  await expect(page.locator("#runnersAvailability")).toHaveText("Status Unknown");
  await expect(page.locator("#runnersAvailability")).toHaveCSS("color", rgb("#5a6b60"));
  // Never green, and never at the cost of the reading it annotates.
  await expect(page.locator("#runnersAvailability")).not.toHaveCSS("color", rgb("#1c6b41"));
  await expect(runnerRows(page)).toHaveCount(good.rows.length);
});

test("the incident detail is reachable on hover, as text and never as markup", async ({ page, baseURL }) => {
  // Statuspage incident bodies carry raw HTML (`<br />`), and this is the one
  // string on the panel sourced from someone else's CMS.
  const detail = 'GitHub Actions: major outage. Incident: <img src=x onerror="alert(1)"> (critical).';
  await gotoWithVerdict(page, baseURL, chip("Major Outage", "#e05a4f", detail));
  await switchPanel(page, "runners");
  await expect(page.locator("#runnersAvailability")).toHaveAttribute("title", detail);
  expect(await page.locator("#runnersAvailability").evaluate((el) => el.querySelector("img"))).toBeNull();
});

test("a payload with no verdict hides the chip rather than emptying it", async ({ page, baseURL }) => {
  await gotoWithVerdict(page, baseURL, null);
  await expect(page.locator("#reposAvailability")).toBeHidden();
  await switchPanel(page, "runners");
  await expect(page.locator("#runnersAvailability")).toBeHidden();
});

/**
 * The chip shares the header with a title that must survive and a staleness
 * warning that is the designated give. It is short and fixed, so it takes
 * `flex:none` — a verdict ellipsised to "GH…" would answer nothing — and it
 * must not cost the card any height.
 */
test("the verdict costs no height and does not truncate", async ({ page, baseURL }) => {
  // Both readings come from ONE page load (#691): the bare payload first, then
  // the one carrying the verdict, through the `refresh()` hook.
  const bareRunners = { ...(await fixture(baseURL, "sample-runners.json")), availability: null };
  const chipped = { ...bareRunners, availability: chip("Fleet Down", "#e05a4f") };
  await gotoWithFixtures(page, baseURL, { runners: { __frames: [bareRunners, chipped] } }, "runners");
  await expect(runnerRows(page).first()).toBeVisible();
  await expect(page.locator("#runnersAvailability")).toBeHidden();
  const bare = await page.locator("#runnersPanel").boundingBox();

  await advance(page, "runners");
  await page.evaluate(() => window.__SOLADOR_GITHUB_TEST__.refresh());
  const withChip = page.locator("#runnersAvailability");
  await expect(withChip).toBeVisible();
  expect((await page.locator("#runnersPanel").boundingBox()).height).toBeCloseTo(bare.height, 1);
  expect(
    await withChip.evaluate((el) => el.scrollWidth <= Math.ceil(el.getBoundingClientRect().width)),
    "the verdict is never the element that ellipsises"
  ).toBe(true);
  // The tile's own name heads its Full view (#640), not the payload's title.
  await expect(page.locator("#runnersTitle")).toHaveText("GitHub Runners");
});

// MARK: - Injection

test("repo and runner names reach the DOM as text, never as markup", async ({ page, baseURL }) => {
  const repos = await fixture(baseURL, "sample-repos.json");
  const runners = await fixture(baseURL, "sample-runners.json");
  // These names come from the GitHub API, and in Tauri the DOM can call
  // `invoke` — an unescaped `<img onerror=...>` would reach the command
  // surface. Building with textContent means markup cannot reach the DOM at
  // all, which is stronger than escaping it.
  const hostile = '<img src=x onerror="window.__PWNED__=1">';
  repos.rows = [{ ...repos.rows[0], name: hostile }];
  runners.rows = [{ ...runners.rows[0], name: hostile }];
  await gotoWithFixtures(page, baseURL, { repos, runners }, "repos");

  await expect(repoRows(page).first().locator(".gh-repo-name")).toHaveText(hostile);
  await switchPanel(page, "runners");
  await expect(runnerRows(page).first().locator(".gh-runner-name")).toHaveText(hostile);
  // Every <img> on the page must be one the page itself put there. This used to
  // read `toHaveCount(0)`, which was the same claim while index.html contained
  // no images at all — the brand mark in the topbar retired that shortcut. The
  // assertion is deliberately about *provenance* rather than about the payload:
  // matching `img[src="x"]` would only catch the exact string this test injects
  // and would pass for the next payload that reaches the DOM as markup.
  const srcs = await page.locator("img").evaluateAll((els) => els.map((el) => el.getAttribute("src")));
  // Non-vacuous on purpose: if the locator found nothing, an injected <img>
  // would also go unseen and the check below would pass on an empty list.
  expect(srcs.length).toBeGreaterThan(0);
  expect([...new Set(srcs)]).toEqual(["mark.svg"]);
  expect(await page.evaluate(() => window.__PWNED__)).toBeUndefined();
});

// MARK: - two-column content

test("a wide panel splits Repos and keeps Runners in one aligned table", async ({ page, baseURL }) => {
  // sample-cockpit.json is dumped at 2732pt, so its two-panel rows are 1358pt
  // each — past both breakpoints (Repos needs 1136, Runners 816).
  const { repos, runners } = await gotoWithFixtures(page, baseURL, {}, "repos");
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  const cols = (id) => cockpit.panelColumns[id];
  expect(cols("ghWorkflows"), "fixture must be wide enough for 2").toBe(2);
  expect(cols("ghRunners")).toBe(2);

  // Rust's number reaches the DOM as the custom property the CSS reads.
  await expect(page.locator("#reposPanel")).toHaveAttribute("data-cols", "2");

  // Repos: two tables, each with its own header, and every repo rendered once.
  await expect(page.locator("#reposBody .gh-col")).toHaveCount(2);
  await expect(page.locator("#reposBody .gh-head")).toHaveCount(2);
  await expect(repoRows(page)).toHaveCount(repos.rows.length);
  // Column-major: the first column holds the first half, in payload order.
  const firstColumn = await page
    .locator("#reposBody .gh-col")
    .first()
    .locator(".gh-row:not(.gh-head) .gh-repo-name")
    .allTextContents();
  expect(firstColumn).toEqual(
    repos.rows.slice(0, Math.ceil(repos.rows.length / 2)).map((r) => r.name)
  );

  // Runners, in their own tile's Full view: one shared set of columns,
  // regardless of panel width.
  await switchPanel(page, "runners");
  await expect(page.locator("#runnersPanel")).toHaveAttribute("data-cols", "2");
  await expect(runnerRows(page)).toHaveCount(runners.rows.length);
  await expect(page.locator("#runnersBody .db-detail-table")).toHaveCount(1);
});

test("a narrow panel keeps every list in one column", async ({ page, baseURL }) => {
  // Same panels, same rows — only the width Rust computed them for differs.
  const narrow = await fixture(baseURL, "sample-cockpit-narrow.json");
  expect(narrow.panelColumns.ghRunners).toBe(1);
  const repos = await fixture(baseURL, "sample-repos.json");
  const runners = await fixture(baseURL, "sample-runners.json");
  await gotoPanel(page, baseURL, { cockpit: narrow, repos, runners }, "repos");

  await expect(page.locator("#reposPanel")).toHaveAttribute("data-cols", "1");
  await expect(page.locator("#reposBody .gh-col")).toHaveCount(1);
  await expect(page.locator("#reposBody .gh-head")).toHaveCount(1);
  await expect(repoRows(page)).toHaveCount(repos.rows.length);
  await switchPanel(page, "runners");
  await expect(page.locator("#runnersBody .db-detail-table")).toHaveCount(1);
});

test("the repo name column is a reservation, so the numeric block never moves", async ({ page, baseURL }) => {
  // #206's rule, now applied to REPO as well: a name that grew to its own text
  // would drag all eight numeric columns right on exactly the longest row.
  const { repos } = await gotoWithFixtures(page, baseURL, {}, "repos");
  const reserved = repos.columns[0].width;
  expect(reserved, "REPO carries a width, not null").toBeGreaterThan(0);
  await expect(repoRows(page).first().locator(".gh-repo-name")).toHaveCSS(
    "width",
    `${reserved}px`
  );
  // Every row's ISSUES cell starts at the same x within its column — header
  // included, which is the alignment the panel exists for. (`.gh-cell` is a
  // class on a span, so `:first-of-type` would never match it; take the first
  // cell per row instead.)
  const xs = await page
    .locator("#reposBody .gh-col")
    .first()
    .locator(".gh-row")
    .evaluateAll((rows) =>
      rows.map((row) => Math.round(row.querySelector(".gh-cell").getBoundingClientRect().x))
    );
  expect(xs.length, "one ISSUES cell per row").toBeGreaterThan(1);
  expect(new Set(xs).size, `ISSUES column x positions ${xs}`).toBe(1);
});

// Both GitHub renderers ask for their own tile's reading (#639). Only the
// six the issue names are pinned elsewhere; these two were covered by
// dashboard.spec.js's `repos` assertion and are pinned here for `runners`.
test("each GitHub panel asks for its own tile's reading, not the source's", async ({ page, baseURL }) => {
  await gotoWithFixtures(page, baseURL, {}, "repos");
  expect(await lastArgs(page, "repos")).toEqual({ tile: tileIdFor("ghWorkflows") });
  await switchPanel(page, "runners");
  expect(await lastArgs(page, "runners")).toEqual({ tile: tileIdFor("ghRunners") });
});
