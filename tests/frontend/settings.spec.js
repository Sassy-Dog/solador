import { test, expect } from "@playwright/test";

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
async function stubIpc(page, cockpit, settings, probe, updates, discover) {
  await page.addInitScript(
    ({ cockpit, settings, testResult, probe, updates, discover }) => {
      window.__CALLS__ = [];
      window.__TAURI__ = {
        core: {
          invoke: async (command, args) => {
            window.__CALLS__.push({ command, args });
            if (command === "cockpit") return cockpit;
            if (command === "settings_view") return settings;
            if (command === "settings_test_host") return { id: args.id, result: testResult };
            // The probe answers in its own shape, not `{status, settings}`: a
            // finding is not a mutation, and the tab renders it without one.
            if (command === "settings_probe_status_vendor") return probe;
            // The discovery probe likewise — ephemeral, never persisted.
            if (command === "settings_discover_repos") return discover;
            // The three updater commands answer in the About tab's own
            // `about.updates` shape — not `{status, settings}` — because an
            // update check is not a settings mutation and nothing is
            // persisted. A test that wants a different state passes one.
            if (command.startsWith("update_")) return updates;
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
      discover: discover || null,
    }
  );
}

const calls = (page, command) =>
  page.evaluate((c) => window.__CALLS__.filter((call) => call.command === c), command);

/** Loads the app with the IPC stub installed and opens Settings. */
async function openSettings(page, baseURL, probe, updates, discover) {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  const settings = await fixture(baseURL, "sample-settings.json");
  await stubIpc(page, cockpit, settings, probe, updates, discover);
  await page.goto("/index.html");
  await expect(page.locator("#settingsToggle")).toBeVisible();
  await page.locator("#settingsToggle").click();
  await expect(page.locator("#settings")).toBeVisible();
  return settings;
}

const tab = (page, id) => page.locator(`.tab[data-tab="${id}"]`);

/**
 * Panels keep their own timers and skip the work while Settings is up, so a
 * setting saved a second ago used to leave its panel displaying the setup
 * instruction that asked for it — for a full slow-cadence tick, ten seconds on
 * the Azure panel. `refreshCockpit` did not help: it repaints the host cards,
 * and the panels are separate.
 */
test("closing Settings brings the panels current, not just the host cards", async ({ page, baseURL }) => {
  await openSettings(page, baseURL);
  // Everything asked for during startup is noise for this assertion; what
  // matters is what happens on close.
  await page.evaluate(() => {
    window.__CALLS__.length = 0;
  });

  await page.locator("#settingsClose").click();
  await expect(page.locator("#settings")).toBeHidden();

  // Every panel, not one: the registry exists so a single close brings them
  // all current, and a panel that quietly stopped registering would still pass
  // a test that only checked its neighbour.
  for (const command of ["azure_cost", "crons", "containers", "repos", "services", "usage"]) {
    await expect
      .poll(async () => (await calls(page, command)).length, {
        timeout: 3000,
        message: `${command} was not re-requested when Settings closed`,
      })
      .toBeGreaterThan(0);
  }
});

test("the Settings button, title and every tab come from Rust", async ({ page, baseURL }) => {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  const settings = await openSettings(page, baseURL);

  // The button's label rides in on the cockpit payload, because it has to
  // exist before anything has asked for the settings payload.
  await expect(page.locator("#settingsToggle")).toHaveText(cockpit.settingsLabel);
  await expect(page.locator("#settingsTitle")).toHaveText(settings.title);
  await expect(page.locator("#settingsClose")).toHaveText(settings.closeLabel);
  await expect(page.locator(".tabs .tab")).toHaveText(settings.tabs.map((t) => t.title));

  // The cockpit is out of the way while Settings is up, and comes back on
  // close -- with a fresh poll rather than a stale, zero-width layout.
  await expect(page.locator("#cockpitView")).toBeHidden();
  await page.locator("#settingsClose").click();
  await expect(page.locator("#cockpitView")).toBeVisible();
  await expect(page.locator("#settings")).toBeHidden();
});

test("General shows the stored values and applies them in one command", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  const g = settings.general;

  await expect(page.locator("#general-interval")).toHaveValue(String(g.refreshInterval.value));
  await expect(page.locator("#general-core-rows")).toHaveValue(String(g.coreRowSpan.value));
  // The fixture's row gap is off the shipped 0 on purpose, so this asserts the
  // field paints the STORED value rather than a constant that agrees with the
  // default.
  await expect(page.locator("#general-row-gap")).toHaveValue(String(g.rowGapPx.value));
  expect(g.rowGapPx.value, "the fixture must carry a non-default gap").not.toBe(0);
  // The picker offers exactly what Rust offers -- an option invented here
  // would be a cadence the store launders straight back to the default.
  await expect(page.locator("#general-interval option")).toHaveText(
    g.refreshInterval.options.map((o) => o.label)
  );
  // The host-overflow picker left this tab: it is per breakpoint now.
  await expect(page.locator("#general-overflow")).toHaveCount(0);

  await page.locator("#general-interval").selectOption("300");
  await page.locator("#general-core-rows").fill("4");
  await page.locator("#general-row-gap").fill("0");
  await page.locator(".btn.apply").click();

  // One command for all three, and `0` travels in it. A field that dropped a
  // zero — the shape `Number(raw) || fallback` produces — would look like the
  // operator never touched the row spacing.
  expect(await calls(page, "settings_save_general")).toEqual([
    {
      command: "settings_save_general",
      args: { refreshIntervalSecs: 300, coreRowSpan: 4, rowGapPx: 0 },
    },
  ]);
  await expect(page.locator("#settingsStatus")).toHaveText("Saved.");
});

test("each panel cadence applies on its own row and paints Rust's two sentences", async ({
  page,
  baseURL,
}) => {
  const settings = await openSettings(page, baseURL);
  const p = settings.general.panelIntervals;

  // Every settable cadence has a row, in Rust's order. A row invented here
  // would be a panel the store has no floor for.
  await expect(page.locator(".cadence-item")).toHaveCount(p.rows.length);
  expect(p.rows.length, "the fixture must carry every settable cadence").toBe(4);

  for (const row of p.rows) {
    const item = page.locator(`.cadence-item[data-panel="${row.id}"]`);
    await expect(item.locator(".input")).toHaveValue(String(row.value));
    // The browser's own hint, taken from Rust — not the enforcement point,
    // which is `check_secs` on the other side of the IPC.
    await expect(item.locator(".input")).toHaveAttribute("min", String(row.min));
    // Both sentences are Rust's, verbatim: which state the row is in, and the
    // floor with the reason for it.
    await expect(item.locator(".cadence-status")).toHaveText(row.status);
    await expect(item.locator(".help").last()).toHaveText(row.help);
    // Live only where there is an override to forget.
    await expect(item.locator(".cadence-reset")).toBeEnabled({ enabled: row.configured });
  }

  // The fixture covers both renderings, or half the group is untested.
  const configured = p.rows.filter((r) => r.configured);
  expect(configured.length, "the fixture needs a configured cadence").toBeGreaterThan(0);
  expect(
    p.rows.length - configured.length,
    "...and an unconfigured one"
  ).toBeGreaterThan(0);

  // One row's Apply sends one row's panel, and nothing else's.
  const containers = page.locator('.cadence-item[data-panel="containers"]');
  await containers.locator(".input").fill("30");
  await containers.locator(".cadence-apply").click();
  expect(await calls(page, "settings_save_panel_interval")).toEqual([
    {
      command: "settings_save_panel_interval",
      args: { panel: "containers", secs: 30 },
    },
  ]);

  // ...and Use default forgets the override rather than writing the default
  // back, which is a different state the store keeps apart.
  const override = page.locator(`.cadence-item[data-panel="${configured[0].id}"]`);
  await override.locator(".cadence-reset").click();
  expect(await calls(page, "settings_clear_panel_interval")).toEqual([
    {
      command: "settings_clear_panel_interval",
      args: { panel: configured[0].id },
    },
  ]);
});

test("the crash-reporting toggle saves on the spot and paints Rust's sentence", async ({
  page,
  baseURL,
}) => {
  const settings = await openSettings(page, baseURL);
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

/** The Layout tab, showing the band the editor has selected. */
async function openLayout(page, baseURL) {
  const settings = await openSettings(page, baseURL);
  await tab(page, "layout").click();
  // The dumped fixture carries two bands on purpose — a narrow one that tabs
  // its host cards and a wide one that does not — so the switcher, the
  // per-band overflow and the removable band are all reachable here.
  expect(settings.layout.breakpoints.length).toBeGreaterThan(1);
  return settings.layout;
}

test("Layout lists every panel in the stored order, with Rust's move bounds", async ({ page, baseURL }) => {
  const t = await openLayout(page, baseURL);
  const band = t.breakpoints[0];

  const rows = page.locator(".layout-row");
  await expect(rows).toHaveCount(band.rows.length);
  await expect(rows.locator(".layout-name")).toHaveText(band.rows.map((r) => r.title));
  for (const [i, panel] of band.rows.entries()) {
    const row = rows.nth(i);
    await expect(row).toHaveAttribute("data-panel", panel.id);
    await expect(row.locator("select")).toHaveValue(panel.span);
    // Whether a move can do anything is Rust's answer, not an index compared
    // here — the ends of the list are where a second implementation shows up.
    expect(await row.locator('.btn[data-direction="up"]').isDisabled()).toBe(!panel.canMoveUp);
    expect(await row.locator('.btn[data-direction="down"]').isDisabled()).toBe(!panel.canMoveDown);
  }
  // The picker offers exactly the widths Rust offers.
  await expect(rows.first().locator("select option")).toHaveText(
    t.spanOptions.map((o) => o.label)
  );
  // …and the band's own overflow mode, which is the whole feature.
  await expect(page.locator("#layout-overflow")).toHaveValue(band.hostOverflow);
});

test("the breakpoint switcher shows one band at a time and edits address it", async ({ page, baseURL }) => {
  const t = await openLayout(page, baseURL);
  const [narrow, wide] = t.breakpoints;

  // One button per band, labelled by Rust, with the first selected.
  const bar = page.locator(".tabs .tab[data-band]");
  await expect(bar).toHaveText(t.breakpoints.map((b) => b.label));
  await expect(bar.first()).toHaveAttribute("data-active", "true");
  await expect(page.locator("#layout-overflow")).toHaveValue(narrow.hostOverflow);

  // Switching bands is a local view change — no command, and the rows and
  // overflow are the other band's.
  await bar.nth(1).click();
  await expect(bar.nth(1)).toHaveAttribute("data-active", "true");
  await expect(page.locator("#layout-overflow")).toHaveValue(wide.hostOverflow);
  expect(wide.hostOverflow).not.toBe(narrow.hostOverflow);
  await expect(page.locator(".layout-row").first()).toHaveAttribute(
    "data-panel",
    wide.rows[0].id
  );
  expect(await calls(page, "settings_move_panel")).toEqual([]);

  // …and every edit now names THAT band by its width, never by index.
  await page.locator(`#layout-span-${wide.rows[1].id}`).selectOption("quarter");
  expect(await calls(page, "settings_set_panel_span")).toEqual([
    {
      command: "settings_set_panel_span",
      args: { minWidth: wide.minWidth, panel: wide.rows[1].id, span: "quarter" },
    },
  ]);
});

test("moving a panel and changing its width each save one command", async ({ page, baseURL }) => {
  const t = await openLayout(page, baseURL);
  const band = t.breakpoints[0];
  const second = band.rows[1];

  await page.locator(`.layout-row[data-panel="${second.id}"] .btn[data-direction="up"]`).click();
  expect(await calls(page, "settings_move_panel")).toEqual([
    {
      command: "settings_move_panel",
      args: { minWidth: band.minWidth, panel: second.id, direction: "up" },
    },
  ]);

  await page.locator(`#layout-span-${second.id}`).selectOption("quarter");
  expect(await calls(page, "settings_set_panel_span")).toEqual([
    {
      command: "settings_set_panel_span",
      args: { minWidth: band.minWidth, panel: second.id, span: "quarter" },
    },
  ]);
  // No Apply button in this tab: each control persists on its own, and the
  // status line is the proof it reached Rust.
  await expect(page.locator("#settingsStatus")).toHaveText("Saved.");
});

test("the host-overflow mode is saved per breakpoint", async ({ page, baseURL }) => {
  const t = await openLayout(page, baseURL);
  const band = t.breakpoints[0];
  const other = t.overflowOptions.find((o) => o.value !== band.hostOverflow);

  await page.locator("#layout-overflow").selectOption(other.value);
  expect(await calls(page, "settings_set_breakpoint_overflow")).toEqual([
    {
      command: "settings_set_breakpoint_overflow",
      args: { minWidth: band.minWidth, hostOverflowMode: other.value },
    },
  ]);
});

test("a breakpoint can be added by width and removed unless it is the last", async ({ page, baseURL }) => {
  const t = await openLayout(page, baseURL);

  await page.locator("#layout-add-width").fill("2400");
  await page.locator(".btn.add").click();
  expect(await calls(page, "settings_add_breakpoint")).toEqual([
    { command: "settings_add_breakpoint", args: { minWidth: 2400 } },
  ]);

  const remove = page.locator(".btn.delete", { hasText: t.removeLabel });
  await remove.click();
  expect(await calls(page, "settings_remove_breakpoint")).toEqual([
    { command: "settings_remove_breakpoint", args: { minWidth: t.breakpoints[0].minWidth } },
  ]);

  // With one band left there is nothing to remove, and `canRemove` is Rust's
  // answer rather than a length compared here.
  const only = JSON.parse(JSON.stringify(t));
  only.breakpoints = [{ ...t.breakpoints[0], canRemove: false }];
  const settings = await fixture(baseURL, "sample-settings.json");
  settings.layout = only;
  await stubIpc(page, await fixture(baseURL, "sample-cockpit.json"), settings);
  await page.goto("/index.html");
  await page.locator("#settingsToggle").click();
  await tab(page, "layout").click();
  await expect(page.locator(".btn.delete", { hasText: t.removeLabel })).toBeDisabled();
});

test("the Layout preview draws Rust's rows at Rust's proportions", async ({ page, baseURL }) => {
  const t = await openLayout(page, baseURL);
  const preview = t.breakpoints[0].preview;

  const lines = page.locator(".layout-preview-row");
  await expect(lines).toHaveCount(preview.rows.length);
  for (const [i, row] of preview.rows.entries()) {
    const line = lines.nth(i);
    await expect(line.locator(".layout-tile-name")).toHaveText(row.map((c) => c.title));
    // Placed on the same four-quarter grid the cockpit is — each tile spans its
    // weight in quarters, starting where the tiles before it left off — so the
    // preview cannot promise an arrangement the cockpit would not render.
    const placed = await line.evaluate((el) =>
      [...el.children].map((c) => {
        const s = getComputedStyle(c);
        return [s.gridColumnStart.trim(), s.gridColumnEnd.trim()];
      })
    );
    let start = 1;
    for (const [j, cell] of row.entries()) {
      expect(placed[j], `tile ${j} of row ${i}`).toEqual([`${start}`, `span ${cell.weight}`]);
      start += cell.weight;
    }
    expect(start, "a row never claims more than four quarters").toBeLessThanOrEqual(5);
  }
});

test("Reset is offered on a customised layout and disabled on the default", async ({ page, baseURL }) => {
  const t = await openLayout(page, baseURL);
  // The dumped fixture carries a customised layout, so the button is live.
  expect(t.isDefault).toBe(false);
  const reset = page.locator(".btn.delete", { hasText: t.resetLabel });
  await reset.click();
  expect(await calls(page, "settings_reset_layout")).toEqual([
    { command: "settings_reset_layout", args: {} },
  ]);

  // …and a store that never carried one has nothing to reset. `isDefault` is
  // Rust's, so this drives the other branch through the payload.
  const settings = await fixture(baseURL, "sample-settings.json");
  settings.layout.isDefault = true;
  await stubIpc(page, await fixture(baseURL, "sample-cockpit.json"), settings);
  await page.goto("/index.html");
  await page.locator("#settingsToggle").click();
  await tab(page, "layout").click();
  await expect(page.locator(".btn.delete", { hasText: t.resetLabel })).toBeDisabled();
});

test("the Hosts tab lists every host with its endpoint and token badge", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "hosts").click();

  const rows = page.locator(".host-row");
  await expect(rows).toHaveCount(settings.hosts.rows.length);
  for (const [i, host] of settings.hosts.rows.entries()) {
    const row = rows.nth(i);
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

  // Hidden volumes hang off their own host's row, with the unhide button that
  // addresses that host.
  const withHidden = settings.hosts.rows.find((h) => h.hiddenVolumes.length > 0);
  const hidden = page.locator(`.host-row[data-host="${withHidden.id}"] .hidden-row`);
  await expect(hidden).toHaveCount(withHidden.hiddenVolumes.length);
  await expect(hidden.first().locator(".mount")).toHaveText(withHidden.hiddenVolumes[0]);
  await hidden.first().locator(".unhide").click();
  expect(await calls(page, "settings_unhide_volume")).toEqual([
    {
      command: "settings_unhide_volume",
      args: { hostId: withHidden.id, mount: withHidden.hiddenVolumes[0] },
    },
  ]);
});

test("the rules editor renders every persisted field, and the Collapse-only ones only for Collapse", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "hosts").click();

  const t = settings.hosts.rules;
  const rows = page.locator(".rule-row");
  await expect(rows).toHaveCount(t.rows.length);

  for (const rule of t.rows) {
    const row = page.locator(`.rule-row[data-rule="${rule.index}"]`);
    await expect(row.locator("select").first()).toHaveValue(rule.action);
    await expect(row.locator("input").first()).toHaveValue(rule.pattern);
    // The host scope, including the one whose host no longer exists: a picker
    // missing its own selection renders blank, which would read as unscoped.
    await expect(row.locator(".rule-host")).toHaveValue(rule.host);
    // A Hide or Expect rule has no aggregate to name or count, so those two
    // fields are absent rather than empty — `collapseOnly` is Rust's call.
    await expect(row.locator(".rule-expected")).toHaveCount(rule.collapseOnly ? 1 : 0);
    if (rule.collapseOnly) {
      await expect(row.locator(".rule-expected")).toHaveValue(rule.expected);
    }
  }

  // The fixture covers both sides of every branch, or the loop above is only
  // ever exercising one.
  expect(t.rows.some((r) => r.collapseOnly)).toBe(true);
  expect(t.rows.some((r) => !r.collapseOnly)).toBe(true);
  expect(t.rows.some((r) => r.expected !== "")).toBe(true);
  expect(t.rows.some((r) => r.host === "")).toBe(true);

  // The action picker offers exactly what Rust offers.
  await expect(page.locator(".rule-row").first().locator("select").first().locator("option"))
    .toHaveText(t.actions.map((a) => a.label));
});

test("a rule adds, edits one field at a time, and deletes", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "hosts").click();

  const t = settings.hosts.rules;
  // A Collapse rule that *has* an expectation, so clearing it below is a real
  // edit rather than a no-op on an already-empty field.
  const collapse = t.rows.find((r) => r.collapseOnly && r.expected !== "");
  expect(collapse, "the fixture must carry a collapse rule with a count").toBeTruthy();
  const row = page.locator(`.rule-row[data-rule="${collapse.index}"]`);

  // Each control writes ONE field. A whole-row write would send four values
  // assembled from whatever this page last painted — the stale client copy the
  // re-read-on-access binding exists to avoid.
  await row.locator("input").first().fill("worker-*");
  await row.locator("input").first().blur();
  await row.locator(".rule-expected").fill("6");
  await row.locator(".rule-expected").blur();
  await row.locator(".rule-host").selectOption("");
  await row.locator("select").first().selectOption("hide");

  expect(await calls(page, "settings_set_container_rule")).toEqual([
    { command: "settings_set_container_rule", args: { index: collapse.index, field: "pattern", value: "worker-*" } },
    { command: "settings_set_container_rule", args: { index: collapse.index, field: "expected", value: "6" } },
    { command: "settings_set_container_rule", args: { index: collapse.index, field: "host", value: "" } },
    { command: "settings_set_container_rule", args: { index: collapse.index, field: "action", value: "hide" } },
  ]);
  await expect(page.locator("#settingsStatus")).toHaveText("Saved.");

  // Emptying the field must reach Rust as "" — that is how an expectation is
  // cleared, and Rust is what decides "" means "no expectation" rather than 0.
  // Cleared with real keystrokes, not `fill("")`: Playwright's empty fill
  // assigns `.value` directly and the browser then fires no `change` on blur,
  // so a passing test would prove nothing about the real interaction.
  const expected = row.locator(".rule-expected");
  await expected.click();
  await expected.press("ControlOrMeta+a");
  await expected.press("Backspace");
  await expected.blur();
  const edits = await calls(page, "settings_set_container_rule");
  expect(edits[edits.length - 1].args).toEqual({
    index: collapse.index,
    field: "expected",
    value: "",
  });

  await page.locator(".btn.add-rule").click();
  expect(await calls(page, "settings_add_container_rule")).toEqual([
    { command: "settings_add_container_rule", args: {} },
  ]);

  await row.locator(".btn.delete").click();
  expect(await calls(page, "settings_remove_container_rule")).toEqual([
    { command: "settings_remove_container_rule", args: { index: collapse.index } },
  ]);
});

