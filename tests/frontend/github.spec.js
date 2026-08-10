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
  // `.first()`: a wide panel renders one header per column, identical by
  // construction — asserting the first proves the contract.
  const header = page.locator("#reposBody .gh-head").first();
  await expect(header.locator(".gh-repo-name")).toHaveText(repos.columns[0].label);
  await expect(header.locator(".gh-repo-name")).toHaveCSS(
    "width",
    `${repos.columns[0].width}px`
  );
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

  // Addressed by repo, not by position: rows are sorted by short name, so
  // `.first()` silently follows any rename of any *other* fixture repo, and
  // the literal below then fails for a reason that has nothing to do with
  // what this test is about.
  const i = repos.rows.findIndex((r) => r.repo === "acme/widget");
  expect(i, "the fixture must contain the repo this test addresses").toBeGreaterThanOrEqual(0);

  await repoRows(page).nth(i).click();

  const opens = await page.evaluate(() =>
    window.__CALLS__.filter((c) => c.command === "plugin:opener|open_url")
  );
  // Exactly one call, exactly one argument, and it is the fixture's own
  // string. A URL rebuilt in JS from the repo name would pass a "it opened
  // something" assertion and still be a second author of the only string the
  // granted ACL scope accepts.
  expect(opens).toEqual([
    { command: "plugin:opener|open_url", args: { url: repos.rows[i].url } },
  ]);
  // Written out, never interpolated -- interpolating would be the very
  // rebuild the comment above rules out.
  expect(repos.rows[i].url).toBe("https://github.com/acme/widget/actions");
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

  // The rollup lives in the trailing above, so no stat repeats it — OFFLINE is
  // the count `3/4` cannot be read off at a glance.
  expect(runners.stats.map((s) => s.label)).toEqual(["BUSY", "IDLE", "OFFLINE"]);
  const stats = page.locator(".gh-stat");
  await expect(stats).toHaveCount(runners.stats.length);
  for (const [index, stat] of runners.stats.entries()) {
    await expect(stats.nth(index).locator(".lbl")).toHaveText(stat.label);
    await expect(stats.nth(index).locator(".gh-stat-value")).toHaveText(stat.value);
    await expect(stats.nth(index).locator(".gh-stat-value")).toHaveCSS("color", rgb(stat.color));
  }

  // Every chip is a string Rust built, in Rust's order — nothing the renderer
  // composed from an OS name and a pair of counts of its own. `Windows 0/0` is
  // the load-bearing entry: this org has no Windows runner, so a chip the
  // frontend only drew when non-zero would fail here.
  expect(runners.chips, "the fixture must exercise an empty tracked platform")
    .toContain("Windows 0/0");
  await expect(page.locator(".gh-chip")).toHaveText(runners.chips);
});

/**
 * The stats and the chips used to be two stacked rows, which cost a glanceable
 * panel a whole line for one short row of pills. Asserted with bounding boxes
 * and derived from the panel's own geometry rather than pixel constants: the
 * Runners panel's rendered width moves with the layout, and only "same line"
 * and "flush right" are the contract.
 */
