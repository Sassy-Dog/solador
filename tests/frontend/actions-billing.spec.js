import { test, expect } from "@playwright/test";
import { gotoTileFull, lastArgs, openTileFull, stubTileIpc, tileIdFor } from "./tile-full.js";

// The Actions billing panel paints what the `actions_billing` command decided
// and nothing else: the thresholds, the money and the minutes are Rust's, and
// every state here is a fixture Rust dumped (`--dump-actions-billing`), never a
// payload written by hand. The few hand-built payloads below exist to prove the
// renderer does not compute anything of its own.

// Same CSP guard as the other panel suites: the bar's colour and width are
// inline styles, and a dropped one would paint a full-width bar in no colour.
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

async function gotoPanel(page, baseURL, name = "sample-actions-billing.json") {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  const payload = await fixture(baseURL, name);
  await gotoTileFull(page, baseURL, { source: "actionsBilling", cockpit, answers: { actions_billing: payload } });
  return payload;
}

/** `#33d17a` as the browser reports a computed colour. */
const rgb = (hex) => {
  const n = parseInt(hex.slice(1), 16);
  return `rgb(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255})`;
};

const bodyText = (page) => page.locator("#actionsBillingBody").innerText();

test("the panel is headed by its tile's name and paints Rust's headline, minutes, trailing label, budget and breakdowns", async ({ page, baseURL }) => {
  const p = await gotoPanel(page, baseURL);
  await expect(page.locator("#actionsBillingPanel")).toBeVisible();
  await expect(page.locator("#actionsBillingTitle")).toHaveText("Actions billing");
  await expect(page.locator("#actionsBillingTrailing")).toHaveText(p.trailing);

  await expect(page.locator("#actionsBillingBody .az-headline .value")).toHaveText(p.headline.value);
  await expect(page.locator("#actionsBillingBody .az-headline .caption")).toHaveText(p.headline.caption);
  expect(p.headline.value).toMatch(/^\$[\d,]+\.\d{2}$/);

  const stats = page.locator("#actionsBillingBody .az-main > .pv-row");
  await expect(stats.locator(".lbl")).toHaveText(p.stats.map((s) => s.label));
  await expect(stats.locator(".val")).toHaveText(p.stats.map((s) => s.value));

  const budget = page.locator('#actionsBillingBody [data-section="budget"]');
  await expect(budget.locator(".lbl")).toHaveText(p.budget.label);
  await expect(budget.locator(".val")).toHaveText(p.budget.value);

  // Per SKU (minutes and net) and the top repos, capped by Rust.
  const sections = page.locator("#actionsBillingBody .az-columns > .pv-section");
  await expect(sections.locator(".lbl")).toHaveText(p.breakdowns.map((b) => b.title));
  for (const [i, column] of p.breakdowns.entries()) {
    const rows = sections.nth(i).locator(".pv-item");
    await expect(rows.locator(".name")).toHaveText(column.rows.map((r) => r.name));
    await expect(rows.locator(".val")).toHaveText(column.rows.map((r) => r.value));
  }

  // The report's own lag is a plain line in the body.
  await expect(page.locator('#actionsBillingBody [data-section="data-through"]')).toHaveText(p.dataThrough.text);
  expect(p.dataThrough.text).toMatch(/^usage through /);
});

test("the panel asks for its tile's reading, not the source's", async ({ page, baseURL }) => {
  await gotoPanel(page, baseURL);
  expect(await lastArgs(page, "actions_billing")).toEqual({ tile: tileIdFor("actionsBilling") });
});

