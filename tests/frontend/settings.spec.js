import { test, expect } from "@playwright/test";
import { stubTileIpc, openTileFull, SOURCE_READS } from "./tile-full.js";

// Same CSP guard as layout.spec.js: the page is served under the app's real
// policy (csp_server.py), and a blocked style or script surfaces as a console
// error rather than a thrown exception. The Settings surface builds its whole
// DOM at runtime, which is exactly the shape of code that reaches for an
// inline `style=""` and silently loses it under `style-src 'self'`.
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

// The overview's toolbar button: the only way into Settings (#647).
const SETTINGS_BUTTON = '.db-chrome [data-action="settings"]';

const TEST_RESULT = "✓ ubu-01 · agent v0.4.0";

/**
 * Stubs the whole IPC surface with the Rust-dumped payloads (there is no real
 * Tauri IPC in a browser context), and records every call so a test can assert
 * *which* command a control invoked and with what — the part a screenshot
 * can't check.
 *
 * Every mutation answers in the one shape the commands really use
 * (`{status, settings}`), so the frontend's "re-render from what was
 * persisted" contract is exercised rather than mocked away.
 */
async function stubIpc(page, cockpit, settings, probe, updates, discover, search) {
  // Discovery and search answer from Node, so a test can delay one reply or
  // shape it from the arguments. `exposeFunction` rather than a stringified
  // callback: the page runs under the app's CSP, which refuses eval.
  // A binding registers once per page, and some tests stub twice; each call
  // swaps the callbacks behind it instead.
  if (!page.repoStubs) {
    page.repoStubs = {};
    await page.exposeFunction("__discoverRepos__", (args) => page.repoStubs.discover(args));
    await page.exposeFunction("__searchRepos__", (args) => page.repoStubs.search(args));
  }
  page.repoStubs.discover = typeof discover === "function" ? discover : defaultDiscovery(settings, discover);
  page.repoStubs.search = search || searchOver(settings);
  await page.addInitScript(
    ({ cockpit, settings, testResult, probe, updates, discover }) => {
      window.__CALLS__ = [];
      window.__TAURI__ = {
        core: {
          invoke: async (command, args) => {
            window.__CALLS__.push({ command, args });
            if (command === "cockpit") return cockpit;
            // The overview the page boots on, and the button that opens Settings.
            if (command === "dashboard_view") return (await fetch("/sample-dashboard.json")).json();
            if (command === "settings_view") return settings;
            if (command === "settings_test_host") return { id: args.id, result: testResult };
            // The probe answers in its own shape, not `{status, settings}`: a
            // finding is not a mutation, and the tab renders it without one.
            if (command === "settings_probe_status_vendor") return probe;
            // A whole source at once, and the one answer that says whether it
            // was applied. A test that wants a refusal sets this.
            if (command === "settings_save_source") {
              return window.__SAVE_SOURCE__ || { status: `${args.kind === "anthropic" ? "Anthropic" : "Source"} saved.`, settings, saved: true };
            }
            // The discovery probe likewise — ephemeral, never persisted —
            // and the search over what it found.
            if (command === "settings_discover_repos") return window.__discoverRepos__(args);
            if (command === "settings_search_repos") return window.__searchRepos__(args);
            // The three updater commands answer in the About tab's own
            // `about.updates` shape — not `{status, settings}` — because an
            // update check is not a settings mutation and nothing is
            // persisted. A test that wants a different state passes one.
            if (command.startsWith("update_")) return window.__UPDATES__ || updates;
            return { status: "Saved.", settings };
          },
        },
      };
    },
    {
      cockpit,
      settings,
      testResult: TEST_RESULT,
      probe: probe || null,
      updates: updates || settings.about.updates,
    }
  );
}

/** What a token at a large organization's scale reads: 900 made-up services, five
 *  `claims-*` repos, one archived, plus the fixture's own tracked repos.
 *  Names are invented; no real organization's repos belong in this repo. */
const GRANTED = [
  ...Array.from({ length: 900 }, (_, i) => `acme/svc-${String(i + 1).padStart(3, "0")}`),
  "acme/claims-api", "acme/claims-batch", "acme/claims-docs", "acme/claims-etl", "acme/claims-web",
  "acme/legacy-vault", "acme/gadget", "acme/widget",
];
const ARCHIVED = new Set(["acme/legacy-vault"]);
/** Watched by the fixture's other account, so its row names that account. */
const FOREIGN = { "acme/claims-web": "personal" };
const untrackPrompt = (slug) => `Stop tracking ${slug}? Its watched workflows (release.yml) are forgotten.`;

/** The summary `settings_discover_repos` answers with, for whichever account
 *  asked. A test passes its own object (a failure's `reason`, orgs) instead. */
const defaultDiscovery = (settings, given) => async (args) =>
  given || {
    id: args.id,
    total: GRANTED.length,
    totalLabel: `This token can read ${GRANTED.length} repos`,
    truncatedNote: null,
    orgs: [],
    reason: null,
  };

/** A stand-in for Rust's `search_answer` at this scale: substring matches,
 *  this account's tracked first, at most 50, with the count, the overflow and
 *  Add by name. Matching itself is tested in Rust; this only has to answer. */
const searchOver = (settings) => async ({ id, query }) => {
  const account = settings.accounts.rows.find((row) => row.id === id);
  const mine = new Set((account?.repos || []).map((repo) => repo.slug.toLowerCase()));
  const q = query.trim().toLowerCase();
  if (!q) return { id, query, results: [], countLabel: null, moreLabel: null, addByName: null };
  const rows = GRANTED.filter((slug) => slug.toLowerCase().includes(q))
    .map((slug) => {
      const start = slug.toLowerCase().indexOf(q);
      const tracked = mine.has(slug.toLowerCase());
      return {
        slug, tracked, trackedBy: FOREIGN[slug] || null, granted: true, archived: ARCHIVED.has(slug),
        untrackPrompt: tracked ? untrackPrompt(slug) : null, matchStart: start, matchEnd: start + q.length,
      };
    })
    .sort((a, b) => (a.tracked === b.tracked ? a.slug.localeCompare(b.slug) : a.tracked ? -1 : 1));
  const listed = GRANTED.some((slug) => slug.toLowerCase() === q);
  const slug = query.trim();
  return {
    id, query,
    results: rows.slice(0, 50),
    countLabel: `${rows.length} of ${GRANTED.length} match`,
    moreLabel: rows.length > 50 ? `${rows.length - 50} more match — keep typing` : null,
    addByName: q.includes("/") && !listed && !mine.has(q) ? { label: `Add ${slug} by name`, slug } : null,
  };
};

const calls = (page, command) =>
  page.evaluate((c) => window.__CALLS__.filter((call) => call.command === c), command);

/** Loads the app with the IPC stub installed and opens Settings. */
async function openSettings(page, baseURL, probe, updates, discover, edit, search) {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  const settings = await fixture(baseURL, "sample-settings.json");
  edit?.(settings);
  await stubIpc(page, cockpit, settings, probe, updates, discover, search);
  await page.goto("/index.html");
  await expect(page.locator(SETTINGS_BUTTON)).toBeVisible();
  await page.locator(SETTINGS_BUTTON).click();
  await expect(page.locator("#settings")).toBeVisible();
  return settings;
}

async function openConnection(page, id) {
  await page.locator('.tab[data-tab="connections"]').click();
  await page.locator(`[data-connection="${id}"]`).click();
}

/** An account's card: its row opens the steps, and the steps lead to the card. */
async function openAccountCard(page, id) {
  await openConnection(page, `account:${id}`);
  await page.locator(".walk-card").click();
}

async function addConnection(page, kind) {
  await page.locator('.tab[data-tab="connections"]').click();
  await page.locator('.connection-add').click();
  await page.locator(`.connection-catalog [data-kind="${kind}"]`).click();
}

// Exercise the visible route into the editor, including catalog-only providers.
const tab = (page, id) => ({ click: async () => {
  if (["connections", "general", "about"].includes(id)) {
    await page.locator(`.tab[data-tab="${id}"]`).click();
    return;
  }
  if (id === "services") { await addConnection(page, "vendor"); return; }
  const kind = { accounts: "account", hosts: "host", usage: "sentry" }[id] || id;
  await page.locator('.tab[data-tab="connections"]').click();
  const row = page.locator(`.connection-row[data-kind="${kind}"]`).first();
  if (await row.count()) {
    await row.click();
    // An account's row opens its steps; its card is one button away.
    if (kind === "account") await page.locator(".walk-card").click();
  } else await addConnection(page, kind);
} });

test("Sources is the landing page and each saved source has one editor", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await expect(page.locator('#settingsTabs [data-tab="connections"]')).toHaveAttribute("aria-current", "page");
  await expect(page.locator(".connection-row")).toHaveCount(settings.connections.rows.length);
  for (const entry of settings.connections.rows) {
    const row = page.locator(`.connection-row[data-connection="${entry.id}"]`);
    await expect(row.locator(".connection-title")).toHaveText(entry.title);
    await expect(row.locator(".connection-feeds")).toHaveText(entry.feeds);
    await expect(row.locator(".connection-state")).toHaveText(entry.status.text);
  }
  await tab(page, "sentry").click();
  await expect(page.locator(".walk")).toBeVisible();
  await expect(page.locator(".walk-pick")).toHaveCount(2);
  await expect(page.locator("#neon-org-id")).toHaveCount(0);
  await expect(page.locator("#walk-secret")).toHaveCount(0);
  expect(await calls(page, "settings_save_secret")).toEqual([]);
});

test("leaving or closing an editor protects typed fields and explicitly discards secrets", async ({ page, baseURL }) => {
  await openSettings(page, baseURL);
  await tab(page, "usage").click();
  await next(page);
  await page.locator("#walk-orgSlug").fill("draft-org");
  await page.locator("#walk-secret").fill("draft-token");
  await page.locator('#settingsTabs [data-tab="general"]').click();
  await expect(page.locator("#settingsConfirm")).toBeVisible();
  await page.locator("#settingsConfirm .primary").click();
  await expect(page.locator("#walk-orgSlug")).toHaveValue("draft-org");
  await expect(page.locator("#walk-secret")).toHaveValue("draft-token");
  await page.locator("#settingsClose").click();
  await expect(page.locator("#settingsConfirm")).toBeVisible();
  await page.locator("#settingsConfirm .discard").click();
  await expect(page.locator("#settings")).toBeHidden();
  await expect(page.locator('#settings input[type="password"]')).toHaveCount(0);
  await page.locator(SETTINGS_BUTTON).click();
  await tab(page, "usage").click();
  await next(page);
  await expect(page.locator("#walk-secret")).toHaveValue("");
  await expect(page.locator("#walk-orgSlug")).not.toHaveValue("draft-org");
  expect(await calls(page, "settings_save_secret")).toEqual([]);
  expect(await saves(page)).toEqual([]);
});

// The step focuses its key field on the next frame. A field the operator has
// already clicked into must keep focus, or what they type lands in the key's
// input instead: the cause of a once-seen failure in the test above.
test("a step's deferred focus does not take focus from a field already clicked into", async ({ page, baseURL }) => {
  await openSettings(page, baseURL);
  await tab(page, "usage").click();
  await next(page);
  await page.locator("#walk-orgSlug").focus();
  await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  await expect(page.locator("#walk-orgSlug")).toBeFocused();
  await page.keyboard.type("typed-slug");
  await expect(page.locator("#walk-secret")).toHaveValue("");
});

test("host edits retain the saved identity and replace the token only explicitly", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const host = settings.hosts.rows[0];
  await openConnection(page, `host:${host.id}`);
  await expect(page.locator("#host-edit-token")).toBeHidden();
  await page.locator("#host-edit-name").fill("Renamed host");
  await page.locator("#host-edit-address").fill("new.example.ts.net");
  await page.locator(".host-save").click();
  expect(await calls(page, "settings_save_host")).toEqual([{
    command: "settings_save_host", args: {id:host.id, name:"Renamed host", address:"new.example.ts.net", port:String(host.port), token:""},
  }]);
  await page.locator(".host-replace-token").click();
  await page.locator("#host-edit-token").fill("replacement-token");
  await page.locator(".host-save").click();
  expect((await calls(page, "settings_save_host")).at(-1).args).toMatchObject({id:host.id, token:"replacement-token"});
  await expect(page.locator("#host-edit-token")).toHaveValue("");
  expect(await calls(page, "settings_add_host")).toEqual([]);
});

test("Replace… swaps the token caption for the field, so the label reads once", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const host = settings.hosts.rows[0];
  await openConnection(page, `host:${host.id}`);
  const details = page.locator(`.host-row[data-host="${host.id}"] .host-details`);
  const label = details.locator(".lbl:visible", { hasText: settings.hosts.add.tokenLabel });
  await expect(label).toHaveCount(1);
  await details.locator(".host-replace-token").click();
  await expect(page.locator("#host-edit-token")).toBeFocused();
  await expect(label).toHaveCount(1);
});

test("a rejected save keeps the connection draft and reports the failure", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "azure").click();
  await page.evaluate(() => {
    const original = window.__TAURI__.core.invoke;
    window.__TAURI__.core.invoke = async (command, args) => {
      if (command === "settings_save_source") throw new Error("IPC unavailable");
      return original(command, args);
    };
  });
  await next(page);
  await next(page);
  await page.locator("#walk-budgetUsd").fill("777");
  await next(page);
  await page.locator(".walk-finish").click();
  await expect(page.locator("#settingsStatus")).toHaveText(settings.connections.saveFailed);
  await expect(page.locator(".walk"), "the steps stay open").toBeVisible();
  await page.locator(".walk-back").click();
  await expect(page.locator("#walk-budgetUsd")).toHaveValue("777");
});

test("Sources and its editor fit a narrow window without clipped controls", async ({ page, baseURL }) => {
  await page.setViewportSize({width:390,height:900});
  await openSettings(page, baseURL);
  const fits = () => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth);
  expect(await fits()).toBe(true);
  await tab(page, "hosts").click();
  await expect(page.locator(".host-save")).toBeVisible();
  expect(await fits()).toBe(true);
});

/**
 * Panels keep their own timers and skip the work while Settings is up, so a
 * setting saved a second ago used to leave its panel displaying the setup
 * instruction that asked for it -- for a full slow-cadence tick, ten seconds on
 * the Azure panel. `refreshCockpit` did not help: it repaints the host cards,
 * and the panels are separate. Only a tile's Full view has a panel on screen
 * (#647), so each source's tile is opened, Settings opened over it and closed.
 */
test("closing Settings brings a tile's Full view current, not just the host cards", async ({ page, baseURL }) => {
  await stubTileIpc(page, baseURL, {
    sources: SOURCE_READS.map(([source]) => source),
    cockpit: await fixture(baseURL, "sample-cockpit.json"),
    answers: { settings_view: await fixture(baseURL, "sample-settings.json") },
  });
  await page.goto("/index.html");
  for (const [source, command] of SOURCE_READS) {
    await openTileFull(page, source);
    await page.locator(SETTINGS_BUTTON).click();
    await expect(page.locator("#settings")).toBeVisible();
    // Everything asked for so far is noise; what matters is what close does.
    await page.evaluate(() => {
      window.__CALLS__.length = 0;
    });
    await page.locator("#settingsClose").click();
    await expect(page.locator("#settings")).toBeHidden();
    await expect(page.locator("#fullView")).toBeVisible();
    // The tile's own source, on every source: a panel that quietly stopped
    // registering a refresher would still pass a test that checked its neighbour.
    await expect
      .poll(async () => (await calls(page, command)).length, {
        timeout: 3000,
        message: `${command} was not re-requested when Settings closed over ${source}'s Full view`,
      })
      .toBeGreaterThan(0);
    await page.keyboard.press("Escape");
    await expect(page.locator("#dashboardOverview")).toBeVisible();
  }
});

/**
 * macOS WebKit applies the system text-input settings ("Capitalize words
 * automatically", autocorrect) to any `<input type="text">` that doesn't
 * opt out, so typing `mac-n87k` into Add host became `Mac-n87k` (#435).
 * Every Settings text field is an identifier or a short name -- none is
 * prose -- so `textInput()` (settings.js:89) opts every one of them out by
 * default now, rather than three call sites each carrying their own copy.
 *
 * This walks every page Settings can render -- the three plain tabs, every
 * catalog kind through the Add flow (including the Add host form the issue
 * reported), the key step of a source's steps, the unattributed-repos view, and
 * every already-saved connection's own edit view (including the host edit
 * form) -- and asserts every `input.input` on it carries all three opt-outs.
 * Headless Chromium never applies the macOS setting itself, so this only
 * proves the attributes are on the element; the manual check belongs on the
 * PR.
 */
