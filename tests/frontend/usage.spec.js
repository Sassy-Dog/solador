import { test, expect } from "@playwright/test";

// Same CSP guard as the other suites: the page is served under the app's real
// policy (csp_server.py), so a blocked style surfaces as a console error rather
// than a thrown exception. This panel sets every figure's colour through CSSOM
// for exactly that reason — an inline `style=""` would be dropped under
// `style-src 'self'` and a muted em dash would render indistinguishable from an
// ink-coloured number.
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
 * Stubs the whole IPC surface and records every call, so a test can assert
 * *which* command painted the panel.
 *
 * `cockpit` has to be answered too: app.js replaces the entire document body
 * with an error line when its first `invoke` rejects, which would take this
 * panel down with it and fail the suite for the wrong reason.
 */
async function stubIpc(page, { cockpit, usage }) {
  await page.addInitScript(
    ({ cockpit, usage }) => {
      window.__CALLS__ = [];
      window.__TAURI__ = {
        core: {
          invoke: async (command, args) => {
            window.__CALLS__.push({ command, args });
            if (command === "cockpit") return cockpit;
            if (command === "usage") return usage;
            return null;
          },
        },
      };
    },
    { cockpit, usage }
  );
}

async function gotoWithUsage(page, baseURL, name = "sample-usage.json") {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  const usage = await fixture(baseURL, name);
  await stubIpc(page, { cockpit, usage });
  await page.goto("/index.html");
  return usage;
}

/** `#33d17a` as the browser reports a computed colour. */
const rgb = (hex) => {
  const n = parseInt(hex.slice(1), 16);
  return `rgb(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255})`;
};

const windowRows = (page) => page.locator("#usageBody .pv-row.window");
const section = (page, id) => page.locator(`#usageBody .pv-section[data-provider="${id}"]`);

/**
 * A warning long enough that no header could ever hold it, and deliberately not
 * a realistic sentence.
 *
 * Containment is a property of the layout, not of today's wording:
 * [#352](https://github.com/Sassy-Dog/solador/issues/352) is shortening these
 * messages to stock sentences, which makes the header comfortable — and a test
 * that passed for *that* reason would stop covering the thing it was written
 * for the moment a vendor returned a paragraph again.
 */
const LONG_WARNING =
  "⚠ neon: the vendor answered with a paragraph rather than a sentence, and " +
  "this is what a paragraph looks like on a card that is authored one quarter " +
  "of a cockpit row wide — which is exactly why the containment asserted here " +
  "cannot be left to depend on how long the message happens to be today " +
  "· last ok 6h ago";

/** How wide an element got, and whether its text had to be cut to fit. */
const fit = (page, selector) =>
  page.locator(selector).evaluate((el) => ({
    right: el.getBoundingClientRect().right,
    // `scrollWidth > clientWidth` is what `text-overflow:ellipsis` reacts to,
    // so it is the same question the browser asked rather than a re-derivation.
    ellipsised: el.scrollWidth > el.clientWidth,
  }));

/** The inside of a panel's box — where its content has to stay. */
const contentRight = (page, selector) =>
  page.locator(selector).evaluate((el) => {
    const style = getComputedStyle(el);
    return (
      el.getBoundingClientRect().right -
      parseFloat(style.paddingRight) -
      parseFloat(style.borderRightWidth)
    );
  });

test("the panel paints Rust's title, trailing label and window rows", async ({ page, baseURL }) => {
  const usage = await gotoWithUsage(page, baseURL);
  await expect(page.locator("#usagePanel")).toBeVisible();

  await expect(page.locator("#usageTitle")).toHaveText(usage.title);
  await expect(page.locator("#usageTrailing")).toHaveText(usage.trailing);

  await expect(windowRows(page)).toHaveCount(usage.windows.length);
  await expect(windowRows(page).locator(".lbl")).toHaveText(usage.windows.map((w) => w.label));
  await expect(windowRows(page).locator(".val")).toHaveText(usage.windows.map((w) => w.value));
  // The abbreviation is Rust's — a `toLocaleString` here would be a second
  // implementation of `ClaudeUsagePanel.tokens`.
  expect(usage.windows[1].value).toMatch(/^\d+(\.\d)?[kM]?$/);

  // The panel is confirmed to be the `usage` command's output, not a fixture
  // the page happened to fetch.
  const calls = await page.evaluate(() => window.__CALLS__.map((c) => c.command));
  expect(calls).toContain("usage");
});