test("the budget bar is amber at 0.9 and red at 1.0, in the colours Rust chose, at the width Rust chose", async ({ page, baseURL }) => {
  const amber = await gotoPanel(page, baseURL);
  expect(amber.budget.bar.fraction, "the default fixture sits in the amber step").toBeGreaterThanOrEqual(0.9);
  expect(amber.budget.bar.fraction).toBeLessThan(1);
  const bar = page.locator('#actionsBillingBody [data-section="budget"] .pbar > span');
  await expect(bar).toHaveCount(1);
  await expect(bar).toHaveCSS("background-color", rgb(amber.budget.bar.color));
  expect(parseFloat(await bar.evaluate((el) => el.style.width))).toBeCloseTo(amber.budget.bar.fraction * 100, 1);

  const red = await gotoPanel(page, baseURL, "sample-actions-billing-over-budget.json");
  expect(red.budget.bar.color, "red is not amber").not.toBe(amber.budget.bar.color);
  await expect(bar).toHaveCSS("background-color", rgb(red.budget.bar.color));
  // Past the budget the width pins full; the colour is what says "past it".
  expect(await bar.evaluate((el) => el.style.width)).toBe("100%");
  // The two colours are the ones Rust's `progress_bar` names for those steps.
  expect(amber.budget.bar.color).toBe("#e0a03a");
  expect(red.budget.bar.color).toBe("#e0614f");
});

test("the budget's label says whether it stops jobs", async ({ page, baseURL }) => {
  const stops = await gotoPanel(page, baseURL);
  expect(stops.budget.label).toMatch(/STOPS JOBS/);
  await expect(page.locator('#actionsBillingBody [data-section="budget"] .lbl')).toHaveText(stops.budget.label);
  const continues = await gotoPanel(page, baseURL, "sample-actions-billing-over-budget.json");
  expect(continues.budget.label).toMatch(/JOBS CONTINUE/);
  await expect(page.locator('#actionsBillingBody [data-section="budget"] .lbl')).toHaveText(continues.budget.label);
});

test("spend over a zero-amount budget is the spend over $0.00 with a full red bar", async ({ page, baseURL }) => {
  const p = await gotoPanel(page, baseURL, "sample-actions-billing-zero-budget-over.json");
  expect(p.budget.value).toMatch(/^\$[\d,.]+ \/ \$0\.00$/);
  expect(p.budget.bar.color).toBe("#e0614f");
  expect(p.budget.label).toMatch(/STOPS JOBS/);
  await expect(page.locator('#actionsBillingBody [data-section="budget"] .val')).toHaveText(p.budget.value);
  const bar = page.locator('#actionsBillingBody [data-section="budget"] .pbar > span');
  await expect(bar).toHaveCount(1);
  await expect(bar).toHaveCSS("background-color", rgb(p.budget.bar.color));
  expect(await bar.evaluate((el) => el.style.width)).toBe("100%");
});

test("no budget says so and draws no bar, never a 0% one", async ({ page, baseURL }) => {
  const p = await gotoPanel(page, baseURL, "sample-actions-billing-no-budget.json");
  expect(p.budget).toBeNull();
  await expect(page.locator('#actionsBillingBody [data-section="budget-note"]')).toHaveText("no Actions budget set");
  await expect(page.locator("#actionsBillingBody .pbar")).toHaveCount(0);
  await expect(page.locator('#actionsBillingBody [data-section="budget"]')).toHaveCount(0);
  // The usage is still there: no budget is not no data.
  await expect(page.locator("#actionsBillingBody .az-headline .value")).toHaveText(p.headline.value);
});

test("a budget with no measured fraction is a label and figures, not an empty track", async ({ page, baseURL }) => {
  const p = await gotoPanel(page, baseURL);
  await page.evaluate(
    (vm) => window.__SOLADOR_ACTIONS_BILLING_TEST__.render({ ...vm, budget: { ...vm.budget, value: "— / $45.00", bar: null } }),
    p
  );
  await expect(page.locator('#actionsBillingBody [data-section="budget"] .val')).toHaveText("— / $45.00");
  await expect(page.locator("#actionsBillingBody .pbar")).toHaveCount(0);
});

const alertRows = (page) => page.locator('#actionsBillingBody [data-section="alert"]');

