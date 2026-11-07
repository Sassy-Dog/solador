import { test, expect } from "@playwright/test";
import { gotoTileFull, lastArgs, tileIdFor } from "./tile-full.js";

// The two AI vendors' panels, painted by `app/ui/ai-sources.js`. Same CSP guard
// as the other suites: the page is served under the app's real policy
// (csp_server.py), so a blocked style surfaces as a console error.
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

/** Enters a source's tile's Full view with its command answered by `payload`. */
async function gotoPanel(page, baseURL, source, payload, cockpit) {
  await gotoTileFull(page, baseURL, {
    source,
    cockpit: cockpit ?? (await fixture(baseURL, "sample-cockpit.json")),
    answers: { [source]: payload },
  });
}

async function gotoAnthropic(page, baseURL, name = "sample-anthropic.json") {
  const anthropic = await fixture(baseURL, name);
  await gotoPanel(page, baseURL, "anthropic", anthropic);
  return anthropic;
}

async function gotoOpenAi(page, baseURL, name = "sample-openai.json") {
  const openai = await fixture(baseURL, name);
  await gotoPanel(page, baseURL, "openai", openai);
  return openai;
}

const render = (page, panel, vm) =>
  page.evaluate(([panel, vm]) => window.__SOLADOR_AI_TEST__[panel].render(vm), [panel, vm]);

/** `#33d17a` as the browser reports a computed colour. */
const rgb = (hex) => {
  const n = parseInt(hex.slice(1), 16);
  return `rgb(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255})`;
};

const windowRows = (page) => page.locator("#anthropicBody .pv-row.window");
const planSection = (page, id) => page.locator(`.pv-section[data-plan="${id}"]`);

/**
 * Asserts one plan section paints `plan` as Rust sent it: the header row, then
 * the Machines tile's meter for each metric, with its note under it.
 */
async function expectPlan(page, plan) {
  const section = planSection(page, plan.id);
  await expect(section).not.toHaveAttribute("data-provider");
  const header = section.locator(".pv-row.plan");
  await expect(header).toHaveCount(1);
  await expect(header.locator(".lbl")).toHaveText(plan.label);
  await expect(header.locator(".val")).toHaveText(plan.value);
  await expect(header.locator(".val")).toHaveCSS("color", rgb(plan.valueColor));

  const metrics = section.locator(".db-metric");
  await expect(metrics).toHaveCount(plan.metrics.length);
  for (const [i, m] of plan.metrics.entries()) {
    const metric = metrics.nth(i);
    await expect(metric).toHaveClass(m.span === "full" ? /db-metric-full/ : /^db-metric$/);
    await expect(metric.locator(".db-stat-label > span")).toHaveText(m.label);
    await expect(metric.locator(".db-stat-label strong")).toHaveText(m.value);
    await expect(metric.locator(".db-meter-note")).toHaveText(m.note);
    const track = metric.locator(".db-meter");
    await expect(track).toHaveAttribute("aria-label", `${plan.label} ${m.label}`);
    await expect(track).toHaveAttribute("aria-description", m.note);
    // The same 5px track the Machines tile draws: the meter rules reach these
    // panels, not only the overview.
    await expect(track).toHaveCSS("height", "5px");
    const fill = track.locator(".db-meter-fill");
    const tick = track.locator(".db-meter-pace");
    if (m.fraction === null) {
      await expect(track).toHaveAttribute("aria-hidden", "true");
      await expect(track).not.toHaveAttribute("role");
      await expect(fill).toHaveCount(0);
      await expect(tick).toHaveCount(0);
      continue;
    }
    await expect(track).toHaveAttribute("role", "meter");
    await expect(track).toHaveAttribute("aria-valuetext", m.value);
    await expect(fill).toHaveCSS("background-color", rgb(m.color));
    // The browser normalises `34.0%` to `34%`, so compare the number.
    const width = await fill.evaluate((el) => parseFloat(el.style.width));
    expect(width).toBeCloseTo(m.fraction * 100, 1);
    if (m.pace === null) {
      await expect(tick).toHaveCount(0);
    } else {
      await expect(tick).toHaveCount(1);
      await expect(tick).toHaveAttribute("aria-hidden", "true");
      const left = await tick.evaluate((el) => parseFloat(el.style.left));
      expect(left).toBeCloseTo(m.pace * 100, 1);
    }
  }
}

