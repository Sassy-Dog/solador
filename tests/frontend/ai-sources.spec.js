import { test, expect } from "@playwright/test";
import { gotoTileFull, lastArgs, tileIdFor } from "./tile-full.js";

// The AI Usage panel, painted by `app/ui/ai-sources.js`. Same CSP guard as the
// other suites: the page is served under the app's real policy (csp_server.py),
// so a blocked style surfaces as a console error.
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

/** Enters the AI Usage tile's Full view with its command answered by `payload`. */
async function gotoPanel(page, baseURL, payload, cockpit) {
  await gotoTileFull(page, baseURL, {
    source: "aiUsage",
    cockpit: cockpit ?? (await fixture(baseURL, "sample-cockpit.json")),
    answers: { ai_usage: payload },
  });
}

async function gotoAiUsage(page, baseURL, name = "sample-ai-usage.json") {
  const payload = await fixture(baseURL, name);
  await gotoPanel(page, baseURL, payload);
  return payload;
}

const render = (page, vm) => page.evaluate((vm) => window.__SOLADOR_AI_TEST__.aiUsage.render(vm), vm);

/** `#33d17a` as the browser reports a computed colour. */
const rgb = (hex) => {
  const n = parseInt(hex.slice(1), 16);
  return `rgb(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255})`;
};

const windowRows = (page) => page.locator("#aiUsageBody .pv-row.window");
const planSection = (page, id) => page.locator(`.pv-section[data-plan="${id}"]`);
const apiSection = (page, id) => page.locator(`#aiUsageBody [data-section="api"][data-api="${id}"]`);

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
    // The same 5px track the Machines tile draws: the meter rules reach this
    // panel, not only the overview.
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

test("the AI Usage panel is headed by its tile's name and paints Rust's trailing label and window rows", async ({ page, baseURL }) => {
  const payload = { ...(await fixture(baseURL, "sample-ai-usage.json")), title: "Rust's own title" };
  await gotoPanel(page, baseURL, payload);
  await expect(page.locator("#aiUsagePanel")).toBeVisible();

  // Headed by the tile's own name (#640), not the payload's `title`. The
  // fixture's title is the tile's name too, so it is changed here.
  await expect(page.locator("#aiUsageTitle")).toHaveText("AI Usage");
  // Labelled by Rust: the tile covers several vendors, so the figure says it is
  // Claude Code's, and this file adds nothing to it.
  expect(payload.trailing).toMatch(/^Claude Code · /);
  await expect(page.locator("#aiUsageTrailing")).toHaveText(payload.trailing);

  await expect(windowRows(page)).toHaveCount(payload.windows.length);
  await expect(windowRows(page).locator(".lbl")).toHaveText(payload.windows.map((w) => w.label));
  await expect(windowRows(page).locator(".val")).toHaveText(payload.windows.map((w) => w.value));
  // The abbreviation is Rust's — a `toLocaleString` here would be a second
  // implementation of `ClaudeUsagePanel.tokens`.
  expect(payload.windows[1].value).toMatch(/^\d+(\.\d)?[kM]?$/);

  // The panel is confirmed to be the `ai_usage` command's output, not a
  // fixture the page happened to fetch.
  const calls = await page.evaluate(() => window.__CALLS__.map((c) => c.command));
  expect(calls).toContain("ai_usage");
  // And Usage's panel never came up for it.
  await expect(page.locator("#usagePanel")).toBeHidden();
});

test("the window rows carry no progress bar, because there is no limit to bar against", async ({ page, baseURL }) => {
  // A subscription publishes no ceiling. A bar against an invented one would be
  // a percentage of a number nobody set, so Rust sends no `bar` at all — and
  // this file must not manufacture one.
  await gotoAiUsage(page, baseURL);
  await expect(windowRows(page)).not.toHaveCount(0);
  await expect(page.locator("#aiUsageBody .pv-row.window .pbar")).toHaveCount(0);
});

