import { test, expect } from "@playwright/test";

// Same CSP guard as the other suites: the page is served under the app's real
// policy (csp_server.py), so a blocked style surfaces as a console error rather
// than a thrown exception. Both panels set colours through CSSOM for exactly
// that reason — an inline `style=""` would be dropped under `style-src 'self'`
// and every dot, count and status word would silently render neutral.
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
 * *which* command painted a panel.
 *
 * `cockpit` has to be answered too: app.js replaces the entire document body
 * with an error line when its first `invoke` rejects, which would take these
 * panels down with it and make the suite fail for the wrong reason.
 */
async function stubIpc(page, { cockpit, repos, runners }) {
  await page.addInitScript(
    ({ cockpit, repos, runners }) => {
      window.__CALLS__ = [];
      window.__TAURI__ = {
        core: {
          invoke: async (command, args) => {
            window.__CALLS__.push({ command, args });
            if (command === "cockpit") return cockpit;
            if (command === "repos") return repos;
            if (command === "runners") return runners;
            return null;
          },
        },
      };
    },
    { cockpit, repos, runners }
  );
}

/** Loads the app with both panels stubbed from the dumped fixtures. */
async function gotoWithFixtures(page, baseURL, overrides = {}) {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  const repos = overrides.repos ?? (await fixture(baseURL, "sample-repos.json"));
  const runners = overrides.runners ?? (await fixture(baseURL, "sample-runners.json"));
  await stubIpc(page, { cockpit, repos, runners });
  await page.goto("/index.html");
  return { repos, runners };
}

/** `#33d17a` as the browser reports a computed colour. */
const rgb = (hex) => {
  const n = parseInt(hex.slice(1), 16);
  return `rgb(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255})`;
};

const repoRows = (page) => page.locator("#reposBody .gh-row:not(.gh-head)");
const runnerRows = (page) => page.locator("#runnersBody .gh-row");

// MARK: - Repos

test("the Repos panel paints Rust's title, trailing label and column header", async ({ page, baseURL }) => {
  const { repos } = await gotoWithFixtures(page, baseURL);
  await expect(page.locator("#reposPanel")).toBeVisible();

  await expect(page.locator("#reposTitle")).toHaveText(repos.title);
  await expect(page.locator("#reposTrailing")).toHaveText(repos.trailing);

  // The header labels AND their widths are Rust's — a column re-typed here
  // could drift from the cells beside it without any Rust test noticing.
  const header = page.locator("#reposBody .gh-head");
  await expect(header.locator(".gh-repo-name")).toHaveText(repos.columns[0].label);
  const labels = await header.locator(".gh-cell").allTextContents();
  expect(labels).toEqual(repos.columns.slice(1).map((c) => c.label));
  for (const [index, column] of repos.columns.slice(1).entries()) {
    await expect(header.locator(".gh-cell").nth(index)).toHaveCSS("width", `${column.width}px`);
  }

  const commands = await page.evaluate(() => window.__CALLS__.map((c) => c.command));
  expect(commands).toContain("repos");
});

test("every repo row carries Rust's cells, widths and dot colour", async ({ page, baseURL }) => {
  const { repos } = await gotoWithFixtures(page, baseURL);
  await expect(repoRows(page)).toHaveCount(repos.rows.length);

  const names = await repoRows(page).locator(".gh-repo-name").allTextContents();
  expect(names).toEqual(repos.rows.map((r) => r.name));

  for (const [index, expected] of repos.rows.entries()) {
    const row = repoRows(page).nth(index);
    await expect(row.locator(".dot")).toHaveCSS("background-color", rgb(expected.dotColor));
    const cells = row.locator(".gh-cell");
    await expect(cells).toHaveCount(expected.cells.length);
    for (const [i, cell] of expected.cells.entries()) {
      await expect(cells.nth(i)).toHaveText(cell.text);
      await expect(cells.nth(i)).toHaveCSS("color", rgb(cell.color));
      await expect(cells.nth(i)).toHaveCSS("width", `${cell.width}px`);
    }
  }
});

/**
 * The load-bearing distinction, asserted through the DOM: "—" is "we could not
 * find out" and "0" is "there are none", and they are visibly different. The
 * fixture is built to contain both, and this fails if the frontend ever starts
 * deriving either from a number.
 */