test("Test probes that one host and paints the line Rust produced", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "hosts").click();

  const host = settings.hosts.rows[1];
  const row = page.locator(`.host-row[data-host="${host.id}"]`);
  await row.locator(".test").click();

  // The result string is Rust's (`settings::health_result`) and has no other
  // path to the DOM: the frontend never composes a ✓/✗ line of its own.
  await expect(row.locator(".result")).toHaveText(TEST_RESULT);
  expect(await calls(page, "settings_test_host")).toEqual([
    { command: "settings_test_host", args: { id: host.id } },
  ]);
  // …and only that row's. A shared result slot is the bug this catches.
  const other = settings.hosts.rows[0];
  await expect(page.locator(`.host-row[data-host="${other.id}"] .result`)).toHaveText("");
});

test("Add Host waits for a name and an address, and keeps the token out of the DOM", async ({ page, baseURL }) => {
  await openSettings(page, baseURL);
  await tab(page, "hosts").click();

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
  const settings = await openSettings(page, baseURL);
  // Sentry, not GitHub: the GitHub tab is retired (its token lives on
  // accounts), and the fixture stores a Sentry token, which is the same
  // claim with a subject that still exists.
  await tab(page, "usage").click();

  const secret = settings.usage.sentry.secret;
  const box = page.locator('.group[data-secret="sentry"]');
  const input = page.locator("#secret-sentry");
  await expect(input).toHaveAttribute("type", "password");
  await expect(input).toHaveValue("");
  // The fixture stores a Sentry token, so the badge is up and Clear is live;
  // Save waits for something to save.
  await expect(box.locator(".badge-ok")).toHaveText(secret.storedLabel);
  await expect(box.locator(".save")).toBeDisabled();
  await expect(box.locator(".clear")).toBeEnabled();

  await input.fill("sntryu_supersecret");
  await expect(box.locator(".save")).toBeEnabled();
  await box.locator(".save").click();
  expect(await calls(page, "settings_save_secret")).toEqual([
    { command: "settings_save_secret", args: { key: "sentry", value: "sntryu_supersecret" } },
  ]);
  await expect(input).toHaveValue("");
  await expect(page.locator("#settingsStatus")).toHaveText("Saved.");

  await box.locator(".clear").click();
  expect(await calls(page, "settings_clear_secret")).toEqual([
    { command: "settings_clear_secret", args: { key: "sentry" } },
  ]);

  // Nothing in the payload, and therefore nothing in the page, can echo a
  // stored credential back: the view-model carries a boolean per credential.
  expect(JSON.stringify(settings)).not.toContain("sntryu_");
  await expect(page.locator("body")).not.toContainText("sntryu_supersecret");
});