test("every text and password field in Settings opts out of system capitalization, autocorrect and spellcheck", async ({
  page,
  baseURL,
}) => {
  const settings = await openSettings(page, baseURL);

  const expectOptedOut = async () => {
    const inputs = page.locator("#settings input.input");
    const count = await inputs.count();
    for (let i = 0; i < count; i++) {
      const input = inputs.nth(i);
      await expect(input).toHaveAttribute("autocapitalize", "off");
      await expect(input).toHaveAttribute("autocorrect", "off");
      await expect(input).toHaveAttribute("spellcheck", "false");
    }
    return count;
  };

  let checked = 0;

  // The two plain tabs (Preferences and About carry no text fields now).
  for (const id of ["general", "about"]) {
    await tab(page, id).click();
    checked += await expectOptedOut();
  }

  // Every catalog kind, through the Add flow -- the form a brand-new
  // connection actually renders, including Add host (#host-name,
  // #host-address) named in the issue.
  for (const kind of ["account", "host", "neon", "sentry", "vercel", "azure", "openclaw", "vendor"]) {
    await addConnection(page, kind);
    checked += await expectOptedOut();
    if (kind === "host") {
      await expect(page.locator("#host-name")).toHaveAttribute("autocapitalize", "off");
      await expect(page.locator("#host-address")).toHaveAttribute("autocapitalize", "off");
    }
  }

  // The one text field in a source's steps: its key.
  for (const kind of ["anthropic", "openai"]) {
    await addConnection(page, kind);
    await page.locator('.walk-pick[data-part="api"]').click();
    await page.locator(".walk-next").click();
    checked += await expectOptedOut();
    await page.locator(".walk-cancel").click();
  }

  // The unattributed-repos view, reached only through its own button
  // (`.connection-orphans`) rather than a fixed connection id -- it renders
  // a workflows textInput per orphaned repo (repoConfig), built into the DOM
  // up front rather than behind the "Configure…" disclosure.
  const unattributedCount = settings.accounts.unattributed.length;
  expect(unattributedCount, "the fixture must carry an unattributed repo to reach this view").toBeGreaterThan(0);
  await page.locator('.tab[data-tab="connections"]').click();
  await page.locator(".connection-orphans").click();
  checked += await expectOptedOut();

  // Every already-saved connection, in its own edit view -- including the
  // host edit form (#host-edit-name) named in the issue.
  let sawHostEdit = false;
  for (const row of settings.connections.rows) {
    await openConnection(page, row.id);
    checked += await expectOptedOut();
    if (row.kind === "host") {
      await expect(page.locator("#host-edit-name")).toHaveAttribute("autocapitalize", "off");
      sawHostEdit = true;
    }
  }
  expect(sawHostEdit, "the fixture must carry a saved host to reach the host edit form").toBe(true);

  // The sweep above is vacuous if nothing it visited ever carried an
  // input.input -- prove real fields were swept, not just empty pages.
  expect(settings.connections.rows.length, "the fixture must carry a saved connection to reach an edit view").toBeGreaterThan(0);
  expect(checked).toBeGreaterThan(10);
});

test("General holds no view settings: they moved to each tile's Configure (#647)", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "general").click();
  const g = settings.general;

  // CPU core rows and Row spacing are per tile now; nothing here edits them.
  expect(g.coreRowSpan).toBeUndefined();
  expect(g.rowGapPx).toBeUndefined();
  await expect(page.locator("#general-core-rows")).toHaveCount(0);
  await expect(page.locator("#general-row-gap")).toHaveCount(0);
  await expect(page.locator("#general-overflow")).toHaveCount(0);
  // Nothing in this tab is saved with Apply, so there is no Apply to press.
  await expect(page.locator(".btn.apply")).toHaveCount(0);
  // The shared Refresh Interval left this tab (#615): no picker, and a note
  // from Rust saying where the cadences went.
  await expect(page.locator("#general-interval")).toHaveCount(0);
  expect(g.refreshInterval).toBeUndefined();
  await expect(page.getByText(g.cadenceNote)).toBeVisible();
});

test("Settings no longer carries the panel poll cadences (#614)", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "general").click();
  // They live in each tile's Configure now; see dashboard.spec.js.
  expect(settings.general.panelIntervals).toBeUndefined();
  await expect(page.locator(".cadence-item")).toHaveCount(0);
  await expect(page.getByText("Panel Poll Cadence")).toHaveCount(0);
});

test("the crash-reporting toggle saves on the spot and paints Rust's sentence", async ({
  page,
  baseURL,
}) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "general").click();
  const c = settings.general.crashReporting;

  const toggle = page.locator("#general-crash-reporting");
  await expect(toggle).toBeChecked({ checked: c.value });
  // The status line is Rust's, verbatim. "On" and "actually reporting" are
  // different facts and only Rust knows the second, so the frontend must not
  // be composing this sentence.
  await expect(page.locator(".crash-status")).toHaveText(c.status);

  // No Apply: consent is not a draft, so ticking the box *is* the save. One
  // `click`, not `setChecked` — the tab re-renders from what Rust persisted, so
  // the stub's unchanging fixture paints the box straight back and a
  // "make it end up unchecked" helper would retry until it timed out. Which is
  // the contract working: what the box shows is Rust's answer, never the click.
  await toggle.click();
  expect(await calls(page, "settings_set_crash_reporting")).toEqual([
    {
      command: "settings_set_crash_reporting",
      args: { enabled: !c.value },
    },
  ]);
  await expect(toggle).toBeChecked({ checked: c.value });
});

// A status is Rust's sentence and can be any length (a disconnected gateway
// says why), so its slot wraps rather than clipping. A right-aligned slot that
// overflows loses the START of the sentence, off the edge where no scrollbar
// reaches: "awaiting device pairing" read "ing device pairing".
test("a long connection status is shown whole, at every width", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const long = settings.connections.rows.find((row) => row.status.text.length > 18);
  expect(long, "the fixture needs a status longer than the 18ch slot").toBeTruthy();
  for (const width of [1100, 760, 420]) {
    await page.setViewportSize({ width, height: 900 });
    await page.locator('.tab[data-tab="connections"]').click();
    const state = page.locator(`.connection-row[data-connection="${long.id}"] .connection-state`);
    await expect(state).toHaveText(long.status.text);
    const box = await state.evaluate((el) => {
      const range = document.createRange();
      range.selectNodeContents(el);
      const text = range.getBoundingClientRect(), slot = el.getBoundingClientRect();
      return { text: [text.left, text.right], slot: [slot.left, slot.right] };
    });
    expect(box.text[0], `${width}px: the status starts inside its slot`).toBeGreaterThanOrEqual(box.slot[0] - 0.5);
    expect(box.text[1], `${width}px: the status ends inside its slot`).toBeLessThanOrEqual(box.slot[1] + 0.5);
  }
});

// MARK: a source's steps

const catalogEntry = (settings, kind) => settings.connections.catalog.find((entry) => entry.kind === kind);
const partOf = (entry, id) => entry.walk.parts.find((part) => part.id === id);
const saves = (page) => calls(page, "settings_save_source");
const pick = (page, part) => page.locator(`.walk-pick[data-part="${part}"]`);
const next = (page) => page.locator(".walk-next").click();
const stepHeading = (page) => page.locator(".walk h2");
const stepLine = (page) => page.locator(".walk-step");

/** A store where Anthropic is added: the row the operator clicks, and the
 *  choices the steps read back. */
function withAnthropic(settings) {
  const entry = catalogEntry(settings, "anthropic");
  partOf(entry, "api").on = true;
  partOf(entry, "api").secret.stored = true;
  partOf(entry, "subscription").on = true;
  partOf(entry, "subscription").status = "On. Reads Claude Code's sign-in every 15m.";
  entry.walk.tile.present = true;
  settings.connections.rows.push({
    id: "anthropic",
    kind: "anthropic",
    title: entry.title,
    feeds: "API key · Subscription",
    status: { text: "Configured", attention: false, color: "#8090ac" },
  });
}

test("Add source lists every source, and nothing sits in a section of its own", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await expect(page.locator(".connection-automatic")).toHaveCount(0);
  await page.locator(".connection-add").click();
  for (const kind of ["local", "anthropic", "openai", "services"]) {
    await expect(page.locator(`.connection-catalog [data-kind="${kind}"]`)).toHaveCount(1);
  }
  await expect(page.locator(".connection-catalog [data-kind]")).toHaveCount(settings.connections.catalog.length);
});

test("Add source walks through only what was picked, and only Finish applies it", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const entry = catalogEntry(settings, "anthropic");
  const labels = settings.connections.walkLabels;
  await addConnection(page, "anthropic");

  await expect(stepHeading(page)).toHaveText(labels.pickHeading);
  await expect(page.locator(".walk-pick-title")).toHaveText(entry.walk.parts.map((part) => part.title));
  await expect(page.locator(".walk-next"), "there is nothing to set up yet").toBeDisabled();

  await pick(page, "subscription").click();
  await expect(stepLine(page)).toHaveText("Anthropic · step 1 of 3");
  await next(page);

  const subscription = partOf(entry, "subscription");
  await expect(stepHeading(page)).toHaveText(subscription.title);
  await expect(stepLine(page)).toHaveText("Anthropic · step 2 of 3");
  await expect(page.locator(".walk .help")).toHaveText(subscription.help);
  await expect(page.locator(".walk-status"), "no sign-in has been read, so none is reported").toHaveCount(0);
  await next(page);

  await expect(stepHeading(page)).toHaveText(labels.readyHeading);
  await expect(page.locator(".walk-summary li")).toHaveText([
    `— API spend: ${labels.notSetUp}`,
    "✓ Plan limits",
    `— Local usage: ${labels.notSetUp}`,
  ]);
  await expect(page.locator(".walk-tile")).toHaveText(entry.walk.tile.label);
  await expect(page.locator(".walk-tile input")).toBeChecked();
  expect(await calls(page, "settings_set_plan_usage"), "the opt-in is part of Finish").toEqual([]);
  expect(await saves(page), "nothing is applied before Finish").toEqual([]);

  await page.locator(".walk-finish").click();
  expect(await saves(page)).toEqual([
    {
      command: "settings_save_source",
      args: { kind: "anthropic", parts: ["subscription"], secret: null, addTile: true },
    },
  ]);
  await expect(page.locator(".walk")).toHaveCount(0);
  await expect(page.locator(".connection-list")).toBeVisible();
  await expect(page.locator("#settingsStatus")).toHaveText("Anthropic saved.");
});

test("the tile can be left out", async ({ page, baseURL }) => {
  await openSettings(page, baseURL);
  await addConnection(page, "openai");
  await pick(page, "subscription").click();
  await next(page);
  await next(page);
  await page.locator(".walk-tile input").uncheck();
  await page.locator(".walk-finish").click();
  expect((await saves(page))[0].args.addTile).toBe(false);
});

test("Cancel and Back change nothing, and Cancel forgets the draft", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await addConnection(page, "anthropic");
  await pick(page, "local").click();
  await next(page);
  await page.locator(".walk-back").click();
  await expect(stepHeading(page)).toHaveText(settings.connections.walkLabels.pickHeading);
  await expect(pick(page, "local").locator("input"), "Back keeps the choices").toBeChecked();

  await page.locator(".walk-cancel").click();
  await expect(page.locator(".connection-list")).toBeVisible();
  expect(await saves(page)).toEqual([]);

  await addConnection(page, "anthropic");
  await expect(pick(page, "local").locator("input")).toBeChecked({ checked: false });
});

test("the key step stops on an empty key, and the key leaves only at Finish", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const api = partOf(catalogEntry(settings, "openai"), "api");
  const labels = settings.connections.walkLabels;
  await addConnection(page, "openai");
  await pick(page, "api").click();
  await next(page);

  await expect(stepHeading(page)).toHaveText(api.title);
  await expect(page.locator(".walk .help").first()).toHaveText(api.help);
  const key = page.locator("#walk-secret");
  await expect(key).toHaveAttribute("type", "password");
  await expect(key).toHaveAttribute("placeholder", api.secret.placeholder);
  await expect(page.locator(".walk-problem")).toHaveCount(0);

  await next(page);
  await expect(stepHeading(page), "no key, no next step").toHaveText(api.title);
  await expect(page.locator(".walk-problem")).toHaveText(labels.keyRequired);
  await expect(page.locator(".walk-problem"), "it reads as a problem, not as help").not.toHaveCSS(
    "color",
    await page.locator(".walk .help").first().evaluate((el) => getComputedStyle(el).color),
  );

  await key.fill("sk-admin-x");
  await key.press("Enter");
  await expect(stepHeading(page)).toHaveText(labels.readyHeading);
  expect(await saves(page)).toEqual([]);

  await page.locator(".walk-back").click();
  await expect(key, "Back keeps what was typed").toHaveValue("sk-admin-x");
  await next(page);
  await page.locator(".walk-finish").click();
  expect((await saves(page))[0].args).toEqual({
    kind: "openai", parts: ["api"], secret: "sk-admin-x", addTile: true,
  });
});

test("a key typed and then unticked is never sent", async ({ page, baseURL }) => {
  await openSettings(page, baseURL);
  await addConnection(page, "openai");
  await pick(page, "api").click();
  await next(page);
  await page.locator("#walk-secret").fill("sk-admin-x");
  await page.locator(".walk-back").click();
  await pick(page, "api").click();
  await pick(page, "subscription").click();
  await next(page);
  await next(page);
  await page.locator(".walk-finish").click();
  expect((await saves(page))[0].args).toEqual({
    kind: "openai", parts: ["subscription"], secret: null, addTile: true,
  });
});

test("a refused Finish stays on the steps with Rust's sentence", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const refusal = "A dashboard can hold up to 48 tiles.";
  await page.evaluate(
    ([status, view]) => { window.__SAVE_SOURCE__ = { status, settings: view, saved: false }; },
    [refusal, settings],
  );
  await addConnection(page, "anthropic");
  await pick(page, "subscription").click();
  await next(page);
  await next(page);
  await page.locator(".walk-finish").click();
  await expect(page.locator("#settingsStatus")).toHaveText(refusal);
  await expect(stepHeading(page)).toHaveText(settings.connections.walkLabels.readyHeading);
  await expect(page.locator(".walk-finish")).toBeEnabled();
});

test("reopening a source reads back its choices, says a key exists, and keeps it unless asked", async ({
  page,
  baseURL,
}) => {
  const settings = await openSettings(page, baseURL, undefined, undefined, undefined, withAnthropic);
  const entry = catalogEntry(settings, "anthropic");
  const row = page.locator('.connection-row[data-kind="anthropic"]');
  await expect(row.locator(".connection-feeds")).toHaveText("API key · Subscription");
  await row.click();

  await expect(pick(page, "api").locator("input")).toBeChecked();
  await expect(pick(page, "subscription").locator("input")).toBeChecked();
  await expect(pick(page, "local").locator("input")).toBeChecked({ checked: false });
  await expect(stepLine(page)).toHaveText("Anthropic · step 1 of 4");
  await next(page);

  await expect(page.locator(".walk-stored")).toHaveText(partOf(entry, "api").secret.storedLabel);
  await expect(page.locator("#walk-secret")).toHaveValue("");
  await next(page);

  await expect(page.locator(".walk-status")).toHaveText(partOf(entry, "subscription").status);
  await next(page);

  await expect(page.locator(".walk-tile"), "it already has a tile").toHaveCount(0);
  await page.locator(".walk-finish").click();
  expect((await saves(page))[0].args).toMatchObject({
    kind: "anthropic", parts: ["api", "subscription"], secret: null,
  });
});

test("Remove source unticks everything and jumps to a Ready step that says so", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL, undefined, undefined, undefined, withAnthropic);
  const labels = settings.connections.walkLabels;
  await page.locator('.connection-row[data-kind="anthropic"]').click();
  await page.locator(".walk-remove").click();

  await expect(stepHeading(page)).toHaveText(labels.removeHeading);
  await expect(page.locator(".walk .help")).toHaveText(labels.removeHelp);
  await expect(page.locator(".walk-tile")).toHaveCount(0);
  expect(await saves(page), "and it is still a draft").toEqual([]);

  await page.locator(".walk-finish").click();
  expect((await saves(page))[0].args).toMatchObject({ kind: "anthropic", parts: [], secret: null });
});

test("Remove source does not come back on the Ready step of a one-part source", async ({ page, baseURL }) => {
  const addedServices = (settings) => {
    const entry = catalogEntry(settings, "services");
    entry.walk.parts[0].on = true;
    settings.connections.rows.push({
      id: "services",
      kind: "services",
      title: entry.title,
      feeds: entry.walk.parts[0].title,
      status: { text: "Configured", attention: false, color: "#8090ac" },
    });
  };
  const settings = await openSettings(page, baseURL, undefined, undefined, undefined, addedServices);
  await page.locator('.connection-row[data-kind="services"]').click();
  await page.locator(".walk-remove").click();
  await expect(stepHeading(page)).toHaveText(settings.connections.walkLabels.removeHeading);
  await expect(page.locator(".walk-remove"), "the steps are only Ready now, which is index 0 again").toHaveCount(0);
});