test("the top-projects list is capped and every row keeps its own figure", async ({ page, baseURL }) => {
  const payload = await gotoAiUsage(page, baseURL);
  const projects = page.locator('#aiUsageBody .pv-section[data-section="projects"]');
  await expect(projects.locator(".lbl")).toHaveText(payload.projects.label);

  const rows = projects.locator(".pv-item");
  await expect(rows).toHaveCount(payload.projects.rows.length);
  expect(payload.projects.rows.length, "Rust caps the list at four").toBe(4);
  await expect(rows.locator(".name")).toHaveText(payload.projects.rows.map((r) => r.name));
  await expect(rows.locator(".val")).toHaveText(payload.projects.rows.map((r) => r.value));
});

test("the three plans lead, then each vendor's spend, then this machine's rollups", async ({ page, baseURL }) => {
  const payload = await gotoAiUsage(page, baseURL);
  expect(payload.plans.map((p) => p.id), "Claude's plan, Codex's, then Copilot's own limit").toEqual(["claude-plan", "codex-plan", "copilot-plan"]);
  expect(payload.apis.map((a) => a.id), "Anthropic's spend, then OpenAI's").toEqual(["anthropic-api", "openai-api"]);
  await expect(page.locator("#aiUsageBody .pv-message"), "and so no setup line").toHaveCount(0);
  expect(
    payload.plans.flatMap((plan) => plan.metrics).some((m) => m.pace !== null),
    "the fixture has a meter with a pace tick",
  ).toBe(true);
  expect(
    payload.plans.flatMap((plan) => plan.metrics).some((m) => m.fraction !== null && m.pace === null),
    "and one too young to project",
  ).toBe(true);

  const classes = (selector) =>
    page.locator(selector).evaluate((el) => [...el.children].map((c) => c.className));
  expect(await classes("#aiUsageBody")).toEqual(["ai-plans", "ai-rollups"]);
  // Plans (Copilot's one card among them), then spend, in that order, inside the one block.
  expect(payload.copilot, "the billing halves ride the Copilot card, not a list of their own").toBeUndefined();
  expect(
    payload.plans.find((p) => p.id === "copilot-plan").halves.map((h) => h.id),
    "the organization's half, then the person's",
  ).toEqual(["copilot-org", "copilot-personal"]);
  expect(
    await page
      .locator("#aiUsageBody .ai-plans")
      .evaluate((el) => [...el.children].map((c) => c.dataset.plan ?? c.dataset.api)),
  ).toEqual(["claude-plan", "codex-plan", "copilot-plan", "anthropic-api", "openai-api"]);
  expect(await classes("#aiUsageBody .ai-rollups")).toEqual([
    "lbl pv-heading",
    "pv-row window",
    "pv-row window",
    "pv-section",
  ]);
  await expect(page.locator("#aiUsageBody .pv-heading")).toHaveText(payload.machineHeading);

  for (const plan of payload.plans) await expectPlan(page, plan);

  // Nothing sits above the first plan, so its rule is not drawn.
  await expect(page.locator("#aiUsageBody .ai-plans .pv-section:first-child .pv-divider")).toBeHidden();
  await expect(page.locator("#aiUsageBody .ai-plans .pv-section:last-child .pv-divider"), "the API spend's is").toBeVisible();
});

/** One billing half, inside the Copilot card. */
const copilotSection = (page, id) =>
  page.locator(`#aiUsageBody .pv-section[data-plan="copilot-plan"] .pv-half[data-copilot="${id}"]`);

const copilotHalves = (payload) => payload.plans.find((p) => p.id === "copilot-plan").halves;

/** The payload with the Copilot card's halves replaced. */
const withHalves = (payload, halves) => ({
  ...payload,
  plans: payload.plans.map((p) => (p.id === "copilot-plan" ? { ...p, halves } : p)),
});

