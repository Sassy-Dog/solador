import { test, expect } from "@playwright/test";
import { stubTileIpc, openTileFull, tileIdFor, lastArgs } from "./tile-full.js";

/**
 * The overview and a tile's Full view stand on their own (#646): neither leans
 * on Solador's Full view's DOM (`#fullView` carries the panels; the old
 * `#cockpitView` wrapper no longer exists), on its icon (`allPanels`), or on the
 * `cockpit` poll for anything that is not a host card.
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

test("the overview, Settings and a tile's Full view work without #cockpitView or the allPanels icon", async ({ page, baseURL }) => {
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  const settings = await fixture(baseURL, "sample-settings.json");
  await stubTileIpc(page, baseURL, {
    sources: ["hosts", "ghRunners", "sentryCrons"], cockpit, answers: { settings_view: settings },
  });
  // Nothing may need the wrapper Solador's Full view used to have.
  await page.addInitScript(() =>
    document.addEventListener("DOMContentLoaded", () => document.getElementById("cockpitView")?.remove()));
  await page.goto("/index.html");
  await expect(page.locator("#dashboardOverview")).toBeVisible();
  expect(await page.locator("#cockpitView").count()).toBe(0);
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

for (const [source, command] of READS) {
  test(`a ${source} tile's Full view reads ${command} and no other panel`, async ({ page, baseURL }) => {
    const cockpit = await fixture(baseURL, "sample-cockpit.json");
    await stubTileIpc(page, baseURL, { sources: READS.map(([s]) => s), cockpit, answers: {} });
    await page.goto("/index.html");
    await expect(page.locator(`[data-tile="${tileIdFor(source)}"]`)).toBeVisible();
    // Past the boot reads, which every panel makes once before the overview is up.
    await page.evaluate(() => { window.__CALLS__.length = 0; });
    await openTileFull(page, source);
    // Long enough for the 1s polls (the overview's and the cockpit's) to fire.
    if (command !== "cockpit") await expect.poll(() => lastArgs(page, command)).toEqual({ tile: tileIdFor(source) });
    await page.waitForTimeout(1500);
    const sent = await page.evaluate(() => window.__CALLS__.map((c) => c.command));
    const others = PANEL_COMMANDS.filter((other) => other !== command && sent.includes(other));
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