// Neon and Vercel: one part and one typed id each, so the question is skipped.
const PROVIDERS = [
  { kind: "neon", part: "neon", title: "Neon", id: "orgId", field: "#walk-orgId", typed: "org-fond-sea-1" },
  { kind: "vercel", part: "vercel", title: "Vercel", id: "teamId", field: "#walk-teamId", typed: "team_abc" },
];

/** A store where the provider is not added, whatever the fixture holds. */
function withoutProvider({ kind }) {
  return (settings) => {
    const part = catalogEntry(settings, kind).walk.parts[0];
    part.on = false;
    part.secret.stored = false;
    for (const input of part.inputs) input.value = "";
    settings.connections.rows = settings.connections.rows.filter((row) => row.kind !== kind);
  };
}

/** A store where the provider is added: its row, its key and its saved id. */
function withProvider({ kind, id }, saved) {
  return (settings) => {
    withoutProvider({ kind })(settings);
    const entry = catalogEntry(settings, kind);
    const part = entry.walk.parts[0];
    part.on = true;
    part.secret.stored = true;
    part.inputs.find((input) => input.id === id).value = saved;
    settings.connections.rows.push({
      id: kind,
      kind,
      title: entry.title,
      feeds: "Usage",
      status: { text: "Configured", attention: false, color: "#8090ac" },
    });
  };
}

for (const provider of PROVIDERS) {
  const { kind, part, title, id, field, typed } = provider;

  test(`${title} has one step for its key and ${id}, then Ready, and only Finish applies it`, async ({ page, baseURL }) => {
    const settings = await openSettings(page, baseURL, undefined, undefined, undefined, withoutProvider(provider));
    const entry = catalogEntry(settings, kind);
    const labels = settings.connections.walkLabels;
    await addConnection(page, kind);

    await expect(page.locator(".walk-pick"), "one part, so nothing to pick").toHaveCount(0);
    await expect(stepLine(page)).toHaveText(`${title} · step 1 of 2`);
    await expect(stepHeading(page)).toHaveText(entry.walk.parts[0].title);
    await expect(page.locator("#walk-secret")).toHaveAttribute("type", "password");
    await expect(page.locator(field)).toHaveValue("");
    await expect(page.locator('[data-secret]'), "no Save that writes before Finish").toHaveCount(0);

    await next(page);
    await expect(page.locator(".walk-problem")).toHaveText(labels.keyRequired);
    await expect(stepLine(page)).toHaveText(`${title} · step 1 of 2`);

    await page.locator("#walk-secret").fill("tok_secret");
    await page.locator(field).fill(typed);
    await next(page);
    await expect(stepHeading(page)).toHaveText(labels.readyHeading);
    await expect(page.locator(".walk-tile")).toHaveText(entry.walk.tile.label);
    expect(await saves(page), "nothing is applied before Finish").toEqual([]);
    expect(await calls(page, "settings_save_secret")).toEqual([]);

    await page.locator(".walk-finish").click();
    const [save] = await saves(page);
    expect(save.args).toMatchObject({ kind, parts: [part], secret: "tok_secret", addTile: true });
    expect(save.args.inputs[id]).toBe(typed);
    await expect(page.locator(".walk")).toHaveCount(0);
    await expect(page.locator("#settingsStatus")).toHaveText("Source saved.");
  });

  test(`${title}: Back and Cancel write nothing, and Cancel forgets the draft`, async ({ page, baseURL }) => {
    await openSettings(page, baseURL, undefined, undefined, undefined, withoutProvider(provider));
    await addConnection(page, kind);
    await page.locator("#walk-secret").fill("tok_secret");
    await page.locator(field).fill(typed);
    await next(page);
    await page.locator(".walk-back").click();
    await expect(page.locator("#walk-secret"), "Back keeps what was typed").toHaveValue("tok_secret");
    await expect(page.locator(field)).toHaveValue(typed);

    await page.locator(".walk-cancel").click();
    await expect(page.locator(".connection-list")).toBeVisible();
    expect(await saves(page)).toEqual([]);
    expect(await calls(page, "settings_save_secret")).toEqual([]);

    await addConnection(page, kind);
    await expect(page.locator("#walk-secret")).toHaveValue("");
    await expect(page.locator(field)).toHaveValue("");
  });

  test(`${title}: its row opens the same steps with the saved ${id} and no key shown`, async ({ page, baseURL }) => {
    const settings = await openSettings(page, baseURL, undefined, undefined, undefined, withProvider(provider, "saved-id"));
    const stored = catalogEntry(settings, kind).walk.parts[0].secret.storedLabel;
    await page.locator(`.connection-row[data-kind="${kind}"]`).click();
    await expect(page.locator(".walk-pick")).toHaveCount(0);
    await expect(page.locator(field)).toHaveValue("saved-id");
    await expect(page.locator(".walk-stored")).toHaveText(stored);
    await expect(page.locator("#walk-secret")).toHaveValue("");

    // The stored key is kept by leaving the field empty.
    await page.locator(field).fill(typed);
    await next(page);
    await page.locator(".walk-finish").click();
    const [save] = await saves(page);
    expect(save.args).toMatchObject({ kind, parts: [part], secret: null });
    expect(save.args.inputs[id]).toBe(typed);
  });

  test(`${title}: Remove source sends no parts, so Finish clears its key and ${id}`, async ({ page, baseURL }) => {
    const settings = await openSettings(page, baseURL, undefined, undefined, undefined, withProvider(provider, "saved-id"));
    await page.locator(`.connection-row[data-kind="${kind}"]`).click();
    await page.locator(".walk-remove").click();
    await expect(stepHeading(page)).toHaveText(settings.connections.walkLabels.removeHeading);
    expect(await saves(page), "and it is still a draft").toEqual([]);
    await page.locator(".walk-finish").click();
    const [save] = await saves(page);
    expect(save.args).toEqual({ kind, parts: [], secret: null, addTile: true });
  });

  test(`${title}: leaving with a typed ${id} asks first`, async ({ page, baseURL }) => {
    await openSettings(page, baseURL, undefined, undefined, undefined, withProvider(provider, "saved-id"));
    await page.locator(`.connection-row[data-kind="${kind}"]`).click();
    await page.locator(field).fill(typed);
    // Past the step, the value is only in the draft, not in a field on screen.
    await next(page);
    await expect(page.locator(field)).toHaveCount(0);
    await page.locator('.tab[data-tab="general"]').click();
    await expect(page.locator("#settingsConfirm")).toBeVisible();
    await page.locator("#settingsConfirm .discard").click();
    await expect(page.locator(".walk")).toHaveCount(0);
    expect(await saves(page)).toEqual([]);
  });
}

test("Neon's rates are typed in its step, behind Advanced options, and sent at Finish", async ({ page, baseURL }) => {
  await openSettings(page, baseURL, undefined, undefined, undefined, withoutProvider(PROVIDERS[0]));
  await addConnection(page, "neon");
  await page.locator("#walk-secret").fill("tok_secret");
  await page.locator(".connection-advanced summary").click();
  await page.locator("#walk-usdPerCuHour").fill("0.106");
  await page.locator("#walk-usdPerGibMonth").fill("0.35");
  await next(page);
  await page.locator(".walk-finish").click();
  const [save] = await saves(page);
  expect(save.args.inputs).toEqual({ orgId: "", usdPerCuHour: "0.106", usdPerGibMonth: "0.35" });
});

test("leaving a walk with a key typed on an earlier step asks first", async ({ page, baseURL }) => {
  await openSettings(page, baseURL);
  await addConnection(page, "openai");
  await pick(page, "api").click();
  await next(page);
  await page.locator("#walk-secret").fill("sk-admin-x");
  await next(page);
  await expect(page.locator("#walk-secret"), "the key's input is gone from the screen").toHaveCount(0);
  await page.locator('.tab[data-tab="general"]').click();
  await expect(page.locator("#settingsConfirm")).toBeVisible();
  await page.locator("#settingsConfirm .discard").click();
  await expect(page.locator("#settingsConfirm")).toBeHidden();
});

test("a source not yet added has nothing to remove", async ({ page, baseURL }) => {
  await openSettings(page, baseURL);
  await addConnection(page, "anthropic");
  await expect(page.locator(".walk-remove")).toHaveCount(0);
});

test("a source with one part skips the question", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const entry = catalogEntry(settings, "services");
  await addConnection(page, "services");

  await expect(page.locator(".walk-pick")).toHaveCount(0);
  await expect(stepHeading(page)).toHaveText(entry.walk.parts[0].title);
  await expect(stepLine(page)).toHaveText(`${entry.title} · step 1 of 2`);
  await next(page);
  await page.locator(".walk-finish").click();
  expect((await saves(page))[0].args).toMatchObject({ kind: "services", parts: ["services"] });
});

test("each row's click reaches the steps its catalog entry carries", async ({ page, baseURL }) => {
  await openSettings(page, baseURL);
  // This machine is added in the fixture, so it is a row with no `walk` of its own.
  await page.locator('.connection-row[data-kind="local"]').click();
  await expect(page.locator(".walk")).toBeVisible();
  await expect(page.locator(".walk-remove")).toBeVisible();
});

test("the Hosts tab lists every host with its endpoint and token badge", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "hosts").click();

  const rows = page.locator(".host-row");
  await expect(rows).toHaveCount(1);
  for (const host of settings.hosts.rows) {
    await openConnection(page, `host:${host.id}`);
    const row = rows.first();
    await expect(row.locator(".host-name")).toHaveText(host.name);
    await expect(row).toContainText(host.endpoint);
    // Both sides of the badge, from the same fixture: a suite that only ever
    // saw "Token stored" would not notice the other branch disappearing.
    await expect(row).toContainText(
      host.tokenStored ? settings.hosts.tokenStoredLabel : settings.hosts.noTokenLabel
    );
    // A disabled host is still listed -- Settings edits a configuration, and
    // it is the cockpit that filters on `enabled`.
    expect(await row.locator(".toggle").isChecked()).toBe(host.enabled);
  }
});

test("Settings carries no machine alert limits or volume list: they moved to the Machines tile's Configure (#612)", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "general").click();
  await expect(page.locator(".machine-alerts")).toHaveCount(0);
  await openConnection(page, `host:${settings.hosts.rows[0].id}`);
  await expect(page.locator(".machine-alerts, .volume-choice, .hidden-row")).toHaveCount(0);
  await tab(page, "connections").click();
  await page.locator('.connection-row[data-kind="local"]').click();
  await expect(page.locator(".machine-alerts, .volume-choice")).toHaveCount(0);
});

// `#rrggbb` -> the `rgb(r, g, b)` a browser reports for a computed colour.
const rgb = (hex) =>
  `rgb(${[1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16)).join(", ")})`;

test("each host row says where its agent stands against the newest verified release, without Test (#489)", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const rows = settings.hosts.rows;

  // The fixture carries all three readings the issue names: behind, up to date,
  // and a host that has reported no version.
  const byState = Object.fromEntries(rows.map((h) => [h.agentRelease.state, h]));
  expect(Object.keys(byState).sort()).toEqual(["behind", "host-unknown", "up-to-date"]);

  const AMBER = rgb(settings.hosts.rows.find((h) => h.agentRelease.state === "behind").agentRelease.color);
  for (const host of rows) {
    await openConnection(page, `host:${host.id}`);
    const line = page.locator(`.host-row[data-host="${host.id}"] .agent-release`);
    // Shown with no Test press, and the words are Rust's.
    await expect(line).toHaveText(host.agentRelease.text);
    await expect(line).toHaveAttribute("data-state", host.agentRelease.state);
    // The colour is Rust's too, and only a host that is behind is amber.
    await expect(line).toHaveCSS("color", rgb(host.agentRelease.color));
    const color = await line.evaluate((el) => getComputedStyle(el).color);
    expect(color === AMBER, host.name).toBe(host.agentRelease.state === "behind");
  }
  expect(await calls(page, "settings_test_host")).toEqual([]);

  // Behind is informational: it names a newer release, it does not demand one.
  expect(byState.behind.agentRelease.text).toContain("available");
});

test("Sources no longer hosts the container group rules editor (#613)", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  // The editor moved to the Containers tile's Configure; the payload no longer
  // carries it, so nothing here can render a second copy.
  expect(settings.hosts.rules).toBeUndefined();
  await openConnection(page, "local");
  await expect(page.locator(".rule-row")).toHaveCount(0);
  await expect(page.locator(".btn.add-rule")).toHaveCount(0);
  expect(await calls(page, "settings_add_container_rule")).toEqual([]);
});

test("Test probes that one host and paints the line Rust produced", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "hosts").click();

  const host = settings.hosts.rows[1];
  await openConnection(page, `host:${host.id}`);
  const row = page.locator(`.host-row[data-host="${host.id}"]`);
  const before = await row.boundingBox();
  await row.locator(".test").click();

  // The result string is Rust's (`settings::health_result`) and has no other
  // path to the DOM: the frontend never composes a ✓/✗ line of its own.
  await expect(row.locator(".result")).toHaveText(TEST_RESULT);
  expect(await row.boundingBox()).toEqual(before);
  expect(await calls(page, "settings_test_host")).toEqual([
    { command: "settings_test_host", args: { id: host.id } },
  ]);
  // …and only that row's. A shared result slot is the bug this catches.
  const other = settings.hosts.rows[0];
  await openConnection(page, `host:${other.id}`);
  await expect(page.locator(`.host-row[data-host="${other.id}"] .result`)).toHaveText("");
});

test("a long Test result wraps across the whole row instead of scrolling, and the buttons stay put", async ({ page, baseURL }) => {
  // `settings::health_result`'s sentence for a host the plain-HTTP guard
  // refused (#449), the longest line Test paints.
  const notPaired = "✗ not paired — the cockpit sends the token over plain HTTP only to loopback or " +
    "Tailscale addresses; check this host's certificate and trust it";
  for (const width of [1280, 760]) {
    await page.setViewportSize({ width, height: 900 });
    // A fresh page each pass: a Test result outlives a re-render, so reusing
    // the page would measure `before` with the sentence already laid out.
    const settings = await openSettings(page, baseURL);
    const host = settings.hosts.rows.find((h) => !h.pinned);
    // The real payload for this sentence, `plainRefused` included, so the
    // rebuild it triggers (the refusal note) is part of what is measured.
    await page.evaluate(({ id, notPaired }) => {
      const original = window.__TAURI__.core.invoke;
      window.__TAURI__.core.invoke = async (command, args) =>
        command === "settings_test_host"
          ? { id, result: notPaired, certificateChanged: false, plainRefused: true }
          : original(command, args);
    }, { id: host.id, notPaired });
    await openConnection(page, `host:${host.id}`);
    const row = page.locator(`.host-row[data-host="${host.id}"]`);
    const test = row.locator(".test");
    const before = await test.boundingBox();
    await test.click();
    const result = row.locator(".result");
    await expect(result).toHaveText(notPaired);

    // Every line is on screen: no scroll box of its own…
    const fit = await result.evaluate((el) => ({ scroll: el.scrollHeight, client: el.clientHeight }));
    expect(fit.scroll, `${width}px`).toBeLessThanOrEqual(fit.client);
    // …the sentence has the row's whole width, not a share split with a spacer…
    const head = await row.locator(":scope > .row").first().boundingBox();
    expect((await result.boundingBox()).width, `${width}px`).toBeGreaterThanOrEqual(head.width - 1);
    // …and growing it moves nothing that comes before it.
    expect(await test.boundingBox(), `${width}px`).toEqual(before);
  }
});

test("Add Host waits for a name and an address, and keeps the token out of the DOM", async ({ page, baseURL }) => {
  await openSettings(page, baseURL);
  await addConnection(page, "host");

  const add = page.locator(".btn.add");
  await expect(add).toBeDisabled();
  await page.locator("#host-name").fill("smoke-box");
  await expect(add).toBeDisabled();
  await page.locator("#host-address").fill("100.64.0.9");
  await expect(add).toBeEnabled();

  await expect(page.locator("#host-port")).toHaveValue("7878");
  await expect(page.locator("#host-token")).toHaveAttribute("type", "password");
  await page.locator("#host-token").fill("s3cret-agent-token");
  await add.click();

  expect(await calls(page, "settings_add_host")).toEqual([
    {
      command: "settings_add_host",
      args: { name: "smoke-box", address: "100.64.0.9", port: "7878", token: "s3cret-agent-token" },
    },
  ]);
  // The token is handed to Rust and dropped: it must not survive anywhere in
  // the page, neither in the field it was typed into nor in a re-render.
  const leaked = await page.evaluate((token) =>
    [...document.querySelectorAll("input")].some((i) => i.value.includes(token)),
    "s3cret-agent-token"
  );
  expect(leaked, "the agent token stayed in the DOM after the save").toBe(false);
});