test("the window rows carry no progress bar, because there is no limit to bar against", async ({ page, baseURL }) => {
  // A subscription publishes no ceiling. A bar against an invented one would be
  // a percentage of a number nobody set, so Rust sends no `bar` at all — and
  // this file must not manufacture one.
  await gotoWithUsage(page, baseURL);
  await expect(windowRows(page)).not.toHaveCount(0);
  await expect(page.locator("#usageBody .pv-row.window .pbar")).toHaveCount(0);
});

test("the top-projects list is capped and every row keeps its own figure", async ({ page, baseURL }) => {
  const usage = await gotoWithUsage(page, baseURL);
  const projects = page.locator('#usageBody .pv-section[data-section="projects"]');
  await expect(projects.locator(".lbl")).toHaveText(usage.projects.label);

  const rows = projects.locator(".pv-item");
  await expect(rows).toHaveCount(usage.projects.rows.length);
  expect(usage.projects.rows.length, "Rust caps the list at four").toBe(4);
  await expect(rows.locator(".name")).toHaveText(usage.projects.rows.map((r) => r.name));
  await expect(rows.locator(".val")).toHaveText(usage.projects.rows.map((r) => r.value));
});

test("a measured provider shows its figures and an unmeasured one shows an em dash", async ({ page, baseURL }) => {
  // The distinction the enums in `crates/usage` exist to make. Both fixtures
  // have the SAME configured providers; only the measurement differs, so a
  // panel that derived "—" from a falsy number would pass one and fail the
  // other — or worse, print a fabricated 0 for both.
  const measured = await gotoWithUsage(page, baseURL);
  await expect(section(page, "neon").locator(".pv-row .lbl")).toHaveText(
    measured.providers[0].rows.map((r) => r.label)
  );
  await expect(section(page, "neon").locator(".pv-row .val")).toHaveText(
    measured.providers[0].rows.map((r) => r.value)
  );
  await expect(section(page, "neon").locator(".pv-row .val").first()).toHaveCSS(
    "color",
    rgb(measured.providers[0].rows[0].valueColor)
  );
  expect(measured.providers[0].rows[0].value).not.toBe("—");

  // The two cost rows ride the same generic row pipeline; their presence in
  // the fixture is the Rust dump's promise, their rendering is this suite's.
  const labels = measured.providers[0].rows.map((r) => r.label);
  expect(labels).toContain("NEON EST. CHARGES (MTD)");
  expect(labels).toContain("NEON LAST INVOICE");

  const unmeasured = await gotoWithUsage(page, baseURL, "sample-usage-unmeasured.json");
  await expect(section(page, "neon").locator(".pv-row .val").first()).toHaveText("—");
  await expect(section(page, "neon").locator(".pv-row .val").first()).toHaveCSS(
    "color",
    rgb(unmeasured.providers[0].rows[0].valueColor)
  );
  // …and it says why, rather than leaving a bare dash — in the header, naming
  // the section it is about. Nothing is appended under the rows: that is what
  // made the card grow and shove the rest of the cockpit around.
  await expect(section(page, "neon").locator(".pv-footer")).toHaveCount(0);
  await expect(page.locator("#usageStale")).toHaveText(unmeasured.footer.text);
  expect(unmeasured.footer.text, "the warning names the section").toContain("neon:");
});

test("the Sentry quota bar is drawn only when the count is known", async ({ page, baseURL }) => {
  // Both fixtures carry the SAME quota. The bar appears in one and not the
  // other purely because the count is unknown in the second — a bar at a
  // defaulted zero would read "comfortably under quota" when the truth is
  // "nobody measured", which is the fabricated-zero bug wearing a bar.
  const measured = await gotoWithUsage(page, baseURL);
  const bar = section(page, "sentry").locator(".pbar > span");
  await expect(bar).toHaveCount(1);
  await expect(bar).toHaveCSS("background-color", rgb(measured.providers[1].bar.color));

  await gotoWithUsage(page, baseURL, "sample-usage-unmeasured.json");
  await expect(section(page, "sentry").locator(".pv-row .val")).toHaveText("—");
  await expect(section(page, "sentry").locator(".pbar")).toHaveCount(0);
});