test("a daily spike is one red row, painted exactly as Rust sent it (#770)", async ({ page, baseURL }) => {
  const p = await gotoPanel(page, baseURL, "sample-actions-billing-spike.json");
  expect(p.alerts).toHaveLength(1);
  const [alert] = p.alerts;
  expect(alert.label).toMatch(/^SPIKE /);
  expect(alert.color, "tripped is red").toBe("#e0614f");
  const row = alertRows(page);
  await expect(row).toHaveCount(1);
  await expect(row.locator(".lbl")).toHaveText(alert.label);
  await expect(row.locator(".val")).toHaveText(alert.value);
  await expect(row.locator(".val")).toHaveCSS("color", rgb(alert.color));
  await expect(row.locator(".pv-note")).toHaveText(alert.detail);
});

test("macOS minutes on a self-hosted repo are red rows, matched on the list Rust held (#770)", async ({ page, baseURL }) => {
  const p = await gotoPanel(page, baseURL, "sample-actions-billing-macos-on-self-hosted.json");
  expect(p.alerts.length).toBeGreaterThan(0);
  expect(p.alerts.every((a) => a.label.startsWith("MACOS ON SELF-HOSTED") && a.color === "#e0614f")).toBe(true);
  const rows = alertRows(page);
  await expect(rows).toHaveCount(p.alerts.length);
  await expect(rows.locator(".lbl")).toHaveText(p.alerts.map((a) => a.label));
  await expect(rows.locator(".val")).toHaveText(p.alerts.map((a) => a.value));
});

test("a fixture with no tripped rule paints no alert row, and an unjudged rule is a muted row (#770)", async ({ page, baseURL }) => {
  const p = await gotoPanel(page, baseURL);
  expect(p.alerts).toEqual([]);
  await expect(page.locator("#actionsBillingBody .az-headline .value")).toHaveText(p.headline.value);
  await expect(alertRows(page)).toHaveCount(0);
  // Rust's muted "unknown" row is painted as given, in the colour it chose.
  await page.evaluate(
    (vm) => window.__SOLADOR_ACTIONS_BILLING_TEST__.render({
      ...vm,
      alerts: [{ id: "spike-unknown", label: "SPIKE CHECK", value: "—", detail: "7-day baseline unknown", color: "#123456" }],
    }),
    p
  );
  const row = alertRows(page);
  await expect(row).toHaveCount(1);
  await expect(row.locator(".val")).toHaveText("—");
  await expect(row.locator(".val")).toHaveCSS("color", rgb("#123456"));
  await expect(row.locator(".pv-note")).toHaveText("7-day baseline unknown");
});

test("a month with no Actions usage says so in words, not 0 or $0.00", async ({ page, baseURL }) => {
  const p = await gotoPanel(page, baseURL, "sample-actions-billing-empty.json");
  await expect(page.locator("#actionsBillingBody .pv-message")).toHaveText(p.message.text);
  expect(p.message.text).toBe("no Actions usage this month");
  await expect(page.locator("#actionsBillingBody .az-headline")).toHaveCount(0);
  await expect(page.locator("#actionsBillingTrailing")).toHaveText("");
  const text = await bodyText(page);
  expect(text).not.toContain("$0.00");
  expect(text).not.toMatch(/\b0 min\b/);
});

test("not configured shows the setup sentence naming Settings → Sources, and a failed read is red and says what the token needs", async ({ page, baseURL }) => {
  const unconfigured = await gotoPanel(page, baseURL, "sample-actions-billing-unconfigured.json");
  await expect(page.locator("#actionsBillingBody .pv-message")).toHaveText(unconfigured.message.text);
  expect(unconfigured.message.text).toContain("Settings → Sources");
  await expect(page.locator("#actionsBillingBody .pv-message")).toHaveCSS("color", rgb(unconfigured.message.color));
  await expect(page.locator("#actionsBillingBody .az-headline")).toHaveCount(0);

  const failed = await gotoPanel(page, baseURL, "sample-actions-billing-error.json");
  // The two states must not be conflated, and the discriminator is a colour
  // Rust chose.
  expect(failed.message.color).not.toBe(unconfigured.message.color);
  await expect(page.locator("#actionsBillingBody .pv-message")).toHaveText(failed.message.text);
  await expect(page.locator("#actionsBillingBody .pv-message")).toHaveCSS("color", rgb(failed.message.color));
  expect(failed.message.text).toContain("Administration: read");
  expect(failed.message.text).toContain("org admin");
});