test("a credential saves, clears, and never comes back", async ({ page, baseURL }) => {
  // OpenClaw's bearer token: the one credential left with a form of its own
  // (every source with steps writes its key at Finish instead), so it is the
  // subject that still exists for the claim.
  const settings = await openSettings(page, baseURL, undefined, undefined, undefined, (s) => {
    s.openclaw.secret.stored = true;
  });
  await tab(page, "openclaw").click();
  await page.locator(".replace-secret").click();

  const secret = settings.openclaw.secret;
  const box = page.locator('.group[data-secret="openclaw"]');
  const input = page.locator("#secret-openclaw");
  await expect(input).toHaveAttribute("type", "password");
  await expect(input).toHaveValue("");
  // The badge is up and Clear is live; Save waits for something to save.
  await expect(box.locator(".badge-ok")).toHaveText(secret.storedLabel);
  await expect(box.locator(".save")).toBeDisabled();
  await expect(box.locator(".clear")).toBeEnabled();

  await input.fill("sntryu_supersecret");
  await expect(box.locator(".save")).toBeEnabled();
  await box.locator(".save").click();
  expect(await calls(page, "settings_save_secret")).toEqual([
    { command: "settings_save_secret", args: { key: "openclaw", value: "sntryu_supersecret" } },
  ]);
  await expect(input).toHaveValue("");
  await expect(page.locator("#settingsStatus")).toHaveText("Saved.");

  await box.locator(".replace-secret").click();
  await box.locator(".clear").click();
  expect(await calls(page, "settings_clear_secret")).toEqual([
    { command: "settings_clear_secret", args: { key: "openclaw" } },
  ]);

  // Nothing in the payload, and therefore nothing in the page, can echo a
  // stored credential back: the view-model carries a boolean per credential.
  expect(JSON.stringify(settings)).not.toContain("sntryu_");
  await expect(page.locator("body")).not.toContainText("sntryu_supersecret");
});

test("the retired GitHub tab is gone and the account's organizations are edited in its steps", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await expect(page.locator('.tab[data-tab="github"]')).toHaveCount(0);
  await tab(page, "accounts").click();

  // The fixture's first account watches one org; the second watches none.
  const rows = settings.accounts.rows;
  const watching = rows.find((r) => r.orgs.length > 0);
  const watchingRow = page.locator(`.group[data-account="${watching.id}"]`);
  await expect(watchingRow.locator(`[data-org="${watching.orgs[0]}"]`)).toBeVisible();

  // Typing and removing organizations is the steps' and writes at Finish. The
  // card keeps only #724's discovery offer (`.org-watch`), which deliberately
  // saves on its click.
  await expect(watchingRow.locator(".org-remove, .org-add")).toHaveCount(0);
  expect(await calls(page, "settings_set_account_org")).toEqual([]);
});

// The group labels moved to the GitHub Runners tile's Configure: one editor, beside
// the groups it changes. A second copy here would be two homes for one list.
test("preferences no longer edits runner group labels", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "general").click();
  expect(settings.general.runnerGroupLabels).toBeUndefined();
  await expect(page.locator("[data-runner-group-labels]")).toHaveCount(0);
});

test("a credential with nothing stored offers nothing to clear", async ({ page, baseURL }) => {
  // OpenClaw, not Azure: the Azure panel has no stored credential at all (it
  // signs its own request per poll), so it cannot stand for "a credential with
  // nothing stored".
  const settings = await openSettings(page, baseURL, undefined, undefined, undefined, (s) => {
    s.openclaw.secret.stored = false;
  });
  await tab(page, "openclaw").click();
  await page.locator(".replace-secret").click();
  expect(settings.openclaw.secret.stored).toBe(false);
  const box = page.locator('.group[data-secret="openclaw"]');
  await expect(box.locator(".clear")).toBeDisabled();
  await expect(box.locator(".badge-ok")).toHaveCount(0);
});

/** Opens the owning account's editor and waits for its discovery to answer:
 *  the search field is disabled until then. */
async function openRepos(page, settings) {
  await tab(page, "accounts").click();
  const { owning } = accountsOf(settings);
  const card = page.locator(`.group[data-account="${owning.id}"]`);
  const field = page.locator(`#repo-search-${owning.id}`);
  return { owning, card, field };
}

const rowsOf = (card) => card.locator(".search-row");
const slugsOf = (card) => rowsOf(card).evaluateAll((rows) => rows.map((row) => row.dataset.pick));

test("repos live under their account card, grouped by owner", async ({ page, baseURL }) => {
  // A second owner on the same account, in the shape Rust groups it.
  const settings = await openSettings(page, baseURL, null, null, null, (s) => {
    const owning = s.accounts.rows.find((row) => row.repos.length > 0);
    const rocket = { slug: "globex/rocket", enabled: true, workflows: "", accountMissing: false };
    owning.repos.push(rocket);
    owning.repoGroups.push({ owner: "globex", label: "globex · 1", repos: [rocket] });
  });
  const { owning, card } = await openRepos(page, settings);
  const groups = owning.repoGroups;
  expect(groups.length, "the fixture must carry two owners").toBe(2);

  // Each owner's heading, then its repos, in Rust's order.
  const sequence = await card.locator(".repo-owner, .repo-row[data-repo]").evaluateAll((els) =>
    els.map((el) => el.dataset.owner ? `# ${el.textContent}` : el.dataset.repo));
  expect(sequence).toEqual(groups.flatMap((g) => [`# ${g.label}`, ...g.repos.map((r) => r.slug)]));
  await expect(card.locator(".repo-owner").first()).toHaveAttribute("data-owner", groups[0].owner);

  // No popup and no button into one.
  await expect(card.locator(".btn.configure-repos")).toHaveCount(0);
  await expect(page.locator(".modal, .modal-scrim")).toHaveCount(0);

  const first = groups[0].repos[0].slug;
  const row = card.locator(`.repo-row[data-repo="${first}"]`);
  const config = row.locator('[data-disclosure="configure"]');
  await expect(config).toBeHidden();
  await row.locator(".btn.configure").click();
  await expect(config).toBeVisible();
  // The disclosure's layout (#620): a 12px indent, an 8px gap, centred rows.
  const layout = await config.evaluate((el) => {
    const css = getComputedStyle(el);
    return { display: css.display, paddingLeft: css.paddingLeft, gap: css.columnGap, alignItems: css.alignItems };
  });
  expect(layout).toEqual({ display: "flex", paddingLeft: "12px", gap: "8px", alignItems: "center" });
  // `input.input` inside the disclosure, not the id: the slug carries a `/`,
  // which a CSS id selector cannot spell unescaped.
  await config.locator("input.input").fill("release.yml, deploy.yml");
  await config.locator("input.input").blur();
  expect(await calls(page, "settings_set_repo_workflows")).toEqual([
    {
      command: "settings_set_repo_workflows",
      args: { slug: first, workflows: "release.yml, deploy.yml" },
    },
  ]);

  // `click`, not `uncheck`: the mutation re-renders the tab from the payload
  // the stub returns (unchanged, by design), so `uncheck`'s own read-back of
  // the checked state would fight the render forever. The assertion that
  // matters is which command the toggle sent.
  await row.locator(".toggle").first().click();
  expect(await calls(page, "settings_set_repo_enabled")).toEqual([
    { command: "settings_set_repo_enabled", args: { slug: first, enabled: false } },
  ]);
});

/**
 * The search that replaced the Configure repos… modal: matches as you type
 * over ~900 repos, the match in bold, the row badges the picker carried, and
 * the untrack two-step exactly when Rust sent a consequence.
 */
test("searching lists matches as you type and two-steps a costly untrack", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const { owning, card, field } = await openRepos(page, settings);
  const t = settings.accounts;
  await expect(field).toBeEnabled();
  await expect(field).toHaveAttribute("placeholder", t.searchPlaceholder);
  await expect(page.locator(`label[for="repo-search-${owning.id}"]`)).toHaveText(t.searchLabel);
  await expect(rowsOf(card), "an empty box lists nothing").toHaveCount(0);

  await field.fill("claims");
  await expect(rowsOf(card)).toHaveCount(5);
  await expect(card.locator(".search-row b")).toHaveText(Array(5).fill("claims"));
  await expect(rowsOf(card).first().locator(".slug")).toHaveText("acme/claims-api");
  await expect(card.locator(".search-count")).toHaveText(`5 of ${GRANTED.length} match`);

  // Watched by the other account: named, and not this card's to tick.
  const foreign = card.locator('[data-pick="acme/claims-web"]');
  await expect(foreign).toContainText(t.fetchedByLabel);
  await expect(foreign).toContainText("personal");
  await expect(foreign.locator(".toggle")).toBeDisabled();

  await card.locator('[data-pick="acme/claims-api"] .toggle').click();
  expect(await calls(page, "settings_set_repo_tracked")).toEqual([
    { command: "settings_set_repo_tracked", args: { id: owning.id, slug: "acme/claims-api", tracked: true } },
  ]);

  await field.fill("vault");
  await expect(card.locator('[data-pick="acme/legacy-vault"]')).toContainText(t.archivedLabel);

  // This account's own: ticked and marked, and unticking waits for the
  // operator's answer to Rust's sentence, verbatim.
  await field.fill("gadget");
  const watched = card.locator('[data-pick="acme/gadget"]');
  await expect(watched.locator(".toggle")).toBeChecked();
  await expect(watched).toContainText(t.watchingLabel);
  const confirm = watched.locator('[data-confirm="untrack"]');
  await expect(confirm).toBeHidden();
  await watched.locator(".toggle").click();
  await expect(confirm).toBeVisible();
  await expect(confirm.locator(".confirm")).toHaveText(untrackPrompt("acme/gadget"));
  expect((await calls(page, "settings_set_repo_tracked")).length).toBe(1);
  await expect(confirm.locator(".btn.untrack-proceed")).toHaveText(t.untrackLabel);
  await confirm.locator(".btn.untrack-proceed").click();
  expect((await calls(page, "settings_set_repo_tracked")).at(-1)).toEqual({
    command: "settings_set_repo_tracked",
    args: { id: owning.id, slug: "acme/gadget", tracked: false },
  });
});

test("a query matching more than fifty says how many more", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const { card, field } = await openRepos(page, settings);
  await field.fill("svc");
  await expect(rowsOf(card)).toHaveCount(50);
  await expect(card.locator(".search-more")).toHaveText("850 more match — keep typing");
});

test("a full owner/name that isn't listed offers Add by name", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const { owning, card, field } = await openRepos(page, settings);
  // A listed repo is ticked, not added. The row first, so the answer has
  // landed before the absence is asserted.
  await field.fill("acme/claims-api");
  await expect(card.locator('[data-pick="acme/claims-api"]')).toBeVisible();
  await expect(card.locator(".btn.repo-add"), "a listed repo offers no add").toHaveCount(0);
  await field.fill("globex/ledger");
  const add = card.locator(".btn.repo-add");
  await expect(add).toHaveText("Add globex/ledger by name");
  await add.click();
  expect(await calls(page, "settings_add_repo")).toEqual([
    { command: "settings_add_repo", args: { slug: "globex/ledger", accountId: owning.id } },
  ]);
  // Submitted, so cleared, as the old slug field was.
  await expect(field).toHaveValue("");
});

test("ticking a result keeps the query and raises no unsaved prompt", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const { card, field } = await openRepos(page, settings);
  await field.fill("claims");
  await card.locator('[data-pick="acme/claims-api"] .toggle').click();
  await expect.poll(async () => (await calls(page, "settings_set_repo_tracked")).length).toBe(1);
  // The mutation re-renders the card; the box and its results come back.
  await expect(field).toHaveValue("claims");
  await expect(rowsOf(card)).toHaveCount(5);
  // A query is not an edit, so leaving asks nothing.
  await page.locator('.tab[data-tab="connections"]').click();
  await expect(page.locator(".connection-row").first()).toBeVisible();
  await expect(page.locator("#settingsConfirm")).toBeHidden();
});

test("a slow reply for an older query never overwrites the newer one", async ({ page, baseURL }) => {
  const settings = await fixture(baseURL, "sample-settings.json");
  const answer = searchOver(settings);
  const slowFirst = async (args) => {
    if (args.query === "cl") await new Promise((resolve) => setTimeout(resolve, 300));
    return answer(args);
  };
  await openSettings(page, baseURL, null, null, null, null, slowFirst);
  const { card, field } = await openRepos(page, settings);
  await expect(field).toBeEnabled();
  await field.fill("cl");
  await field.fill("claims");
  await expect(rowsOf(card)).toHaveCount(5);
  await page.waitForTimeout(400);
  expect(await slugsOf(card)).toEqual([
    "acme/claims-api", "acme/claims-batch", "acme/claims-docs", "acme/claims-etl", "acme/claims-web",
  ]);
  await expect(card.locator(".search-count")).toHaveText(`5 of ${GRANTED.length} match`);
});

test("a failed discovery says why instead of an empty list", async ({ page, baseURL }) => {
  const reason = "GitHub refused this account's token. Replace it in this account's settings.";
  const settings = await openSettings(page, baseURL, null, null, { id: "any", reason });
  const { card, field } = await openRepos(page, settings);
  await expect(card.locator("p.confirm", { hasText: reason })).toBeVisible();
  await expect(card).not.toContainText(settings.accounts.discoverPendingLabel);
  // Still open: a full owner/name can be added without the list.
  await expect(field).toBeEnabled();
});

test("a discovery call that never answers says so instead of loading forever", async ({ page, baseURL }) => {
  const refused = async () => { throw new Error("the IPC call was refused"); };
  const settings = await openSettings(page, baseURL, null, null, refused);
  const { card } = await openRepos(page, settings);
  await expect(card.locator("p.confirm", { hasText: settings.accounts.discoverFailedLabel })).toBeVisible();
  await expect(card).not.toContainText(settings.accounts.discoverPendingLabel);
});

test("the search box says it is loading until discovery answers", async ({ page, baseURL }) => {
  const settings = await fixture(baseURL, "sample-settings.json");
  const answer = defaultDiscovery(settings);
  const slow = async (args) => {
    await new Promise((resolve) => setTimeout(resolve, 300));
    return answer(args);
  };
  await openSettings(page, baseURL, null, null, slow);
  const { card, field } = await openRepos(page, settings);
  await expect(field).toBeDisabled();
  await expect(card.locator(".repo-search-note")).toHaveText(settings.accounts.discoverPendingLabel);
  await expect(field).toBeEnabled();
  await expect(card.locator(".repo-search-note")).toHaveText(`This token can read ${GRANTED.length} repos`);
  // Opening the editor asks once, and the re-render that follows doesn't ask again.
  expect((await calls(page, "settings_discover_repos")).length).toBe(1);
});

test("arrow keys and Enter toggle the highlighted result; Escape clears", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const { owning, card, field } = await openRepos(page, settings);
  await field.fill("claims");
  await expect(rowsOf(card)).toHaveCount(5);
  await field.press("ArrowDown");
  await expect(rowsOf(card).nth(0)).toHaveAttribute("data-active", "true");
  await field.press("ArrowDown");
  await expect(rowsOf(card).nth(1)).toHaveAttribute("data-active", "true");
  await expect(card.locator('.search-row[data-active="true"]')).toHaveCount(1);
  await field.press("ArrowUp");
  await field.press("ArrowDown");
  await field.press("Enter");
  await expect.poll(() => calls(page, "settings_set_repo_tracked")).toEqual([
    { command: "settings_set_repo_tracked", args: { id: owning.id, slug: "acme/claims-batch", tracked: true } },
  ]);
  // The re-render hands the box its focus back, caret at the end.
  await expect(field).toBeFocused();
  await expect(field).toHaveValue("claims");
  await field.press("Escape");
  await expect(field).toHaveValue("");
  await expect(rowsOf(card)).toHaveCount(0);
});

test("discovered organizations are offered on the card", async ({ page, baseURL }) => {
  const settings = await fixture(baseURL, "sample-settings.json");
  const { owning } = accountsOf(settings);
  const discover = {
    id: owning.id,
    total: 3,
    totalLabel: "This token can read 3 repos",
    truncatedNote: "the walk was cut short",
    orgs: [
      { name: "acme", selected: true, selectedBy: null },
      { name: "beta", selected: false, selectedBy: null },
      { name: "gamma", selected: false, selectedBy: "personal" },
    ],
    reason: null,
  };
  await openSettings(page, baseURL, null, null, discover);
  const { card } = await openRepos(page, settings);
  await expect(card, "a cut walk says so").toContainText(discover.truncatedNote);
  const orgs = card.locator(`[data-orgs="${owning.id}"]`);
  await expect(orgs.locator('[data-discovered-org="acme"]'), "already watched").toHaveCount(0);
  const gamma = orgs.locator('[data-discovered-org="gamma"]');
  await expect(gamma).toContainText(settings.accounts.watchedByLabel);
  await expect(gamma).toContainText("personal");
  await expect(gamma.locator(".org-watch")).toHaveCount(0);
  await orgs.locator('[data-discovered-org="beta"] .btn.org-watch').click();
  expect(await calls(page, "settings_set_account_org")).toEqual([
    { command: "settings_set_account_org", args: { id: owning.id, org: "beta", selected: true } },
  ]);
});