test("each Copilot half paints Rust's billed value, owner, note, and products with their models, and no meter", async ({
  page,
  baseURL,
}) => {
  const payload = await gotoAiUsage(page, baseURL);
  const halves = copilotHalves(payload);
  expect(halves).toHaveLength(2);
  for (const half of halves) {
    const section = copilotSection(page, half.id);
    await expect(section).toHaveCount(1);
    await expect(section.locator(".pv-row .lbl")).toHaveText(half.label);
    await expect(section.locator(".pv-row .val")).toHaveText(half.value);
    await expect(section.locator(".pv-row .val")).toHaveCSS("color", rgb(half.valueColor));
    // Whose bill it is has a line of its own, so no width can cut the dollars.
    await expect(section.locator(".pv-owner")).toHaveText(half.owner);
    await expect(section.locator(".pv-note:not(.pv-owner)")).toHaveText(half.note);
    expect(half.models, "the flat models list is gone").toBeUndefined();
    expect(half.products.length, "the products are named").toBeGreaterThan(0);
    // Each product line, then its model lines, in Rust's order.
    const lines = half.products.flatMap((p) => [
      { cls: "pv-product", name: p.name, value: p.value },
      ...p.models.map((m) => ({ cls: "pv-model", name: m.name, value: m.value })),
    ]);
    await expect(section.locator(".pv-item")).toHaveCount(lines.length);
    await expect(section.locator(".pv-item .name")).toHaveText(lines.map((l) => l.name));
    await expect(section.locator(".pv-item .val")).toHaveText(lines.map((l) => l.value));
    await expect(section.locator(".pv-item")).toHaveClass(lines.map((l) => new RegExp(`\\b${l.cls}\\b`)));
    // A model is visibly subordinate to its product: indented past it.
    const product = await section.locator(".pv-product .name").first().boundingBox();
    const model = await section.locator(".pv-model .name").first().boundingBox();
    expect(model.x, "a model line sits to the right of its product line").toBeGreaterThan(product.x);
    // The API reports no pool, so there is nothing for a meter to be a share of.
    await expect(section.locator(".db-meter, .db-meter-fill, .pbar")).toHaveCount(0);
  }
  // Rust's strings: GitHub's own term, then whose bill it is.
  expect(halves.map((h) => h.label)).toEqual(["Additional usage", "Additional usage"]);
  expect(halves[0].owner, "the organization's login").not.toMatch(/billed to/);
  expect(halves[1].owner, "the person's half says it is billed to the account").toMatch(/^billed to /);
  expect(halves[0].note, "the note drops the AI").toMatch(/^[\d.]+ credits? used/);
});

test("the Copilot card is one card: the seat's budget meter, then each half, in the plans", async ({
  page,
  baseURL,
}) => {
  const payload = await gotoAiUsage(page, baseURL);
  const card = planSection(page, "copilot-plan");
  await expect(card).toHaveCount(1);
  await expect(card.locator(".pv-row.plan .lbl")).toHaveText("Copilot · Business");
  // The meter first, then the halves, in the card's own order.
  const kinds = await card.evaluate((el) => [...el.children].map((c) => c.className.split(" ")[0]));
  expect(kinds).toEqual(["pv-divider", "pv-row", "db-host-stats", "pv-half", "pv-half"]);
  await expect(card.locator(".db-metric")).toHaveCount(1);
  await expect(card.locator(".pv-half")).toHaveCount(copilotHalves(payload).length);
  // Nothing else in the plans is a Copilot section.
  await expect(page.locator('#aiUsageBody [data-section="copilot"]')).toHaveCount(0);
});

test("a Copilot card with only a billing half has no meter and no figure of its own", async ({ page, baseURL }) => {
  const payload = await gotoAiUsage(page, baseURL);
  const card = payload.plans.find((p) => p.id === "copilot-plan");
  const only = {
    ...payload,
    plans: payload.plans.map((p) =>
      p.id === "copilot-plan"
        ? { ...p, label: "Copilot", value: "", metrics: [], halves: [card.halves[0]] }
        : p,
    ),
  };
  await render(page, only);
  const section = planSection(page, "copilot-plan");
  await expect(section.locator(".pv-row.plan .lbl")).toHaveText("Copilot");
  await expect(section.locator(".pv-row.plan .val")).toHaveText("");
  await expect(section.locator(".db-metric")).toHaveCount(0);
  await expect(section.locator(".pv-half")).toHaveCount(1);
});

