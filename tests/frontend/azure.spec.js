import { test, expect } from "@playwright/test";

// Same CSP guard as the other suites — see usage.spec.js. This panel leans on
// CSSOM harder than most: the caption's amber is the whole signal that the
// headline covers a month the reader would not assume, and a dropped inline
// style would render it in the muted colour that means the opposite.
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

async function stubIpc(page, { cockpit, azure }) {
  await page.addInitScript(
    ({ cockpit, azure }) => {
      window.__CALLS__ = [];
      window.__TAURI__ = {
        core: {
          invoke: async (command, args) => {
            window.__CALLS__.push({ command, args });
            if (command === "cockpit") return cockpit;
            if (command === "azure_cost") return azure;
            return null;
          },
        },
      };
    },
    { cockpit, azure }
  );
}

async function gotoWithAzure(page, baseURL, name = "sample-azure.json") {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  const azure = await fixture(baseURL, name);
  await stubIpc(page, { cockpit, azure });
  await page.goto("/index.html");
  return azure;
}

/** `#33d17a` as the browser reports a computed colour. */
const rgb = (hex) => {
  const n = parseInt(hex.slice(1), 16);
  return `rgb(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255})`;
};

/**
 * A warning long enough that no header could ever hold it, and deliberately not
 * a realistic sentence — see the twin in `usage.spec.js`. The shape is the one
 * that reported this: an `az` traceback reaching the header verbatim, with a
 * `last ok` clock behind it that has to survive.
 */
const LONG_WARNING =
  "⚠ Azure CLI could not mint a SAS: ERROR: The command failed with an " +
  "unexpected error. Here is the traceback: Traceback (most recent call " +
  "last): File \"knack/cli.py\", line 233, in invoke — and the point of " +
  "asserting against something this long is that containment is a property " +
  "of the layout, not of how long the message happens to be today " +
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

test("the panel paints Rust's headline, caption, trailing label and stat rows", async ({ page, baseURL }) => {
  const azure = await gotoWithAzure(page, baseURL);
  await expect(page.locator("#azurePanel")).toBeVisible();

  await expect(page.locator("#azureTitle")).toHaveText(azure.title);
  await expect(page.locator("#azureTrailing")).toHaveText(azure.trailing);

  await expect(page.locator("#azureBody .az-headline .value")).toHaveText(azure.headline.value);
  await expect(page.locator("#azureBody .az-headline .caption")).toHaveText(azure.headline.caption);
  // The currency formatting is Rust's en_US twin of the original NumberFormatter —
  // a `toLocaleString` here would render the bill in the webview's locale.
  expect(azure.headline.value).toMatch(/^\$[\d,]+\.\d{2}$/);

  const stats = page.locator("#azureBody .az-main > .pv-row");
  await expect(stats).toHaveCount(azure.stats.length);
  await expect(stats.locator(".lbl")).toHaveText(azure.stats.map((s) => s.label));
  await expect(stats.locator(".val")).toHaveText(azure.stats.map((s) => s.value));

  const calls = await page.evaluate(() => window.__CALLS__.map((c) => c.command));
  expect(calls).toContain("azure_cost");
});

test("the rollover-gap caption is amber and stamps the month it actually covers", async ({ page, baseURL }) => {
  // On the 1st the current month may not be exported yet, so the figures are
  // last month's. Both fixtures carry the SAME dollar amounts — only the
  // caption and its colour differ, which is exactly the signal a reader has
  // that the headline is not what they would assume.
  const normal = await gotoWithAzure(page, baseURL);
  await expect(page.locator("#azureBody .az-headline .caption")).toHaveCSS(
    "color",
    rgb(normal.headline.captionColor)
  );
  expect(normal.headline.caption).toBe("month-to-date");

  const fallback = await gotoWithAzure(page, baseURL, "sample-azure-fallback.json");
  expect(fallback.headline.value).toBe(normal.headline.value);
  expect(fallback.headline.captionColor).not.toBe(normal.headline.captionColor);
  await expect(page.locator("#azureBody .az-headline .caption")).toHaveText(
    fallback.headline.caption
  );
  await expect(page.locator("#azureBody .az-headline .caption")).toHaveCSS(
    "color",
    rgb(fallback.headline.captionColor)
  );
  // The trailing label stamps it too, so the month is visible without reading
  // the caption.
  await expect(page.locator("#azureTrailing")).toHaveText(fallback.trailing);
});

test("the budget bar pairs projected with budget and takes its colour from Rust", async ({ page, baseURL }) => {
  const azure = await gotoWithAzure(page, baseURL);
  const budget = page.locator('#azureBody .pv-section[data-section="budget"]');
  await expect(budget.locator(".lbl")).toHaveText(azure.budget.label);
  await expect(budget.locator(".val")).toHaveText(azure.budget.value);
  await expect(budget.locator(".pbar > span")).toHaveCSS(
    "background-color",
    rgb(azure.budget.bar.color)
  );
});