test("'usage through' is never in the status footer or the freshness clock, healthy or stale", async ({ page, baseURL }) => {
  const live = await gotoPanel(page, baseURL);
  expect(live.footer, "a healthy tile warns about nothing").toBeNull();
  expect(live.freshness, "and has no clock to show").toBeNull();
  await expect(page.locator("#actionsBillingStale")).toBeHidden();
  await expect(page.locator("#actionsBillingFreshness")).toBeHidden();
  await expect(page.locator("#actionsBillingBody")).toContainText("usage through");

  const stale = await gotoPanel(page, baseURL, "sample-actions-billing-stale.json");
  // The poller is late: both header lines speak, and neither is the report's lag.
  await expect(page.locator("#actionsBillingStale")).toHaveText(stale.footer.text);
  await expect(page.locator("#actionsBillingFreshness")).toHaveText(stale.freshness.text);
  expect(JSON.stringify(stale.footer)).not.toContain("usage through");
  expect(JSON.stringify(stale.freshness)).not.toContain("usage through");
  await expect(page.locator("#actionsBillingStale")).not.toContainText("usage through");
  await expect(page.locator("#actionsBillingFreshness")).not.toContainText("usage through");
  // The lag line is the same in both: it is about the report, not the poller.
  expect(stale.dataThrough).toEqual(live.dataThrough);
  await expect(page.locator("#actionsBillingBody")).toContainText("usage through");
});

test("a stale reading keeps its figures, dims the headline and dates it in the header", async ({ page, baseURL }) => {
  const live = await fixture(baseURL, "sample-actions-billing.json");
  const stale = await gotoPanel(page, baseURL, "sample-actions-billing-stale.json");
  expect(stale.headline.value).toBe(live.headline.value);
  expect(stale.headline.valueColor).not.toBe(live.headline.valueColor);
  await expect(page.locator("#actionsBillingBody .az-headline .value")).toHaveCSS("color", rgb(stale.headline.valueColor));
  await expect(page.locator("#actionsBillingFreshness")).toHaveCSS("color", rgb(stale.freshness.color));
});

test("the renderer computes no thresholds and formats no money", async ({ page, baseURL }) => {
  const p = await gotoPanel(page, baseURL);
  // A fraction and colour no threshold of 0.9 or 1.0 would produce, and figures
  // no formatter would produce: they must reach the page exactly as sent.
  await page.evaluate(
    (vm) =>
      window.__SOLADOR_ACTIONS_BILLING_TEST__.render({
        ...vm,
        headline: { ...vm.headline, value: "12.5 dollars" },
        budget: { ...vm.budget, value: "abc / xyz", bar: { fraction: 0.99, color: "#123456" } },
      }),
    p
  );
  await expect(page.locator("#actionsBillingBody .az-headline .value")).toHaveText("12.5 dollars");
  await expect(page.locator('#actionsBillingBody [data-section="budget"] .val')).toHaveText("abc / xyz");
  const bar = page.locator("#actionsBillingBody .pbar > span");
  await expect(bar).toHaveCSS("background-color", "rgb(18, 52, 86)");
  expect(await bar.evaluate((el) => el.style.width)).toBe("99%");
});

test("repository names are text, never markup", async ({ page, baseURL }) => {
  const p = await gotoPanel(page, baseURL);
  const hostile = '<img src=x onerror="window.__PWNED__=1">';
  await page.evaluate(
    ([vm, name]) =>
      window.__SOLADOR_ACTIONS_BILLING_TEST__.render({
        ...vm,
        breakdowns: [{ ...vm.breakdowns[1], rows: [{ ...vm.breakdowns[1].rows[0], name }] }],
      }),
    [p, hostile]
  );
  await expect(page.locator("#actionsBillingBody .pv-item .name")).toHaveText(hostile);
  await expect(page.locator("#actionsBillingBody img")).toHaveCount(0);
  expect(await page.evaluate(() => window.__PWNED__)).toBeUndefined();
});

