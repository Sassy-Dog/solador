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

/**
 * The busiest clock line this panel can actually produce: every configured
 * provider dated at once, each naming itself
 * (`panel::merged_freshness`).
 *
 * Unlike `LONG_WARNING` this is not exaggerated — it is the cost the Decision
 * on [#355](https://github.com/Sassy-Dog/solador/issues/355) accepted for
 * hoisting these clocks into the header, so it is the case that has to stay
 * inside the tile rather than a pathological one.
 */
const THREE_CLOCKS =
  "neon: as of 23h ago · sentry: as of 23h ago · vercel: as of 23h ago";

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

test("a stale metered section is dated and warned about in the header, on two separate lines", async ({ page, baseURL }) => {
  // The two clocks, answering different questions, in two different elements —
  // both in the header now. The `.panel-asof` line dates the figures ("neon: as
  // of 23h ago"); `.panel-stale` says the poller is late. Neither may sit
  // beside the rows it describes: both are absent while the section is healthy,
  // so each made the card taller the moment it fired and `.panel-row` stretched
  // every other card in the row to match. Both fixtures carry the same numbers,
  // so the only difference a reader sees is the claim being made about them —
  // which is the whole point of marking staleness rather than rendering a
  // nearly-hour-old figure as a current one.
  const live = await gotoWithUsage(page, baseURL);
  expect(live.freshness, "a current panel dates nothing").toBeNull();
  expect(live.footer, "and warns about nothing").toBeNull();
  await expect(page.locator("#usageFreshness")).toBeHidden();
  await expect(page.locator("#usageStale")).toBeHidden();

  const stale = await gotoWithUsage(page, baseURL, "sample-usage-stale.json");
  for (const provider of stale.providers) {
    expect(provider.freshness, "a section carries no clock of its own").toBeUndefined();
    expect(provider.footer, "a section carries no warning of its own").toBeUndefined();
  }
  // Nothing at all was appended under the rows — neither class, at either
  // spelling.
  await expect(page.locator("#usageBody .panel-asof")).toHaveCount(0);
  await expect(page.locator("#usageBody .pv-footer")).toHaveCount(0);

  // Same figures either way — the mark is the line, never a changed number.
  expect(stale.providers[0].rows.map((r) => r.value)).toEqual(
    live.providers[0].rows.map((r) => r.value)
  );

  // Both sections went stale at once, so each header line carries both of them,
  // each naming itself. Unattributed the clocks would be the byte-identical
  // `as of 23h ago` twice over — a line saying the same thing twice and
  // identifying neither, exactly as the warnings were before #351.
  const freshness = page.locator("#usageFreshness");
  await expect(freshness).toBeVisible();
  await expect(freshness).toHaveText(stale.freshness.text);
  await expect(freshness).toHaveAttribute("title", stale.freshness.text);
  await expect(freshness).toHaveCSS("color", rgb(stale.freshness.color));

  const warning = page.locator("#usageStale");
  await expect(warning).toBeVisible();
  await expect(warning).toHaveText(stale.footer.text);
  await expect(warning).toHaveAttribute("title", stale.footer.text);

  for (const provider of stale.providers) {
    expect(stale.freshness.text, "the clock names its section").toContain(`${provider.id}:`);
    expect(stale.footer.text, "and so does the warning").toContain(`${provider.id}:`);
  }
  // …and the clock is not the warning wearing a different class: two elements,
  // two strings, two questions. Sharing a header is not being folded together.
  expect(stale.freshness.text).not.toBe(stale.footer.text);
  expect(stale.footer.text).not.toContain(stale.freshness.text);
});