test("the Anthropic panel is headed by its tile's name and paints Rust's trailing label and window rows", async ({ page, baseURL }) => {
  const anthropic = { ...(await fixture(baseURL, "sample-anthropic.json")), title: "Rust's own title" };
  await gotoPanel(page, baseURL, "anthropic", anthropic);
  await expect(page.locator("#anthropicPanel")).toBeVisible();

  // Headed by the tile's own name (#640), not the payload's `title`. The
  // fixture's title is the tile's name too, so it is changed here.
  await expect(page.locator("#anthropicTitle")).toHaveText("Anthropic");
  await expect(page.locator("#anthropicTrailing")).toHaveText(anthropic.trailing);

  await expect(windowRows(page)).toHaveCount(anthropic.windows.length);
  await expect(windowRows(page).locator(".lbl")).toHaveText(anthropic.windows.map((w) => w.label));
  await expect(windowRows(page).locator(".val")).toHaveText(anthropic.windows.map((w) => w.value));
  // The abbreviation is Rust's — a `toLocaleString` here would be a second
  // implementation of `ClaudeUsagePanel.tokens`.
  expect(anthropic.windows[1].value).toMatch(/^\d+(\.\d)?[kM]?$/);

  // The panel is confirmed to be the `anthropic` command's output, not a
  // fixture the page happened to fetch.
  const calls = await page.evaluate(() => window.__CALLS__.map((c) => c.command));
  expect(calls).toContain("anthropic");
  // And Usage's panel never came up for it.
  await expect(page.locator("#usagePanel")).toBeHidden();
});

test("the window rows carry no progress bar, because there is no limit to bar against", async ({ page, baseURL }) => {
  // A subscription publishes no ceiling. A bar against an invented one would be
  // a percentage of a number nobody set, so Rust sends no `bar` at all — and
  // this file must not manufacture one.
  await gotoAnthropic(page, baseURL);
  await expect(windowRows(page)).not.toHaveCount(0);
  await expect(page.locator("#anthropicBody .pv-row.window .pbar")).toHaveCount(0);
});

test("the top-projects list is capped and every row keeps its own figure", async ({ page, baseURL }) => {
  const anthropic = await gotoAnthropic(page, baseURL);
  const projects = page.locator('#anthropicBody .pv-section[data-section="projects"]');
  await expect(projects.locator(".lbl")).toHaveText(anthropic.projects.label);

  const rows = projects.locator(".pv-item");
  await expect(rows).toHaveCount(anthropic.projects.rows.length);
  expect(anthropic.projects.rows.length, "Rust caps the list at four").toBe(4);
  await expect(rows.locator(".name")).toHaveText(anthropic.projects.rows.map((r) => r.name));
  await expect(rows.locator(".val")).toHaveText(anthropic.projects.rows.map((r) => r.value));
});

test("the plan leads the Anthropic panel, then this machine's rollups", async ({ page, baseURL }) => {
  const anthropic = await gotoAnthropic(page, baseURL);
  expect(anthropic.plans.length, "the measured fixture has Claude's plan on").toBe(1);
  await expect(page.locator("#anthropicBody .pv-message"), "and so no setup line").toHaveCount(0);
  expect(
    anthropic.plans.flatMap((plan) => plan.metrics).some((m) => m.pace !== null),
    "the fixture has a meter with a pace tick",
  ).toBe(true);

  const classes = (selector) =>
    page.locator(selector).evaluate((el) => [...el.children].map((c) => c.className));
  expect(await classes("#anthropicBody")).toEqual(["ai-plans", "ai-rollups"]);
  expect(await classes("#anthropicBody .ai-rollups")).toEqual([
    "lbl pv-heading",
    "pv-row window",
    "pv-row window",
    "pv-section",
  ]);
  await expect(page.locator("#anthropicBody .pv-heading")).toHaveText(anthropic.machineHeading);

  for (const plan of anthropic.plans) await expectPlan(page, plan);

  // Nothing sits above the plan, so its rule is not drawn.
  await expect(page.locator("#anthropicBody .ai-plans .pv-section:first-child .pv-divider")).toBeHidden();
  await expect(page.locator("#anthropicBody .ai-plans .pv-section:last-child .pv-divider"), "the API spend's is").toBeVisible();
});

test("the OpenAI panel paints Codex's plan and nothing of Claude's", async ({ page, baseURL }) => {
  const openai = await gotoOpenAi(page, baseURL);
  expect(openai.plans.length, "the measured fixture has Codex's plan on").toBe(1);
  expect(
    openai.plans.flatMap((plan) => plan.metrics).some((m) => m.fraction !== null && m.pace === null),
    "and it is too young to project",
  ).toBe(true);
  await expect(page.locator("#openaiTitle")).toHaveText("OpenAI");
  await expect(page.locator("#openaiTrailing")).toHaveText("");
  for (const plan of openai.plans) await expectPlan(page, plan);
  await expect(page.locator("#openaiBody .pv-heading")).toHaveCount(0);
  await expect(page.locator("#openaiBody .pv-row.window")).toHaveCount(0);
  await expect(page.locator("#anthropicPanel")).toBeHidden();

  const calls = await page.evaluate(() => window.__CALLS__.map((c) => c.command));
  expect(calls).toContain("openai");
});