test("a warning costs the card no height, however long it is", async ({ page, baseURL }) => {
  const healthy = await gotoPanel(page, baseURL);
  const panel = page.locator("#actionsBillingPanel");
  const header = page.locator("#actionsBillingPanel .panel-hdr");
  await expect(panel).toBeVisible();
  await expect(header).toBeVisible();
  const before = await panel.boundingBox();
  const headerBefore = await header.boundingBox();

  const long = "⚠ usage: " + "a very long failure sentence ".repeat(20) + "· last ok 6h ago";
  await page.evaluate(
    ([vm, text]) => window.__SOLADOR_ACTIONS_BILLING_TEST__.render({ ...vm, footer: { text, color: "#e0a03a" } }),
    [healthy, long]
  );
  const warning = page.locator("#actionsBillingStale");
  await expect(warning).toBeVisible();
  await expect(warning).toHaveText(long);
  await expect(warning).toHaveAttribute("title", long);

  const headerAfter = await header.boundingBox();
  expect(headerAfter.height, "one header line, never two").toBeCloseTo(headerBefore.height, 1);
  const after = await panel.boundingBox();
  expect(after.height).toBeCloseTo(before.height, 1);
  expect(after.width).toBeCloseTo(before.width, 1);
});

// The daily hosted-minutes trend (#771). The series, its range, its caption and
// the sentence for the 1st of a month are Rust's; the page draws the series
// with the host sparkline painter and computes nothing.

const trend = (page) => page.locator('#actionsBillingBody [data-section="trend"]');

/** The y of each point of the trend's polyline (0 top, 100 bottom). Read inside
 *  `expect.poll`: the 10 s read may replace the node between two statements. */
const trendYs = async (page) => {
  const points = await trend(page).locator(".plot svg polyline").getAttribute("points", { timeout: 1000 }).catch(() => "");
  return points ? points.split(" ").map((pt) => Number(pt.split(",")[1])) : [];
};

test("the Full view draws one point per day, 1st through the newest, with Rust's label and caption (#771)", async ({ page, baseURL }) => {
  const p = await gotoPanel(page, baseURL);
  expect(p.trend.values.length).toBeGreaterThan(2);
  expect(p.trend.values, "the fixture holds a measured zero day").toContain(0);
  await expect(trend(page).locator(".lbl")).toHaveText(p.trend.label);
  await expect(trend(page).locator(".pv-note")).toHaveText(p.trend.caption);
  await expect(trend(page).locator(".plot svg")).toBeVisible();
  await expect.poll(async () => (await trendYs(page)).length).toBe(p.trend.values.length);
  // No sentence beside a line.
  await expect(page.locator('#actionsBillingBody [data-section="trend-note"]')).toHaveCount(0);
});

test("a day with no items is drawn at the floor of the range, and the peak at the top (#771)", async ({ page, baseURL }) => {
  const p = await gotoPanel(page, baseURL);
  const zero = p.trend.values.indexOf(0);
  const peak = p.trend.values.indexOf(p.trend.hi);
  expect(zero).toBeGreaterThanOrEqual(0);
  expect(peak).toBeGreaterThanOrEqual(0);
  await expect(trend(page).locator(".plot svg polyline")).toBeVisible();
  // The painter's y runs 0 (top) to 100 (bottom): lo is the bottom.
  await expect.poll(async () => (await trendYs(page))[zero]).toBe(100);
  await expect.poll(async () => (await trendYs(page))[peak]).toBe(0);
});