test("a degraded panel is exactly as tall as a healthy one, however long the warning and the clock", async ({ page, baseURL }) => {
  // The regression these two issues are about. A `.pv-footer` under a provider
  // section carried no `white-space:nowrap`, so a long message wrapped — six
  // lines, on the screenshot that reported it — the card grew, `.panel-row`
  // stretched every other card in the row to match, and every row below moved
  // down. The `.panel-asof` under it was one line rather than six and otherwise
  // identical: absent while the section polls on cadence, present the moment a
  // poll is missed. One Neon read going stale rearranged the cockpit. Mirrors
  // the identical assertion on the Repos panel in `github.spec.js`.
  const healthy = await gotoWithUsage(page, baseURL);
  expect(healthy.footer, "the healthy fixture warns about nothing").toBeNull();
  expect(healthy.freshness, "and dates nothing").toBeNull();
  const before = await page.locator("#usagePanel").boundingBox();
  const headerBefore = await page.locator("#usagePanel .panel-hdr").boundingBox();

  // First the real thing: the dumped stale payload, which carries BOTH lines
  // and is otherwise the same panel — `dump_usage` moves only the providers'
  // last-success clock, so every row, section and bar is identical.
  const stale = await fixture(baseURL, "sample-usage-stale.json");
  expect(stale.footer.text, "the stale fixture carries a warning").toBeTruthy();
  expect(stale.freshness.text, "and a clock").toBeTruthy();
  await page.evaluate((vm) => window.__SOLADOR_USAGE_TEST__.render(vm), stale);
  await expect(page.locator("#usageFreshness")).toBeVisible();
  await expect(page.locator("#usageStale")).toBeVisible();
  const dated = await page.locator("#usagePanel").boundingBox();
  expect(dated.height, "both lines arriving cost the card nothing").toBeCloseTo(
    before.height,
    1
  );

  // Then the pathological one, on both lines at once: Rust's amber, read off
  // the fixture rather than written out here — this test is about height, and a
  // literal would be a second author of a colour `viewmodel::color` owns.
  const amber = stale.footer.color;
  const degraded = {
    ...healthy,
    footer: { text: LONG_WARNING, color: amber },
    freshness: { text: THREE_CLOCKS, color: amber },
  };
  await page.evaluate((vm) => window.__SOLADOR_USAGE_TEST__.render(vm), degraded);

  const warning = page.locator("#usageStale");
  await expect(warning).toBeVisible();
  await expect(warning).toHaveText(LONG_WARNING);
  // Ellipsised rather than wrapped, so the whole message has to stay reachable
  // somewhere.
  await expect(warning).toHaveAttribute("title", LONG_WARNING);
  const freshness = page.locator("#usageFreshness");
  await expect(freshness).toHaveText(THREE_CLOCKS);
  await expect(freshness).toHaveAttribute("title", THREE_CLOCKS);

  // The header did not become two lines, and the card did not become one line
  // taller — the two ways these messages could have cost height.
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

  // Both header lines at their worst at once: the warning exaggerated, the
  // clock at the busiest arrangement this panel can really produce.
  await page.evaluate(
    ([vm, warning, clocks, color]) =>
      window.__SOLADOR_USAGE_TEST__.render({
        ...vm,
        footer: { text: warning, color },
        freshness: { text: clocks, color },
      }),
    [healthy, LONG_WARNING, THREE_CLOCKS, amber]
  );

  // Nothing bleeds into the neighbouring tile or the page gutter: the panel's
  // own box is unmoved, and every header element sits inside it.
  const degraded = await page.locator("#usagePanel").boundingBox();
  expect(degraded.x).toBeCloseTo(cell.x, 1);
  expect(degraded.width).toBeCloseTo(cell.width, 1);
  const inside = await contentRight(page, "#usagePanel");
  for (const selector of ["#usageTitle", "#usageFreshness", "#usageStale", "#usageTrailing"]) {
    const el = await fit(page, selector);
    expect(el.right, `${selector} must stay inside the card`).toBeLessThanOrEqual(inside + 0.5);
  }

  // NEITHER message disappears. This is the acceptance criterion "degrades by
  // ellipsis, never by dropping a clause silently", asserted the only way it
  // can be: an element flexed to a width of zero renders no ellipsis and no
  // characters, so it reads as absent rather than as cut. Both carry the
  // designated give's `flex-shrink:100`, which splits the shortfall in
  // proportion to their length — each loses the same fraction, so they run out
  // of room together and not one before the other. At any lopsided pair of
  // factors one of these two is zero here.
  await expect(page.locator("#usageStale")).toBeVisible();
  await expect(page.locator("#usageFreshness")).toBeVisible();
  for (const selector of ["#usageStale", "#usageFreshness"]) {
    const el = await fit(page, selector);
    expect(el.ellipsised, `${selector} is cut, not deleted`).toBe(true);
  }

  // And the two elements that are not messages keep every character: the
  // trailing figure cannot shrink at all, and the panel's name is what tells a
  // reader which card this is.
  expect((await fit(page, "#usageTrailing")).ellipsised).toBe(false);
  await expect(page.locator("#usageTrailing")).toHaveText(healthy.trailing);
  expect((await fit(page, "#usageTitle")).ellipsised).toBe(false);
  await expect(page.locator("#usageTitle")).toHaveText(healthy.title);
});

test("a section with no reading publishes no age rather than a fresh-looking zero", async ({ page, baseURL }) => {
  // `sample-usage-empty.json` has no provider configured at all, so there is
  // no section — and nothing anywhere claiming an age of zero.
  const usage = await gotoWithUsage(page, baseURL, "sample-usage-empty.json");
  expect(usage.providers).toEqual([]);
  expect(usage.freshness).toBeNull();
  await expect(page.locator("#usagePanel .panel-asof")).toBeHidden();

  // A configured provider whose first read has not landed classifies to
  // `unknown` and contributes no segment — Rust's decision, pinned by
  // `usage.rs::a_section_that_has_never_read_publishes_no_age_rather_than_a_zero`,
  // and it reaches this file as a null `freshness`. What this suite owns is
  // that null renders NOTHING: not an empty amber element, and not a reserved
  // line's worth of blank space that would read as a measured value.
  const healthy = await gotoWithUsage(page, baseURL);
  const before = await page.locator("#usagePanel .panel-hdr").boundingBox();

  const loading = { ...healthy, freshness: null };
  await page.evaluate((vm) => window.__SOLADOR_USAGE_TEST__.render(vm), loading);
  const clock = page.locator("#usageFreshness");
  await expect(clock).toBeHidden();
  await expect(clock).toHaveText("");
  expect(await clock.boundingBox(), "a hidden clock occupies no box at all").toBeNull();
  const after = await page.locator("#usagePanel .panel-hdr").boundingBox();
  expect(after.height).toBeCloseTo(before.height, 1);
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