test("the retired GitHub tab is gone and org watching lives on the account", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await expect(page.locator('.tab[data-tab="github"]')).toHaveCount(0);
  await tab(page, "accounts").click();

  // The fixture's first account watches one org; the second watches none.
  const rows = settings.accounts.rows;
  const watching = rows.find((r) => r.orgs.length > 0);
  const fresh = rows.find((r) => r.orgs.length === 0);
  const watchingRow = page.locator(`.group[data-account="${watching.id}"]`);
  const freshRow = page.locator(`.group[data-account="${fresh.id}"]`);

  await expect(watchingRow.locator(`[data-org="${watching.orgs[0]}"]`)).toBeVisible();
  await expect(freshRow.locator(".result", { hasText: settings.accounts.noOrgsLabel }).first())
    .toBeVisible();

  // Stop watching sends the row's own org, unchecked.
  await watchingRow.locator(`[data-org="${watching.orgs[0]}"] .org-remove`).click();
  expect(await calls(page, "settings_set_account_org")).toEqual([
    {
      command: "settings_set_account_org",
      args: { id: watching.id, org: watching.orgs[0], selected: false },
    },
  ]);

  // Watch sends the typed org, checked, and clears the field before the
  // round trip. Disabled until something is typed — a hint; Rust validates.
  const orgInput = page.locator(`#account-org-${fresh.id}`);
  const watch = freshRow.locator(".org-add");
  await expect(watch).toBeDisabled();
  await orgInput.fill("beta");
  await expect(watch).toBeEnabled();
  await watch.click();
  expect((await calls(page, "settings_set_account_org")).at(-1)).toEqual({
    command: "settings_set_account_org",
    args: { id: fresh.id, org: "beta", selected: true },
  });
  await expect(orgInput).toHaveValue("");
});

