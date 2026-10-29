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
 * sizes the host grid for it. Answering every width with one payload is how a
 * narrow shot once showed overlapping host cards: a layout computed for 2732px
 * painted into a 900px window. So each viewport below has its own dump, and a
 * width with none is refused, never approximated. The widths are what those
 * viewports measure (1024, 908, 900, 466 and 462 wide, less the page padding; a tile's Full view asks
 * at the window's own width too, which is why 900 has a dump); a CSS change that
 * moves them fails here loudly, and the fix is to re-measure and re-pin them in package.json's `fixtures:showcase`.
 */
const COCKPITS = {
  992: "sample-showcase-cockpit-992.json",
  876: "sample-showcase-cockpit-876.json",
  868: "sample-showcase-cockpit-868.json",
  900: "sample-showcase-cockpit-900.json",
  430: "sample-showcase-cockpit-430.json",
  434: "sample-showcase-cockpit-434.json",
};

/** The box `.tile-shot img` crops each panel image into on solador.app (site.css, 311x195 at the 3-up layout). */
const SHOT_W = 311;
const SHOT_H = 195;
/**
 * Every box the card is seen in, as [width, height]. `.tile-shot img` keeps one
 * aspect, 311:195 (site.css, #699), at every width, so the crop is the same at
 * 3-up (1280: 311 wide), 2-up (800: about 330) and 1-up (390: about 328): with
 * the aspect fixed, only the aspect decides what is cut. One entry,
 * not three; `site.spec.js` measures the real boxes at those widths.
 */
const SHOT_BOXES = [[SHOT_W, SHOT_H]];

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

/**
 * The Containers card says "on this machine and on a remote host", so the crop
 * the site shows must include a remote host's section, not only this machine's.
 * The image is scaled to cover the card box, anchored top left: whatever lies
 * past the covered region is cut.
 */
async function expectRemoteHostInCrop(panel) {
  const box = await panel.boundingBox();
  const sections = panel.locator(".cont-section");
  expect(await sections.count()).toBeGreaterThan(1);
  const remote = await sections.last().boundingBox();
  const scale = Math.max(SHOT_W / box.width, SHOT_H / box.height);
  const visibleW = SHOT_W / scale;
  const visibleH = SHOT_H / scale;
  expect(remote.y - box.y + remote.height).toBeLessThanOrEqual(visibleH);
  expect(remote.x - box.x).toBeLessThan(visibleW / 2);
}

/**
 * The Hosts card's alt names only the CPU section ("overall and per-core CPU
 * sparklines", #692), because the CPU section is the one part of the card that
 * the crop is certain to show. So the overall CPU chart and every
 * core sparkline must lie wholly inside the top-left `cover` crop of every box
 * in SHOT_BOXES. The width bound cannot fail at these boxes (the scale is set by
 * the width, so `visibleW` equals the panel's width); it guards a future box.
 */
async function expectCpuSectionInCrop(panel) {
  const box = await panel.boundingBox();
  await expect(panel.locator(".cpuChart"), "the overall CPU chart the alt names").toHaveCount(1);
  const charts = panel.locator(".cpuChart, .cores .core .plot");
  const count = await charts.count();
  expect(await panel.locator(".cores .core .plot").count(), "per-core sparklines the alt names").toBeGreaterThan(0);
  for (const [w, h] of SHOT_BOXES) {
    const scale = Math.max(w / box.width, h / box.height);
    const visibleW = w / scale;
    const visibleH = h / scale;
    for (let i = 0; i < count; i++) {
      const c = await charts.nth(i).boundingBox();
      expect(c.y - box.y + c.height, `chart ${i} ends inside the ${w}x${h} crop`).toBeLessThanOrEqual(visibleH);
      expect(c.x - box.x + c.width, `chart ${i} fits the ${w}x${h} crop's width`).toBeLessThanOrEqual(visibleW + 0.5);
    }
  }
}

/**
 * The Usage card says "Neon, Sentry and Vercel consumption", so each of those
 * three sections must lie wholly inside the crop the site shows. Usage is the
 * one card anchored bottom left (`.tile-shot-bottom`): `cover` scales the image
 * to fill the box, and what lies above the bottom `visibleH` rows is cut. The
 * crop must also keep the panel's full width, or a two-column panel (a wider
 * window than 466pt) would cut the providers' values on the right.
 */
async function expectProvidersInBottomCrop(panel) {
  const box = await panel.boundingBox();
  for (const id of ["neon", "sentry", "vercel"]) {
    await expect(panel.locator(`.pv-section[data-provider="${id}"]`)).toHaveCount(1);
  }
  for (const [w, h] of SHOT_BOXES) {
    const scale = Math.max(w / box.width, h / box.height);
    const visibleW = w / scale;
    const visibleH = h / scale;
    expect(visibleW, `${w}x${h} shows the panel's full width`).toBeGreaterThanOrEqual(box.width - 0.5);
    for (const id of ["neon", "sentry", "vercel"]) {
      const section = await panel.locator(`.pv-section[data-provider="${id}"]`).boundingBox();
      const top = section.y - box.y;
      expect(top, `${id} starts inside the ${w}x${h} crop`).toBeGreaterThanOrEqual(box.height - visibleH);
      expect(top + section.height, `${id} ends inside the ${w}x${h} crop`).toBeLessThanOrEqual(box.height + 0.5);
    }
  }
}

/**
 * The crop's top edge cuts no row and no bar (#699). The Usage card's text no
 * longer names the Codex plan rows, but they show, so the edge must not leave a
 * label-less sliver of one: every child of a plan section lies wholly inside the
 * crop or wholly outside it, in every box.
 */
async function expectNoPlanRowCutByBottomCrop(panel) {
  const box = await panel.boundingBox();
  const sections = panel.locator(".pv-section[data-plan]");
  expect(await sections.count(), "plan sections to check").toBeGreaterThan(0);
  const kids = panel.locator(".pv-section[data-plan] > *");
  const count = await kids.count();
  expect(count, "children of the plan sections").toBeGreaterThan(0);
  for (const [w, h] of SHOT_BOXES) {
    const scale = Math.max(w / box.width, h / box.height);
    const cut = box.height - h / scale;
    for (let i = 0; i < count; i++) {
      const k = await kids.nth(i).boundingBox();
      const top = k.y - box.y;
      const bottom = top + k.height;
      expect(
        bottom <= cut + 0.5 || top >= cut - 0.5,
        `plan child ${i} (rows ${top.toFixed(1)}-${bottom.toFixed(1)}) is not cut by the ${w}x${h} crop's top edge at ${cut.toFixed(1)}`,
      ).toBe(true);
    }
  }
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
  // its panel the whole window and lays it out in columns, so the window is sized
  // to the shape each image needs, not to the old card: `site/index.html` crops
  // every image into a 311x195 box with `object-fit: cover`, so the
  // image's shape decides what the card shows (#672). 908 for the panels whose
  // two-column rows read as a card (repos, runners), 462 for the rest, where a
  // panel stacks as it did in its old card: Containers keeps the remote host
  // under this machine's, Azure Cost keeps the prior month, projection and
  // resource groups. Usage is 466, four wider: at 462 the bottom crop's top
  // edge fell 1pt into the Codex 5H row's own padding, and at 466 it falls just
  // above that section's divider (#699, expectNoPlanRowCutByBottomCrop), so the
  // panel is asked for at 434 as well as the overview's widths. Hosts is one card — this machine's — at 900: the whole host
  // grid at 1800 wide and 2x is a 3500px image for a tile that shows a few
  // hundred. Hosts asks Rust for the cockpit at the window's own width (900) as
  // well as the overview's (868), so both have a dump.
  for (const [selector, name, width, source] of [
    ["#cockpit > .card >> nth=0", "hosts", 900, "hosts"],
    ["#reposPanel", "repos", 908, "ghWorkflows"],
    ["#runnersPanel", "runners", 908, "ghRunners"],
    ["#containersPanel", "containers", 462, "containers"],
    ["#usagePanel", "usage", 466, "claudeUsage"],
    ["#cronsPanel", "crons", 462, "sentryCrons"],
    ["#servicesPanel", "services", 462, "services"],
    ["#azurePanel", "azure-cost", 462, "azureCost"],
    ["#openclawPanel", "openclaw", 462, "openclawAgents"],
  ]) {
    test(`panel: ${name}`, async ({ page, baseURL }) => {
      await page.setViewportSize({ width, height: 1200 });
      await loadTileFull(page, baseURL, source);
      const panel = page.locator(selector);
      await expect(panel).toBeVisible();
      if (name === "containers") await expectRemoteHostInCrop(panel);
      if (name === "usage") {
        await expectProvidersInBottomCrop(panel);
        await expectNoPlanRowCutByBottomCrop(panel);
      }
      if (name === "hosts") await expectCpuSectionInCrop(panel);
      await panel.screenshot({ path: path.join(OUT, `panel-${name}.png`) });
    });
  }
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
