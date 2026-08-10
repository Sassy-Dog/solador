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
async function stubIpc(page, cockpit, settings) {
  await page.addInitScript(
    ({ cockpit, settings, testResult }) => {
      window.__CALLS__ = [];
      window.__TAURI__ = {
        core: {
          invoke: async (command, args) => {
            window.__CALLS__.push({ command, args });
            if (command === "cockpit") return cockpit;
            if (command === "settings_view") return settings;
            if (command === "settings_test_host") return { id: args.id, result: testResult };
            return { status: "Saved.", settings };
          },
        },
      };
    },
    { cockpit, settings, testResult: TEST_RESULT }
  );
}

const calls = (page, command) =>
  page.evaluate((c) => window.__CALLS__.filter((call) => call.command === c), command);

/** Loads the app with the IPC stub installed and opens Settings. */
async function openSettings(page, baseURL) {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  const settings = await fixture(baseURL, "sample-settings.json");
  await stubIpc(page, cockpit, settings);
  await page.goto("/index.html");
  await expect(page.locator("#settingsToggle")).toBeVisible();
  await page.locator("#settingsToggle").click();
  await expect(page.locator("#settings")).toBeVisible();
  return settings;
}

const tab = (page, id) => page.locator(`.tab[data-tab="${id}"]`);

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
  // The picker offers exactly what Rust offers -- an option invented here
  // would be a cadence the store launders straight back to the default.
  await expect(page.locator("#general-interval option")).toHaveText(
    g.refreshInterval.options.map((o) => o.label)
  );
  // The host-overflow picker left this tab: it is per breakpoint now.
  await expect(page.locator("#general-overflow")).toHaveCount(0);

  await page.locator("#general-interval").selectOption("300");
  await page.locator("#general-core-rows").fill("4");
  await page.locator(".btn.apply").click();

  expect(await calls(page, "settings_save_general")).toEqual([
    {
      command: "settings_save_general",
      args: { refreshIntervalSecs: 300, coreRowSpan: 4 },
    },
  ]);
  await expect(page.locator("#settingsStatus")).toHaveText("Saved.");
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
  await tab(page, "github").click();

  const secret = settings.github.secret;
  const box = page.locator('.group[data-secret="github"]');
  const input = page.locator("#secret-github");
  await expect(input).toHaveAttribute("type", "password");
  await expect(input).toHaveValue("");
  // The fixture stores a GitHub token, so the badge is up and Clear is live;
  // Save waits for something to save.
  await expect(box.locator(".badge-ok")).toHaveText(secret.storedLabel);
  await expect(box.locator(".save")).toBeDisabled();
  await expect(box.locator(".clear")).toBeEnabled();

  await input.fill("ghp_supersecret");
  await expect(box.locator(".save")).toBeEnabled();
  await box.locator(".save").click();
  expect(await calls(page, "settings_save_secret")).toEqual([
    { command: "settings_save_secret", args: { key: "github", value: "ghp_supersecret" } },
  ]);
  await expect(input).toHaveValue("");
  await expect(page.locator("#settingsStatus")).toHaveText("Saved.");

  await box.locator(".clear").click();
  expect(await calls(page, "settings_clear_secret")).toEqual([
    { command: "settings_clear_secret", args: { key: "github" } },
  ]);

  // Nothing in the payload, and therefore nothing in the page, can echo a
  // stored credential back: the view-model carries a boolean per credential.
  expect(JSON.stringify(settings)).not.toContain("ghp_");
  await expect(page.locator("body")).not.toContainText("ghp_supersecret");
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

test("Portfolio adds, toggles and edits watched workflows", async ({ page, baseURL }) => {
  const settings = await openSettings(page, baseURL);
  await tab(page, "portfolio").click();

  const rows = page.locator(".repo-row");
  await expect(rows).toHaveCount(settings.portfolio.rows.length);
  await expect(rows.first().locator(".slug")).toHaveText(settings.portfolio.rows[0].slug);

  const add = page.locator(".btn.add");
  await expect(add).toBeDisabled();
  await page.locator("#repo-slug").fill("gadget");
  await expect(add, "a bare name is not owner/name").toBeDisabled();
  await page.locator("#repo-slug").fill("acme/lathe");
  await expect(add).toBeEnabled();
  await add.click();
  expect(await calls(page, "settings_add_repo")).toEqual([
    { command: "settings_add_repo", args: { slug: "acme/lathe" } },
  ]);

  const first = settings.portfolio.rows[0].slug;
  const row = page.locator(`.repo-row[data-repo="${first}"]`);
  await row.locator(".input").fill("release.yml, deploy.yml");
  await row.locator(".input").blur();
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
  await row.locator(".toggle").click();
  expect(await calls(page, "settings_set_repo_enabled")).toEqual([
    { command: "settings_set_repo_enabled", args: { slug: first, enabled: false } },
  ]);
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
