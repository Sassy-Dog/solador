import { test, expect } from "@playwright/test";
import { gotoTileFull, lastArgs, tileIdFor } from "./tile-full.js";

// Same CSP guard as the other suites: the page is served under the app's real
// policy, so a blocked style surfaces as a console error rather than a thrown
// exception. This panel sets every colour through CSSOM for that reason.
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

/** `#e05a4f` as the browser reports a computed colour. */
const rgb = (hex) => {
  const n = parseInt(hex.slice(1), 16);
  return `rgb(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255})`;
};

async function gotoWithServices(page, baseURL, overrides) {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  const svc = overrides ?? (await fixture(baseURL, "sample-services.json"));
  await gotoTileFull(page, baseURL, { source: "services", cockpit, answers: { services: svc } });
  return svc;
}

const rows = (page) => page.locator("#servicesBody .svc-row");

test("the panel paints one row per vendor, in Rust's order", async ({ page, baseURL }) => {
  const svc = await gotoWithServices(page, baseURL);
  await expect(page.locator("#servicesPanel")).toBeVisible();
  // A tile's Full view is headed by the tile's own name (#640), not the
  // payload's `title`; this tile is named for its source.
  await expect(page.locator("#servicesTitle")).toHaveText("Service Health");
  await expect(page.locator("#servicesTrailing")).toHaveText(svc.trailing);

  // Order is fixed and Rust's. A list that re-sorted itself as things broke
  // would be unreadable in the one moment it matters.
  await expect(rows(page)).toHaveCount(svc.rows.length);
  await expect(rows(page).locator(".svc-name")).toHaveText(svc.rows.map((r) => r.label));
  await expect(rows(page).locator(".svc-state")).toHaveText(svc.rows.map((r) => r.state));
});

test("every row wears the colour Rust chose, on its dot and its state word", async ({ page, baseURL }) => {
  const svc = await gotoWithServices(page, baseURL);
  for (const [i, row] of svc.rows.entries()) {
    const el = rows(page).nth(i);
    await expect(el).toHaveAttribute("data-service", row.id);
    await expect(el.locator(".dot")).toHaveCSS("background-color", rgb(row.color));
    await expect(el.locator(".svc-state")).toHaveCSS("color", rgb(row.color));
  }
});

/**
 * The distinction the Azure adapter exists to preserve. Its feed lists active
 * incidents and never publishes health, so a quiet feed is *no known incidents*
 * — a weaker claim than "operational", and one the panel has to word
 * differently or it would be asserting something Azure never said.
 */
test("Azure reads as no-incidents while the others read as operational", async ({ page, baseURL }) => {
  const svc = await gotoWithServices(page, baseURL);
  const azure = svc.rows.find((r) => r.id === "azure");
  const github = svc.rows.find((r) => r.id === "github");
  expect(azure.state).toBe("No Incidents");
  expect(github.state).toBe("Operational");
  expect(azure.state, "the two healthy readings are not the same claim").not.toBe(github.state);

  await expect(rows(page).filter({ hasText: "Azure" }).locator(".svc-state")).toHaveText("No Incidents");
});

/** A vendor nobody has read yet is muted and says so — never green. */
test("an unread vendor is unknown, never operational", async ({ page, baseURL }) => {
  const svc = await gotoWithServices(page, baseURL);
  const unknown = svc.rows.find((r) => r.state === "Unknown");
  expect(unknown, "the fixture must carry a never-read vendor").toBeTruthy();
  const el = rows(page).filter({ hasText: unknown.label });
  await expect(el.locator(".svc-state")).toHaveText("Unknown");
  await expect(el.locator(".svc-state")).not.toHaveCSS("color", rgb("#1c6b41"));
});

test("the incident behind a row is reachable on hover, as text and never as markup", async ({ page, baseURL }) => {
  const detail = 'the Claude API: <img src=x onerror="alert(1)"> (critical).';
  const svc = await fixture(baseURL, "sample-services.json");
  svc.rows[1].detail = detail;
  await gotoWithServices(page, baseURL, svc);

  const el = rows(page).nth(1);
  await expect(el).toHaveAttribute("title", detail);
  expect(await el.evaluate((n) => n.querySelector("img"))).toBeNull();
});