test("a product with no models is one line, and an unnamed product is Rust's words", async ({ page, baseURL }) => {
  const payload = await gotoAiUsage(page, baseURL);
  const half = {
    ...copilotHalves(payload)[0],
    products: [
      { name: "Code Quality", value: "—", dotColor: "#4a9", models: [] },
      { name: "Product not named", value: "12", dotColor: "#4a9", models: [] },
    ],
  };
  await render(page, withHalves(payload, [half]));
  const section = copilotSection(page, "copilot-org");
  await expect(section.locator(".pv-item .name")).toHaveText(["Code Quality", "Product not named"]);
  await expect(section.locator(".pv-item .val")).toHaveText(["—", "12"]);
  await expect(section.locator(".pv-model")).toHaveCount(0);
});

test("the Copilot plan row is a plan: its label, one spend-cap meter and Rust's reset note", async ({
  page,
  baseURL,
}) => {
  const payload = await gotoAiUsage(page, baseURL);
  const plan = payload.plans.find((p) => p.id === "copilot-plan");
  expect(plan, "the dump carries an invented business seat").toBeTruthy();
  expect(plan.label).toBe("Copilot · Business");
  expect(plan.metrics).toHaveLength(1);
  const [meter] = plan.metrics;
  expect(meter.label).toBe("Budget");
  expect(meter.value, "dollars used of the limit, with the share").toMatch(/^\$[\d,.]+ \/ \$[\d,.]+ · \d+%$/);
  expect(meter.note, "the reset, not the no-reset sentence").toMatch(/^resets in /);
  expect(meter.fraction).toBeGreaterThan(0);
  // `expectPlan` paints and checks the header, the meter, its fill colour and its note.
  await expectPlan(page, plan);
  // The seat's meter has no product list of its own: the lines under the card are its halves'.
  await expect(planSection(page, "copilot-plan").locator(".db-metric .pv-item")).toHaveCount(0);
});

test("an unlimited Copilot seat is a spend with no fill, and a failed one is a dash", async ({ page, baseURL }) => {
  const payload = await gotoAiUsage(page, baseURL);
  const unlimited = {
    ...payload,
    plans: payload.plans.map((p) =>
      p.id === "copilot-plan"
        ? {
            ...p,
            metrics: [{ ...p.metrics[0], value: "$50.00 spent", fraction: null, color: "#8b8b8b", note: "resets in 3d 4h" }],
          }
        : p,
    ),
  };
  await render(page, unlimited);
  await expectPlan(page, unlimited.plans.find((p) => p.id === "copilot-plan"));
  await expect(planSection(page, "copilot-plan").locator(".db-meter-fill")).toHaveCount(0);

  const dash = {
    ...payload,
    plans: payload.plans.map((p) =>
      p.id === "copilot-plan" ? { ...p, label: "Copilot", value: "—", metrics: [] } : p,
    ),
  };
  await render(page, dash);
  const header = planSection(page, "copilot-plan").locator(".pv-row.plan");
  await expect(header.locator(".val")).toHaveText("—");
  await expect(planSection(page, "copilot-plan").locator(".db-metric")).toHaveCount(0);
});

test("an unmeasured Copilot figure is the em dash, with no note and no products, never a zero", async ({
  page,
  baseURL,
}) => {
  const payload = await gotoAiUsage(page, baseURL);
  const unmeasured = withHalves(payload, [
    { ...copilotHalves(payload)[0], value: "\u2014", note: null, products: [] },
  ]);
  await render(page, unmeasured);
  const section = copilotSection(page, "copilot-org");
  await expect(section.locator(".pv-row .val")).toHaveText("\u2014");
  await expect(section.locator(".pv-note:not(.pv-owner)")).toHaveCount(0);
  await expect(section.locator(".pv-item")).toHaveCount(0);
  await expect(copilotSection(page, "copilot-personal")).toHaveCount(0);
});