test("a stale metered section is dated on its own line, and warned about in the header", async ({ page, baseURL }) => {
  // The two clocks, answering different questions and now living in two
  // different places. The `.panel-asof` line dates the figures ("as of 23h
  // ago") and stays in the body, beside the numbers it qualifies; the warning
  // that the poller is late is hoisted to the panel header, because a line
  // under the body makes the card taller and `.panel-row` stretches every other
  // card in the row to match. Both fixtures carry the same numbers, so the only
  // difference a reader sees is the claim being made about them — which is the
  // whole point of marking staleness rather than rendering a nearly-hour-old
  // figure as a current one.
  const live = await gotoWithUsage(page, baseURL);
  for (const provider of live.providers) {
    expect(provider.freshness.state).toBe("live");
    expect(provider.freshness.text, "a current reading paints nothing").toBeNull();
  }
  await expect(page.locator("#usageBody .panel-asof")).toHaveCount(0);
  expect(live.footer, "and a fresh panel warns about nothing").toBeNull();
  await expect(page.locator("#usageStale")).toBeHidden();

  const stale = await gotoWithUsage(page, baseURL, "sample-usage-stale.json");
  for (const provider of stale.providers) {
    expect(provider.freshness.state).toBe("stale");
    expect(provider.freshness.measured_secs_ago).toBeGreaterThan(0);
    const asOf = section(page, provider.id).locator(".panel-asof");
    await expect(asOf).toBeVisible();
    await expect(asOf).toHaveText(provider.freshness.text);
    await expect(asOf).toHaveCSS("color", rgb(provider.freshness.color));
    // The freshness line is the LAST thing in the section: nothing is appended
    // under it any more.
    const order = await section(page, provider.id).evaluate((el) =>
      [...el.children].map((c) => c.className)
    );
    expect(order[order.length - 1]).toBe("panel-asof");
    expect(provider.footer, "a section carries no warning of its own").toBeUndefined();
  }
  await expect(page.locator("#usageBody .pv-footer")).toHaveCount(0);

  // Same figures either way — the mark is the line, never a changed number.
  expect(stale.providers[0].rows.map((r) => r.value)).toEqual(
    live.providers[0].rows.map((r) => r.value)
  );

  // Both sections went stale at once and both are on the one header line, each
  // naming itself. Unattributed they would be the byte-identical
  // `⚠ stale · updated 23h ago` twice over — a line saying the same thing twice
  // and identifying neither.
  const warning = page.locator("#usageStale");
  await expect(warning).toBeVisible();
  await expect(warning).toHaveText(stale.footer.text);
  await expect(warning).toHaveAttribute("title", stale.footer.text);
  for (const provider of stale.providers) {
    expect(stale.footer.text).toContain(`${provider.id}:`);
  }
  // …and it is not the freshness line wearing a different class: two strings,
  // two questions.
  expect(stale.footer.text).not.toBe(stale.providers[0].freshness.text);
});

