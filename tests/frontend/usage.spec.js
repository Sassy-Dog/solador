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
  // …and it says why, rather than leaving a bare dash.
  await expect(section(page, "neon").locator(".pv-footer")).toHaveText(
    unmeasured.providers[0].footer.text
  );
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
  await expect(page.locator("#usageFooter")).toHaveText(usage.footer.text);
});