test("a window whose reset has passed is an empty track: no fill, no tick, no zero", async ({ page, baseURL }) => {
  // `fraction: null` is Rust's "nothing honest to draw": the reading was true
  // of a window that has since rolled over. A track drawn at zero would read as
  // an unused plan.
  const healthy = await gotoAiUsage(page, baseURL);
  const muted = {
    ...healthy,
    plans: healthy.plans.map((plan) => ({
      ...plan,
      metrics: plan.metrics.map((m) => ({ ...m, fraction: null, pace: null, note: "reset since read" })),
    })),
  };
  await render(page, muted);
  for (const plan of muted.plans) await expectPlan(page, plan);
  await expect(page.locator("#aiUsageBody .ai-plans .db-meter-fill")).toHaveCount(0);
});

test("an Enterprise budget is one full-width meter", async ({ page, baseURL }) => {
  const payload = await gotoAiUsage(page, baseURL, "sample-ai-usage-plans.json");
  const [claude] = payload.plans;
  expect(claude.metrics.map((m) => m.label), "a seat with no windows").toEqual(["Budget"]);
  await expectPlan(page, claude);
});

test("a plan that failed first is a dash with no meters, and the header says why", async ({ page, baseURL }) => {
  const payload = await gotoAiUsage(page, baseURL, "sample-ai-usage-plans.json");
  const codex = payload.plans.find((plan) => plan.id === "codex-plan");
  expect(codex.metrics, "a failure before any reading").toEqual([]);
  await expectPlan(page, codex);
  // The reason is the header's, in Rust's words and under its row's id; the
  // body says only "—".
  expect(payload.footer.text).toContain("codex-plan:");
  await expect(page.locator("#aiUsageStale")).toHaveText(payload.footer.text);
});

test("a stale plan is dated in the header, and costs the card no height", async ({ page, baseURL }) => {
  const healthy = await gotoAiUsage(page, baseURL);
  expect(healthy.freshness, "a current panel dates nothing").toBeNull();
  await expect(page.locator("#aiUsageFreshness")).toBeHidden();
  await expect(page.locator("#aiUsageStale")).toBeHidden();
  await expect(page.locator("#aiUsagePanel")).toBeVisible();
  const before = await page.locator("#aiUsagePanel").boundingBox();

  const stale = await fixture(baseURL, "sample-ai-usage-stale.json");
  expect(stale.freshness.text, "each clock names its section").toContain("claude-plan:");
  expect(stale.freshness.text).toContain("openai-api:");
  await render(page, stale);
  const clock = page.locator("#aiUsageFreshness");
  await expect(clock).toHaveText(stale.freshness.text);
  await expect(clock).toHaveAttribute("title", stale.freshness.text);
  await expect(clock).toHaveCSS("color", rgb(stale.freshness.color));
  await expect(page.locator("#aiUsageBody .panel-asof")).toHaveCount(0);

  const after = await page.locator("#aiUsagePanel").boundingBox();
  expect(after.height).toBeCloseTo(before.height, 1);
});

test("with nothing set up, AI Usage has the one sentence and no block of its own", async ({ page, baseURL }) => {
  const payload = await gotoAiUsage(page, baseURL, "sample-ai-usage-empty.json");
  expect(payload.plans).toEqual([]);
  expect(payload.apis).toEqual([]);
  expect(payload.machineHeading).toBeNull();

  const message = page.locator("#aiUsageBody > .pv-message");
  await expect(message).toHaveText(payload.message.text);
  await expect(message).toHaveCSS("color", rgb(payload.message.color));
  await expect(page.locator("#aiUsageBody .pv-message")).toHaveCount(1);
  await expect(page.locator("#aiUsageBody .ai-plans")).toHaveCount(0);
  await expect(page.locator("#aiUsageBody .pv-heading")).toHaveCount(0);
  await expect(page.locator("#aiUsageBody .pv-section[data-plan]")).toHaveCount(0);
  await expect(page.locator("#aiUsageTrailing")).toHaveText("");
  await expect(page.locator("#aiUsageStale"), "nothing was read, so nothing went wrong").toBeHidden();
});