test("the Runners header keeps the stats and the chips on one line", async ({ page, baseURL }) => {
  await gotoWithFixtures(page, baseURL);
  await expect(page.locator("#runnersPanel")).toHaveAttribute("data-cols", "2");

  const header = page.locator("#runnersBody .gh-header");
  await expect(header).toHaveCount(1);
  const stats = await header.locator(".gh-stats").boundingBox();
  const chips = await header.locator(".gh-chips").boundingBox();

  // Shared line: the chips' whole vertical extent falls inside the stats'.
  const overlap =
    Math.min(stats.y + stats.height, chips.y + chips.height) - Math.max(stats.y, chips.y);
  expect(
    Math.round(overlap),
    `stats ${JSON.stringify(stats)} vs chips ${JSON.stringify(chips)}`
  ).toBe(Math.round(chips.height));

  // Right-flush against the panel body, and clear of the stats beside them.
  const body = await page.locator("#runnersBody").boundingBox();
  expect(Math.round(chips.x + chips.width)).toBe(Math.round(body.x + body.width));
  expect(chips.x).toBeGreaterThan(stats.x);

  // And the header really is one line, not a tall box holding two.
  const box = await header.boundingBox();
  expect(Math.round(box.height)).toBe(Math.round(stats.height));
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

/**
 * The status column is a reservation, not a measurement (#206).
 *
 * A presence label ("recycling 40s") is nearly three times the width of a
 * state word ("idle"), so a column that sizes itself to its own text drags the
 * OS chip left on exactly the rows something is wrong with — the panel loses
 * its grid at the moment it is being read hardest. Rust hands every row the
 * same `statusWidth`, so what the roster is doing cannot move a column.
 */
test("the OS column holds one position across every runner state", async ({ page, baseURL }) => {
  const runners = await fixture(baseURL, "sample-runners.json");
  const words = [...new Set(runners.rows.map((r) => r.status.split(" ")[0]))].sort();
  expect(words, "the fixture must mix the steady states with both presence labels").toEqual([
    "busy",
    "idle",
    "missing",
    "offline",
    "recycling",
  ]);

  await gotoWithFixtures(page, baseURL, { runners });
  // The narrowest the cockpit ever hands this panel: `PanelKind::GhRunners`'s
  // own 400pt min_width. Aligned there, aligned at every width above it.
  //
  // 400pt is also below the 816 a second column needs, which is what makes the
  // assertion below legitimate: one shared x across ALL rows only holds while
  // they share one column.
  await page.locator("#runnersPanel").evaluate((el) => {
    el.style.width = "400px";
    el.style.setProperty("--panel-cols", "1");
  });

  const rows = runnerRows(page);
  await expect(rows).toHaveCount(runners.rows.length);
  const chips = [];
  for (let i = 0; i < runners.rows.length; i++) {
    chips.push(await rows.nth(i).locator(".gh-runner-os").boundingBox());
  }
  // Every OS label is five characters of one monospace font, so a shared left
  // edge and a shared right edge are the same claim made twice. Both are
  // asserted because the reservation is what guarantees the right edge, and
  // the left edge is what the eye actually reads as a column.
  expect(new Set(chips.map((b) => b.x.toFixed(2))).size, "one x-position").toBe(1);
  expect(new Set(chips.map((b) => (b.x + b.width).toFixed(2))).size, "one right edge").toBe(1);
});

test("the widest presence label fits the reserved slot without truncating", async ({ page, baseURL }) => {
  const { runners } = await gotoWithFixtures(page, baseURL);
  await page.locator("#runnersPanel").evaluate((el) => {
    el.style.width = "400px";
  });

  const rows = runnerRows(page);
  for (const [index, expected] of runners.rows.entries()) {
    const status = rows.nth(index).locator(".gh-runner-status");
    // Rust's figure reaches the element as a width, not as a minimum.
    await expect(status).toHaveCSS("width", `${expected.statusWidth}px`);
    // `overflow:hidden` means an outgrown reservation clips silently instead
    // of shifting the row, so the panel would look right and read wrong —
    // this is the assertion that would catch it.
    const [scroll, client] = await status.evaluate((el) => [el.scrollWidth, el.clientWidth]);
    expect(scroll, `"${expected.status}" is clipped by its reserved slot`).toBeLessThanOrEqual(client);
  }
});

test("a healthy Runners panel shows no warning", async ({ page, baseURL }) => {
  const { runners } = await gotoWithFixtures(page, baseURL);
  expect(runners.footer).toBeNull();
  await expect(page.locator("#runnersStale")).toBeHidden();
});

/**
 * The clock-freeze contract from the DOM's side: a failed fetch keeps every
 * last-good row on screen and adds a warning. Blanking the panel here would
 * undo the retention Rust deliberately implements.
 */
test("a failed fetch keeps the last-good rows and surfaces the error in the header", async ({ page, baseURL }) => {
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
  const stale = page.locator("#runnersStale");
  await expect(stale).toBeVisible();
  await expect(stale).toHaveText(failing.footer.text);
  await expect(stale).toHaveCSS("color", rgb(failing.footer.color));
  // Ellipsised rather than wrapped in a narrow panel, so the whole message has
  // to stay reachable somewhere.
  await expect(stale).toHaveAttribute("title", failing.footer.text);
  await expect(page.locator("#runnersBody .gh-message")).toHaveCount(0);
});

/**
 * The reason the warning moved into the header at all. It used to be a `<p>`
 * after `.panel-body`, so a panel grew a line the moment it degraded — and
 * because `.panel-row` stretches every card in a row to the tallest, Repos
 * beside it grew too. A token losing a scope moved half the cockpit.
 */
test("a Runners error does not make the card taller", async ({ page, baseURL }) => {
  const good = await fixture(baseURL, "sample-runners.json");
  await gotoWithFixtures(page, baseURL, { runners: good });
  const healthy = await page.locator("#runnersPanel").boundingBox();

  const failing = {
    ...good,
    footer: {
      text: "⚠ couldn't read runners — token needs org self-hosted runners (read) · last ok 4m ago",
      color: "#e09a26",
    },
  };
  await gotoWithFixtures(page, baseURL, { runners: failing });
  await expect(page.locator("#runnersStale")).toBeVisible();
  const degraded = await page.locator("#runnersPanel").boundingBox();

  expect(degraded.height).toBeCloseTo(healthy.height, 1);

  // Sharing the header means competing for it, and the first cut of this rule
  // copied the host card's `flex-shrink:100` — which is on the *title* there,
  // because a CPU model is the disposable half of that line. Here the title is
  // the panel's name, and the copy ate "GitHub Runners" down to zero the moment
  // the warning appeared. The message is what ellipsises now, not the label.
  const title = page.locator("#runnersTitle");
  await expect(title).toHaveText("GitHub Runners");
  const [titled, staled] = await Promise.all([title.boundingBox(), page.locator("#runnersStale").boundingBox()]);
  expect(titled.width, "the panel keeps its name when it degrades").toBeGreaterThan(0);
  expect(
    await title.evaluate((el) => el.scrollWidth <= Math.ceil(el.getBoundingClientRect().width)),
    "the title is not the one being truncated"
  ).toBe(true);
  // …and the warning sits beside the label rather than out at the far edge.
  expect(staled.x).toBeGreaterThan(titled.x);
});

/**
 * The reported bug, from the DOM's side: at launch Rust has not read the
 * credential store yet, and both panels used to render "connect a GitHub token
 * in Settings" at an operator whose token was fine. The payload now says it is
 * loading, and the panel paints that.
 */
test("a loading payload paints the loading line, not the connect-a-token one", async ({ page, baseURL }) => {
  const repos = { ...(await fixture(baseURL, "sample-repos.json")), message: { text: "loading…" }, rows: [], loading: true };
  const runners = { ...(await fixture(baseURL, "sample-runners.json")), message: { text: "loading runners…" }, rows: [], loading: true };
  await gotoWithFixtures(page, baseURL, { repos, runners });

  await expect(page.locator("#reposBody .gh-message")).toHaveText("loading…");
  await expect(page.locator("#runnersBody .gh-message")).toHaveText("loading runners…");
  for (const text of await page.locator(".gh-message").allTextContents()) {
    expect(text, "a panel that has not looked may not send anyone to Settings").not.toContain("Settings");
  }
});

/**
 * …and having fixed the message, the cockpit must not then sit on it. Each
 * panel's own timer is 10s with its first tick at load, so a correct "loading…"
 * would still outlive the data by up to ten seconds. `payload.loading` is what
 * tightens the cadence, and dropping it is what relaxes it again.
 */
test("a loading panel is re-asked promptly, and settles once it is not", async ({ page, baseURL }) => {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  const loading = { ...(await fixture(baseURL, "sample-runners.json")), message: { text: "loading runners…" }, rows: [], loading: true };
  const settled = await fixture(baseURL, "sample-runners.json");

  // Answers `loading` until the fourth ask, then the real payload — and counts
  // every call so the cadence itself can be asserted rather than inferred.
  await page.addInitScript(
    (vms) => {
      window.__RUNNER_CALLS__ = [];
      window.__TAURI__ = {
        core: {
          invoke: async (command) => {
            if (command === "cockpit") return vms.cockpit;
            if (command === "repos") return null;
            if (command !== "runners") return null;
            window.__RUNNER_CALLS__.push(Date.now());
            return window.__RUNNER_CALLS__.length > 3 ? vms.settled : vms.loading;
          },
        },
      };
    },
    { cockpit, loading, settled }
  );
  await page.goto("/index.html");

  // Four asks inside a couple of seconds is only reachable on the fast cadence:
  // at the settled 10s the fourth would be half a minute away.
  await expect
    .poll(() => page.evaluate(() => window.__RUNNER_CALLS__.length), { timeout: 6000 })
    .toBeGreaterThanOrEqual(4);
  await expect(page.locator("#runnersBody .gh-row").first()).toBeVisible();

  const gaps = await page.evaluate(() =>
    window.__RUNNER_CALLS__.slice(1).map((t, i) => t - window.__RUNNER_CALLS__[i])
  );
  for (const gap of gaps) {
    expect(gap, `polls while loading were ${gaps.join("/")}ms apart`).toBeLessThan(5000);
  }

  // …and once settled it backs off, or the cockpit would poll every second
  // forever.
  const settledCount = await page.evaluate(() => window.__RUNNER_CALLS__.length);
  await page.waitForTimeout(2500);
  expect(await page.evaluate(() => window.__RUNNER_CALLS__.length)).toBeLessThanOrEqual(settledCount + 1);
});

test("with no token the Runners panel is one sentence, no stats and no warning", async ({ page, baseURL }) => {
  const empty = await fixture(baseURL, "sample-runners-empty.json");
  await gotoWithFixtures(page, baseURL, { runners: empty });

  await expect(page.locator("#runnersBody .gh-message")).toHaveText(empty.message.text);
  expect(empty.message.text).toBe("connect a GitHub token in Settings");
  await expect(runnerRows(page)).toHaveCount(0);
  await expect(page.locator(".gh-stat")).toHaveCount(0);
  await expect(page.locator(".gh-chip")).toHaveCount(0);
  // Nothing has been fetched, so there is nothing to be stale.
  await expect(page.locator("#runnersStale")).toBeHidden();
  await expect(page.locator("#runnersTrailing")).toHaveText("");
});

// MARK: - GitHub availability (the conjunction chip)

/** The chip as Rust builds it, for a fabricated matrix row. */
const chip = (label, color, detail = "why") => ({ label, color, detail });

/** Both panels' payloads carrying the same verdict. */
async function gotoWithVerdict(page, baseURL, availability) {
  const repos = { ...(await fixture(baseURL, "sample-repos.json")), availability };
  const runners = { ...(await fixture(baseURL, "sample-runners.json")), availability };
  return gotoWithFixtures(page, baseURL, { repos, runners });
}

test("both panels paint the verdict beside their own title", async ({ page, baseURL }) => {
  // One shared element would be orphaned when reflow splits the two panels
  // onto separate rows, which is why the verdict travels on both payloads.
  const { repos } = await gotoWithVerdict(page, baseURL, chip("Operational", "#1c6b41"));
  for (const [chipId, titleId] of [
    ["#reposAvailability", "#reposTitle"],
    ["#runnersAvailability", "#runnersTitle"],
  ]) {
    await expect(page.locator(chipId)).toHaveText(repos.availability.label);
    await expect(page.locator(chipId)).toHaveCSS("color", rgb(repos.availability.color));
    const [c, t] = await Promise.all([
      page.locator(chipId).boundingBox(),
      page.locator(titleId).boundingBox(),
    ]);
    expect(c.x, `${chipId} sits beside ${titleId}`).toBeGreaterThan(t.x);
  }
});

test("every verdict paints Rust's label and colour", async ({ page, baseURL }) => {
  // Every string and colour here is Rust's; this asserts only that the frontend
  // applies what it is given. Amber is "GitHub is slow", red is "runs are
  // failing" or "it's ours" — the label is what tells those two apart.
  for (const [label, color] of [
    ["Operational", "#1c6b41"], // operational + fleet online
    ["Services Degraded", "#e09a26"], // degraded_performance / partial_outage
    ["Major Outage", "#e05a4f"], // major_outage — red, not amber
    ["Fleet Down", "#e05a4f"], // operational + fleet dark -> it's us
    ["Status Unknown", "#5a6b60"], // statuspage unreadable
  ]) {
    await gotoWithVerdict(page, baseURL, chip(label, color));
    await expect(page.locator("#runnersAvailability")).toHaveText(label);
    await expect(page.locator("#runnersAvailability")).toHaveCSS("color", rgb(color));
  }
});

test("an unreachable status page reads as unknown and keeps the fleet rows", async ({ page, baseURL }) => {
  const good = await fixture(baseURL, "sample-runners.json");
  await gotoWithVerdict(page, baseURL, chip("Status Unknown", "#5a6b60", "Couldn't read GitHub's status page."));
  await expect(page.locator("#runnersAvailability")).toHaveText("Status Unknown");
  await expect(page.locator("#runnersAvailability")).toHaveCSS("color", rgb("#5a6b60"));
  // Never green, and never at the cost of the reading it annotates.
  await expect(page.locator("#runnersAvailability")).not.toHaveCSS("color", rgb("#1c6b41"));
  await expect(runnerRows(page)).toHaveCount(good.rows.length);
});

test("the incident detail is reachable on hover, as text and never as markup", async ({ page, baseURL }) => {
  // Statuspage incident bodies carry raw HTML (`<br />`), and this is the one
  // string on the panel sourced from someone else's CMS.
  const detail = 'GitHub Actions: major outage. Incident: <img src=x onerror="alert(1)"> (critical).';
  await gotoWithVerdict(page, baseURL, chip("Major Outage", "#e05a4f", detail));
  await expect(page.locator("#runnersAvailability")).toHaveAttribute("title", detail);
  expect(await page.locator("#runnersAvailability").evaluate((el) => el.querySelector("img"))).toBeNull();
});

test("a payload with no verdict hides the chip rather than emptying it", async ({ page, baseURL }) => {
  await gotoWithVerdict(page, baseURL, null);
  await expect(page.locator("#runnersAvailability")).toBeHidden();
  await expect(page.locator("#reposAvailability")).toBeHidden();
});

/**
 * The chip shares the header with a title that must survive and a staleness
 * warning that is the designated give. It is short and fixed, so it takes
 * `flex:none` — a verdict ellipsised to "GH…" would answer nothing — and it
 * must not cost the card any height.
 */
test("the verdict costs no height and does not truncate", async ({ page, baseURL }) => {
  await gotoWithVerdict(page, baseURL, null);
  const bare = await page.locator("#runnersPanel").boundingBox();

  await gotoWithVerdict(page, baseURL, chip("Fleet Down", "#e05a4f"));
  const withChip = page.locator("#runnersAvailability");
  await expect(withChip).toBeVisible();
  expect((await page.locator("#runnersPanel").boundingBox()).height).toBeCloseTo(bare.height, 1);
  expect(
    await withChip.evaluate((el) => el.scrollWidth <= Math.ceil(el.getBoundingClientRect().width)),
    "the verdict is never the element that ellipsises"
  ).toBe(true);
  await expect(page.locator("#runnersTitle")).toHaveText("GitHub Runners");
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

// MARK: - two-column content

test("a wide panel splits Repos into two tables and the runner list into two columns", async ({ page, baseURL }) => {
  // sample-cockpit.json is dumped at 2732pt, so its two-panel rows are 1358pt
  // each — past both breakpoints (Repos needs 1136, Runners 816).
  const { repos, runners } = await gotoWithFixtures(page, baseURL);
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  const cols = (id) =>
    cockpit.panelRows.flat().find((p) => p.id === id).columns;
  expect(cols("ghWorkflows"), "fixture must be wide enough for 2").toBe(2);
  expect(cols("ghRunners")).toBe(2);

  // Rust's number reaches the DOM as the custom property the CSS reads.
  await expect(page.locator("#reposPanel")).toHaveAttribute("data-cols", "2");
  await expect(page.locator("#runnersPanel")).toHaveAttribute("data-cols", "2");

  // Repos: two tables, each with its own header, and every repo rendered once.
  await expect(page.locator("#reposBody .gh-col")).toHaveCount(2);
  await expect(page.locator("#reposBody .gh-head")).toHaveCount(2);
  await expect(repoRows(page)).toHaveCount(repos.rows.length);
  // Column-major: the first column holds the first half, in payload order.
  const firstColumn = await page
    .locator("#reposBody .gh-col")
    .first()
    .locator(".gh-row:not(.gh-head) .gh-repo-name")
    .allTextContents();
  expect(firstColumn).toEqual(
    repos.rows.slice(0, Math.ceil(repos.rows.length / 2)).map((r) => r.name)
  );

  // Runners: one list, two CSS columns — balancing keeps the reading order.
  await expect(runnerRows(page)).toHaveCount(runners.rows.length);
  expect(
    await page.locator("#runnersBody .gh-list").evaluate((el) =>
      getComputedStyle(el).columnCount
    )
  ).toBe("2");
});

test("a narrow panel keeps every list in one column", async ({ page, baseURL }) => {
  // Same panels, same rows — only the width Rust computed them for differs.
  const narrow = await fixture(baseURL, "sample-cockpit-narrow.json");
  expect(narrow.panelRows.flat().find((p) => p.id === "ghRunners").columns).toBe(1);
  const repos = await fixture(baseURL, "sample-repos.json");
  const runners = await fixture(baseURL, "sample-runners.json");
  await stubIpc(page, { cockpit: narrow, repos, runners });
  await page.goto("/index.html");

  await expect(page.locator("#reposPanel")).toHaveAttribute("data-cols", "1");
  await expect(page.locator("#reposBody .gh-col")).toHaveCount(1);
  await expect(page.locator("#reposBody .gh-head")).toHaveCount(1);
  await expect(repoRows(page)).toHaveCount(repos.rows.length);
  expect(
    await page.locator("#runnersBody .gh-list").evaluate((el) =>
      getComputedStyle(el).columnCount
    )
  ).toBe("1");
});

test("the repo name column is a reservation, so the numeric block never moves", async ({ page, baseURL }) => {
  // #206's rule, now applied to REPO as well: a name that grew to its own text
  // would drag all seven numeric columns right on exactly the longest row.
  const { repos } = await gotoWithFixtures(page, baseURL);
  const reserved = repos.columns[0].width;
  expect(reserved, "REPO carries a width, not null").toBeGreaterThan(0);
  await expect(repoRows(page).first().locator(".gh-repo-name")).toHaveCSS(
    "width",
    `${reserved}px`
  );
  // Every row's ISSUES cell starts at the same x within its column — header
  // included, which is the alignment the panel exists for. (`.gh-cell` is a
  // class on a span, so `:first-of-type` would never match it; take the first
  // cell per row instead.)
  const xs = await page
    .locator("#reposBody .gh-col")
    .first()
    .locator(".gh-row")
    .evaluateAll((rows) =>
      rows.map((row) => Math.round(row.querySelector(".gh-cell").getBoundingClientRect().x))
    );
  expect(xs.length, "one ISSUES cell per row").toBeGreaterThan(1);
  expect(new Set(xs).size, `ISSUES column x positions ${xs}`).toBe(1);
});