/**
 * The picker half of #292, relocated with the Portfolio tab's retirement.
 *
 * A repo added while exactly one account exists is attributed to it in Rust
 * and never reaches this control as a question. A repo that arrives
 * unattributed sits in its own section, and the Fetched-by picker (behind
 * Configure) is what resolves it — showing that state honestly rather than
 * pre-selecting the first account, which is the guess the whole change exists
 * to refuse.
 */
test("the unattributed section offers the picker, and a card's repo can be re-homed", async ({
  page,
  baseURL,
}) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "accounts").click();

  const t = settings.accounts;
  const { owning } = accountsOf(settings);
  const attributed = owning.repos[0];
  const orphan = t.unattributed[0];
  expect(orphan, "the fixture has no unattributed repo").toBeTruthy();

  const configOf = async (slug) => {
    const row = page.locator(`.repo-row[data-repo="${slug}"]`);
    await row.locator(".btn.configure").click();
    return row.locator("select.input");
  };

  await tab(page, "connections").click();
  await page.locator(".connection-orphans").click();
  const orphanPicker = await configOf(orphan.slug);
  await expect(
    orphanPicker,
    "an unattributed repo shows the unattributed option, not the first account"
  ).toHaveValue("");
  // Every account, plus that option.
  await expect(orphanPicker.locator("option")).toHaveCount(t.accountOptions.length);

  const chosen = t.accountOptions.find((option) => option.value);
  await orphanPicker.selectOption(chosen.value);
  expect(await calls(page, "settings_set_repo_account")).toEqual([
    {
      command: "settings_set_repo_account",
      args: { slug: orphan.slug, accountId: chosen.value },
    },
  ]);

  // …and back the other way: a card's repo starts on its own account, and the
  // empty option is `null` on the wire — Rust's "unattributed" rather than an
  // id of no characters.
  await openAccountCard(page, owning.id);
  const cardPicker = await configOf(attributed.slug);
  await expect(cardPicker).toHaveValue(owning.id);
  await cardPicker.selectOption("");
  expect(await calls(page, "settings_set_repo_account")).toContainEqual({
    command: "settings_set_repo_account",
    args: { slug: attributed.slug, accountId: null },
  });
});

/** The two accounts the fixture carries: one with a token and repos depending
 *  on it, one with neither. Named by what they are *for* rather than by index,
 *  so a fixture reordering fails loudly here instead of quietly asserting the
 *  wrong row. */
const accountsOf = (settings) => {
  const rows = settings.accounts.rows;
  const owning = rows.find((row) => row.repos.length > 0);
  const spare = rows.find((row) => row.repos.length === 0);
  expect(owning, "the fixture has no account with repos attributed").toBeTruthy();
  expect(spare, "the fixture has no account with nothing attributed").toBeTruthy();
  return { owning, spare };
};

test("Accounts lists every account with its credential badge and what it fetches", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "accounts").click();

  const t = settings.accounts;
  await expect(page.locator(".group[data-account]")).toHaveCount(1);
  const { owning, spare } = accountsOf(settings);

  const first = page.locator(`.group[data-account="${owning.id}"]`);
  await expect(first.locator(".group-hdr")).toHaveText(owning.label);
  await expect(first.locator(".dim").first()).toHaveText(owning.vendorLabel);
  // The slugs themselves, on the card that removes them.
  for (const repo of owning.repos) await expect(first).toContainText(repo.slug);
  await expect(first.locator(".badge-ok")).toHaveText(t.tokenStoredLabel);

  // An account with no token yet says so — `stored: false` is a fact the
  // payload carries, not a key it omits.
  await openAccountCard(page, spare.id);
  const second = page.locator(`.group[data-account="${spare.id}"]`);
  await expect(second.locator(".badge-dim")).toHaveText(t.noTokenLabel);
  await expect(second).toContainText(t.noReposLabel);
});

// MARK: GitHub accounts' steps: a token and a name, then the organizations, keyed by account

const accountRow = (settings, id) => settings.connections.rows.find((row) => row.entityId === id);
const walkWrites = async (page) =>
  (await writesOf(page)).filter((command) => command !== "settings_view");

/**
 * Removing an account leaves its repos tracked but unattributed, so the repos
 * are named *before* the removal rather than discovered after it. Remove source
 * sits on the account's own steps, and its Ready step carries Rust's sentence;
 * nothing is sent until Finish.
 */
test("removing an account that repos depend on names them and waits for Finish", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const { owning, spare } = accountsOf(settings);
  await openConnection(page, `account:${owning.id}`);
  await page.locator(".walk-remove").click();
  await expect(stepHeading(page)).toHaveText(settings.connections.walkLabels.removeHeading);
  // Rust's sentence, verbatim — this file writes none of it.
  await expect(page.locator(".walk")).toContainText(accountRow(settings, owning.id).walk.removeHelp);
  expect(await walkWrites(page), "nothing may be removed before Finish").toEqual([]);
  await page.locator(".walk-finish").click();
  expect(await saves(page)).toEqual([
    {
      command: "settings_save_source",
      args: { kind: "account", row: owning.id, parts: [], secret: null, addTile: true },
    },
  ]);
  // Only that account: the other account's row was never named.
  expect(JSON.stringify(await saves(page))).not.toContain(spare.id);
});

/** An account nothing depends on says the generic sentence, not a warning. */
test("an account nothing depends on is removed with the plain sentence", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const { spare } = accountsOf(settings);
  await openConnection(page, `account:${spare.id}`);
  await page.locator(".walk-remove").click();
  await expect(page.locator(".walk")).toContainText(settings.connections.walkLabels.removeHelp);
  await page.locator(".walk-finish").click();
  const [save] = await saves(page);
  expect(save.args).toMatchObject({ kind: "account", row: spare.id, parts: [] });
});

test("Add source → GitHub creates an account only at Finish, with its token and organizations", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await addConnection(page, "account");
  const entry = settings.connections.catalog.find((item) => item.kind === "account");
  await expect(stepLine(page)).toHaveText("GitHub · step 1 of 3");
  await expect(page.locator(".walk-pick"), "both parts are always set up").toHaveCount(0);
  await expect(page.locator(".walk-remove"), "a new account has nothing to remove").toHaveCount(0);
  await expect(page.locator(".walk-card"), "a new account has no card yet").toHaveCount(0);
  await expect(stepHeading(page)).toHaveText(entry.walk.parts[0].title);

  // A token is required to continue, and nothing is written for typing it.
  await page.locator("#walk-name").fill("personal-2");
  await next(page);
  await expect(page.locator(".walk-problem")).toBeVisible();
  await page.locator("#walk-secret").fill("ghp_secret");
  await next(page);
  await expect(stepHeading(page)).toHaveText(entry.walk.parts[1].title);
  await page.locator("#walk-orgs").fill("acme, globex");
  await next(page);
  await expect(stepHeading(page)).toHaveText(settings.connections.walkLabels.readyHeading);
  expect(await walkWrites(page), "nothing is applied before Finish").toEqual([]);

  await page.locator(".walk-finish").click();
  expect(await saves(page)).toEqual([
    {
      command: "settings_save_source",
      args: {
        kind: "account", row: null, parts: ["token", "orgs"], secret: "ghp_secret",
        inputs: { name: "personal-2", orgs: "acme, globex" }, addTile: true,
      },
    },
  ]);
  // The token is written through the one save, never `settings_save_account`
  // or `settings_save_secret`.
  expect(await walkWrites(page)).toEqual(["settings_save_source"]);
});

test("Back and Cancel leave nothing written for a new account or an existing one", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const { owning } = accountsOf(settings);

  await addConnection(page, "account");
  await page.locator("#walk-name").fill("draft");
  await page.locator("#walk-secret").fill("ghp_draft");
  await next(page);
  await page.locator("#walk-orgs").fill("draft-org");
  await page.locator(".walk-back").click();
  await expect(page.locator("#walk-name")).toHaveValue("draft");
  await next(page);
  await next(page);
  await page.locator(".walk-back").click();
  await page.locator(".walk-cancel").click();
  await expect(page.locator(".walk")).toHaveCount(0);

  await openConnection(page, `account:${owning.id}`);
  await page.locator("#walk-name").fill("renamed");
  await next(page);
  await page.locator("#walk-orgs").fill("someone-elses");
  await page.locator(".walk-back").click();
  await page.locator(".walk-cancel").click();

  expect(await walkWrites(page), "no account, token or organization change").toEqual([]);
  expect(await calls(page, "settings_set_account_org")).toEqual([]);
});

test("an existing account's row opens its own steps, filled from that account only", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const { owning, spare } = accountsOf(settings);

  await openConnection(page, `account:${owning.id}`);
  await expect(page.locator("#walk-name")).toHaveValue(owning.label);
  await expect(page.locator(".walk-stored")).toBeVisible();
  await expect(page.locator(".walk-remove")).toBeVisible();
  await next(page);
  await expect(page.locator("#walk-orgs")).toHaveValue(owning.orgs.join(", "));
  await page.locator("#walk-orgs").fill(`${owning.orgs.join(", ")}, zeta`);
  await next(page);
  await page.locator(".walk-finish").click();
  const [first] = await saves(page);
  expect(first.args).toEqual({
    kind: "account", row: owning.id, parts: ["token", "orgs"], secret: null,
    inputs: { name: owning.label, orgs: `${owning.orgs.join(", ")}, zeta` }, addTile: true,
  });

  // The card Finish lands on is this account's; opening the other account's row
  // afterwards starts from that account, not from the draft just finished.
  await expect(page.locator(`.group[data-account="${owning.id}"]`)).toBeVisible();
  await page.locator(".connection-back").click();
  await openConnection(page, `account:${spare.id}`);
  await expect(page.locator("#walk-name")).toHaveValue(spare.label);
  await expect(page.locator(".walk-stored"), "this account has no token").toHaveCount(0);
  await page.locator("#walk-secret").fill("ghp_spare");
  await next(page);
  await expect(page.locator("#walk-orgs")).toHaveValue(spare.orgs.join(", "));
  await next(page);
  await page.locator(".walk-finish").click();
  const second = (await saves(page))[1];
  expect(second.args.row).toBe(spare.id);
  expect(second.args.secret).toBe("ghp_spare");
  expect(second.args.inputs.orgs).toBe(spare.orgs.join(", "));
});

test("two accounts are set up one after the other without either's steps touching the other", async ({ page, baseURL }) => {
  await openSettings(page, baseURL);
  for (const [name, token, orgs] of [["first", "ghp_one", "acme"], ["second", "ghp_two", ""]]) {
    await addConnection(page, "account");
    await expect(page.locator("#walk-name"), "a new account starts empty").toHaveValue("");
    await page.locator("#walk-name").fill(name);
    await page.locator("#walk-secret").fill(token);
    await next(page);
    await expect(page.locator("#walk-orgs")).toHaveValue("");
    await page.locator("#walk-orgs").fill(orgs);
    await next(page);
    await page.locator(".walk-finish").click();
    await expect(page.locator(".walk")).toHaveCount(0);
    await page.locator('.tab[data-tab="connections"]').click();
  }
  const [one, two] = await saves(page);
  expect(one.args).toMatchObject({ row: null, secret: "ghp_one", inputs: { name: "first", orgs: "acme" } });
  expect(two.args).toMatchObject({ row: null, secret: "ghp_two", inputs: { name: "second", orgs: "" } });
});

test("a refusal at Finish keeps the account's steps open with what was typed", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await page.evaluate((view) => {
    window.__SAVE_SOURCE__ = { status: "Skipped — an account with that name already exists.", settings: view, saved: false };
  }, settings);
  await addConnection(page, "account");
  await page.locator("#walk-name").fill("taken");
  await page.locator("#walk-secret").fill("ghp_x");
  await next(page);
  await next(page);
  await page.locator(".walk-finish").click();
  await expect(page.locator("#settingsStatus")).toHaveText("Skipped — an account with that name already exists.");
  await expect(page.locator(".walk-finish")).toBeEnabled();
});

test("the card keeps repo tracking and reaches the steps and back", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const { owning } = accountsOf(settings);
  await openConnection(page, `account:${owning.id}`);
  await page.locator(".walk-card").click();
  const card = page.locator(`.group[data-account="${owning.id}"]`);
  await expect(card).toBeVisible();
  // What the steps own is not editable here.
  await expect(card.locator(".org-add, .org-remove, .rename, .replace-token, .btn.delete")).toHaveCount(0);
  await expect(card.locator(`#account-org-${owning.id}`)).toHaveCount(0);
  await card.locator(".edit-account").click();
  await expect(page.locator("#walk-name")).toHaveValue(owning.label);
});

// MARK: Azure Cost's steps: two parts, no key, and inputs a ticked part must have

/** A store where Azure Cost is not added, whatever the fixture holds. */
function withoutAzure(settings) {
  const entry = catalogEntry(settings, "azure");
  for (const part of entry.walk.parts) {
    part.on = false;
    for (const input of part.inputs) input.value = "";
  }
  entry.walk.tile.present = false;
  settings.connections.rows = settings.connections.rows.filter((row) => row.kind !== "azure");
}

/** Every command sent that is not a known read: the stub answers any unknown
 *  command with "Saved.", so a stray write of any name (`dashboard_save`,
 *  `settings_clear_secret`, `panel_interval_save`...) would otherwise pass
 *  unseen. A Finish on Azure Cost's steps must leave exactly
 *  `["settings_save_source"]`, and Back and Cancel `[]`. */
const READS = new Set([
  "cockpit", "dashboard_view", "settings_view", "settings_test_host",
  // The panels' own reads, which the overview polls under Settings.
  "containers", "repos", "runners", "anthropic", "openai", "usage", "azure_cost",
  "services", "crons", "openclaw",
  "settings_probe_status_vendor", "settings_probe_host_certificate",
  "settings_discover_repos", "settings_search_repos",
]);
const writesOf = (page) =>
  page.evaluate(
    (reads) => window.__CALLS__.map((call) => call.command).filter((command) => !reads.includes(command)),
    [...READS],
  );

test("Azure Cost asks which parts, then each picked part's values, and never for a key", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL, undefined, undefined, undefined, withoutAzure);
  const entry = catalogEntry(settings, "azure");
  const labels = settings.connections.walkLabels;
  await addConnection(page, "azure");

  await expect(stepHeading(page)).toHaveText(labels.pickHeading);
  await expect(page.locator(".walk-pick")).toHaveCount(2);
  await expect(page.locator(".walk-next"), "nothing to set up yet").toBeDisabled();
  await pick(page, "export").locator("input").check();
  await pick(page, "budget").locator("input").check();
  await expect(stepLine(page)).toHaveText("Azure Cost · step 1 of 4");
  await next(page);

  await expect(stepHeading(page)).toHaveText(partOf(entry, "export").title);
  await expect(page.locator("#walk-secret"), "the az CLI session signs each read").toHaveCount(0);
  await expect(page.locator('[data-secret]'), "no Save that writes before Finish").toHaveCount(0);
  await page.locator("#walk-account").fill("acmestorage");
  await page.locator("#walk-container").fill("cost-exports");
  await next(page);
  await expect(stepHeading(page)).toHaveText(partOf(entry, "budget").title);
  await page.locator("#walk-budgetUsd").fill("250");
  await next(page);

  await expect(stepHeading(page)).toHaveText(labels.readyHeading);
  await expect(page.locator(".walk-tile")).toHaveText(entry.walk.tile.label);
  expect(await saves(page), "nothing is applied before Finish").toEqual([]);
  await page.locator(".walk-finish").click();
  const [save] = await saves(page);
  expect(save.args).toEqual({
    kind: "azure", parts: ["export", "budget"], secret: null,
    inputs: { account: "acmestorage", container: "cost-exports", budgetUsd: "250" }, addTile: true,
  });
  expect(await calls(page, "settings_save_secret")).toEqual([]);
  expect(await writesOf(page)).toEqual(["settings_save_source"]);
  await expect(page.locator(".walk")).toHaveCount(0);
});