test("an unknown count renders an em dash, a real zero renders a dimmed zero", async ({ page, baseURL }) => {
  const { repos } = await gotoWithFixtures(page, baseURL);
  const texts = repos.rows.flatMap((r) => r.cells.map((c) => c.text));
  expect(texts, "the fixture must contain an unknown count").toContain("—");
  expect(texts, "…and a genuine zero beside it").toContain("0");

  const painted = await repoRows(page).locator(".gh-cell").allTextContents();
  expect(painted.filter((t) => t === "—").length).toBe(texts.filter((t) => t === "—").length);
  expect(painted.filter((t) => t === "0").length).toBe(texts.filter((t) => t === "0").length);

  // Neither is ever painted as the other's colour, and neither is ink.
  const dash = repos.rows.flatMap((r) => r.cells).find((c) => c.text === "—");
  const zero = repos.rows.flatMap((r) => r.cells).find((c) => c.text === "0");
  expect(dash.color).toBe(zero.color);
  const number = repos.rows.flatMap((r) => r.cells).find((c) => /^[1-9]/.test(c.text));
  expect(number.color).not.toBe(zero.color);
});

test("only a repo awaiting approval pulses its dot", async ({ page, baseURL }) => {
  const { repos } = await gotoWithFixtures(page, baseURL);
  const blinking = repos.rows.filter((r) => r.blinking);
  expect(blinking.length, "the fixture must contain an approval gate").toBeGreaterThan(0);

  await expect(repoRows(page).locator(".dot.blink")).toHaveCount(blinking.length);
  for (const [index, row] of repos.rows.entries()) {
    await expect(repoRows(page).nth(index).locator(".dot")).toHaveClass(
      row.blinking ? /blink/ : /^dot$/
    );
  }
});

test("the health line is Rust's sentence and Rust's colour", async ({ page, baseURL }) => {
  const { repos } = await gotoWithFixtures(page, baseURL);
  const health = page.locator("#reposHealth");
  await expect(health).toBeVisible();
  await expect(health).toHaveText(repos.health.text);
  await expect(health).toHaveCSS("color", rgb(repos.health.color));
});

test("with no token the Repos panel is one sentence, no table and no health claim", async ({ page, baseURL }) => {
  const empty = await fixture(baseURL, "sample-repos-empty.json");
  await gotoWithFixtures(page, baseURL, { repos: empty });

  await expect(page.locator("#reposBody .gh-message")).toHaveText(empty.message.text);
  expect(empty.message.text).toBe("connect a GitHub token in Settings");
  await expect(repoRows(page)).toHaveCount(0);
  await expect(page.locator("#reposBody .gh-head")).toHaveCount(0);
  // No credential means no claim about anyone's health, and no counts.
  await expect(page.locator("#reposHealth")).toBeHidden();
  await expect(page.locator("#reposTrailing")).toHaveText("");
});

test("the loading state replaces the table rather than showing an empty one", async ({ page, baseURL }) => {
  const empty = await fixture(baseURL, "sample-repos-empty.json");
  // The authenticated-but-not-yet-fetched payload, as Rust builds it.
  const loading = { ...empty, message: { text: "loading…" } };
  await gotoWithFixtures(page, baseURL, { repos: loading });

  await expect(page.locator("#reposBody .gh-message")).toHaveText("loading…");
  await expect(page.locator("#reposBody .gh-head")).toHaveCount(0);
});

// MARK: - Repos: tap to open

test("clicking a repo row asks the opener plugin for Rust's URL, unmodified", async ({ page, baseURL }) => {
  const { repos } = await gotoWithFixtures(page, baseURL);

  await repoRows(page).first().click();

  const opens = await page.evaluate(() =>
    window.__CALLS__.filter((c) => c.command === "plugin:opener|open_url")
  );
  // Exactly one call, exactly one argument, and it is the fixture's own
  // string. A URL rebuilt in JS from the repo name would pass a "it opened
  // something" assertion and still be a second author of the only string the
  // granted ACL scope accepts.
  expect(opens).toEqual([
    { command: "plugin:opener|open_url", args: { url: repos.rows[0].url } },
  ]);
  expect(repos.rows[0].url).toBe("https://github.com/Sassy-Dog/devcanopy/actions");
});