test("both breakdown columns render, capped at five rows each", async ({ page, baseURL }) => {
  const azure = await gotoWithAzure(page, baseURL);
  const columns = page.locator("#azureBody .az-columns > .pv-section");
  await expect(columns).toHaveCount(azure.breakdowns.length);
  expect(azure.breakdowns.length).toBe(2);

  for (const [i, column] of azure.breakdowns.entries()) {
    await expect(columns.nth(i).locator(".lbl")).toHaveText(column.title);
    await expect(columns.nth(i).locator(".pv-item")).toHaveCount(column.rows.length);
    expect(column.rows.length, "Rust caps each column at TOP_N").toBeLessThanOrEqual(5);
  }
  await expect(columns.first().locator(".pv-item .name")).toHaveText(
    azure.breakdowns[0].rows.map((r) => r.name)
  );
});

test("on a half-width card the breakdowns sit beside the costs, not under them", async ({ page, baseURL }) => {
  // Azure Cost is authored half a row wide -- it gave its other two quarters to
  // the Services and Sentry Crons panels -- and a half is still enough for
  // `panel_columns` to afford it two content columns, though with far less
  // headroom than the Three-quarters it used to have (2 * 400 + 16 = 816pt of a
  // half, which needs a 1648pt cockpit rather than a 1094pt one). The split
  // itself is CSS over `--panel-cols`, so this is about the frontend applying
  // Rust's count rather than deciding one. The fixture is dumped at 2732pt.
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  const azurePanel = cockpit.panelRows.flat().find((p) => p.id === "azureCost");
  expect(
    azurePanel.span,
    "a half is what still buys the breakdowns their own column"
  ).toBe("half");
  expect(azurePanel.columns, "wide enough for the two-column body").toBe(2);

  await gotoWithAzure(page, baseURL);
  const box = async (selector) => await page.locator(selector).boundingBox();
  const costs = await box("#azureBody .az-main");
  const breakdowns = await box("#azureBody .az-columns");
  expect(breakdowns.x, "the breakdowns start to the right of the costs").toBeGreaterThan(
    costs.x + costs.width - 1
  );
  expect(
    Math.abs(breakdowns.y - costs.y),
    "and on the same line, not below"
  ).toBeLessThan(2);
  // The rule that separated them when stacked has nothing left to separate.
  await expect(page.locator("#azureBody > .pv-divider")).toBeHidden();
});

test("a narrow card stacks the breakdowns under the costs, divider and all", async ({ page, baseURL }) => {
  // Same panel, same DOM — only the column count Rust derived differs.
  const narrow = await fixture(baseURL, "sample-cockpit-narrow.json");
  expect(narrow.panelRows.flat().find((p) => p.id === "azureCost").columns).toBe(1);
  const azure = await fixture(baseURL, "sample-azure.json");
  await stubIpc(page, { cockpit: narrow, azure });
  await page.goto("/index.html");

  const costs = await page.locator("#azureBody .az-main").boundingBox();
  const breakdowns = await page.locator("#azureBody .az-columns").boundingBox();
  expect(breakdowns.y).toBeGreaterThan(costs.y + costs.height - 1);
  await expect(page.locator("#azureBody > .pv-divider")).toBeVisible();
});

test("a stale reading is dimmed and dated, on its own line beside the footer", async ({ page, baseURL }) => {
  // The two clocks, side by side and answering different questions:
  // `#azureFreshness` dates the figure ("as of 23h ago"), `#azureStale` is the
  // status footer warning that the poller is late. Both fixtures carry the same
  // dollars, so the only difference a reader sees is the claim being made about
  // them — which is the whole point of marking staleness rather than rendering
  // a day-old number as a current one.
  const live = await gotoWithAzure(page, baseURL);
  expect(live.freshness.state).toBe("live");
  expect(live.freshness.text, "a current reading is painted as it always was").toBeNull();
  await expect(page.locator("#azureFreshness")).toBeHidden();

  const stale = await gotoWithAzure(page, baseURL, "sample-azure-stale.json");
  expect(stale.freshness.state).toBe("stale");
  expect(stale.freshness.measured_secs_ago).toBeGreaterThan(0);
  await expect(page.locator("#azureFreshness")).toHaveText(stale.freshness.text);
  await expect(page.locator("#azureFreshness")).toHaveCSS(
    "color",
    rgb(stale.freshness.color)
  );

  // Not folded into the footer: two elements, two strings, both visible.
  await expect(page.locator("#azureStale")).toHaveText(stale.footer.text);
  expect(stale.footer.text).not.toBe(stale.freshness.text);

  // Same money, dimmer green — the mark itself.
  expect(stale.headline.value).toBe(live.headline.value);
  expect(stale.headline.valueColor).not.toBe(live.headline.valueColor);
  await expect(page.locator("#azureBody .az-headline .value")).toHaveCSS(
    "color",
    rgb(stale.headline.valueColor)
  );
});