test("a degraded panel is exactly as tall as a healthy one, however long the warning", async ({ page, baseURL }) => {
  // The regression this issue is about. A `.pv-footer` under a provider section
  // carried no `white-space:nowrap`, so a long message wrapped — six lines, on
  // the screenshot that reported it — the card grew, `.panel-row` stretched
  // every other card in the row to match, and every row below moved down. One
  // Neon read going stale rearranged the cockpit. Mirrors the identical
  // assertion on the Repos panel in `github.spec.js`.
  const healthy = await gotoWithUsage(page, baseURL);
  expect(healthy.footer, "the healthy fixture warns about nothing").toBeNull();
  const before = await page.locator("#usagePanel").boundingBox();
  const headerBefore = await page.locator("#usagePanel .panel-hdr").boundingBox();

  // Rust's amber, read off a fixture that carries a real warning rather than
  // written out here — this test is about height, and a literal would be a
  // second author of a colour `viewmodel::color` owns.
  const amber = (await fixture(baseURL, "sample-usage-stale.json")).footer.color;
  const degraded = { ...healthy, footer: { text: LONG_WARNING, color: amber } };
  await page.evaluate((vm) => window.__SOLADOR_USAGE_TEST__.render(vm), degraded);

  const warning = page.locator("#usageStale");
  await expect(warning).toBeVisible();
  await expect(warning).toHaveText(LONG_WARNING);
  // Ellipsised rather than wrapped, so the whole message has to stay reachable
  // somewhere.
  await expect(warning).toHaveAttribute("title", LONG_WARNING);

  // The header did not become two lines, and the card did not become one line
  // taller — the two ways this message could have cost height.
  const headerAfter = await page.locator("#usagePanel .panel-hdr").boundingBox();
  expect(headerAfter.height).toBeCloseTo(headerBefore.height, 1);
  const after = await page.locator("#usagePanel").boundingBox();
  expect(after.height).toBeCloseTo(before.height, 1);
  expect(after.width).toBeCloseTo(before.width, 1);
});

test("a pathologically long warning stays inside the tile and starves nothing beside it", async ({ page, baseURL }) => {
  const healthy = await gotoWithUsage(page, baseURL);
  const cell = await page.locator("#usagePanel").boundingBox();
  const amber = (await fixture(baseURL, "sample-usage-stale.json")).footer.color;

  await page.evaluate(
    ([vm, text, color]) =>
      window.__SOLADOR_USAGE_TEST__.render({ ...vm, footer: { text, color } }),
    [healthy, LONG_WARNING, amber]
  );

  // Nothing bleeds into the neighbouring tile or the page gutter: the panel's
  // own box is unmoved, and every header element sits inside it.
  const degraded = await page.locator("#usagePanel").boundingBox();
  expect(degraded.x).toBeCloseTo(cell.x, 1);
  expect(degraded.width).toBeCloseTo(cell.width, 1);
  const inside = await contentRight(page, "#usagePanel");
  for (const selector of ["#usageTitle", "#usageStale", "#usageTrailing"]) {
    const el = await fit(page, selector);
    expect(el.right, `${selector} must stay inside the card`).toBeLessThanOrEqual(inside + 0.5);
  }

  // The warning is the designated give and the only one: the trailing figure
  // beside it keeps every character.
  expect((await fit(page, "#usageStale")).ellipsised).toBe(true);
  expect((await fit(page, "#usageTrailing")).ellipsised).toBe(false);
  await expect(page.locator("#usageTrailing")).toHaveText(healthy.trailing);
});

test("a section with no reading publishes no age rather than a fresh-looking zero", async ({ page, baseURL }) => {
  // `sample-usage-empty.json` has no provider configured at all, so there is
  // no section — and nothing anywhere claiming an age of zero.
  const usage = await gotoWithUsage(page, baseURL, "sample-usage-empty.json");
  expect(usage.providers).toEqual([]);
  await expect(page.locator("#usageBody .panel-asof")).toHaveCount(0);

  // A configured provider whose first read has not landed publishes `unknown`
  // and a null age — never a 0, which would paint it as the freshest section on
  // the card. Built by editing the payload, because no dumped fixture holds a
  // provider mid-first-read.
  const loading = await gotoWithUsage(page, baseURL);
  loading.providers[0].freshness = {
    state: "unknown",
    measured_secs_ago: null,
    text: null,
    color: null,
  };
  await page.evaluate((vm) => window.__SOLADOR_USAGE_TEST__.render(vm), loading);
  await expect(page.locator("#usageBody .panel-asof")).toHaveCount(0);
});

test("an unconfigured provider contributes no section at all", async ({ page, baseURL }) => {
  // Not an em dash, not an empty heading: no markup. The em dash is for
  // "configured, and we could not find out"; a provider nobody set up is not a
  // question that was asked.
  const usage = await gotoWithUsage(page, baseURL, "sample-usage-empty.json");
  expect(usage.providers).toEqual([]);
  await expect(page.locator("#usageBody .pv-section[data-provider]")).toHaveCount(0);

  // And the Claude half says what state it is in, in Rust's words.
  await expect(page.locator("#usageBody .pv-message")).toHaveText(usage.message.text);
  await expect(page.locator("#usageTrailing")).toHaveText("");
  await expect(page.locator("#usageStale")).toHaveText(usage.footer.text);
});