test("a credential with nothing stored offers nothing to clear", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  // Neon, not Azure: the Azure panel has no stored credential at all any more
  // (it signs its own request per poll), so it can no longer stand for "a
  // credential with nothing stored". The fixture deliberately stores no Neon
  // key, which is the same claim with a subject that still exists.
  await tab(page, "usage").click();
  expect(settings.usage.neon.secret.stored).toBe(false);
  const box = page.locator('.group[data-secret="neon"]');
  await expect(box.locator(".clear")).toBeDisabled();
  await expect(box.locator(".badge-ok")).toHaveCount(0);
});

test("repos live under their account card, edited behind Configure", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "accounts").click();
  const { owning } = accountsOf(settings);
  const card = page.locator(`.group[data-account="${owning.id}"]`);

  const rows = card.locator(".repo-row[data-repo]");
  await expect(rows).toHaveCount(owning.repos.length);
  await expect(rows.first().locator(".slug")).toHaveText(owning.repos[0].slug);

  // Add by name sits behind its button, and the add carries the card's own
  // account — the attribution is the operator's answer, not a deduction.
  const byName = card.locator('[data-disclosure="add-by-name"]');
  await expect(byName).toBeHidden();
  await card.locator(".btn.add-by-name").click();
  await expect(byName).toBeVisible();
  const add = card.locator(".btn.repo-add");
  await expect(add).toBeDisabled();
  await page.locator(`#repo-slug-${owning.id}`).fill("gadget");
  await expect(add, "a bare name is not owner/name").toBeDisabled();
  await page.locator(`#repo-slug-${owning.id}`).fill("acme/lathe");
  await expect(add).toBeEnabled();
  await add.click();
  expect(await calls(page, "settings_add_repo")).toEqual([
    { command: "settings_add_repo", args: { slug: "acme/lathe", accountId: owning.id } },
  ]);
  await expect(page.locator(`#repo-slug-${owning.id}`)).toHaveValue("");

  const first = owning.repos[0].slug;
  const row = card.locator(`.repo-row[data-repo="${first}"]`);
  const config = row.locator('[data-disclosure="configure"]');
  await expect(config).toBeHidden();
  await row.locator(".btn.configure").click();
  await expect(config).toBeVisible();
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
 * The Choose repos… picker: checkbox rows over what the token was granted,
 * with the checkbox state read from the live view, a foreign repo disabled
 * with its owner named, a reported truncation, and the untrack two-step
 * shown exactly when Rust sent a consequence.
 */