test("the trailing count agrees with the rows under it", async ({ page, baseURL }) => {
  const svc = await gotoWithServices(page, baseURL);
  const degraded = svc.rows.filter((r) => r.degraded).length;
  expect(degraded, "the fixture must carry something degraded").toBeGreaterThan(0);
  await expect(page.locator("#servicesTrailing")).toHaveText(`${degraded} degraded`);

  // …and an all-clear cockpit says so rather than "0 degraded".
  const calm = { ...svc, trailing: "all clear", rows: svc.rows.map((r) => ({ ...r, degraded: false })) };
  await gotoWithServices(page, baseURL, calm);
  await expect(page.locator("#servicesTrailing")).toHaveText("all clear");
});

/**
 * The watched vendors are the operator's, derived from configuration (#284) and
 * adopted by the poll pass in #375 — so the panel now has two ways of having no
 * rows, and they are different facts. Neither may render as an empty body under
 * a green trailing count, which is the empty-green-panel failure this whole
 * area exists to remove.
 */
test("a cockpit watching nothing says so instead of showing an all-clear", async ({ page, baseURL }) => {
  const svc = await gotoWithServices(page, baseURL, await fixture(baseURL, "sample-services-empty.json"));
  expect(svc.rows, "the fixture must be the no-rows rendering").toHaveLength(0);
  expect(svc.empty, "…carrying Rust's sentence").toBeTruthy();

  await expect(rows(page)).toHaveCount(0);
  await expect(page.locator("#servicesBody .svc-empty")).toHaveText(svc.empty.message);
  // Never "all clear", and never "0 degraded": both are verdicts about vendors
  // nobody read.
  await expect(page.locator("#servicesTrailing")).toHaveText("");
});

/** …and a populated panel carries no such sentence. */
test("a panel with rows paints no empty sentence", async ({ page, baseURL }) => {
  await gotoWithServices(page, baseURL);
  await expect(rows(page)).not.toHaveCount(0);
  await expect(page.locator("#servicesBody .svc-empty")).toHaveCount(0);
});

/**
 * The staleness footer (#627) is Rust's line, painted in the header: amber with
 * Rust's colour when the last successful pass is older than the cadence's
 * window, absent on a fresh pass and before any pass.
 */
test("a stale last pass paints Rust's footer", async ({ page, baseURL }) => {
  const stale = await gotoWithServices(page, baseURL, await fixture(baseURL, "sample-services-stale.json"));
  expect(stale.footer, "the fixture must carry the footer").toBeTruthy();
  const footer = page.locator("#servicesStale");
  await expect(footer).toBeVisible();
  await expect(footer).toHaveText(stale.footer.text);
  await expect(footer).toHaveCSS("color", rgb(stale.footer.color));
});

test("a fresh pass paints no footer", async ({ page, baseURL }) => {
  // Rust's payload after a real `pass_finished` just now (`--fresh`): the
  // footer is null because the clock is current, not because no pass ran.
  const fresh = await gotoWithServices(page, baseURL, await fixture(baseURL, "sample-services-fresh.json"));
  expect(fresh.footer).toBeNull();
  await expect(rows(page)).not.toHaveCount(0);
  await expect(page.locator("#servicesStale")).toBeHidden();
});

test("before any pass there is the loading line and no footer", async ({ page, baseURL }) => {
  // Rust's payload before any pass (`--loading`), not a hand-edited fixture.
  const loading = await gotoWithServices(page, baseURL, await fixture(baseURL, "sample-services-loading.json"));
  expect(loading.loading).toBe(true);
  await expect(page.locator("#servicesBody .svc-empty")).toHaveText(loading.empty.message);
  await expect(page.locator("#servicesStale")).toBeHidden();
});

// The renderer asks for its own tile's reading (#639): `{}` would be the
// source-wide payload, wrong in a tile's Full view whenever its scope narrows.
test("the panel asks for its tile's reading, not the source's", async ({ page, baseURL }) => {
  await gotoWithServices(page, baseURL);
  expect(await lastArgs(page, "services")).toEqual({ tile: tileIdFor("services") });
});
