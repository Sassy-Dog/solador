import { test, expect } from "@playwright/test";
import { stubTileIpc, openTileFull, tileIdFor, lastArgs, advance } from "./tile-full.js";

/**
 * The overview and a tile's Full view stand on their own (#646): neither leans
 * on its icon (`allPanels`), or on the `cockpit` payload for the Settings label
 * and the spacing, or on a `cockpit` poll for anything that is not a host card.
 *
 * Each test names the line it guards; the PR lists the negative controls.
 */

const fixture = async (baseURL, name) => (await fetch(`${baseURL}/${name}`)).json();

/** Every source a tile's Full view can open, with the command it reads. */
const READS = [
  ["hosts", "cockpit"],
  ["ghWorkflows", "repos"],
  ["ghRunners", "runners"],
  ["containers", "containers"],
  ["services", "services"],
  ["sentryCrons", "crons"],
  ["claudeUsage", "usage"],
  ["azureCost", "azure_cost"],
  ["openclawAgents", "openclaw"],
];
const PANEL_COMMANDS = READS.map(([, command]) => command).filter((command) => command !== "cockpit");
const gear = '.db-chrome [data-action="settings"]';

test("Settings and a tile's Full view work without the allPanels icon, or a Settings label or spacing from cockpit", async ({ page, baseURL }) => {
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const dashboard = await fixture(baseURL, "sample-dashboard.json");
  // The cockpit payload with everything the overview reads from it taken out:
  // the Settings label and the spacing can then only come from `dashboard_view`.
  const { settingsLabel, spacing, rowGapPx, ...cockpit } = await fixture(baseURL, "sample-cockpit.json");
  const settings = await fixture(baseURL, "sample-settings.json");
  await stubTileIpc(page, baseURL, {
    sources: ["hosts", "ghRunners", "sentryCrons"], cockpit, answers: { settings_view: settings },
    dashboardKeys: { rowGapPx: 9, spacing: 11 },
  });
  await page.goto("/index.html");
  await expect(page.locator("#dashboardOverview")).toBeVisible();
  await expect(page.locator(gear)).toHaveAttribute("aria-label", dashboard.labels.settings);
  // ...nor the toolbar's Full view icon.
  await page.locator('[data-action="allPanels"]').evaluate((icon) => icon.remove());

  // Settings opens and closes over the overview.
  await page.locator(gear).click();
  await expect(page.locator("#settings")).toBeVisible();
  await expect(page.locator("#dashboardOverview")).toBeHidden();
  await page.locator("#settingsClose").click();
  await expect(page.locator("#dashboardOverview")).toBeVisible();

  for (const source of ["hosts", "ghRunners", "sentryCrons"]) {
    await openTileFull(page, source);
    // Focus moves into the view that opened, not to an icon that may not exist.
    await expect(page.locator("#fullView")).toBeFocused();
    // The Machines view has read `cockpit` by now, whose payload names no
    // Settings label: the button keeps the overview's.
    if (source === "hosts") await expect.poll(() => lastArgs(page, "cockpit")).toMatchObject({ tile: tileIdFor(source) });
    await expect(page.locator(gear)).toBeVisible();
    await expect(page.locator(gear)).toHaveAttribute("aria-label", dashboard.labels.settings);
    // The gaps are the overview's, since the cockpit payload carries none.
    expect(await page.evaluate(() => [
      document.documentElement.style.getPropertyValue("--row-gap"),
      document.documentElement.style.getPropertyValue("--cockpit-gap"),
    ])).toEqual(["9px", "11px"]);

    // Settings over a tile's Full view puts that same view back.
    await page.locator(gear).click();
    await expect(page.locator("#settings")).toBeVisible();
    await expect(page.locator("#fullView")).toBeHidden();
    await page.locator("#settingsClose").click();
    await expect(page.locator("#fullView")).toBeVisible();
    await expect(page.locator("#fullView")).toHaveAttribute("data-detail-source", source);
    await expect(page.locator("#dashboardOverview")).toBeHidden();

    // Escape returns to the overview and to that tile's View button.
    await page.keyboard.press("Escape");
    await expect(page.locator("#dashboardOverview")).toBeVisible();
    await expect(page.locator("#fullView")).toBeHidden();
    await expect(
      page.locator(`[data-tile="${tileIdFor(source)}"]`).getByRole("button", { name: "View", exact: true }),
    ).toBeFocused();
  }
  expect(errors).toEqual([]);
});