// What keeps the chart out of the overview is Rust, not this page: the overview
// tile is made from the dashboard source's rows, and `dashboard.rs`'s
// `the_daily_trend_is_in_no_overview_row` pins that none carries the trend. The
// overview never calls `actions_billing` (`panelWanted` is false there), so this
// proves only the two things the page does: the Full view asks with its tile,
// and the overview tile holds no `.plot`.
for (const [name, line] of [
  ["sample-actions-billing.json", true],
  ["sample-actions-billing-first-day.json", false],
]) {
  test(`the Full view asks for its tile and the overview tile has no .plot (${name}, #771)`, async ({ page, baseURL }) => {
    const cockpit = await fixture(baseURL, "sample-cockpit.json");
    const payload = await fixture(baseURL, name);
    const tileId = tileIdFor("actionsBilling");
    await stubTileIpc(page, baseURL, { source: "actionsBilling", cockpit, answers: { actions_billing: { __byTile: { [tileId]: payload } } } });
    await page.goto("/index.html");
    const tile = page.locator(`[data-tile="${tileId}"]`);
    await expect(tile).toBeVisible();
    await expect(tile.locator("svg, .plot")).toHaveCount(0);
    await expect(page.locator("#actionsBillingBody .plot")).toHaveCount(0);

    await openTileFull(page, "actionsBilling");
    expect(await lastArgs(page, "actions_billing")).toEqual({ tile: tileId });
    if (line) await expect(trend(page).locator(".plot svg polyline")).toBeVisible();
    else await expect(page.locator('#actionsBillingBody [data-section="trend-note"]')).toBeVisible();
    await expect(page.locator("#actionsBillingBody .plot")).toHaveCount(line ? 1 : 0);
    await expect(tile.locator("svg, .plot")).toHaveCount(0);
  });
}

test("an all-zero series draws a flat line at the floor, with no NaN (#771)", async ({ page, baseURL }) => {
  const p = await gotoPanel(page, baseURL);
  await page.evaluate(
    (vm) =>
      window.__SOLADOR_ACTIONS_BILLING_TEST__.render({
        ...vm,
        trend: { ...vm.trend, values: [0, 0, 0], lo: 0, hi: 0, caption: "Nov 1 \u2013 Nov 3 \u00b7 peak 0 min" },
      }),
    p
  );
  await expect(trend(page).locator(".pv-note")).toHaveText("Nov 1 \u2013 Nov 3 \u00b7 peak 0 min");
  await expect.poll(() => trendYs(page)).toEqual([100, 100, 100]);
  const points = await trend(page).locator(".plot svg polyline").getAttribute("points");
  expect(points).not.toContain("NaN");
});

test("on the 1st of a month the Full view says the trend starts on the 2nd and draws no line (#771)", async ({ page, baseURL }) => {
  const p = await gotoPanel(page, baseURL, "sample-actions-billing-first-day.json");
  expect(p.trend).toBeNull();
  const note = page.locator('#actionsBillingBody [data-section="trend-note"]');
  await expect(note).toHaveText(p.trendNote.text);
  await expect(note).toHaveCSS("color", rgb(p.trendNote.color));
  await expect(page.locator("#actionsBillingBody .plot")).toHaveCount(0);
  await expect(page.locator("#actionsBillingBody svg")).toHaveCount(0);
});

test("a failed read, a month with no usage and an unset panel draw no chart and no trend sentence (#771)", async ({ page, baseURL }) => {
  for (const name of ["error", "empty", "unconfigured"]) {
    const p = await gotoPanel(page, baseURL, `sample-actions-billing-${name}.json`);
    expect(p.trend, name).toBeNull();
    expect(p.trendNote, name).toBeNull();
    await expect(page.locator("#actionsBillingBody .pv-message"), name).toBeVisible();
    await expect(page.locator("#actionsBillingBody .plot"), name).toHaveCount(0);
    await expect(page.locator('#actionsBillingBody [data-section="trend-note"]'), name).toHaveCount(0);
  }
});

test("the renderer draws the series it is given and invents no day (#771)", async ({ page, baseURL }) => {
  const p = await gotoPanel(page, baseURL);
  // Three days, the middle one the peak: nothing here pads, sorts or fills.
  await page.evaluate(
    (vm) =>
      window.__SOLADOR_ACTIONS_BILLING_TEST__.render({
        ...vm,
        trend: { ...vm.trend, values: [0, 5, 0], lo: 0, hi: 5, caption: "any words Rust chose" },
      }),
    p
  );
  await expect(trend(page).locator(".pv-note")).toHaveText("any words Rust chose");
  await expect.poll(() => trendYs(page)).toEqual([100, 0, 100]);
});