test("a window whose reset has passed is an empty track: no fill, no tick, no zero", async ({ page, baseURL }) => {
  // `fraction: null` is Rust's "nothing honest to draw": the reading was true
  // of a window that has since rolled over. A track drawn at zero would read as
  // an unused plan.
  const healthy = await gotoAnthropic(page, baseURL);
  const muted = {
    ...healthy,
    plans: healthy.plans.map((plan) => ({
      ...plan,
      metrics: plan.metrics.map((m) => ({ ...m, fraction: null, pace: null, note: "reset since read" })),
    })),
  };
  await render(page, "anthropic", muted);
  for (const plan of muted.plans) await expectPlan(page, plan);
  await expect(page.locator("#anthropicBody .ai-plans .db-meter-fill")).toHaveCount(0);
});

test("an Enterprise budget is one full-width meter", async ({ page, baseURL }) => {
  const anthropic = await gotoAnthropic(page, baseURL, "sample-anthropic-plans.json");
  const [claude] = anthropic.plans;
  expect(claude.metrics.map((m) => m.label), "a seat with no windows").toEqual(["Budget"]);
  await expectPlan(page, claude);
});

test("a plan that failed first is a dash with no meters, and the header says why", async ({ page, baseURL }) => {
  const openai = await gotoOpenAi(page, baseURL, "sample-openai-plans.json");
  const [codex] = openai.plans;
  expect(codex.metrics, "a failure before any reading").toEqual([]);
  await expectPlan(page, codex);
  // The reason is the header's, in Rust's words; the body says only "—".
  await expect(page.locator("#openaiStale")).toHaveText(openai.footer.text);
});

test("a stale plan is dated in the header, and costs the card no height", async ({ page, baseURL }) => {
  const healthy = await gotoAnthropic(page, baseURL);
  expect(healthy.freshness, "a current panel dates nothing").toBeNull();
  await expect(page.locator("#anthropicFreshness")).toBeHidden();
  await expect(page.locator("#anthropicStale")).toBeHidden();
  const before = await page.locator("#anthropicPanel").boundingBox();

  const stale = await fixture(baseURL, "sample-anthropic-stale.json");
  expect(stale.freshness.text, "the plan's clock names its section").toContain("claude-plan:");
  await render(page, "anthropic", stale);
  const clock = page.locator("#anthropicFreshness");
  await expect(clock).toHaveText(stale.freshness.text);
  await expect(clock).toHaveAttribute("title", stale.freshness.text);
  await expect(clock).toHaveCSS("color", rgb(stale.freshness.color));
  await expect(page.locator("#anthropicBody .panel-asof")).toHaveCount(0);

  const after = await page.locator("#anthropicPanel").boundingBox();
  expect(after.height).toBeCloseTo(before.height, 1);
});

test("with nothing set up, Anthropic has the one sentence and no block of its own", async ({ page, baseURL }) => {
  const anthropic = await gotoAnthropic(page, baseURL, "sample-anthropic-empty.json");
  expect(anthropic.plans).toEqual([]);
  expect(anthropic.machineHeading).toBeNull();

  const rollups = page.locator("#anthropicBody > .pv-message");
  await expect(rollups).toHaveText(anthropic.message.text);
  await expect(rollups).toHaveCSS("color", rgb(anthropic.message.color));
  await expect(page.locator("#anthropicBody .pv-message")).toHaveCount(1);
  await expect(page.locator("#anthropicBody .ai-plans")).toHaveCount(0);
  await expect(page.locator("#anthropicBody .pv-heading")).toHaveCount(0);
  await expect(page.locator("#anthropicBody .pv-section[data-plan]")).toHaveCount(0);
  await expect(page.locator("#anthropicTrailing")).toHaveText("");
  await expect(page.locator("#anthropicStale"), "nothing was read, so nothing went wrong").toBeHidden();
});

test("a part that is not set up says nothing: no plan, no line about it", async ({ page, baseURL }) => {
  const healthy = await gotoAnthropic(page, baseURL);
  await render(page, "anthropic", { ...healthy, plans: [] });
  await expect(page.locator("#anthropicBody .pv-section[data-plan]")).toHaveCount(0);
  await expect(page.locator("#anthropicBody .pv-message")).toHaveCount(0);
  await expect(windowRows(page)).toHaveCount(healthy.windows.length);
});

test("local usage off leaves the Claude Code line out altogether", async ({ page, baseURL }) => {
  const healthy = await gotoAnthropic(page, baseURL);
  await render(page, "anthropic", { ...healthy, windows: [], machineHeading: null, projects: null });
  await expect(page.locator("#anthropicBody .ai-rollups")).toHaveCount(0);
  await expect(page.locator("#anthropicBody")).not.toContainText("Claude Code");
});