test("the discovery picker checkboxes the grants and two-steps a costly untrack", async ({
  page,
  baseURL,
}) => {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  const settings = await fixture(baseURL, "sample-settings.json");
  const owning = settings.accounts.rows.find((row) => row.repos.length > 0);
  const tracked = owning.repos[0];
  const discover = {
    id: owning.id,
    repos: [
      { slug: "acme/fresh", tracked: false, trackedBy: null, granted: true, archived: false, untrackPrompt: null },
      { slug: tracked.slug, tracked: true, trackedBy: null, granted: true, archived: false,
        untrackPrompt: "Stop tracking " + tracked.slug + "? Its watched workflows (release.yml) are forgotten." },
      { slug: "acme/vault", tracked: false, trackedBy: "personal", granted: true, archived: true, untrackPrompt: null },
    ],
    orgs: [{ name: "beta", selected: false, selectedBy: null }],
    truncated: true,
    truncatedNote: "the walk was cut short",
    reason: null,
  };
  await stubIpc(page, cockpit, settings, null, null, discover);
  await page.goto("/index.html");
  await page.locator("#settingsToggle").click();
  await tab(page, "accounts").click();

  const card = page.locator(`.group[data-account="${owning.id}"]`);
  await card.locator(".btn.choose-repos").click();
  const picker = card.locator(`[data-picker="${owning.id}"]`);
  await expect(picker.locator('[data-pick="acme/fresh"] .toggle')).not.toBeChecked();
  const trackedRow = picker.locator(`[data-pick="${tracked.slug}"]`);
  await expect(
    trackedRow.locator(".toggle"),
    "the checkbox reads the live view, not the probe snapshot"
  ).toBeChecked();
  const foreign = picker.locator('[data-pick="acme/vault"]');
  await expect(foreign.locator(".toggle")).toBeDisabled();
  await expect(foreign, "a foreign repo names its owner").toContainText("personal");
  await expect(foreign).toContainText(settings.accounts.archivedLabel);
  await expect(picker, "a cut walk says so").toContainText(discover.truncatedNote);

  await picker.locator('[data-pick="acme/fresh"] .toggle').click();
  expect(await calls(page, "settings_set_repo_tracked")).toEqual([
    {
      command: "settings_set_repo_tracked",
      args: { id: owning.id, slug: "acme/fresh", tracked: true },
    },
  ]);

  // Unchecking the one with a consequence waits for the operator's answer —
  // Rust's sentence, verbatim, and nothing sent until Proceed.
  const confirm = trackedRow.locator('[data-confirm="untrack"]');
  await expect(confirm).toBeHidden();
  await trackedRow.locator(".toggle").click();
  await expect(confirm).toBeVisible();
  await expect(confirm.locator(".confirm")).toHaveText(discover.repos[1].untrackPrompt);
  expect((await calls(page, "settings_set_repo_tracked")).length).toBe(1);
  await confirm.locator(".btn.untrack-proceed").click();
  expect((await calls(page, "settings_set_repo_tracked")).at(-1)).toEqual({
    command: "settings_set_repo_tracked",
    args: { id: owning.id, slug: tracked.slug, tracked: false },
  });

  // The discovery's derived org is offered beside the stored selection.
  await card.locator('[data-discovered-org="beta"] .btn.org-watch').click();
  expect((await calls(page, "settings_set_account_org")).at(-1)).toEqual({
    command: "settings_set_account_org",
    args: { id: owning.id, org: "beta", selected: true },
  });
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
  await expect(page.locator(".group[data-account]")).toHaveCount(t.rows.length);
  const { owning, spare } = accountsOf(settings);

  const first = page.locator(`.group[data-account="${owning.id}"]`);
  await expect(first.locator(".group-hdr")).toHaveText(owning.label);
  await expect(first.locator(".dim").first()).toHaveText(owning.vendorLabel);
  // The slugs themselves, on the card that removes them.
  for (const repo of owning.repos) await expect(first).toContainText(repo.slug);
  await expect(first.locator(".badge-ok")).toHaveText(t.tokenStoredLabel);

  // An account with no token yet says so — `stored: false` is a fact the
  // payload carries, not a key it omits.
  const second = page.locator(`.group[data-account="${spare.id}"]`);
  await expect(second.locator(".badge-dim")).toHaveText(t.noTokenLabel);
  await expect(second).toContainText(t.noReposLabel);
});