test("Azure Cost's budget alone sends only the budget part's value", async ({ page, baseURL }) => {
  await openSettings(page, baseURL, undefined, undefined, undefined, withoutAzure);
  await addConnection(page, "azure");
  await pick(page, "budget").locator("input").check();
  await expect(stepLine(page)).toHaveText("Azure Cost · step 1 of 3");
  await next(page);
  await page.locator("#walk-budgetUsd").fill("40");
  await next(page);
  await page.locator(".walk-finish").click();
  expect((await saves(page))[0].args).toEqual({
    kind: "azure", parts: ["budget"], secret: null, inputs: { budgetUsd: "40" }, addTile: true,
  });
  expect(await writesOf(page)).toEqual(["settings_save_source"]);
});

// Back keeps the draft, Cancel forgets it, and neither writes anything.
for (const [name, picks, fields] of [
  ["Cost export", ["export"], ["account", "container"]],
  ["Monthly budget", ["budget"], ["budgetUsd"]],
  ["both parts", ["export", "budget"], ["account", "container", "budgetUsd"]],
]) {
  test(`Azure Cost (${name}): Back and Cancel write nothing, and Cancel forgets the draft`, async ({ page, baseURL }) => {
    await openSettings(page, baseURL, undefined, undefined, undefined, withoutAzure);
    await addConnection(page, "azure");
    for (const id of picks) await pick(page, id).locator("input").check();
    await next(page);
    // Walk every field step, typing into each, then step back over the last.
    const typed = { account: "acmestorage", container: "cost-exports", budgetUsd: "250" };
    for (const id of picks) {
      for (const field of fields.filter((f) => (id === "budget") === (f === "budgetUsd"))) {
        await page.locator(`#walk-${field}`).fill(typed[field]);
      }
      await next(page);
    }
    await page.locator(".walk-back").click();
    const last = fields[fields.length - 1];
    await expect(page.locator(`#walk-${last}`), "Back keeps what was typed").toHaveValue(typed[last]);

    await page.locator(".walk-cancel").click();
    await expect(page.locator(".connection-list")).toBeVisible();
    expect(await saves(page)).toEqual([]);
    expect(await calls(page, "settings_save_secret")).toEqual([]);
    expect(await writesOf(page)).toEqual([]);

    await addConnection(page, "azure");
    for (const id of picks) await pick(page, id).locator("input").check();
    await next(page);
    for (const id of picks) {
      for (const field of fields.filter((f) => (id === "budget") === (f === "budgetUsd"))) {
        await expect(page.locator(`#walk-${field}`)).toHaveValue("");
      }
      await next(page);
    }
  });
}

test("a refusal from Rust leaves the Azure steps open with what was typed", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL, undefined, undefined, undefined, withoutAzure);
  await page.evaluate((view) => {
    window.__SAVE_SOURCE__ = {
      status: "Enter the storage account that holds the cost export.", settings: view, saved: false,
    };
  }, settings);
  await addConnection(page, "azure");
  await pick(page, "export").locator("input").check();
  await next(page);
  await page.locator("#walk-container").fill("cost-exports");
  await next(page);
  await page.locator(".walk-finish").click();
  await expect(page.locator("#settingsStatus")).toHaveText("Enter the storage account that holds the cost export.");
  await expect(page.locator(".walk")).toBeVisible();
  await page.locator(".walk-back").click();
  await expect(page.locator("#walk-container")).toHaveValue("cost-exports");
});

test("Azure Cost's row opens the same steps with both parts ticked and the saved values", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const entry = catalogEntry(settings, "azure");
  await page.locator('.connection-row[data-kind="azure"]').click();
  await expect(pick(page, "export").locator("input")).toBeChecked();
  await expect(pick(page, "budget").locator("input")).toBeChecked();
  await next(page);
  await expect(page.locator("#walk-account")).toHaveValue(partOf(entry, "export").inputs[0].value);
  await expect(page.locator("#walk-container")).toHaveValue(partOf(entry, "export").inputs[1].value);
  await next(page);
  await expect(page.locator("#walk-budgetUsd")).toHaveValue(partOf(entry, "budget").inputs[0].value);

  // Unticking the budget sends only the export's values: the budget is forgotten.
  await page.locator(".walk-back").click();
  await page.locator(".walk-back").click();
  await pick(page, "budget").locator("input").uncheck();
  await next(page);
  await next(page);
  await page.locator(".walk-finish").click();
  const [save] = await saves(page);
  expect(save.args.parts).toEqual(["export"]);
  expect(Object.keys(save.args.inputs)).toEqual(["account", "container"]);
  expect(await calls(page, "settings_save_secret")).toEqual([]);
  expect(await writesOf(page)).toEqual(["settings_save_source"]);
});

test("Azure Cost: Remove source sends no parts, and only settings_save_source writes", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await page.locator('.connection-row[data-kind="azure"]').click();
  await page.locator(".walk-remove").click();
  await expect(stepHeading(page)).toHaveText(settings.connections.walkLabels.removeHeading);
  await page.locator(".walk-finish").click();
  expect((await saves(page))[0].args).toEqual({ kind: "azure", parts: [], secret: null, addTile: true });
  expect(await writesOf(page)).toEqual(["settings_save_source"]);
});

// MARK: Sentry's steps: two parts over one token and one slug

/** A store where Sentry is not added, whatever the fixture holds. */
function withoutSentry(settings) {
  const entry = catalogEntry(settings, "sentry");
  entry.walk.added = false;
  for (const part of entry.walk.parts) {
    part.on = false;
    part.secret.stored = false;
    // Neither tile is on the overview yet, so Ready offers both.
    part.tile.present = false;
    for (const input of part.inputs) input.value = "";
  }
  settings.connections.rows = settings.connections.rows.filter((row) => row.kind !== "sentry");
}

test("Sentry asks which parts, then its token and slug once, whichever are picked", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL, undefined, undefined, undefined, withoutSentry);
  const entry = catalogEntry(settings, "sentry");
  const labels = settings.connections.walkLabels;
  await addConnection(page, "sentry");

  await expect(stepHeading(page)).toHaveText(labels.pickHeading);
  await expect(page.locator(".walk-pick")).toHaveCount(2);
  await expect(pick(page, "usage").locator("input")).not.toBeChecked();
  await expect(pick(page, "crons").locator("input")).not.toBeChecked();
  await expect(page.locator(".walk-next"), "nothing to set up yet").toBeDisabled();

  await pick(page, "usage").locator("input").check();
  await pick(page, "crons").locator("input").check();
  await expect(stepLine(page)).toHaveText("Sentry · step 1 of 3");
  await next(page);

  // The first picked part asks for the token and the slug, and the quota that
  // is the Usage tile's alone.
  await expect(stepHeading(page)).toHaveText(partOf(entry, "usage").title);
  await expect(stepLine(page)).toHaveText("Sentry · step 2 of 3");
  await expect(page.locator("#walk-secret")).toHaveAttribute("type", "password");
  await expect(page.locator('[data-secret]'), "no Save that writes before Finish").toHaveCount(0);
  await next(page);
  await expect(page.locator(".walk-problem")).toHaveText(labels.keyRequired);

  await page.locator("#walk-secret").fill("sntrys_secret");
  await page.locator("#walk-orgSlug").fill("acme-fond-sea");
  await page.locator(".connection-advanced summary").click();
  await page.locator("#walk-quota").fill("5000");
  await next(page);

  // Scheduled jobs has nothing left to ask, so the next step is Ready, with one
  // offer covering both tiles.
  await expect(stepHeading(page)).toHaveText(labels.readyHeading);
  await expect(page.locator("#walk-secret")).toHaveCount(0);
  await expect(page.locator(".walk-summary li")).toHaveCount(2);
  await expect(page.locator(".walk-tile")).toHaveText("Add a Usage tile and a Sentry Crons tile");
  expect(await saves(page), "nothing is applied before Finish").toEqual([]);
  expect(await calls(page, "settings_save_secret")).toEqual([]);

  await page.locator(".walk-finish").click();
  const [save] = await saves(page);
  expect(save.args).toEqual({
    kind: "sentry", parts: ["usage", "crons"], secret: "sntrys_secret",
    inputs: { orgSlug: "acme-fond-sea", quota: "5000" }, addTile: true,
  });
});

test("Sentry picked for Scheduled jobs alone asks the token and slug there, and no quota", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL, undefined, undefined, undefined, withoutSentry);
  const entry = catalogEntry(settings, "sentry");
  await addConnection(page, "sentry");
  await pick(page, "crons").locator("input").check();
  await next(page);

  await expect(stepHeading(page)).toHaveText(partOf(entry, "crons").title);
  await expect(page.locator("#walk-secret")).toBeVisible();
  await expect(page.locator("#walk-orgSlug")).toBeVisible();
  await expect(page.locator("#walk-quota")).toHaveCount(0);
  await page.locator("#walk-secret").fill("sntrys_secret");
  await page.locator("#walk-orgSlug").fill("acme");
  await next(page);
  await expect(page.locator(".walk-tile")).toHaveText("Add a Sentry Crons tile");
  await page.locator(".walk-finish").click();
  const [save] = await saves(page);
  expect(save.args).toEqual({
    kind: "sentry", parts: ["crons"], secret: "sntrys_secret", inputs: { orgSlug: "acme" }, addTile: true,
  });
});

test("Sentry picked for Usage alone leaves the other tile's feed off", async ({ page, baseURL }) => {
  await openSettings(page, baseURL, undefined, undefined, undefined, withoutSentry);
  await addConnection(page, "sentry");
  await pick(page, "usage").locator("input").check();
  await next(page);
  await page.locator("#walk-secret").fill("sntrys_secret");
  await next(page);
  await expect(page.locator(".walk-summary li").nth(1)).toHaveClass(/walk-off/);
  await expect(page.locator(".walk-tile")).toHaveText("Add a Usage tile");
  await page.locator(".walk-finish").click();
  const [save] = await saves(page);
  expect(save.args).toMatchObject({ kind: "sentry", parts: ["usage"], secret: "sntrys_secret" });
});

test("Sentry: Back and Cancel write nothing, and Cancel forgets the draft", async ({ page, baseURL }) => {
  await openSettings(page, baseURL, undefined, undefined, undefined, withoutSentry);
  await addConnection(page, "sentry");
  await pick(page, "usage").locator("input").check();
  await next(page);
  await page.locator("#walk-secret").fill("sntrys_secret");
  await page.locator("#walk-orgSlug").fill("acme");
  await next(page);
  await page.locator(".walk-back").click();
  await expect(page.locator("#walk-secret"), "Back keeps what was typed").toHaveValue("sntrys_secret");
  await expect(page.locator("#walk-orgSlug")).toHaveValue("acme");

  await page.locator(".walk-cancel").click();
  await expect(page.locator(".connection-list")).toBeVisible();
  expect(await saves(page)).toEqual([]);
  expect(await calls(page, "settings_save_secret")).toEqual([]);

  await addConnection(page, "sentry");
  await pick(page, "usage").locator("input").check();
  await next(page);
  await expect(page.locator("#walk-secret")).toHaveValue("");
  await expect(page.locator("#walk-orgSlug")).toHaveValue("");
});

test("Sentry's row opens the same steps with both parts ticked and the saved slug", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const entry = catalogEntry(settings, "sentry");
  await page.locator('.connection-row[data-kind="sentry"]').click();
  await expect(pick(page, "usage").locator("input")).toBeChecked();
  await expect(pick(page, "crons").locator("input")).toBeChecked();
  await next(page);
  await expect(page.locator("#walk-orgSlug")).toHaveValue(partOf(entry, "usage").inputs[0].value);
  await expect(page.locator(".walk-stored")).toHaveText(partOf(entry, "usage").secret.storedLabel);
  await expect(page.locator("#walk-secret")).toHaveValue("");

  // Unticking one part: the token is kept by leaving the field empty, and the
  // other part's slug is still sent.
  await page.locator(".walk-back").click();
  await pick(page, "crons").locator("input").uncheck();
  await next(page);
  await next(page);
  await page.locator(".walk-finish").click();
  const [save] = await saves(page);
  expect(save.args).toMatchObject({ kind: "sentry", parts: ["usage"], secret: null });
  expect(save.args.inputs.orgSlug).toBe(partOf(entry, "usage").inputs[0].value);
});

test("Sentry: Remove source sends no parts, so Finish turns both parts off and clears the token", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await page.locator('.connection-row[data-kind="sentry"]').click();
  await page.locator(".walk-remove").click();
  await expect(stepHeading(page)).toHaveText(settings.connections.walkLabels.removeHeading);
  expect(await saves(page), "and it is still a draft").toEqual([]);
  await page.locator(".walk-finish").click();
  const [save] = await saves(page);
  expect(save.args).toEqual({ kind: "sentry", parts: [], secret: null, addTile: true });
});

test("a stored Sentry token with both parts off is still an added source that can be removed", async ({ page, baseURL }) => {
  await openSettings(page, baseURL, undefined, undefined, undefined, (settings) => {
    const entry = catalogEntry(settings, "sentry");
    entry.walk.added = true;
    for (const part of entry.walk.parts) part.on = false;
  });
  await page.locator('.connection-row[data-kind="sentry"]').click();
  await expect(page.locator(".walk-next"), "nothing picked is how a source is removed").toBeEnabled();
  await expect(page.locator(".walk-remove")).toBeVisible();
  await page.locator(".walk-remove").click();
  await page.locator(".walk-finish").click();
  expect((await saves(page))[0].args).toEqual({ kind: "sentry", parts: [], secret: null, addTile: true });
});

test("Sentry with a credential store that did not answer says so and finishes without a typed key", async ({ page, baseURL }) => {
  await openSettings(page, baseURL, undefined, undefined, undefined, (settings) => {
    const entry = catalogEntry(settings, "sentry");
    for (const part of entry.walk.parts) {
      part.secret.storedLabel = "The credential store didn\u2019t answer. Leave this empty to keep any token it holds.";
    }
    const row = settings.connections.rows.find((item) => item.kind === "sentry");
    row.status = { text: "Not read", attention: true, color: "#f0b400" };
  });
  await expect(page.locator('.connection-row[data-kind="sentry"] .connection-state')).toHaveText("Not read");
  await page.locator('.connection-row[data-kind="sentry"]').click();
  await next(page);
  await expect(page.locator(".walk-stored")).toContainText("didn\u2019t answer");
  await next(page);
  await page.locator(".walk-finish").click();
  expect((await saves(page))[0].args).toMatchObject({ kind: "sentry", secret: null });
});

test("Services lists every watched status page with the component it watches", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await openConnection(page, `vendor:${settings.services.rows[0].id}`);

  const t = settings.services;
  await expect(page.locator(".vendor-row")).toHaveCount(1);
  const first = page.locator(`.vendor-row[data-vendor="${t.rows[0].id}"]`);
  await expect(first.locator(".host-name")).toHaveText(t.rows[0].label);
  await expect(first.locator(".dim")).toHaveText(t.rows[0].baseUrl);
  // The component's own name, beside the id that is actually polled: the
  // pairing is what makes a vendor's later rename visible.
  await expect(first).toContainText(t.rows[0].component);
  await expect(first.locator(".toggle")).toBeChecked({ checked: t.rows[0].enabled });

  await first.locator(".btn.delete").click();
  expect(await calls(page, "settings_remove_status_vendor")).toEqual([
    { command: "settings_remove_status_vendor", args: { id: t.rows[0].id } },
  ]);
});

/**
 * The whole reason the probe reports findings rather than a boolean.
 *
 * A failure renders the sentence Rust produced and offers no component picker
 * at all — an empty picker would turn "we could not look" into "this page has
 * no components", which is a different fact with a different fix.
 */
test("a failed probe shows its reason inline and offers nothing to pick", async ({ page, baseURL }) => {
  const reason = "that page is JSON but lists no components, so it isn't a Statuspage";
  await openSettings(page, baseURL, {
    baseUrl: "https://neonstatus.com",
    components: null,
    reason,
  });
  await tab(page, "services").click();

  await page.locator("#vendor-url").fill("https://neonstatus.com");
  await page.locator(".btn.probe").click();

  await expect(page.locator(".reason")).toHaveText(reason);
  await expect(page.locator("#vendor-component")).toHaveCount(0);
  await expect(page.locator("#vendor-name")).toHaveCount(0);
  // The address that was rejected is still on screen, or it cannot be corrected.
  await expect(page.locator("#vendor-url")).toHaveValue("https://neonstatus.com");
});