test("a part that is not set up says nothing: no plan, no spend, no line about it", async ({ page, baseURL }) => {
  const healthy = await gotoAiUsage(page, baseURL);
  await render(page, { ...healthy, plans: [], apis: [] });
  await expect(page.locator("#aiUsageBody .pv-section[data-plan]")).toHaveCount(0);
  await expect(page.locator('#aiUsageBody [data-section="api"]')).toHaveCount(0);
  await expect(page.locator("#aiUsageBody .pv-message")).toHaveCount(0);
  await expect(windowRows(page)).toHaveCount(healthy.windows.length);
});

test("local usage off leaves the Claude Code line out altogether", async ({ page, baseURL }) => {
  const healthy = await gotoAiUsage(page, baseURL);
  await render(page, { ...healthy, windows: [], machineHeading: null, projects: null, trailing: "", claudeToday: "" });
  await expect(page.locator("#aiUsageBody .ai-rollups")).toHaveCount(0);
  await expect(page.locator("#aiUsageBody")).not.toContainText("Claude Code");
  await expect(page.locator("#aiUsageTrailing")).toHaveText("");
});

test("each vendor's API spend is its own block, named for the vendor, and says how far the figure reaches", async ({
  page,
  baseURL,
}) => {
  const payload = await gotoAiUsage(page, baseURL);
  expect(payload.apis, "the measured fixture has both Admin keys").toHaveLength(2);
  for (const api of payload.apis) {
    const block = apiSection(page, api.id);
    await expect(block.locator(".pv-row .lbl")).toHaveText(api.label);
    await expect(block.locator(".pv-row .val")).toHaveText(api.value);
    if (api.note) await expect(block.locator(".pv-note")).toHaveText(api.note);
  }
  expect(
    payload.apis.map((a) => a.label),
    "the vendor is in the label",
  ).toEqual(["Anthropic API spend (MTD)", "OpenAI API spend (MTD)"]);

  const [anthropic, openai] = payload.apis;
  await render(page, { ...payload, apis: [{ ...anthropic, note: null }, openai] });
  await expect(apiSection(page, anthropic.id).locator(".pv-note"), "a figure with no date claims none").toHaveCount(0);

  // One vendor's key removed hides only that vendor's spend.
  await render(page, { ...payload, apis: [openai] });
  await expect(apiSection(page, anthropic.id)).toHaveCount(0);
  await expect(apiSection(page, openai.id)).toHaveCount(1);
  await render(page, { ...payload, apis: [] });
  await expect(page.locator('#aiUsageBody [data-section="api"]')).toHaveCount(0);
});

async function gotoWithColumns(page, baseURL, columns, mutate = () => {}) {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  cockpit.panelColumns.aiUsage = columns;
  const payload = await fixture(baseURL, "sample-ai-usage.json");
  mutate(payload);
  await gotoPanel(page, baseURL, payload, cockpit);
  return payload;
}

test("on a full-width card the plans sit beside the rollups", async ({ page, baseURL }) => {
  await gotoWithColumns(page, baseURL, 2);
  await expect(page.locator("#aiUsagePanel")).toHaveAttribute("data-cols", "2");
  await expect(page.locator("#aiUsageBody .ai-plans")).toBeVisible();
  await expect(page.locator("#aiUsageBody .ai-rollups")).toBeVisible();
  const plans = await page.locator("#aiUsageBody .ai-plans").boundingBox();
  const rollups = await page.locator("#aiUsageBody .ai-rollups").boundingBox();
  expect(rollups.x, "the rollups start to the right of the plans").toBeGreaterThan(plans.x + plans.width - 1);
  expect(Math.abs(rollups.y - plans.y), "and on the same line, not below").toBeLessThan(2);
  // Side by side the gutter separates them, so the heading needs no rule.
  await expect(page.locator("#aiUsageBody .pv-heading")).toHaveCSS("border-top-width", "0px");
  // The projects rule still has the windows to separate itself from.
  await expect(page.locator('#aiUsageBody .pv-section[data-section="projects"] .pv-divider')).toBeVisible();
});