test("a failed cockpit call leaves the overview on screen with Rust's sentence", async ({ page, baseURL }) => {
  const dashboard = await fixture(baseURL, "sample-dashboard.json");
  await stubTileIpc(page, baseURL, {
    source: "ghRunners", cockpit: { __throw: "the IPC said no" }, answers: {},
  });
  await page.goto("/index.html");
  await expect(page.locator("#dashboardOverview")).toBeVisible();
  expect(dashboard.labels.cockpitFailed).toBeTruthy();
  await expect(page.locator("#dashboardOverview .db-action-error")).toHaveText(dashboard.labels.cockpitFailed);
  // The page is still the page: no document replaced by an error line.
  await expect(page.locator("body > pre")).toHaveCount(0);
  await expect(page.locator(`[data-tile="${tileIdFor("ghRunners")}"]`)).toBeVisible();
  // ...and Settings, which no longer waits on that call for its button.
  await expect(page.locator(gear)).toBeVisible();
  await expect(page.locator(gear)).toHaveAttribute("aria-label", dashboard.labels.settings);
});

test("an overview that cannot load says so in the sentence the cockpit payload carries, and points nowhere", async ({ page, baseURL }) => {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  expect(cockpit.overviewLoadFailed).toBeTruthy();
  await stubTileIpc(page, baseURL, { source: "ghRunners", cockpit, answers: {} });
  await page.addInitScript(() => { window.__ANSWERS__.dashboard_view = { __throw: "ipc down" }; });
  await page.goto("/index.html");
  const notice = page.locator("#dashboardLoadError");
  await expect(notice).toHaveText(cockpit.overviewLoadFailed);
  await expect(notice).not.toContainText(/below|full view|panel/i);
});

test("a failed boot cockpit read shows Rust's line, and it clears on its own once cockpit answers", async ({ page, baseURL }) => {
  const dashboard = await fixture(baseURL, "sample-dashboard.json");
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  await stubTileIpc(page, baseURL, {
    source: "ghRunners", cockpit: { __frames: [{ __throw: "cockpit is down" }, cockpit] }, answers: {},
  });
  await page.goto("/index.html");
  const line = page.locator("#dashboardOverview .db-action-error");
  await expect(line).toHaveText(dashboard.labels.cockpitFailed);
  await advance(page, "cockpit");
  // Still the overview: nothing sent the operator anywhere to clear it.
  await expect(line).toHaveCount(0);
  await expect(page.locator("#dashboardOverview")).toBeVisible();
});

test("a Machines tile removed while its Full view is open shows the tile-gone message, never the cockpit line", async ({ page, baseURL }) => {
  const dashboard = await fixture(baseURL, "sample-dashboard.json");
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  await stubTileIpc(page, baseURL, { source: "hosts", cockpit, answers: {} });
  // Records the cockpit line if it is ever on the page, however briefly.
  await page.addInitScript((words) => {
    window.__SAW_COCKPIT_LINE__ = false;
    new MutationObserver(() => {
      if (document.body?.textContent.includes(words)) window.__SAW_COCKPIT_LINE__ = true;
    }).observe(document, { childList: true, subtree: true, characterData: true });
  }, dashboard.labels.cockpitFailed);
  await page.goto("/index.html");
  await openTileFull(page, "hosts");
  // The tile leaves the overview, and Rust refuses its read as it would.
  await page.evaluate(() => {
    const view = structuredClone(window.__ANSWERS__.dashboard_view);
    view.tiles = []; view.layout.tiles = [];
    window.__ANSWERS__.dashboard_view = view;
    window.__ANSWERS__.cockpit = { __throw: "That tile is no longer on the overview." };
  });
  await expect(page.locator("#dashboardOverview")).toBeVisible();
  await expect(page.locator("#dashboardOverview .db-action-error")).toHaveText(dashboard.labels.tileGone);
  await page.waitForTimeout(1500);
  expect(await page.evaluate(() => window.__SAW_COCKPIT_LINE__)).toBe(false);
  await expect(page.locator("#dashboardOverview .db-action-error")).toHaveText(dashboard.labels.tileGone);
});

