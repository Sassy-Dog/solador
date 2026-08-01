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

test("the panel paints Rust's headline, caption, trailing label and stat rows", async ({ page, baseURL }) => {
  const azure = await gotoWithAzure(page, baseURL);
  await expect(page.locator("#azurePanel")).toBeVisible();

  await expect(page.locator("#azureTitle")).toHaveText(azure.title);
  await expect(page.locator("#azureTrailing")).toHaveText(azure.trailing);

  await expect(page.locator("#azureBody .az-headline .value")).toHaveText(azure.headline.value);
  await expect(page.locator("#azureBody .az-headline .caption")).toHaveText(azure.headline.caption);
  // The currency formatting is Rust's en_US twin of the Swift NumberFormatter —
  // a `toLocaleString` here would render the bill in the webview's locale.
  expect(azure.headline.value).toMatch(/^\$[\d,]+\.\d{2}$/);

  const stats = page.locator("#azureBody > .pv-row");
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