test("each AI panel shows its API spend beside what the plan says, and how far the figure reaches", async ({
  page,
  baseURL,
}) => {
  const anthropic = await gotoAnthropic(page, baseURL);
  expect(anthropic.api, "the measured fixture has an Admin key").not.toBeNull();
  const block = page.locator('#anthropicBody [data-section="api"]');
  await expect(block.locator(".pv-row .lbl")).toHaveText(anthropic.api.label);
  await expect(block.locator(".pv-row .val")).toHaveText(anthropic.api.value);
  await expect(block.locator(".pv-note")).toHaveText(anthropic.api.note);

  await render(page, "anthropic", { ...anthropic, api: { ...anthropic.api, note: null } });
  await expect(page.locator('#anthropicBody [data-section="api"] .pv-note'), "a figure with no date claims none").toHaveCount(0);

  await render(page, "anthropic", { ...anthropic, api: null });
  await expect(page.locator('#anthropicBody [data-section="api"]')).toHaveCount(0);
});

test("with its plan off, OpenAI says how to turn it on", async ({ page, baseURL }) => {
  const openai = await gotoOpenAi(page, baseURL, "sample-openai-empty.json");
  expect(openai.plans).toEqual([]);
  await expect(page.locator("#openaiBody > .pv-message")).toHaveText(openai.message.text);
  await expect(page.locator("#openaiBody .pv-section")).toHaveCount(0);
  await expect(page.locator("#openaiStale")).toBeHidden();
});

async function gotoWithColumns(page, baseURL, source, columns, mutate = () => {}) {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  cockpit.panelColumns[source] = columns;
  const payload = await fixture(baseURL, `sample-${source}.json`);
  mutate(payload);
  await gotoPanel(page, baseURL, source, payload, cockpit);
  return payload;
}

test("on a full-width card the plan sits beside the rollups", async ({ page, baseURL }) => {
  await gotoWithColumns(page, baseURL, "anthropic", 2);
  await expect(page.locator("#anthropicPanel")).toHaveAttribute("data-cols", "2");
  const plans = await page.locator("#anthropicBody .ai-plans").boundingBox();
  const rollups = await page.locator("#anthropicBody .ai-rollups").boundingBox();
  expect(rollups.x, "the rollups start to the right of the plan").toBeGreaterThan(plans.x + plans.width - 1);
  expect(Math.abs(rollups.y - plans.y), "and on the same line, not below").toBeLessThan(2);
  // Side by side the gutter separates them, so the heading needs no rule.
  await expect(page.locator("#anthropicBody .pv-heading")).toHaveCSS("border-top-width", "0px");
  // The projects rule still has the windows to separate itself from.
  await expect(page.locator('#anthropicBody .pv-section[data-section="projects"] .pv-divider')).toBeVisible();
});

test("a narrow card stacks the rollups under the plan, with a rule between them", async ({ page, baseURL }) => {
  await gotoWithColumns(page, baseURL, "anthropic", 1);
  const plans = await page.locator("#anthropicBody .ai-plans").boundingBox();
  const rollups = await page.locator("#anthropicBody .ai-rollups").boundingBox();
  expect(rollups.y).toBeGreaterThan(plans.y + plans.height - 1);
  await expect(page.locator("#anthropicBody .pv-heading")).toHaveCSS("border-top-width", "1px");
});

for (const { name, source, block, mutate } of [
  {
    name: "the rollups",
    source: "anthropic",
    block: ".ai-rollups",
    mutate: (payload) => {
      payload.plans = [];
      payload.api = null;
    },
  },
  { name: "Codex's plan", source: "openai", block: ".ai-plans", mutate: () => {} },
]) {
  test(`one block on a two-column card takes both tracks: ${name}`, async ({ page, baseURL }) => {
    await gotoWithColumns(page, baseURL, source, 2, mutate);
    const body = `#${source}Body`;
    const bodyWidth = await page.locator(body).evaluate((el) => el.clientWidth);
    const box = await page.locator(`${body} ${block}`).boundingBox();
    expect(Math.abs(box.width - bodyWidth), `${name} span both tracks`).toBeLessThan(1);
  });
}

// The renderer asks for its own tile's reading (#639): `{}` would be the
// source-wide payload, wrong in a tile's Full view whenever its scope narrows.
test("each panel asks for its tile's reading, not the source's", async ({ page, baseURL }) => {
  await gotoAnthropic(page, baseURL);
  expect(await lastArgs(page, "anthropic")).toEqual({ tile: tileIdFor("anthropic") });
  await gotoOpenAi(page, baseURL);
  expect(await lastArgs(page, "openai")).toEqual({ tile: tileIdFor("openai") });
});