for (const [source, command] of READS) {
  test(`opening a ${source} tile's Full view reads ${command}${command === "cockpit" ? " with its tile" : " and no other panel, cockpit included"}`, async ({ page, baseURL }) => {
    const cockpit = await fixture(baseURL, "sample-cockpit.json");
    await stubTileIpc(page, baseURL, { sources: READS.map(([s]) => s), cockpit, answers: {} });
    await page.goto("/index.html");
    await expect(page.locator(`[data-tile="${tileIdFor(source)}"]`)).toBeVisible();
    // Past the boot reads, which every panel makes once before the overview is up.
    await page.evaluate(() => { window.__CALLS__.length = 0; });
    await openTileFull(page, source);
    // The reads the view makes on opening and over the next 1s polls; slower
    // panel timers (10s and up) are not waited for.
    await expect.poll(() => lastArgs(page, command)).toMatchObject({ tile: tileIdFor(source) });
    await page.waitForTimeout(1500);
    const sent = await page.evaluate(() => window.__CALLS__.map((c) => c.command));
    // Every other panel command, and `cockpit` unless this is the Machines tile.
    const others = [...PANEL_COMMANDS, "cockpit"].filter((other) => other !== command && sent.includes(other));
    expect(others, `commands sent: ${[...new Set(sent)]}`).toEqual([]);
  });
}

test("a tile's Full view has the operator's spacing before any cockpit answer", async ({ page, baseURL }) => {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  // The cockpit call never answers with a payload; the spacing the Full view
  // paints can only have come from the overview's own read.
  await stubTileIpc(page, baseURL, {
    source: "ghRunners", cockpit: { __throw: "no cockpit" }, answers: {},
    dashboardKeys: { rowGapPx: cockpit.rowGapPx + 7, spacing: cockpit.spacing + 5 },
  });
  await page.goto("/index.html");
  await openTileFull(page, "ghRunners");
  const painted = () => page.evaluate(() => [
    document.documentElement.style.getPropertyValue("--row-gap"),
    document.documentElement.style.getPropertyValue("--cockpit-gap"),
  ]);
  expect(await painted()).toEqual([`${cockpit.rowGapPx + 7}px`, `${cockpit.spacing + 5}px`]);
});

test("a Machines tile's Full view sizes its host cards from the window", async ({ page, baseURL }) => {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  await stubTileIpc(page, baseURL, { source: "hosts", cockpit, answers: {} });
  // A grid that is not laid out measures zero, as it does while hidden.
  await page.addInitScript(() => {
    const clientWidth = Object.getOwnPropertyDescriptor(Element.prototype, "clientWidth");
    Object.defineProperty(Element.prototype, "clientWidth", {
      configurable: true,
      get() { return this.id === "cockpit" ? 0 : clientWidth.get.call(this); },
    });
  });
  await page.goto("/index.html");
  await openTileFull(page, "hosts");
  const window_ = await page.evaluate(() => document.documentElement.clientWidth);
  expect(window_).toBeGreaterThan(0);
  await expect.poll(async () => (await lastArgs(page, "cockpit")).width).toBe(window_);
});