test("a narrow card stacks the rollups under the plans, with a rule between them", async ({ page, baseURL }) => {
  await gotoWithColumns(page, baseURL, 1);
  await expect(page.locator("#aiUsageBody .ai-plans")).toBeVisible();
  await expect(page.locator("#aiUsageBody .ai-rollups")).toBeVisible();
  const plans = await page.locator("#aiUsageBody .ai-plans").boundingBox();
  const rollups = await page.locator("#aiUsageBody .ai-rollups").boundingBox();
  expect(rollups.y).toBeGreaterThan(plans.y + plans.height - 1);
  await expect(page.locator("#aiUsageBody .pv-heading")).toHaveCSS("border-top-width", "1px");
});

for (const { name, block, mutate } of [
  {
    name: "the rollups",
    block: ".ai-rollups",
    mutate: (payload) => {
      payload.plans = [];
      payload.apis = [];
    },
  },
  {
    name: "the plans and spend",
    block: ".ai-plans",
    mutate: (payload) => {
      payload.windows = [];
      payload.machineHeading = null;
      payload.projects = null;
    },
  },
]) {
  test(`one block on a two-column card takes both tracks: ${name}`, async ({ page, baseURL }) => {
    await gotoWithColumns(page, baseURL, 2, mutate);
    const bodyWidth = await page.locator("#aiUsageBody").evaluate((el) => el.clientWidth);
    await expect(page.locator(`#aiUsageBody ${block}`)).toBeVisible();
    const box = await page.locator(`#aiUsageBody ${block}`).boundingBox();
    expect(Math.abs(box.width - bodyWidth), `${name} span both tracks`).toBeLessThan(1);
  });
}

// The renderer asks for its own tile's reading (#639): `{}` would be the
// source-wide payload, wrong in a tile's Full view whenever its scope narrows.
test("the panel asks for its tile's reading, not the source's", async ({ page, baseURL }) => {
  await gotoAiUsage(page, baseURL);
  expect(await lastArgs(page, "ai_usage")).toEqual({ tile: tileIdFor("aiUsage") });
});

// A paint-only check, on a payload built by hand: given Codex's rows alone and
// no Claude header figure, the renderer paints nothing of Claude's around them
// (no rollups heading or windows, no Claude Code figure). What Rust sends a
// scoped tile is not tested here; it is tested in `tile_scope.rs`.
test("a Codex-only scoped payload paints no Claude heading, window rows or Claude Code figure", async ({ page, baseURL }) => {
  const full = await gotoAiUsage(page, baseURL);
  const codex = full.plans.find((plan) => plan.id === "codex-plan");
  const openai = full.apis.find((api) => api.id === "openai-api");
  await render(page, {
    ...full,
    plans: [codex],
    apis: [openai],
    windows: [],
    machineHeading: null,
    projects: null,
    trailing: "",
    claudeToday: "",
    footer: null,
    freshness: null,
  });
  await expect(page.locator("#aiUsageBody .pv-heading")).toHaveCount(0);
  await expect(windowRows(page)).toHaveCount(0);
  await expect(page.locator("#aiUsageBody .ai-rollups")).toHaveCount(0);
  await expect(page.locator("#aiUsageTrailing")).toHaveText("");
  await expect(page.locator("#aiUsageStale")).toBeHidden();
  await expect(page.locator("#aiUsagePanel")).not.toContainText("Claude");
  await expectPlan(page, codex);
});