test("every repo row is its own tap target, including the unreachable one", async ({ page, baseURL }) => {
  const { repos } = await gotoWithFixtures(page, baseURL);
  const rows = repoRows(page);
  await expect(rows).toHaveCount(repos.rows.length);

  for (const [i, row] of repos.rows.entries()) {
    // Rust's accessible name, not the row's seven numbers read aloud.
    await expect(rows.nth(i)).toHaveAttribute("aria-label", row.linkLabel);
    await expect(rows.nth(i)).toHaveAttribute("role", "link");
    await expect(rows.nth(i)).toHaveJSProperty("tabIndex", 0);
  }
  // `platform` is the repo whose runs could not be fetched — being unable to
  // read a repo's CI is exactly when you want to go and look at it.
  const unreachable = repos.rows.findIndex((r) => r.name === "platform");
  expect(unreachable).toBeGreaterThan(-1);
  await rows.nth(unreachable).click();
  expect(
    await page.evaluate(() => window.__CALLS__.filter((c) => c.command === "plugin:opener|open_url"))
  ).toEqual([
    { command: "plugin:opener|open_url", args: { url: repos.rows[unreachable].url } },
  ]);
});

test("a repo row opens from the keyboard, not only from a click", async ({ page, baseURL }) => {
  const { repos } = await gotoWithFixtures(page, baseURL);

  await repoRows(page).first().focus();
  await page.keyboard.press("Enter");
  expect(
    await page.evaluate(() => window.__CALLS__.filter((c) => c.command === "plugin:opener|open_url"))
  ).toEqual([
    { command: "plugin:opener|open_url", args: { url: repos.rows[0].url } },
  ]);
});

test("a row with no URL is not a link and opens nothing", async ({ page, baseURL }) => {
  const repos = await fixture(baseURL, "sample-repos.json");
  // A payload from a build that predates tap-to-open. The row must degrade to
  // the plain, unclickable row it used to be rather than throwing on click and
  // taking the panel's rendering down with it.
  repos.rows = repos.rows.map(({ url, linkLabel, ...rest }) => rest);
  await gotoWithFixtures(page, baseURL, { repos });

  const row = repoRows(page).first();
  await expect(row).toHaveCount(1);
  await expect(row).not.toHaveAttribute("role", "link");
  await row.click();
  expect(
    await page.evaluate(() => window.__CALLS__.filter((c) => c.command === "plugin:opener|open_url"))
  ).toEqual([]);
});

// MARK: - GitHub Runners

test("the Runners panel paints Rust's stats, chips and trailing label", async ({ page, baseURL }) => {
  const { runners } = await gotoWithFixtures(page, baseURL);
  await expect(page.locator("#runnersPanel")).toBeVisible();

  await expect(page.locator("#runnersTitle")).toHaveText(runners.title);
  await expect(page.locator("#runnersTrailing")).toHaveText(runners.trailing);
  expect(runners.trailing, "the fixture must exercise the missing count").toContain("missing");

  const stats = page.locator(".gh-stat");
  await expect(stats).toHaveCount(runners.stats.length);
  for (const [index, stat] of runners.stats.entries()) {
    await expect(stats.nth(index).locator(".lbl")).toHaveText(stat.label);
    await expect(stats.nth(index).locator(".gh-stat-value")).toHaveText(stat.value);
    await expect(stats.nth(index).locator(".gh-stat-value")).toHaveCSS("color", rgb(stat.color));
  }

  const chips = await page.locator(".gh-chip").allTextContents();
  expect(chips).toEqual(runners.chips);
});