test("a warning costs the card no height, however long it is", async ({ page, baseURL }) => {
  // The same assertion the Repos panel carries in `github.spec.js` and the
  // Usage panel in `usage.spec.js`: a message must never make its own card
  // taller, because `.panel-row` stretches every other card in the row to match
  // and the rows below then move down.
  const healthy = await gotoWithAzure(page, baseURL);
  expect(healthy.footer, "the healthy fixture warns about nothing").toBeNull();
  const before = await page.locator("#azurePanel").boundingBox();
  const headerBefore = await page.locator("#azurePanel .panel-hdr").boundingBox();

  // Rust's amber, read off a fixture that carries a real warning rather than
  // written out here.
  const amber = (await fixture(baseURL, "sample-azure-stale.json")).footer.color;
  await page.evaluate(
    ([vm, text, color]) =>
      window.__SOLADOR_AZURE_TEST__.render({ ...vm, footer: { text, color } }),
    [healthy, LONG_WARNING, amber]
  );

  const warning = page.locator("#azureStale");
  await expect(warning).toBeVisible();
  await expect(warning).toHaveText(LONG_WARNING);
  await expect(warning).toHaveAttribute("title", LONG_WARNING);

  const headerAfter = await page.locator("#azurePanel .panel-hdr").boundingBox();
  expect(headerAfter.height, "one header line, never two").toBeCloseTo(headerBefore.height, 1);
  const after = await page.locator("#azurePanel").boundingBox();
  expect(after.height).toBeCloseTo(before.height, 1);
  expect(after.width).toBeCloseTo(before.width, 1);
});

test("a long warning yields its width before the freshness clock does", async ({ page, baseURL }) => {
  // This header carries BOTH `.panel-asof` and `.panel-stale`, and the two used
  // to share one declaration — `flex-shrink:100` included. Flex distributes a
  // shortfall in proportion to shrink factor TIMES base size, so at an equal
  // factor the longest element keeps the most characters: the screenshot that
  // reported this rendered `as of 6h a…` beside a warning that kept most of a
  // traceback. The clocks are short and fixed-format and the warning is the
  // designated give, so the give is now the warning's alone.
  const stale = await gotoWithAzure(page, baseURL, "sample-azure-stale.json");
  expect(stale.freshness.text, "this fixture has both clocks in the header").toBeTruthy();
  expect(stale.footer.text).toBeTruthy();
  const cell = await page.locator("#azurePanel").boundingBox();

  await page.evaluate(
    ([vm, text]) =>
      window.__SOLADOR_AZURE_TEST__.render({
        ...vm,
        footer: { ...vm.footer, text },
      }),
    [stale, LONG_WARNING]
  );

  // The freshness clock keeps every character…
  await expect(page.locator("#azureFreshness")).toHaveText(stale.freshness.text);
  expect((await fit(page, "#azureFreshness")).ellipsised).toBe(false);
  // …and so does the trailing figure at the other end of the row.
  await expect(page.locator("#azureTrailing")).toHaveText(stale.trailing);
  expect((await fit(page, "#azureTrailing")).ellipsised).toBe(false);
  // …while the warning is cut, which is what it is for.
  expect((await fit(page, "#azureStale")).ellipsised).toBe(true);

  // And nothing bleeds into the neighbouring tile or the page gutter.
  const degraded = await page.locator("#azurePanel").boundingBox();
  expect(degraded.x).toBeCloseTo(cell.x, 1);
  expect(degraded.width).toBeCloseTo(cell.width, 1);
  const inside = await contentRight(page, "#azurePanel");
  for (const selector of ["#azureTitle", "#azureFreshness", "#azureStale", "#azureTrailing"]) {
    const el = await fit(page, selector);
    expect(el.right, `${selector} must stay inside the card`).toBeLessThanOrEqual(inside + 0.5);
  }
});

test("a panel with no reading publishes no age rather than a fresh-looking zero", async ({ page, baseURL }) => {
  const unconfigured = await gotoWithAzure(page, baseURL, "sample-azure-empty.json");
  expect(unconfigured.freshness.state).toBe("unknown");
  expect(unconfigured.freshness.measured_secs_ago).toBeNull();
  await expect(page.locator("#azureFreshness")).toBeHidden();
});

test("a missing SAS URL reads as setup and a failed read reads as a failure", async ({ page, baseURL }) => {
  // The two states this panel must never conflate, and the discriminator is a
  // colour Rust chose: rendering "add a SAS URL" in red would send an operator
  // hunting a break that does not exist.
  const unconfigured = await gotoWithAzure(page, baseURL, "sample-azure-empty.json");
  await expect(page.locator("#azureBody .pv-message")).toHaveText(unconfigured.message.text);
  await expect(page.locator("#azureBody .pv-message")).toHaveCSS(
    "color",
    rgb(unconfigured.message.color)
  );
  await expect(page.locator("#azureBody .az-headline")).toHaveCount(0);
  await expect(page.locator("#azureTrailing")).toHaveText("");

  const failed = await gotoWithAzure(page, baseURL, "sample-azure-error.json");
  expect(failed.message.color).not.toBe(unconfigured.message.color);
  await expect(page.locator("#azureBody .pv-message")).toHaveText(failed.message.text);
  await expect(page.locator("#azureBody .pv-message")).toHaveCSS(
    "color",
    rgb(failed.message.color)
  );
});
