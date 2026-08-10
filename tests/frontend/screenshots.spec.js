import { test, expect } from "@playwright/test";
import fs from "node:fs/promises";
import path from "node:path";

/**
 * Generates the README's screenshots from the same fixtures the test suite
 * asserts against.
 *
 * Not a test — it makes no assertion about what is *right*, only a faithful
 * picture of what the frontend renders. It lives here because everything it
 * needs already does: `csp_server.py` serves `app/ui/` under the app's real
 * CSP, and `npm run fixtures` regenerates every payload from Rust. So the
 * screenshots are produced by the shipped code from the shipped palette, on
 * any machine, headless, with no GUI session and nothing to capture by hand.
 *
 * The point of that is drift. A README screenshot taken once with a window
 * manager is a photograph of a build that no longer exists — it survives
 * re-palettes, renames and layout changes without anyone noticing it has
 * started lying. Re-running `npm run screenshots` is how this one cannot.
 *
 * Excluded from `npm test` by `testMatch` in playwright.config.js; run it with
 * `npm run screenshots`.
 */

const OUT = path.resolve(__dirname, "..", "..", "Docs", "assets", "screenshots");

/** Every command the frontend issues, answered from the dumped fixtures. */
const PAYLOADS = {
  cockpit: "sample-cockpit.json",
  repos: "sample-repos.json",
  runners: "sample-runners.json",
  containers: "sample-containers.json",
  usage: "sample-usage.json",
  azure_cost: "sample-azure.json",
  crons: "sample-crons.json",
  openclaw: "sample-openclaw.json",
  services: "sample-services.json",
};

const fixture = async (baseURL, name) => (await fetch(`${baseURL}/${name}`)).json();

async function loadCockpit(page, baseURL) {
  const entries = await Promise.all(
    Object.entries(PAYLOADS).map(async ([command, file]) => [
      command,
      await fixture(baseURL, file),
    ])
  );
  const answers = Object.fromEntries(entries);
  await page.addInitScript((vms) => {
    window.__TAURI__ = {
      core: { invoke: async (command) => vms[command] ?? null },
    };
  }, answers);
  await page.goto("/index.html");
  // Every panel paints from its own command, so waiting on the last one to
  // have a title is what "the cockpit is drawn" means here.
  await expect(page.locator("#cronsPanel")).toBeVisible();
  await page.waitForTimeout(300); // sparklines animate in
}

test.beforeAll(async () => {
  await fs.mkdir(OUT, { recursive: true });
});

test("the whole cockpit, at the width it was designed for", async ({ page, baseURL }) => {
  // 1800pt clears the widest layout band, so this is the arrangement the
  // reflow math actually produces rather than a narrow fallback.
  await page.setViewportSize({ width: 1800, height: 1200 });
  await loadCockpit(page, baseURL);
  await page.screenshot({ path: path.join(OUT, "cockpit.png"), fullPage: true });
});

test("a narrow cockpit, showing the reflow", async ({ page, baseURL }) => {
  await page.setViewportSize({ width: 900, height: 1200 });
  await loadCockpit(page, baseURL);
  await page.screenshot({ path: path.join(OUT, "cockpit-narrow.png"), fullPage: true });
});

for (const [id, name] of [
  ["reposPanel", "repos"],
  ["runnersPanel", "runners"],
  ["containersPanel", "containers"],
  ["usagePanel", "usage"],
  ["cronsPanel", "crons"],
  ["servicesPanel", "services"],
  ["azurePanel", "azure-cost"],
  ["openclawPanel", "openclaw"],
]) {
  test(`panel: ${name}`, async ({ page, baseURL }) => {
    await page.setViewportSize({ width: 1800, height: 1200 });
    await loadCockpit(page, baseURL);
    const panel = page.locator(`#${id}`);
    await expect(panel).toBeVisible();
    await panel.screenshot({ path: path.join(OUT, `panel-${name}.png`) });
  });
}