test("a probe that finds components adds a vendor in two steps", async ({ page, baseURL }) => {
  await openSettings(page, baseURL, {
    baseUrl: "https://status.example.org",
    components: [
      { id: "k8w3r06qmzrp", name: "API" },
      { id: "3f2p8q1x7z0d", name: "Dashboard" },
    ],
    reason: null,
  });
  await tab(page, "services").click();

  await page.locator("#vendor-url").fill("https://status.example.org/");
  await page.locator(".btn.probe").click();
  expect(await calls(page, "settings_probe_status_vendor")).toEqual([
    {
      command: "settings_probe_status_vendor",
      args: { baseUrl: "https://status.example.org/" },
    },
  ]);

  // Step two exists only now, and carries every component the host named.
  await expect(page.locator(".reason")).toHaveCount(0);
  await expect(page.locator("#vendor-component option")).toHaveText(["API", "Dashboard"]);
  // Refilled from the answer, so the field shows the address that was actually
  // read rather than the raw text.
  await expect(page.locator("#vendor-url")).toHaveValue("https://status.example.org");

  // Nothing to add until it has a name -- a hint, and Rust re-checks it.
  const save = page.locator(".btn.add");
  await expect(save).toBeDisabled();
  await page.locator("#vendor-name").fill("Example");
  await page.locator("#vendor-component").selectOption("3f2p8q1x7z0d");
  await save.click();

  expect(await calls(page, "settings_save_status_vendor")).toEqual([
    {
      command: "settings_save_status_vendor",
      args: {
        baseUrl: "https://status.example.org",
        label: "Example",
        // Both halves of the component, stored together.
        componentId: "3f2p8q1x7z0d",
        componentLabel: "Dashboard",
      },
    },
  ]);
});

/**
 * A component list belongs to the address it was read from. Editing the
 * address after an answer must retract the picker, or the previous host's
 * component would be stored against the new host's URL — a row polling
 * something nobody chose.
 */
test("editing the address after a probe retracts the component picker", async ({ page, baseURL }) => {
  await openSettings(page, baseURL, {
    baseUrl: "https://status.example.org",
    components: [{ id: "k8w3r06qmzrp", name: "API" }],
    reason: null,
  });
  await tab(page, "services").click();

  await page.locator("#vendor-url").fill("https://status.example.org");
  await page.locator(".btn.probe").click();
  await expect(page.locator("#vendor-component")).toHaveCount(1);

  await page.locator("#vendor-url").fill("https://status.other.example");
  await expect(page.locator("#vendor-component")).toHaveCount(0);
  await expect(page.locator("#vendor-name")).toHaveCount(0);
  // The typed address survives, because retracting the picker must not rebuild
  // the field under the caret.
  await expect(page.locator("#vendor-url")).toHaveValue("https://status.other.example");
});

test("About names the app, its version and its links", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "about").click();

  await expect(page.locator(".about-name")).toHaveText(settings.about.name);
  await expect(page.locator(".settings-body")).toContainText(settings.about.version);
  await expect(page.locator(".link-row")).toHaveCount(settings.about.links.length);
  await expect(page.locator(".link-row .link-url").first()).toHaveText(settings.about.links[0].url);
  // Shown as text, never as an anchor: following one would navigate the
  // cockpit's own webview away from the app.
  await expect(page.locator(".settings-body a")).toHaveCount(0);
});

test("About offers the update Rust found, with the label Rust chose", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "about").click();

  const updates = settings.about.updates;
  const group = page.locator('.group[data-group="updates"]');
  await expect(group.locator(".group-hdr")).toHaveText(updates.heading);
  await expect(group.locator(".update-status")).toHaveText(updates.status.text);
  await expect(group.locator(".update-notes")).toHaveText(updates.notes);
  // The colour is Rust's, applied through the CSSOM. A `style=""` attribute
  // would be dropped by `style-src 'self'` (the afterEach guard would catch
  // it), and a class would be this file deciding what amber means.
  await expect(group.locator(".update-status")).toHaveCSS("color", "rgb(224, 160, 58)");
  await expect(group.locator(".btn.install-update")).toHaveText(updates.installLabel);
  await expect(group.locator(".btn.check-updates")).toHaveText(updates.checkLabel);

  await group.locator(".btn.install-update").click();
  await expect
    .poll(async () => (await calls(page, "update_install")).length)
    .toBe(1);
});

test("release notes render readable structure while keeping remote content inert", async ({page, baseURL}) => {
  const settings = await fixture(baseURL, 'sample-settings.json');
  const notes = [
    '## Highlights', '',
    '- **Remember** window size.',
    '  Preserve the selected position.',
    '  - Keep `critical` alerts enabled.',
    '- Read the *new* layout.', '',
    '### Installation', '',
    '1. Download the app.',
    '2. Reopen Solador.', '',
    '```sh', 'solador --version', '```', '',
    'See [PR #567](https://github.com/Sassy-Dog/solador/pull/567).', '',
    '<img src=x onerror="window.NOTES_EXECUTED=true">',
    '[unsafe](javascript:window.NOTES_EXECUTED=true)',
    '', '## C#', '_Emphasis_ and __strong__ alongside file_name_here.',
  ].join('\n');
  await openSettings(page, baseURL, null, {...settings.about.updates, notes});
  await tab(page, 'about').click();
  const rendered = page.locator('.update-notes');
  await expect(rendered.getByRole('heading', {name:'Highlights',exact:true})).toBeVisible();
  await expect(rendered.getByRole('heading', {name:'Installation',exact:true})).toBeVisible();
  await expect(rendered.getByRole('heading', {name:'C#',exact:true})).toBeVisible();
  await expect(rendered.locator(':scope > ul > li')).toHaveCount(2);
  await expect(rendered.locator('ul ul li')).toHaveText('Keep critical alerts enabled.');
  await expect(rendered.locator('strong')).toHaveText(['Remember', 'strong']);
  await expect(rendered.locator('em')).toHaveText(['new', 'Emphasis']);
  await expect(rendered).toContainText('file_name_here');
  await expect(rendered.locator('ol > li')).toHaveText(['Download the app.', 'Reopen Solador.']);
  await expect(rendered.locator('pre code')).toHaveText('solador --version');
  await expect(rendered).toContainText('PR #567 (https://github.com/Sassy-Dog/solador/pull/567)');
  await expect(rendered.locator('img, script, a, iframe')).toHaveCount(0);
  expect(await page.evaluate(() => window.NOTES_EXECUTED)).toBeUndefined();
  // The tab list has no Layout tab: Solador's Full view is gone (#647).
  await expect(page.locator('#settings .tab[data-tab="layout"]')).toHaveCount(0);
});

/**
 * The distinction the whole feature rests on: a check that could not run must
 * not paint as one that ran and found nothing. Neither may offer an install.
 */
test("About update actions fit without their own scrollbar", async ({ page, baseURL }) => {
  await openSettings(page, baseURL);
  await tab(page, "about").click();
  for (const width of [1280, 390, 240]) {
    await page.setViewportSize({ width, height: 900 });
    const actions = page.locator(".update-controls");
    expect(await actions.evaluate(el => ({
      vertical: el.scrollHeight > el.clientHeight,
      horizontal: el.scrollWidth > el.clientWidth,
    }))).toEqual({ vertical: false, horizontal: false });
    for (const button of await actions.locator("button").all()) {
      const bounds = await button.boundingBox();
      const row = await actions.boundingBox();
      expect(bounds.y).toBeGreaterThanOrEqual(row.y);
      expect(bounds.y + bounds.height).toBeLessThanOrEqual(row.y + row.height);
    }
  }
});

test("a failed update check does not render as being up to date", async ({ page, baseURL }) => {
  const failed = {
    heading: "Updates",
    status: { text: "Could not check for updates: the network is unreachable", color: "#e0a03a" },
    notes: null,
    checkLabel: "Check for updates",
    installLabel: null,
    help: "Solador checks once when it starts.",
  };
  await openSettings(page, baseURL, null, failed);
  await tab(page, "about").click();

  const group = page.locator('.group[data-group="updates"]');
  await expect(group.locator(".update-status")).toHaveText(failed.status.text);
  // No install offer, and no notes for a version nobody found.
  await expect(group.locator(".btn.install-update")).toHaveCount(0);
  await expect(group.locator(".update-notes")).toHaveCount(0);
  // Amber, not green: this state is a gap in knowledge, and green is a claim
  // it cannot make.
  await expect(group.locator(".update-status")).toHaveCSS("color", "rgb(224, 160, 58)");

  await group.locator(".btn.check-updates").click();
  await expect
    .poll(async () => (await calls(page, "update_check")).length)
    .toBe(1);
});

/**
 * The group is polled while About is open — a download that takes thirty
 * seconds must not leave it frozen — and only while it is open. Rust never
 * pushes; there is not one `emit` in the shell.
 */
test("the Updates group is polled only while the About tab is showing", async ({ page, baseURL }) => {
  await openSettings(page, baseURL);
  // Settings opens on General. Nothing should be asking about updates yet.
  await page.waitForTimeout(1800);
  expect(await calls(page, "update_status")).toEqual([]);

  await tab(page, "about").click();
  await expect
    .poll(async () => (await calls(page, "update_status")).length, { timeout: 5000 })
    .toBeGreaterThan(0);

  // Leaving the tab stops it, and so does closing Settings.
  await tab(page, "general").click();
  const afterLeaving = (await calls(page, "update_status")).length;
  await page.waitForTimeout(1800);
  expect((await calls(page, "update_status")).length).toBe(afterLeaving);
});

test("OpenClaw round-trips the gateway URL and its bearer token", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "openclaw").click();

  const url = page.locator("#openclaw-gateway");
  await expect(url).toHaveValue(settings.openclaw.gateway.value);
  await url.fill("wss://gateway.example");
  await page.locator(".btn.apply").click();
  expect(await calls(page, "settings_save_openclaw")).toEqual([
    { command: "settings_save_openclaw", args: { gatewayUrl: "wss://gateway.example" } },
  ]);

  // The bearer token rides the shared credential controls, so it is written
  // through the same command every other secret is — and the field is emptied
  // the moment the value is handed over.
  await page.locator(".replace-secret").click();
  const token = page.locator("#secret-openclaw");
  await expect(token).toHaveAttribute("type", "password");
  await token.fill("gateway-token");
  await page.locator('.group[data-secret="openclaw"] .btn.save').click();
  expect(await calls(page, "settings_save_secret")).toEqual([
    { command: "settings_save_secret", args: { key: "openclaw", value: "gateway-token" } },
  ]);
  await expect(page.locator("#secret-openclaw")).toHaveValue("");
});

test("the pairing block shows the fingerprint, the approve command and a retry", async ({ page, baseURL }) => {
  // The fixture is dumped mid-pairing precisely so this block exists: it is the
  // only part of Settings built from live session state, and it is the part an
  // operator actually has to act on.
  const settings = await openSettings(page, baseURL);
  await tab(page, "openclaw").click();

  await expect(page.locator('[data-row="status"] .result')).toHaveText(
    settings.openclaw.status.text
  );
  await expect(page.locator('[data-row="device"] .link-url')).toHaveText(
    settings.openclaw.deviceId
  );

  const block = page.locator('[data-row="pairing"]');
  await expect(block).toContainText(settings.openclaw.pairing.explanation);
  // Verbatim, and selectable: this is the line pasted into a shell, so a
  // frontend-assembled variant of it would be a second implementation of the
  // one string whose whole value is being exactly right.
  await expect(block.locator(".oc-command")).toHaveText(settings.openclaw.pairing.command);
  await expect(block.locator(".oc-command")).toHaveCSS("user-select", "text");
  await expect(block).toContainText(settings.openclaw.pairing.hint);

  await block.locator(".btn.retry").click();
  expect(await calls(page, "settings_openclaw_retry")).toEqual([
    { command: "settings_openclaw_retry", args: {} },
  ]);
});

test("the device key is not a field anyone can type into", async ({ page, baseURL }) => {
  // It is 32 bytes minted by the app, never entered by a human. The tab has one
  // credential control and it is the bearer token; a second one here would be a
  // way to overwrite the identity the gateway has already approved.
  await openSettings(page, baseURL);
  await tab(page, "openclaw").click();

  await expect(page.locator(".settings-body [data-secret]")).toHaveCount(1);
  await expect(page.locator(".settings-body [data-secret]")).toHaveAttribute(
    "data-secret",
    "openclaw"
  );
});

test("update status, notes and buttons change inside a stable group", async ({ page, baseURL }) => {
  // `help` is one constant in Rust (`viewmodel::update::help`) and carries no fixed height
  // (#540), so the stub must not vary it between the two renders.
  const settings = await fixture(baseURL, "sample-settings.json");
  const failed = { heading:"Updates", status:{text:"Could not check: " + "network unavailable ".repeat(20), color:"#e0a03a"}, notes:null, checkLabel:"Check for updates", installLabel:null, help:settings.about.updates.help };
  await openSettings(page, baseURL, null, failed);
  await tab(page, "about").click();
  const group = page.locator('.group[data-group="updates"]');
  const before = await group.boundingBox();
  await group.locator('.check-updates').click();
  await expect(group.locator('.update-status')).toHaveText(failed.status.text);
  expect(await group.boundingBox()).toEqual(before);
});

// The real v2026.10.47 release body (`gh release view`), pasted so CI stays hermetic.
const RELEASE_NOTES_V2026_10_47 = [
  "The Windows installer is Authenticode-signed via Azure Trusted Signing (publisher: Sassy Dog Enterprises LLC) and RFC 3161-timestamped. The macOS disk image is Developer ID-signed and notarized.",
  "",
  "## What's Changed",
  "* feat(agent): own CalVer, agent-v mint, and agent-v feed producer + installer (#472) by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/494",
  "* ci: agent-v release and publish-agent-feed workflows, desktop-only publish-feed (#472) by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/495",
  "* docs(versioning): declare the agent release-counter mint as a deviation from \u00a77 by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/499",
  "* feat(cockpit): show a verified agent update available per host by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/500",
  "* feat(agent): install.sh carries only documented env-file keys (#496) by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/501",
  "* test(cockpit): permanent guard that the agent-release client stays HTTPS-only by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/503",
  "* feat(agent): opt-in tailnet-only bind, SOLADOR_AGENT_REQUIRE_TAILNET=1 (#497) by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/504",
  "* feat(agent): install.sh --system-daemon, service user stages and root loads a LaunchDaemon (macOS) by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/508",
  "* feat(agent): update/rollback a system-domain LaunchDaemon (SIGTERM + KeepAlive, macOS) by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/509",
  "* Add machine meters, configurable repo tables and runner groups by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/510",
  "* Tighten dashboard tile heading spacing by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/511",
  "* Make machine CPU and RAM alert thresholds configurable by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/512",
  "* Add compact Detail tables and a saved Table/List switch by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/513",
  "* Unify detail styling and stabilize live table columns by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/514",
  "* fix: make runner group clicks open inline Detail by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/515",
  "* fix: compact the Overview app header by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/516",
  "* feat(screenshots): showcase fixtures with exactly one tile out of true, at 2x by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/517",
  "* Fix duplicate repository count labels at wide widths by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/518",
  "* Report thermal state from remote Macs by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/519",
  "* feat(agent): enable daily updates for macOS system daemons by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/520",
  "* ci: select builds and tests by changed areas by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/521",
  "* feat: remember window size and position by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/522",
  "* feat(site): solador.app, a static marketing site deployed by GitHub Pages by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/523",
  "* chore(deps): bump tauri-plugin-opener from 2.5.5 to 2.7.0 by @dependabot[bot] in https://github.com/Sassy-Dog/solador/pull/528",
  "* chore(deps): bump starship-battery from 0.11.1 to 0.12.0 by @dependabot[bot] in https://github.com/Sassy-Dog/solador/pull/525",
  "* chore(deps): bump tokio-rustls from 0.26.4 to 0.26.6 by @dependabot[bot] in https://github.com/Sassy-Dog/solador/pull/530",
  "* chore(deps): bump tauri-plugin-notification from 2.4.0 to 2.5.1 by @dependabot[bot] in https://github.com/Sassy-Dog/solador/pull/526",
  "* chore(deps): bump rcgen to 0.14 and axum-server to 0.8 by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/531",
  "* chore(deps): bump tauri-plugin-updater from 2.13.0 to 2.13.1 by @dependabot[bot] in https://github.com/Sassy-Dog/solador/pull/527",
  "* test(agent): make two update tests independent of the host's system daemon by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/534",
  "* fix(localhost): report no battery when the platform cannot state a capacity by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/535",
  "* chore: track the sassy-dog plugin config, and guard everything else in .claude/ by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/536",
  "* docs: make AGENTS.md the single agent-instruction source; CLAUDE.md imports it (#537) by @cpmadrid in https://github.com/Sassy-Dog/solador/pull/538",
  "",
  "",
  "**Full Changelog**: https://github.com/Sassy-Dog/solador/compare/v2026.10.14...v2026.10.47",
].join("\n");

/**
 * Long, real release notes must remain in the page scroll at narrow and wide
 * sizes; neither formatting nor polling may reintroduce an inner scroll box.
 */