/**
 * The whole point of `account_removal_impact`.
 *
 * Removing an account leaves its repos tracked but unattributed, so the repos
 * are named *before* the removal rather than discovered after it. Nothing is
 * sent until the operator confirms.
 */
test("removing an account that repos depend on names them and waits", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "accounts").click();
  const { owning } = accountsOf(settings);

  const row = page.locator(`.group[data-account="${owning.id}"]`);
  const confirm = row.locator('[data-confirm="remove"]');
  await expect(confirm).toBeHidden();

  await row.locator(".btn.delete").first().click();
  await expect(confirm).toBeVisible();
  // Rust's sentence, verbatim — this file writes none of it.
  await expect(confirm.locator(".confirm")).toHaveText(owning.removePrompt);
  expect(
    await calls(page, "settings_remove_account"),
    "nothing may be removed before the operator has answered"
  ).toEqual([]);

  await confirm.locator(".btn.cancel").click();
  await expect(confirm).toBeHidden();
  expect(await calls(page, "settings_remove_account")).toEqual([]);

  await row.locator(".btn.delete").first().click();
  await confirm.locator(".btn.delete").click();
  expect(await calls(page, "settings_remove_account")).toEqual([
    { command: "settings_remove_account", args: { id: owning.id } },
  ]);
});

