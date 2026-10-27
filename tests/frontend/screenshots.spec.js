import { test, expect } from "@playwright/test";
import fs from "node:fs/promises";
import path from "node:path";
import { dashboardWithTiles, openTileFull } from "./tile-full.js";

/**
 * Generates the product screenshots solador.app and the README show, from the
 * **showcase** fixtures (`npm run fixtures:showcase`, `--showcase` on the dump
 * flags).
 *
 * Not a test — it makes no claim about what is *right*, only a faithful
 * picture of what the frontend renders. It lives here because everything it
 * needs already does: `csp_server.py` serves `app/ui/` under the app's real
 * CSP, and every payload is dumped by Rust. So the screenshots are produced by
 * the shipped code from the shipped palette, on any machine, headless, with no
 * GUI session and nothing to capture by hand.
 *
 * The point of that is drift. A screenshot taken once with a window manager is
 * a photograph of a build that no longer exists — it survives re-palettes,
 * renames and layout changes without anyone noticing it has started lying.
 * Re-running `npm run screenshots` is how this one cannot.
 *
 * The showcase is not the e2e suite's fixture set. Those exercise every
 * failure a panel can render, which is right for a test and wrong for a
 * picture of the product; the showcase is a working morning with exactly one
 * tile out of true. That claim is asserted where the payload is built
 * (`the_showcase_dashboard_has_exactly_one_tile_out_of_true` in main.rs), and
 * the overview shot below checks the rendered chip row says the same.
 *
 * Excluded from `npm test` by `testMatch` in playwright.config.js; run it with
 * `npm run screenshots`.
 */

const OUT = path.resolve(__dirname, "..", "..", "site", "assets", "screenshots");

/** Every panel command the frontend issues, answered from the showcase dumps. */
const PAYLOADS = {
  dashboard_view: "sample-showcase-dashboard.json",
  repos: "sample-showcase-repos.json",
  runners: "sample-showcase-runners.json",
  containers: "sample-showcase-containers.json",
  usage: "sample-showcase-usage.json",
  azure_cost: "sample-showcase-azure.json",
  crons: "sample-showcase-crons.json",
  openclaw: "sample-showcase-openclaw.json",
  services: "sample-showcase-services.json",
};

/**
 * The cockpit payload, keyed by the grid width it was laid out for.
 *
 * `app.js` asks for the cockpit *at* its measured grid width, because Rust
 * reflows the host grid and the panel rows for it. Answering every width with
 * one payload is how `cockpit-narrow.png` once showed overlapping host cards: a
 * layout computed for 2732px painted into a 900px window. So each viewport
 * below has its own dump, and a width with none is refused, never approximated.
 * The widths are what those viewports measure (1800, 1024, 908, 900 and 462
 * wide, less the page padding); a CSS change that moves them fails here loudly, and the
 * fix is to re-measure and re-pin them in package.json's `fixtures:showcase`.
 */
const COCKPITS = {
  1768: "sample-showcase-cockpit-1768.json",
  992: "sample-showcase-cockpit-992.json",
  876: "sample-showcase-cockpit-876.json",
  868: "sample-showcase-cockpit-868.json",
  430: "sample-showcase-cockpit-430.json",
};

const fixture = async (baseURL, name) => (await fetch(`${baseURL}/${name}`)).json();

/**
 * `tileSource` swaps the showcase overview for one `all`-scoped tile of that
 * source (the showcase overview has no Containers, Usage, Azure Cost or
 * OpenClaw tile), so the panel shot is that tile's Full view of the showcase
 * readings. Without it the overview is the showcase's own.
 */
async function stubShowcase(page, baseURL, tileSource = null) {
  const answers = {};
  for (const [command, file] of Object.entries(PAYLOADS)) {
    answers[command] = await fixture(baseURL, file);
  }
  if (tileSource) answers.dashboard_view = dashboardWithTiles(answers.dashboard_view, [tileSource]);
  const cockpits = {};
  for (const [width, file] of Object.entries(COCKPITS)) {
    cockpits[width] = await fixture(baseURL, file);
  }
  await page.addInitScript(([vms, cockpits]) => {
    window.__showcaseMisses = [];
    window.__TAURI__ = {
      core: {
        invoke: async (command, args) => {
          if (command !== "cockpit") return vms[command] ?? null;
          const vm = cockpits[args?.width];
          if (!vm) {
            window.__showcaseMisses.push(args?.width);
            throw new Error(`no showcase cockpit dumped for width ${args?.width}`);
          }
          return vm;
        },
      },
    };
  }, [answers, cockpits]);
}

/** Every cockpit request so far was answered at its own width. */
async function expectNoMisses(page) {
  expect(await page.evaluate(() => window.__showcaseMisses)).toEqual([]);
}