const INSTALLED_UPDATE = {
  heading: "Updates",
  status: { text: "Update installed. It takes effect the next time you open Solador.", color: "#3fb950" },
  notes: RELEASE_NOTES_V2026_10_47,
  checkLabel: "Check for updates",
  installLabel: null,
  help: "", // the real sentence is read from the fixture (`viewmodel::update::help`) per test
};
const installedUpdate = async (baseURL) => ({
  ...INSTALLED_UPDATE,
  help: (await fixture(baseURL, "sample-settings.json")).about.updates.help,
});

for (const width of [390, 1000]) {
  test(`update controls precede freely flowing messages (${width}px)`, async ({ page, baseURL }) => {
    await page.setViewportSize({ width, height: 900 });
    await openSettings(page, baseURL, null, await installedUpdate(baseURL));
    await tab(page, "about").click();
    const group = page.locator('.group[data-group="updates"]');
    await expect(group.locator('.update-notes').getByRole('heading', {name:"What's Changed"})).toBeVisible();
    await expect(group.locator('.update-notes > ul > li')).toHaveCount(33);
    await expect(group.locator('.update-notes')).toContainText('SOLADOR_AGENT_REQUIRE_TAILNET=1');

    const m = await group.evaluate((g) => {
      const box = (sel) => {
        const el = g.querySelector(sel);
        return {
          sh: el.scrollHeight,
          ch: el.clientHeight,
          h: el.getBoundingClientRect().height,
          lh: parseFloat(getComputedStyle(el).lineHeight),
        };
      };
      return {
        help: box(":scope > .help"),
        status: box(".update-status"),
        slot: box(".update-notes-slot"),
        text: box(".update-notes"),
        notesOverflow: getComputedStyle(g.querySelector(".update-notes")).overflowY,
        controlsBottom: g.querySelector(".update-controls").getBoundingClientRect().bottom,
        statusTop: g.querySelector(".update-status").getBoundingClientRect().top,
        notesTop: g.querySelector(".update-notes-slot").getBoundingClientRect().top,
      };
    });
    expect(m.help.sh).toBeLessThanOrEqual(m.help.ch);
    expect(m.status.sh).toBeLessThanOrEqual(m.status.ch);
    // All release notes participate in the page scroll, with no nested box.
    expect(m.slot.sh).toBeLessThanOrEqual(m.slot.ch);
    expect(m.notesOverflow).toBe("visible");
    expect(m.statusTop - m.controlsBottom).toBeGreaterThanOrEqual(16);
    expect(m.notesTop).toBeGreaterThan(m.controlsBottom);
    expect(m.slot.h).toBeGreaterThan(120);
  });
}

test("update controls stay above changing messages", async ({ page, baseURL }) => {
  await page.setViewportSize({ width: 390, height: 900 });
  const base = await installedUpdate(baseURL);
  await openSettings(page, baseURL, null, base);
  await tab(page, "about").click();
  const group = page.locator('.group[data-group="updates"]');
  const y = async () => (await group.locator(".update-controls").boundingBox()).y;
  const installed = await y();
  const states = [
    { ...base, notes: null, status: { text: "Checking for updates...", color: "#8b949e" } },
    { ...base, notes: null, status: { text: "Solador is up to date.", color: "#3fb950" } },
    { ...base, installLabel: "Install update", status: { text: "Update 2026.10.47 is available.", color: "#e0a03a" } },
    base,
  ];
  for (const next of states) {
    await page.evaluate((u) => { window.__UPDATES__ = u; }, next);
    await group.locator(".btn.check-updates").click();
    await expect(group.locator(".update-status")).toHaveText(next.status.text);
    expect(await y()).toBe(installed);
  }
});

// MARK: pairing a host's agent certificate (#448)
//
// The Tauri IPC boundary is not automated (README's smoke checklist covers it),
// so the probe is stubbed here: what these tests pin is the contract the
// frontend keeps with Rust -- a fingerprint is only ever SENT after the Trust
// click, only for the address it was read from, and never typed in.

const FINGERPRINT_A = "45:39:AF:05:7E:7E:3C:EF:F7:0C:79:2B:1A:31:B4:04:B8:F8:98:CE:5D:2D:BE:60:83:A8:33:5C:C7:E1:F5:4F";
const FINGERPRINT_B = "0B:0B:0B:0B:0B:0B:0B:0B:0B:0B:0B:0B:0B:0B:0B:0B:0B:0B:0B:0B:0B:0B:0B:0B:0B:0B:0B:0B:0B:0B:0B:0B";

/** Answers `settings_probe_host_certificate` with `answer`; the call itself is
 *  recorded in `__CALLS__` like every other command. */
async function stubHostProbe(page, answer) {
  await page.evaluate((answer) => {
    const original = window.__TAURI__.core.invoke;
    window.__TAURI__.core.invoke = async (command, args) => {
      if (command === "settings_probe_host_certificate") {
        window.__CALLS__.push({ command, args });
        return { address: args.address, port: Number(args.port), ...answer };
      }
      return original(command, args);
    };
  }, answer);
}

const TLS_FOUND = {
  status: "tls",
  fingerprint: FINGERPRINT_A,
  message: "This agent presented the certificate below. Compare its fingerprint.",
};

test("Add Host shows the fingerprint the address presents and pins nothing until Trust", async ({ page, baseURL }) => {
  await openSettings(page, baseURL);
  await addConnection(page, "host");
  await stubHostProbe(page, TLS_FOUND);
  await page.locator("#host-name").fill("tls-box");
  await page.locator("#host-address").fill("100.64.0.9");
  const add = page.locator(".btn.add");
  await expect(add).toBeEnabled();

  await page.locator(".pair-check").click();

  // What Rust found, verbatim, and the fingerprint to compare.
  await expect(page.locator(".pair-found .pair-fingerprint")).toHaveText(FINGERPRINT_A);
  await expect(page.locator(".pair-message")).toHaveText(TLS_FOUND.message);
  expect(await calls(page, "settings_probe_host_certificate")).toEqual([
    { command: "settings_probe_host_certificate", args: { address: "100.64.0.9", port: "7878" } },
  ]);
  // A found-but-untrusted certificate holds Add back: adding now would poll a
  // TLS agent over plain HTTP. Nothing has been persisted or even sent.
  await expect(add).toBeDisabled();
  expect(await calls(page, "settings_add_host")).toEqual([]);

  await page.locator(".pair-trust").click();
  await expect(page.locator(".pair-trusted")).toBeVisible();
  await expect(page.locator(".pair-trust")).toHaveCount(0);
  await expect(add).toBeEnabled();
  // Trust itself persists nothing either: the fingerprint travels with Add.
  expect(await calls(page, "settings_add_host")).toEqual([]);

  await add.click();
  expect(await calls(page, "settings_add_host")).toEqual([
    {
      command: "settings_add_host",
      args: { name: "tls-box", address: "100.64.0.9", port: "7878", token: "", tlsFingerprint: FINGERPRINT_A },
    },
  ]);
});

test("a fingerprint trusted for one address is not carried to another", async ({ page, baseURL }) => {
  await openSettings(page, baseURL);
  await addConnection(page, "host");
  await stubHostProbe(page, TLS_FOUND);
  await page.locator("#host-name").fill("tls-box");
  await page.locator("#host-address").fill("100.64.0.9");
  await page.locator(".pair-check").click();
  await page.locator(".pair-trust").click();
  await expect(page.locator(".pair-trusted")).toBeVisible();

  // Editing the address drops the answer and the trust with it.
  await page.locator("#host-address").fill("100.64.0.10");
  await expect(page.locator(".pair-found .pair-fingerprint")).toHaveCount(0);
  await expect(page.locator(".pair-trusted")).toHaveCount(0);
  await page.locator(".btn.add").click();

  const [add] = await calls(page, "settings_add_host");
  expect(add.args).toEqual({ name: "tls-box", address: "100.64.0.10", port: "7878", token: "" });
  expect(add.args).not.toHaveProperty("tlsFingerprint");
});

test("a fingerprint trusted for one port is not carried to another, on Add or on Save", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await addConnection(page, "host");
  await stubHostProbe(page, TLS_FOUND);
  await page.locator("#host-name").fill("tls-box");
  await page.locator("#host-address").fill("100.64.0.9");
  await page.locator(".pair-check").click();
  await page.locator(".pair-trust").click();
  await page.locator("#host-port").fill("7879");
  await expect(page.locator(".pair-trusted")).toHaveCount(0);
  await page.locator(".btn.add").click();
  expect((await calls(page, "settings_add_host"))[0].args).not.toHaveProperty("tlsFingerprint");

  // The same guard on an existing host's edit form.
  const host = settings.hosts.rows.find((h) => !h.pinned);
  await openConnection(page, `host:${host.id}`);
  await page.locator(".pair-check").click();
  await page.locator(".pair-trust").click();
  await page.locator("#host-edit-port").fill("7880");
  await expect(page.locator(".pair-trusted")).toHaveCount(0);
  await page.locator(".host-save").click();
  expect((await calls(page, "settings_save_host")).at(-1).args).not.toHaveProperty("tlsFingerprint");
});

test("an agent that answers plain HTTP can be added as it is, with no pin", async ({ page, baseURL }) => {
  await openSettings(page, baseURL);
  await addConnection(page, "host");
  const none = { status: "no-tls", fingerprint: null, message: "This agent answers plain HTTP: there is no certificate to pair." };
  await stubHostProbe(page, none);
  await page.locator("#host-name").fill("plain-box");
  await page.locator("#host-address").fill("100.64.0.11");

  await page.locator(".pair-check").click();

  await expect(page.locator(".pair-message")).toHaveText(none.message);
  // "No TLS" has no fingerprint and no Trust button: there is nothing to trust.
  await expect(page.locator(".pair-found .pair-fingerprint")).toHaveCount(0);
  await expect(page.locator(".pair-trust")).toHaveCount(0);
  await expect(page.locator(".btn.add")).toBeEnabled();
  await page.locator(".btn.add").click();
  const [add] = await calls(page, "settings_add_host");
  expect(add.args).not.toHaveProperty("tlsFingerprint");
});

test("a probe that finds nothing says why and pins nothing", async ({ page, baseURL }) => {
  await openSettings(page, baseURL);
  await addConnection(page, "host");
  const failed = { status: "failed", fingerprint: null, message: "couldn't reach that host — check the address" };
  await stubHostProbe(page, failed);
  await page.locator("#host-name").fill("down-box");
  await page.locator("#host-address").fill("100.64.0.12");

  await page.locator(".pair-check").click();

  await expect(page.locator(".pair-message")).toHaveText(failed.message);
  await expect(page.locator(".pair-trust")).toHaveCount(0);
  expect(await calls(page, "settings_add_host")).toEqual([]);
});

test("an existing unpaired host is paired from its edit form, and only Trust sends the pin", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const host = settings.hosts.rows.find((h) => !h.pinned);
  await openConnection(page, `host:${host.id}`);
  await stubHostProbe(page, TLS_FOUND);

  await page.locator(".pair-check").click();
  await expect(page.locator(".pair-found .pair-fingerprint")).toHaveText(FINGERPRINT_A);
  // Saved before Trust: no pin is sent, so the host keeps whatever it had.
  await page.locator(".host-save").click();
  expect((await calls(page, "settings_save_host")).at(-1).args).not.toHaveProperty("tlsFingerprint");

  await page.locator(".pair-check").click();
  await page.locator(".pair-trust").click();
  await page.locator(".host-save").click();
  expect((await calls(page, "settings_save_host")).at(-1).args).toMatchObject({
    id: host.id,
    tlsFingerprint: FINGERPRINT_A,
  });
});

test("a pinned host whose certificate changed offers Re-pair, and Re-pair replaces the pin", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const host = settings.hosts.rows.find((h) => h.certificateChanged);
  expect(host, "the fixture carries a pinned host whose certificate changed").toBeTruthy();
  await openConnection(page, `host:${host.id}`);

  // The current pin is on screen, in full, and so is what to do about it.
  await expect(page.locator(".pinned-fingerprint")).toHaveText(host.fingerprint);
  await expect(page.locator(".pair-check")).toHaveText(settings.hosts.pair.repairLabel);
  await expect(page.locator(".host-details")).toContainText(settings.hosts.pair.repairHelp);

  await stubHostProbe(page, { ...TLS_FOUND, fingerprint: FINGERPRINT_B });
  await page.locator(".pair-check").click();
  await expect(page.locator(".pair-found .pair-fingerprint")).toHaveText(FINGERPRINT_B);

  // Untrusted: Save must not touch the pin.
  await page.locator(".host-save").click();
  expect((await calls(page, "settings_save_host")).at(-1).args).not.toHaveProperty("tlsFingerprint");

  await page.locator(".pair-check").click();
  await page.locator(".pair-trust").click();
  await page.locator(".host-save").click();
  expect((await calls(page, "settings_save_host")).at(-1).args).toMatchObject({
    id: host.id,
    tlsFingerprint: FINGERPRINT_B,
  });
});

test("a pinned host that is fine offers no Re-pair until Test finds its certificate changed", async ({ page, baseURL }) => {
  // The fixture's pinned host is flagged; clear the flag so this one is healthy.
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  const settings = await fixture(baseURL, "sample-settings.json");
  const host = settings.hosts.rows.find((h) => h.pinned);
  host.certificateChanged = false;
  await stubIpc(page, cockpit, settings);
  await page.goto("/index.html");
  await page.locator(SETTINGS_BUTTON).click();
  await openConnection(page, `host:${host.id}`);

  await expect(page.locator(".pinned-fingerprint")).toHaveText(host.fingerprint);
  await expect(page.locator(".pair-check")).toHaveCount(0);

  await page.evaluate((id) => {
    const original = window.__TAURI__.core.invoke;
    window.__TAURI__.core.invoke = async (command, args) =>
      command === "settings_test_host"
        ? { id, result: "✗ certificate changed — re-pair this host if that was expected", certificateChanged: true }
        : original(command, args);
  }, host.id);
  await page.locator(".host-row .test").click();

  await expect(page.locator(".pair-check")).toHaveText(settings.hosts.pair.repairLabel);
});

test("an unpinned host shows no pin line and no Re-pair", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const host = settings.hosts.rows.find((h) => !h.pinned);
  await openConnection(page, `host:${host.id}`);
  await expect(page.locator(".pinned-fingerprint")).toHaveCount(0);
  await expect(page.locator(".pair-check")).toHaveText(settings.hosts.pair.checkLabel);
  await expect(page.locator(".host-details")).not.toContainText(settings.hosts.pair.repairHelp);
});

test("an unpaired host the cockpit refuses to dial over plain HTTP says so, and is fixed with the pairing step (#449)", async ({ page, baseURL }) => {
  // The live flag is Rust's; the fixture's unpaired host is marked here, the way
  // the "fine pinned host" case clears the pinned one.
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  const settings = await fixture(baseURL, "sample-settings.json");
  const host = settings.hosts.rows.find((h) => !h.pinned);
  host.plainRefused = true;
  await stubIpc(page, cockpit, settings);
  await page.goto("/index.html");
  await page.locator(SETTINGS_BUTTON).click();
  await openConnection(page, `host:${host.id}`);

  // Why it is not polled, above the step that fixes it -- and no Re-pair, which
  // is for a pin that changed.
  await expect(page.locator(".pair-unpaired")).toHaveText(settings.hosts.pair.unpairedHelp);
  await expect(page.locator(".pair-check")).toHaveText(settings.hosts.pair.checkLabel);
  await expect(page.locator(".host-details")).not.toContainText(settings.hosts.pair.repairHelp);

  // Fixed with the existing probe / Trust flow, and only Trust sends the pin.
  await stubHostProbe(page, TLS_FOUND);
  await page.locator(".pair-check").click();
  await expect(page.locator(".pair-found .pair-fingerprint")).toHaveText(FINGERPRINT_A);
  await page.locator(".pair-trust").click();
  await page.locator(".host-save").click();
  expect((await calls(page, "settings_save_host")).at(-1).args).toMatchObject({
    id: host.id,
    tlsFingerprint: FINGERPRINT_A,
  });
});

test("an unpaired host shows no refusal note until Test finds it refused, and a pinned one never does (#449)", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const host = settings.hosts.rows.find((h) => !h.pinned);
  expect(host.plainRefused).toBe(false);
  await openConnection(page, `host:${host.id}`);
  await expect(page.locator(".pair-unpaired")).toHaveCount(0);

  await page.evaluate((id) => {
    const original = window.__TAURI__.core.invoke;
    window.__TAURI__.core.invoke = async (command, args) =>
      command === "settings_test_host"
        ? { id, result: "✗ not paired", certificateChanged: false, plainRefused: true }
        : original(command, args);
  }, host.id);
  await page.locator(".host-row .test").first().click();
  await expect(page.locator(".pair-unpaired")).toBeVisible();

  // A pinned host is dialled over TLS, which the guard does not touch.
  const pinned = settings.hosts.rows.find((h) => h.pinned);
  expect(pinned.plainRefused).toBe(false);
});