/** An account nothing depends on removes in one click, like a host: a
 *  confirmation over no consequence teaches an operator to click through the
 *  one that has a consequence. */
test("an account nothing depends on removes without a confirmation", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "accounts").click();
  const { spare } = accountsOf(settings);

  const row = page.locator(`.group[data-account="${spare.id}"]`);
  await row.locator(".btn.delete").first().click();
  expect(await calls(page, "settings_remove_account")).toEqual([
    { command: "settings_remove_account", args: { id: spare.id } },
  ]);
});

test("an account is added with its vendor, and re-saved without exposing its token", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "accounts").click();
  const { owning } = accountsOf(settings);

  // The form sits behind Add account… — every form on this tab does.
  const addBox = page.locator('[data-disclosure="add-account"]');
  await expect(addBox).toBeHidden();
  await page.locator(".btn.add-account").click();
  await expect(addBox).toBeVisible();
  const add = page.locator(".btn.add");
  await expect(add, "an account needs a name").toBeDisabled();
  await page.locator("#account-name").fill("personal-2");
  await page.locator("#account-token").fill("ghp_secret");
  await expect(add).toBeEnabled();
  await add.click();
  expect(await calls(page, "settings_save_account")).toEqual([
    {
      command: "settings_save_account",
      // `id: null` is what makes this a create rather than a second row on
      // somebody else's credential.
      args: { id: null, vendor: "github", label: "personal-2", token: "ghp_secret" },
    },
  ]);
  // Handed to Rust and dropped, never carried across the re-render.
  await expect(page.locator("#account-token")).toHaveValue("");

  // Replace token… reveals the card's token field; the save carries the
  // card's own label, so a rename cannot ride a token replacement.
  const card = page.locator(`.group[data-account="${owning.id}"]`);
  const tokenBox = card.locator('[data-disclosure="replace-token"]');
  await expect(tokenBox).toBeHidden();
  await card.locator(".btn.replace-token").click();
  await expect(tokenBox).toBeVisible();
  await card.locator(`#account-token-${owning.id}`).fill("ghp_replacement");
  await card.locator(".btn.token-save").click();
  const saves = await calls(page, "settings_save_account");
  expect(saves[1]).toEqual({
    command: "settings_save_account",
    args: { id: owning.id, vendor: owning.vendor, label: owning.label, token: "ghp_replacement" },
  });
  await expect(card.locator(`#account-token-${owning.id}`)).toHaveValue("");
});