async function loadDetails(page, baseURL) {
  await stubShowcase(page, baseURL);
  await page.goto("/index.html?view=details");
  // Every panel paints from its own command, so waiting on the last one to
  // have a title is what "the cockpit is drawn" means here.
  await expect(page.locator("#cronsPanel")).toBeVisible();
  await page.waitForTimeout(300); // sparklines animate in
  await expectNoMisses(page);
}

/**
 * A panel as an operator sees it from a tile: the overview, the tile's View
 * button, then Full view (`tileFull`). Rust's showcase payload answers the
 * panel's command whatever the tile asked for.
 */
async function loadTileFull(page, baseURL, source) {
  await stubShowcase(page, baseURL, source);
  await page.goto("/index.html");
  await openTileFull(page, source);
  await page.waitForTimeout(300); // sparklines animate in
  await expectNoMisses(page);
}

test.beforeAll(async () => {
  await fs.mkdir(OUT, { recursive: true });
});

test.describe("at 2x: the shots people look at", () => {
  test.use({ deviceScaleFactor: 2 });

  test("the compact overview at laptop size", async ({ page, baseURL }) => {
    await page.setViewportSize({ width: 1024, height: 768 });
    await stubShowcase(page, baseURL);
    await page.goto("/index.html");
    await expect(page.locator("#dashboardOverview")).toBeVisible();
    await expect(page.locator(".db-tile")).toHaveCount(5);
    // The picture has to say what the copy beside it says: one thing is not
    // right, and it is the failing repo.
    const chips = page.locator(".db-attention-items [data-action=\"attention\"]");
    await expect(chips).toHaveCount(1);
    await expect(chips).toContainText("GitHub Repos");
    await expectNoMisses(page);
    await page.screenshot({ path: path.join(OUT, "overview.png"), fullPage: true });
  });

  // [selector, file name, viewport width, tile source]. A tile's Full view gives
  // its panel the whole window, so the window is sized to the panel: 908 for the
  // panels that were half a cockpit wide (876), 462 for the quarter-wide ones
  // (430), keeping each image the shape the site and README lay out. Hosts is
  // one card — this machine's — at the narrow width: the whole host grid at 1800
  // wide and 2x is a 3500px image for a tile that shows a few hundred.
  for (const [selector, name, width, source] of [
    ["#cockpit > .card >> nth=0", "hosts", 900, "hosts"],
    ["#reposPanel", "repos", 908, "ghWorkflows"],
    ["#runnersPanel", "runners", 908, "ghRunners"],
    ["#containersPanel", "containers", 908, "containers"],
    ["#usagePanel", "usage", 462, "claudeUsage"],
    ["#cronsPanel", "crons", 462, "sentryCrons"],
    ["#servicesPanel", "services", 462, "services"],
    ["#azurePanel", "azure-cost", 908, "azureCost"],
    ["#openclawPanel", "openclaw", 462, "openclawAgents"],
  ]) {
    test(`panel: ${name}`, async ({ page, baseURL }) => {
      await page.setViewportSize({ width, height: 1200 });
      await loadTileFull(page, baseURL, source);
      const panel = page.locator(selector);
      await expect(panel).toBeVisible();
      await panel.screenshot({ path: path.join(OUT, `panel-${name}.png`) });
    });
  }
});

// Full-page and tall: at 2x these would add megabytes to git history on every
// regeneration for detail nobody zooms into.
test.describe("at 1x: the full-height shots", () => {
  test("the whole cockpit, at the width it was designed for", async ({ page, baseURL }) => {
    // 1800pt clears the widest layout band, so this is the arrangement the
    // reflow math actually produces rather than a narrow fallback.
    await page.setViewportSize({ width: 1800, height: 1200 });
    await loadDetails(page, baseURL);
    await page.screenshot({ path: path.join(OUT, "cockpit.png"), fullPage: true });
  });

  test("a narrow cockpit, showing the reflow", async ({ page, baseURL }) => {
    await page.setViewportSize({ width: 900, height: 1200 });
    await loadDetails(page, baseURL);
    await page.screenshot({ path: path.join(OUT, "cockpit-narrow.png"), fullPage: true });
  });
});

/**
 * solador.app's social card, from `og-card.html` (not shipped). Its images are
 * answered from `site/` itself, so the card shows the same mark and the same
 * overview the site serves, the overview written by the test above in this run.
 * Runs last for that reason: Playwright runs a file's tests in order unless
 * told otherwise, and this config does not.
 */
test("the social card", async ({ page }) => {
  const site = path.resolve(__dirname, "..", "..", "site");
  await page.setViewportSize({ width: 1200, height: 630 });
  await page.route("https://og.test/**", async (route) => {
    const { pathname } = new URL(route.request().url());
    if (pathname === "/") return route.fulfill({ path: path.join(__dirname, "og-card.html") });
    return route.fulfill({ path: path.join(site, pathname) });
  });
  await page.goto("https://og.test/");
  await page.waitForLoadState("networkidle");
  await page.screenshot({ path: path.join(site, "assets", "og.png") });
});