test("registered and absent runner rows carry Rust's state words and colours", async ({ page, baseURL }) => {
  const { runners } = await gotoWithFixtures(page, baseURL);
  await expect(runnerRows(page)).toHaveCount(runners.rows.length);

  // The order is Rust's: macOS before Linux, digit-aware by name within — and
  // an absent runner holds the exact slot it occupied while registered.
  const names = await runnerRows(page).locator(".gh-runner-name").allTextContents();
  expect(names).toEqual(runners.rows.map((r) => r.name));

  for (const [index, expected] of runners.rows.entries()) {
    const row = runnerRows(page).nth(index);
    await expect(row).toHaveAttribute("data-kind", expected.kind);
    await expect(row.locator(".gh-runner-os")).toHaveText(expected.os);
    await expect(row.locator(".gh-runner-status")).toHaveText(expected.status);
    await expect(row.locator(".dot")).toHaveCSS("background-color", rgb(expected.dotColor));
    await expect(row.locator(".gh-runner-status")).toHaveCSS("color", rgb(expected.statusColor));
  }

  // The presence semantics the roster exists for: amber while recycling, red
  // once absence passes grace.
  const recycling = runners.rows.find((r) => r.status.startsWith("recycling"));
  const missing = runners.rows.find((r) => r.status.startsWith("missing"));
  expect(recycling.kind).toBe("absent");
  expect(missing.kind).toBe("absent");
  expect(recycling.dotColor).not.toBe(missing.dotColor);
});

test("a healthy Runners panel shows no footer", async ({ page, baseURL }) => {
  const { runners } = await gotoWithFixtures(page, baseURL);
  expect(runners.footer).toBeNull();
  await expect(page.locator("#runnersFooter")).toBeHidden();
});

/**
 * The clock-freeze contract from the DOM's side: a failed fetch keeps every
 * last-good row on screen and adds a footer. Blanking the panel here would
 * undo the retention Rust deliberately implements.
 */
test("a failed fetch keeps the last-good rows and surfaces the error in the footer", async ({ page, baseURL }) => {
  const good = await fixture(baseURL, "sample-runners.json");
  const failing = {
    ...good,
    footer: {
      text: "⚠ couldn't read runners — token needs org self-hosted runners (read) · last ok 4m ago",
      color: "#e09a26",
    },
  };
  await gotoWithFixtures(page, baseURL, { runners: failing });

  await expect(runnerRows(page)).toHaveCount(good.rows.length);
  const footer = page.locator("#runnersFooter");
  await expect(footer).toBeVisible();
  await expect(footer).toHaveText(failing.footer.text);
  await expect(footer).toHaveCSS("color", rgb(failing.footer.color));
  await expect(page.locator("#runnersBody .gh-message")).toHaveCount(0);
});

test("with no token the Runners panel is one sentence, no stats and no footer", async ({ page, baseURL }) => {
  const empty = await fixture(baseURL, "sample-runners-empty.json");
  await gotoWithFixtures(page, baseURL, { runners: empty });

  await expect(page.locator("#runnersBody .gh-message")).toHaveText(empty.message.text);
  expect(empty.message.text).toBe("connect a GitHub token in Settings");
  await expect(runnerRows(page)).toHaveCount(0);
  await expect(page.locator(".gh-stat")).toHaveCount(0);
  await expect(page.locator(".gh-chip")).toHaveCount(0);
  // Nothing has been fetched, so there is nothing to be stale.
  await expect(page.locator("#runnersFooter")).toBeHidden();
  await expect(page.locator("#runnersTrailing")).toHaveText("");
});

// MARK: - Injection

test("repo and runner names reach the DOM as text, never as markup", async ({ page, baseURL }) => {
  const repos = await fixture(baseURL, "sample-repos.json");
  const runners = await fixture(baseURL, "sample-runners.json");
  // These names come from the GitHub API, and in Tauri the DOM can call
  // `invoke` — an unescaped `<img onerror=...>` would reach the command
  // surface. Building with textContent means markup cannot reach the DOM at
  // all, which is stronger than escaping it.
  const hostile = '<img src=x onerror="window.__PWNED__=1">';
  repos.rows = [{ ...repos.rows[0], name: hostile }];
  runners.rows = [{ ...runners.rows[0], name: hostile }];
  await gotoWithFixtures(page, baseURL, { repos, runners });

  await expect(repoRows(page).first().locator(".gh-repo-name")).toHaveText(hostile);
  await expect(runnerRows(page).first().locator(".gh-runner-name")).toHaveText(hostile);
  await expect(page.locator("img")).toHaveCount(0);
  expect(await page.evaluate(() => window.__PWNED__)).toBeUndefined();
});