test("Usage and Azure write every provider preference together", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "usage").click();

  await expect(page.locator("#neon-org-id")).toHaveValue(settings.usage.neon.orgId);
  await expect(page.locator("#sentry-org-slug")).toHaveValue(settings.usage.sentry.orgSlug);
  await expect(page.locator("#sentry-quota")).toHaveValue(String(settings.usage.sentry.quota));

  await page.locator("#neon-org-id").fill("org-abc");
  await page.locator("#sentry-quota").fill("100000");
  await page.locator("#neon-usd-cu-hour").fill("0.106");
  await page.locator("#neon-usd-gib-month").fill("0.35");
  await expect(page.locator("#vercel-team-id")).toHaveValue(settings.usage.vercel.teamId);
  await page.locator("#vercel-team-id").fill("team_abc");
  await page.locator(".btn.apply").click();

  // The Azure budget travels with them, unchanged: `settings_save_providers`
  // writes every non-secret provider preference in one go, so sending only
  // this tab's fields would blank the ones it doesn't show.
  expect(await calls(page, "settings_save_providers")).toEqual([
    {
      command: "settings_save_providers",
      args: {
        prefs: {
          neonOrgId: "org-abc",
          sentryOrgSlug: settings.usage.sentry.orgSlug,
          sentryMonthlyEventQuota: 100000,
          azureMonthlyBudgetUsd: settings.azure.budget.value,
          neonUsdPerCuHour: 0.106,
          neonUsdPerGibMonth: 0.35,
          vercelTeamId: "team_abc",
        },
      },
    },
  ]);
});

/// The mirror image, and the edit most likely to be missed: the Azure tab
/// re-sends every Usage preference, so a save from *there* must carry the
/// Vercel team id too or applying a budget silently blanks it.
test("the Azure tab passes the Vercel team id through untouched", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "azure").click();
  await page.locator("#azure-budget").fill("250");
  // `.first()`: the tab now carries a second Apply for the export address, and
  // this test is about the budget one. Addressed by position rather than by a
  // new hook because the order is the reading order.
  await page.locator(".btn.apply").first().click();

  const [save] = await calls(page, "settings_save_providers");
  expect(save.args.prefs.vercelTeamId).toBe(settings.usage.vercel.teamId);
  expect(save.args.prefs.azureMonthlyBudgetUsd).toBe(250);
});

test("a typed org ID survives saving that provider's key", async ({ page, baseURL }) => {
  await openSettings(page, baseURL);
  await tab(page, "usage").click();

  // The natural fill-in order: type the org ID, paste the key, and press the
  // Save sitting right under the key — before the tab-level Apply. The secret
  // save re-renders the tab from persisted state, and the unapplied org ID
  // must survive that render or it is silently lost while the "Saved." status
  // says otherwise.
  await page.locator("#neon-org-id").fill("org-fond-sea-12345678");
  await page.locator("#secret-neon").fill("napi_smoke");
  await page.locator('.group[data-secret="neon"] .btn.save').click();
  expect(await calls(page, "settings_save_secret")).toEqual([
    { command: "settings_save_secret", args: { key: "neon", value: "napi_smoke" } },
  ]);
  await expect(page.locator("#neon-org-id")).toHaveValue("org-fond-sea-12345678");

  // And the surviving value is what Apply hands to Rust.
  await page.locator(".btn.apply").click();
  const saved = await calls(page, "settings_save_providers");
  expect(saved.at(-1).args.prefs.neonOrgId).toBe("org-fond-sea-12345678");
});

test("Services lists every watched status page with the component it watches", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "services").click();

  const t = settings.services;
  await expect(page.locator(".vendor-row")).toHaveCount(t.rows.length);
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

/**
 * The distinction the whole feature rests on: a check that could not run must
 * not paint as one that ran and found nothing. Neither may offer an install.
 */
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