/**
 * The two-column case, built by editing the payload rather than by picking a
 * dumped window size.
 *
 * No dumped cockpit gives Usage two content columns: it is authored a quarter,
 * so even at 2732pt it pairs with OpenClaw and lands at 675 — one column. The 2
 * arrives at >= 696pt (`PanelKind::ClaudeUsage::min_width` is 340) and that
 * derivation is pinned by the Rust tests; the payload is the contract, and what
 * this suite owns is the frontend honouring the count it is handed.
 */
async function gotoWithColumns(page, baseURL, columns, mutate) {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  for (const panel of cockpit.panelRows.flat()) {
    if (panel.id === "claudeUsage") panel.columns = columns;
  }
  const usage = await fixture(baseURL, "sample-usage.json");
  if (mutate) mutate(usage);
  await stubIpc(page, { cockpit, usage });
  await page.goto("/index.html");
  return usage;
}

test("on a full-width card the providers sit beside the rollups, not under them", async ({ page, baseURL }) => {
  const usage = await gotoWithColumns(page, baseURL, 2);
  expect(usage.providers.length, "both wrappers need content").toBe(2);
  await expect(page.locator("#usagePanel")).toHaveAttribute("data-cols", "2");

  const box = async (selector) => await page.locator(selector).boundingBox();
  const claude = await box("#usageBody .usage-main");
  const providers = await box("#usageBody .usage-providers");
  expect(providers.x, "the providers start to the right of the rollups").toBeGreaterThan(
    claude.x + claude.width - 1
  );
  expect(
    Math.abs(providers.y - claude.y),
    "and on the same line, not below"
  ).toBeLessThan(2);

  // A provider's leading rule separates it from the block ABOVE it. The first
  // one now opens the right-hand column, where there is nothing above it.
  await expect(section(page, usage.providers[0].id).locator(".pv-divider")).toBeHidden();
  await expect(section(page, usage.providers[1].id).locator(".pv-divider")).toBeVisible();
  // The projects rule lives inside `.usage-main` and still has the windows to
  // separate itself from, so it survives the split.
  await expect(
    page.locator('#usageBody .pv-section[data-section="projects"] .pv-divider')
  ).toBeVisible();
});

test("a narrow card stacks the providers under the rollups, dividers and all", async ({ page, baseURL }) => {
  // Same panel, same DOM — only the column count Rust derived differs.
  const narrow = await fixture(baseURL, "sample-cockpit-narrow.json");
  expect(narrow.panelRows.flat().find((p) => p.id === "claudeUsage").columns).toBe(1);
  const usage = await fixture(baseURL, "sample-usage.json");
  await stubIpc(page, { cockpit: narrow, usage });
  await page.goto("/index.html");

  const claude = await page.locator("#usageBody .usage-main").boundingBox();
  const providers = await page.locator("#usageBody .usage-providers").boundingBox();
  expect(providers.y).toBeGreaterThan(claude.y + claude.height - 1);
  for (const provider of usage.providers) {
    await expect(section(page, provider.id).locator(".pv-divider")).toBeVisible();
  }
});

test("with no providers the Claude block takes the whole two-column body", async ({ page, baseURL }) => {
  // The panel with nothing configured but Claude has to stay pixel-identical to
  // its Claude-only self at either count — half a card of rollups with an empty
  // track beside them is not that.
  await gotoWithColumns(page, baseURL, 2, (usage) => delete usage.providers);
  await expect(page.locator("#usageBody .usage-providers")).toHaveCount(0);

  const body = await page.locator("#usageBody").boundingBox();
  const claude = await page.locator("#usageBody .usage-main").boundingBox();
  expect(
    Math.abs(claude.width - body.width),
    "the rollups span both tracks, not one"
  ).toBeLessThan(1);
});
