import { test, expect } from "@playwright/test";

// Real UI and Rust-dumped readings under Tauri's CSP. The IPC double projects
// edited tiles; Rust tests separately cover policy and actual disk persistence.
async function openDashboard(page, baseURL) {
  const names = {
    dashboard_view: "dashboard",
    cockpit: "cockpit",
    settings_view: "settings",
    repos: "repos",
    runners: "runners",
    containers: "containers",
    services: "services",
    crons: "crons",
    usage: "usage",
    azure_cost: "azure",
    openclaw: "openclaw",
  };
  const fixtures = {};
  for (const [command, name] of Object.entries(names)) {
    fixtures[command] = await (
      await fetch(`${baseURL}/sample-${name}.json`)
    ).json();
  }
  await page.addInitScript(
    ({ fixtures }) => {
      const original = fixtures.dashboard_view;
      let layout =
        JSON.parse(localStorage.getItem("test-dashboard") || "null") ||
        original.layout;
      window.__CALLS__ = [];
      window.__SET_LAYOUT__ = (next) => { layout = structuredClone(next); };
      function project(previewTile) {
        const view = structuredClone(original);
        view.layout = structuredClone(layout);
        const projectTile = (t) => {
            const source = view.sources.find((s) => s.id === t.source);
            const base =
              original.tiles.find((old) => old.source === t.source) || {};
            const rows = source.rows.filter(
              (r) =>
                t.scope === "all" ||
                (t.scope === "attention" && r.attention) ||
                t.scope === `item:${r.id}` ||
                r.scopes.includes(t.scope),
            );
            const limit =
              t.presentation === "detailed" ? 12 : t.source === "hosts" ? 4 : 5;
            return {
              ...base,
              ...t,
              rows: rows.slice(0, limit),
              warnings: source.warnings,
              scopeLabel:
                source.scopes.find((s) => s.value === t.scope)?.label ||
                "Resource no longer available",
              moreCount: Math.max(0, rows.length - limit),
              footer: `${Math.min(rows.length, limit)} shown`,
              empty: source.message,
            };
          };
        view.tiles = (previewTile ? [previewTile] : layout.tiles.filter(t => !t.hidden)).map(projectTile);
        view.hiddenTiles = layout.tiles.filter(t => t.hidden).map(projectTile);
        return view;
      }
      window.__TAURI__ = {
        core: {
          invoke: async (command, args) => {
            window.__CALLS__.push({ command, args });
            if (command === "dashboard_preview") {
              const preview = project(args.tile).tiles[0];
              if (window.__HOLD_PREVIEW__) {
                window.__HOLD_PREVIEW__ = false;
                return await new Promise(resolve => { window.__RELEASE_PREVIEW__ = () => resolve(preview); });
              }
              return preview;
            }
            if (command === "settings_view" && args?.route) return { ...fixtures.settings_view, connectionRoute: window.__SETTINGS_ROUTE__ };
            if (command === "dashboard_view") {
              if (window.__FAIL_READ__) throw new Error("Reading unavailable");
              const snapshot = project();
              if (window.__HOLD_READ__) {
                window.__HOLD_READ__ = false;
                return await new Promise((resolve) => {
                  window.__RELEASE_READ__ = () => resolve(snapshot);
                });
              }
              return snapshot;
            }
            if (command === "dashboard_save") {
              if (window.__FAIL_SAVE__)
                throw new Error(
                  "Could not save the dashboard: disk unavailable.",
                );
              if (args.expectedRevision !== layout.revision)
                throw new Error(
                  "The dashboard changed while you were editing.",
                );
              layout = {
                ...structuredClone(args.layout),
                revision: layout.revision + 1,
              };
              localStorage.setItem("test-dashboard", JSON.stringify(layout));
              return project();
            }
            if (command === "plugin:opener|open_url") return null;
            return fixtures[command] || null;
          },
        },
      };
    },
    { fixtures },
  );
  await page.goto("/index.html");
  await expect(page.locator("#dashboardOverview")).toBeVisible();
  await expect(page.locator(".db-tile")).toHaveCount(5);
  return fixtures.dashboard_view;
}
const action = (page, name) =>
  page.locator(`#dashboardOverview [data-action="${name}"]`);
const tile = (page, source) => page.locator(`[data-tile="overview-${source}"]`);
const savedLayout = (page) =>
  page.evaluate(() => JSON.parse(localStorage.getItem("test-dashboard")));

test("hidden library previews the saved scope and restores the original slot", async ({ page, baseURL }) => {
  await page.setViewportSize({ width: 375, height: 812 });
  const original = await openDashboard(page, baseURL);
  await action(page, "edit").click();
  await tile(page, "hosts").locator('[data-action="configure"]').click();
  const title = '<img src=x onerror="alert(1)"> $& Remote';
  await page.locator("#dashboard-title").fill(title);
  await page.locator("#dashboard-scope").selectOption("remote");
  await page.locator("#dashboard-width").selectOption("wide");
  await action(page, "apply").click();
  const configured = (await savedLayout(page)).tiles[0];
  await tile(page, "hosts").locator('[data-action="hide"]').click();
  await tile(page, "ghRunners").locator('[data-action="hide"]').click();
  await page.locator(".db-hidden-count").click();
  await expect(page.locator(".db-hidden-list button")).toHaveCount(2);
  await expect(page.locator(".db-hidden-list strong").first()).toHaveText(title);
  await expect(page.locator(".db-hidden-list button").first()).toContainText("Wide · Summary");
  await expect(page.locator(".db-hidden-preview .db-tile-title")).toHaveText(title);
  await expect(page.locator(".db-hidden-preview .db-item")).toHaveCount(3);
  await expect(page.locator(".db-hidden-library img")).toHaveCount(0);
  await page.locator('.db-hidden-list [data-id="overview-ghRunners"]').click();
  await expect(page.locator('.db-hidden-list [data-id="overview-ghRunners"]')).toBeFocused();
  await expect(page.locator(".db-hidden-preview .db-tile-title")).toHaveText("Runners");
  await page.locator('.db-hidden-list [data-id="overview-hosts"]').click();
  await action(page, "restore").focus();
  const reads = await page.evaluate(() => window.__CALLS__.filter(c => c.command === "dashboard_view").length);
  await expect.poll(() => page.evaluate(() => window.__CALLS__.filter(c => c.command === "dashboard_view").length)).toBeGreaterThan(reads);
  await expect(action(page, "restore")).toBeFocused();
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(375);
  await action(page, "restore").click();
  expect((await savedLayout(page)).tiles[0]).toEqual(configured);
  await expect(tile(page, "hosts").locator('[data-action="configure"]')).toBeFocused();
  await expect(page.locator(".db-attention-items button")).toHaveText(original.attention.map(a => a.label));
});

test("removing a visible tile keeps source alerts, persists, and Undo restores its full configuration", async ({ page, baseURL }) => {
  await page.setViewportSize({ width: 1024, height: 500 });
  const original = await openDashboard(page, baseURL);
  await action(page, "edit").click();
  await tile(page, "ghWorkflows").locator('[data-action="configure"]').click();
  await expect(page.locator(".db-removal")).toContainText("keeps its connection and monitoring");
  await action(page, "remove").click();
  expect((await savedLayout(page)).tiles).toEqual(original.layout.tiles.filter(t => t.source !== "ghWorkflows"));
  await expect(tile(page, "ghWorkflows")).toHaveCount(0);
  await expect(action(page, "undo")).toBeFocused();
  await expect(action(page, "undo")).toBeInViewport({ ratio: 1 });
  await expect(page.locator(".db-attention-items button")).toHaveText(original.attention.map(a => a.label));
  await action(page, "undo").click();
  expect((await savedLayout(page)).tiles).toEqual(original.layout.tiles);
  await tile(page, "ghWorkflows").locator('[data-action="configure"]').click();
  await action(page, "remove").click();
  await page.reload();
  await expect(tile(page, "ghWorkflows")).toHaveCount(0);
  await expect(page.locator(".db-grid .db-tile")).toHaveCount(4);
});

test("a long hidden library keeps the selected tile and its recovery controls in view", async ({ page, baseURL }) => {
  await page.setViewportSize({ width: 1024, height: 768 });
  const original = await openDashboard(page, baseURL);
  await page.evaluate(layout => {
    const hidden = Array.from({ length: 12 }, (_, i) => ({ ...layout.tiles[0], id: `saved-hosts-${i}`, title: `Saved machines ${i + 1}`, scope:"remote", hidden:true }));
    window.__SET_LAYOUT__({ ...layout, revision:1, tiles:[...layout.tiles, ...hidden] });
  }, original.layout);
  await action(page, "edit").click();
  await expect(page.locator(".db-hidden-count")).toHaveText("Hidden tiles · 12");
  await page.locator(".db-hidden-count").click();
  const last = page.locator('.db-hidden-list [data-id="saved-hosts-11"]');
  await last.click();
  await expect(last).toBeFocused();
  await expect(last).toBeInViewport({ ratio:1 });
  await expect(page.locator(".db-hidden-preview .db-tile-title")).toHaveText("Saved machines 12");
  await expect(action(page, "restore")).toBeInViewport({ ratio:1 });
  await action(page, "restore").click();
  await expect(page.locator('[data-tile="saved-hosts-11"]')).toBeVisible();
  expect((await savedLayout(page)).tiles.filter(t => t.hidden)).toHaveLength(11);
});

test("a failed removal preserves the hidden library and Undo restores a removed hidden tile", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL);
  await action(page, "edit").click();
  await tile(page, "hosts").locator('[data-action="hide"]').click();
  const before = await savedLayout(page);
  await page.locator(".db-hidden-count").click();
  await page.evaluate(() => { window.__FAIL_SAVE__ = true; });
  await action(page, "remove").click();
  await expect(page.locator(".db-live-note")).toContainText("disk unavailable");
  expect(await savedLayout(page)).toEqual(before);
  await expect(page.locator(".db-hidden-preview .db-tile-title")).toHaveText("Machines");
  await expect(action(page, "restore")).toBeEnabled();
  await page.evaluate(() => { window.__FAIL_SAVE__ = false; });
  await action(page, "remove").click();
  await expect(page.locator(".db-hidden-preview")).toContainText("No hidden tiles.");
  await expect(page.locator(".db-hidden-list button")).toHaveCount(0);
  expect((await savedLayout(page)).tiles).toHaveLength(4);
  await action(page, "undo").click();
  expect((await savedLayout(page)).tiles).toEqual(before.tiles);
  await expect(tile(page, "hosts")).toHaveCount(0);
  await page.locator(".db-hidden-count").click();
  await action(page, "restore").click();
  await expect(tile(page, "hosts")).toBeVisible();
});

test("removing the final tile retains the empty dashboard and its independent alerts", async ({ page, baseURL }) => {
  const original = await openDashboard(page, baseURL);
  await action(page, "edit").click();
  for (const t of original.layout.tiles) {
    await page.locator(`[data-tile="${t.id}"] [data-action="configure"]`).click();
    await action(page, "remove").click();
  }
  await expect(page.locator(".db-grid .db-empty")).toBeVisible();
  await expect(page.locator(".db-attention-items button")).toHaveText(original.attention.map(a => a.label));
  await page.reload();
  await expect(page.locator(".db-grid .db-tile")).toHaveCount(0);
  await action(page, "edit").click();
  await action(page, "catalog").click();
  await page.locator('[data-action="preset"][data-id="remote-machines"]').click();
  await expect(page.locator(".db-placement-tile")).toHaveCount(1);
  await action(page, "apply").click();
  await expect(page.locator(".db-grid .db-tile")).toHaveCount(1);
});

test("presets open scoped drafts, can be customized, and never save on selection or Return", async ({ page, baseURL }) => {
  const original = await openDashboard(page, baseURL);
  await action(page, "edit").click();
  for (const preset of original.presets) {
    await action(page, "catalog").click();
    await page.locator(`[data-action="preset"][data-id="${preset.id}"]`).click();
    await expect(page.locator("#dashboard-title")).toHaveValue(preset.tile.title);
    await expect(page.locator("#dashboard-scope")).toHaveValue(preset.tile.scope);
    await expect(page.locator("#dashboard-width")).toHaveValue(preset.tile.width);
    await expect(page.locator(".db-preview-tile .db-tile-title")).toHaveText(preset.tile.title);
    const rows = original.sources.find(s => s.id === preset.tile.source).rows.filter(r => preset.tile.scope === "attention" ? r.attention : r.scopes.includes(preset.tile.scope));
    await expect(page.locator(".db-preview-tile .db-item-name")).toHaveText(rows.slice(0, preset.tile.source === "hosts" ? 4 : 5).map(r => r.label));
    await page.locator("#dashboard-scope").press("Enter");
    expect(await savedLayout(page)).toBeNull();
    await action(page, "close").click();
  }
  await action(page, "catalog").click();
  await page.locator('[data-action="preset"][data-id="linux-runners"]').click();
  await page.locator("#dashboard-title").fill("Build runners");
  await page.locator("#dashboard-presentation").selectOption("detailed");
  await page.locator("#dashboard-position").selectOption("start");
  await action(page, "apply").click();
  const saved = (await savedLayout(page)).tiles[0];
  expect(saved).toMatchObject({ title:"Build runners", source:"ghRunners", scope:"LINUX", presentation:"detailed", hidden:false });
  expect(saved.id).not.toBe("draft");
  await action(page, "undo").click();
  expect((await savedLayout(page)).tiles).toEqual(original.layout.tiles);
});

test("tile placement previews an insertion without moving saved tiles, then persists and undoes it", async ({ page, baseURL }) => {
  const original = await openDashboard(page, baseURL);
  await action(page, "edit").click();
  await action(page, "catalog").click();
  await page.locator('[data-action="add"][data-id="hosts"]').click();
  await page.getByLabel("Position", { exact: true }).selectOption("after:overview-ghWorkflows");
  await page.locator("#dashboard-title").fill("Build machines");
  await page.locator("#dashboard-width").selectOption("wide");
  await expect(page.locator('.db-placement-tile[aria-current="true"]')).toHaveAttribute("data-width", "wide");
  const order = ["Machines", "GitHub repos", "Build machines", "Runners", "Service health", "Scheduled jobs"];
  await expect(page.locator(".db-placement-title")).toHaveText(order);
  await expect(page.locator(".db-grid .db-tile-title")).toHaveText(original.tiles.map(t => t.title));
  expect(await savedLayout(page)).toBeNull();
  await action(page, "apply").click();
  await expect(page.locator(".db-grid .db-tile-title")).toHaveText(order);
  expect((await savedLayout(page)).tiles[2].title).toBe("Build machines");
  await expect(page.getByRole("button", {name:"Configure Build machines",exact:true})).toBeFocused();
  await page.reload();
  await expect(page.locator(".db-grid .db-tile-title")).toHaveText(order);
  await action(page, "edit").click();
  await page.getByRole("button", {name:"Configure Build machines",exact:true}).click();
  await page.getByLabel("Position", { exact: true }).selectOption("start");
  await action(page, "apply").click();
  await expect(page.locator(".db-grid .db-tile-title").first()).toHaveText("Build machines");
  await action(page, "undo").click();
  await expect(page.locator(".db-grid .db-tile-title")).toHaveText(order);
});

test("keeping a position retains hidden slots and a missing destination requires another choice", async ({ page, baseURL }) => {
  const original = await openDashboard(page, baseURL);
  await action(page, "edit").click();
  await tile(page, "ghWorkflows").locator('[data-action="hide"]').click();
  await tile(page, "ghRunners").locator('[data-action="configure"]').click();
  await page.locator("#dashboard-title").fill("Build runners");
  await expect(page.locator(".db-placement-tile")).toHaveCount(4);
  await action(page, "apply").click();
  expect((await savedLayout(page)).tiles.map(t => t.id)).toEqual(original.layout.tiles.map(t => t.id));
  await tile(page, "hosts").locator('[data-action="configure"]').click();
  await page.getByLabel("Position", { exact: true }).selectOption("after:overview-services");
  await page.evaluate(() => {
    const next = JSON.parse(localStorage.getItem("test-dashboard"));
    next.tiles.find(t => t.source === "services").hidden = true;
    next.revision++;
    window.__SET_LAYOUT__(next);
  });
  await expect(tile(page, "services")).toHaveCount(0);
  await expect(page.locator(".db-placement-error")).toHaveText("That tile is no longer visible. Choose another position.");
  const saves = await page.evaluate(() => window.__CALLS__.filter(c => c.command === "dashboard_save").length);
  await action(page, "apply").click();
  expect(await page.evaluate(() => window.__CALLS__.filter(c => c.command === "dashboard_save").length)).toBe(saves);
  await page.getByLabel("Position", { exact: true }).selectOption("end");
  await action(page, "apply").click();
  expect((await savedLayout(page)).tiles.at(-1).source).toBe("hosts");
});

test.beforeEach(async ({ page }) => {
  page.dashboardErrors = [];
  page.on("requestfailed", (request) =>
    page.dashboardErrors.push(
      `${request.url()}: ${request.failure()?.errorText}`,
    ),
  );
  page.on("pageerror", (error) => page.dashboardErrors.push(error.message));
  page.on("console", (msg) => {
    if (
      msg.type() === "error" &&
      /content security policy|refused to (apply|load)/i.test(msg.text())
    )
      page.dashboardErrors.push(msg.text());
  });
});
test.afterEach(async ({ page }) => {
  expect(page.dashboardErrors).toEqual([]);
});

test("the default overview fits a laptop and keeps all source alerts", async ({
  page,
  baseURL,
}) => {
  await page.setViewportSize({ width: 1024, height: 768 });
  const model = await openDashboard(page, baseURL);
  await expect(page.locator("#cockpitView")).toBeHidden();
  await expect(page.locator(".db-tile-title")).toHaveText(
    model.tiles.map((t) => t.title),
  );
  await expect(page.locator(".db-attention-items button")).toHaveText(
    model.attention.map((a) => a.label),
  );
  expect(
    model.attention.some(
      (a) => !model.tiles.some((t) => t.source === a.source),
    ),
  ).toBe(true);
  const rects = await page.locator(".db-tile").evaluateAll((els) =>
    els.map((el) => {
      const r = el.getBoundingClientRect();
      return { x: r.x, y: r.y, width: r.width };
    }),
  );
  expect(rects[0].y).toBe(rects[1].y);
  expect(rects[2].y).toBe(rects[3].y);
  expect(rects[3].y).toBe(rects[4].y);
  expect(rects[2].y).toBeGreaterThan(rects[0].y);
  expect(
    await page.evaluate(() => document.documentElement.scrollHeight),
  ).toBeLessThanOrEqual(768);
  const row = tile(page, "hosts").locator('[data-action="row"]').first();
  await row.focus();
  const reads = await page.evaluate(
    () => window.__CALLS__.filter((c) => c.command === "dashboard_view").length,
  );
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          window.__CALLS__.filter((c) => c.command === "dashboard_view").length,
      ),
    )
    .toBeGreaterThan(reads);
  await expect(row).toBeFocused();
});

test("hidden tiles survive reopening, keep alerts, and can be restored", async ({
  page,
  baseURL,
}) => {
  const model = await openDashboard(page, baseURL);
  await action(page, "edit").click();
  await tile(page, "ghWorkflows").locator('[data-action="hide"]').click();
  await expect(tile(page, "ghWorkflows")).toHaveCount(0);
  await expect(page.locator(".db-attention-items button")).toHaveText(
    model.attention.map((a) => a.label),
  );
  expect(
    (await savedLayout(page)).tiles.find((t) => t.source === "ghWorkflows")
      .hidden,
  ).toBe(true);
  await page.reload();
  await expect(page.locator(".db-tile")).toHaveCount(4);
  await action(page, "edit").click();
  await action(page, "catalog").click();
  await page.locator('.db-library-link').click();
  await action(page, "restore").click();
  await expect(tile(page, "ghWorkflows")).toBeVisible();
  expect((await savedLayout(page)).revision).toBe(2);
});

test("adding a tile waits for its name, scope and size before saving", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL);
  await action(page, "edit").click();
  await action(page, "catalog").click();
  await page.locator('[data-action="add"][data-id="hosts"]').click();
  await expect(page.locator("#dashboardForm")).toBeVisible();
  expect(await savedLayout(page)).toBeNull();
  await expect(page.locator(".db-grid > .db-tile")).toHaveCount(5);
  await page.locator("#dashboard-title").fill("Remote build machines");
  await page.locator("#dashboard-scope").selectOption("remote");
  await page.locator("#dashboard-width").selectOption("wide");
  await expect(page.locator(".db-preview-tile")).toHaveAttribute("data-width", "wide");
  await expect(page.locator(".db-preview-tile .db-tile-title")).toHaveText("Remote build machines");
  await expect(page.locator(".db-preview-tile .db-item")).toHaveCount(3);
  await page.locator("#dashboard-scope").press("Enter");
  expect(await savedLayout(page)).toBeNull();
  await action(page, "apply").click();
  expect((await savedLayout(page)).tiles.at(-1)).toMatchObject({
    title: "Remote build machines", source: "hosts", scope: "remote", width: "wide",
  });
  await expect(page.locator(".db-grid > .db-tile")).toHaveCount(6);
  await action(page, "catalog").click();
  await page.locator('[data-action="add"][data-id="claudeUsage"]').click();
  await action(page, "close").click();
  expect((await savedLayout(page)).tiles).toHaveLength(6);
});

test("a connection detour keeps a new tile's draft and returns to its preview", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL);
  await page.evaluate(() => {
    window.__SETTINGS_ROUTE__ = { editor: { kind: "local", entityId: null }, kinds: ["local"] };
  });
  await action(page, "edit").click();
  await action(page, "catalog").click();
  await page.locator('[data-action="add"][data-id="hosts"]').click();
  await page.locator("#dashboard-title").fill("My workstation");
  await page.locator("#dashboard-scope").selectOption("local");
  await page.getByLabel("Position", { exact: true }).selectOption("start");
  await action(page, "manage").click();
  await expect(page.locator("#settings .connection-heading h2")).toHaveText("This machine");
  expect(await page.evaluate(() => window.__CALLS__.find(c => c.command === "settings_view").args)).toEqual({route:{source:"hosts",scope:"local"}});
  await page.locator("#settingsClose").click();
  await expect(page.locator("#dashboard-title")).toHaveValue("My workstation");
  await expect(page.locator("#dashboard-scope")).toHaveValue("local");
  await expect(page.getByLabel("Position", { exact: true })).toHaveValue("start");
  await expect(page.locator(".db-preview-tile .db-item")).toHaveCount(1);
  expect(await savedLayout(page)).toBeNull();
});

test("resource details open their editor while aggregate links show only relevant connections", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL);
  await page.evaluate(() => { window.__SETTINGS_ROUTE__ = {editor:null,kinds:["account"],heading:"GitHub connections",help:"Choose a connection."}; });
  await tile(page, "ghWorkflows").locator('.db-tile-footer [data-action="details"]').click();
  await action(page, "manage").click();
  await expect(page.locator("#settingsBody > .connection-heading h2")).toHaveText("GitHub connections");
  await expect(page.locator(".connection-row:not([data-kind='account'])")).toHaveCount(0);
  await page.locator("#settingsClose").click();
  await page.evaluate(() => { window.__SETTINGS_ROUTE__ = {editor:{kind:"sentry"},kinds:["sentry"]}; });
  await action(page, "close").click();
  await tile(page, "sentryCrons").locator('[data-action="row"]').first().click();
  await action(page, "manage").click();
  await expect(page.locator("#sentry-org-slug")).toBeVisible();
  await expect(page.locator("#neon-org-id")).toHaveCount(0);
  expect(await page.evaluate(() => window.__CALLS__.filter(c => c.command === "settings_view").at(-1).args.route.source)).toBe("sentryCrons");
});

test("a late preview cannot replace a newer scope choice", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL);
  await action(page, "edit").click();
  await tile(page, "hosts").locator('[data-action="configure"]').click();
  await expect(page.locator(".db-preview-tile .db-item")).toHaveCount(4);
  await page.evaluate(() => { window.__HOLD_PREVIEW__ = true; });
  await page.locator("#dashboard-scope").selectOption("remote");
  await expect.poll(() => page.evaluate(() => typeof window.__RELEASE_PREVIEW__)).toBe("function");
  await page.locator("#dashboard-scope").selectOption("local");
  await expect(page.locator(".db-preview-tile .db-item")).toHaveCount(1);
  await page.evaluate(() => window.__RELEASE_PREVIEW__());
  await expect(page.locator(".db-preview-tile .db-item")).toHaveCount(1);
  await expect(page.locator(".db-preview-tile .db-tile-note")).toContainText("This machine");
  expect(await savedLayout(page)).toBeNull();
});

test("a duplicate has an independent scope, width and draft that polling preserves", async ({
  page,
  baseURL,
}) => {
  await openDashboard(page, baseURL);
  await action(page, "edit").click();
  await tile(page, "hosts").locator('[data-action="configure"]').click();
  await action(page, "duplicate").click();
  await expect(page.locator(".db-tile")).toHaveCount(5);
  await page.locator("#dashboard-title").fill("Remote machines only");
  await page.locator("#dashboard-scope").selectOption("remote");
  await page.locator("#dashboard-width").selectOption("wide");
  await page.locator("#dashboard-presentation").selectOption("detailed");
  const reads = await page.evaluate(
    () => window.__CALLS__.filter((c) => c.command === "dashboard_view").length,
  );
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          window.__CALLS__.filter((c) => c.command === "dashboard_view").length,
      ),
    )
    .toBeGreaterThan(reads);
  await expect(page.locator("#dashboard-title")).toHaveValue(
    "Remote machines only",
  );
  await page.locator("#dashboard-title").press("Enter");
  await expect(page.locator("#dashboardInspector")).toBeVisible();
  await action(page, "apply").click();
  await expect(page.locator("#dashboardInspector")).toBeHidden();
  const layout = await savedLayout(page),
    duplicate = layout.tiles[1];
  expect(duplicate).toMatchObject({
    title: "Remote machines only",
    scope: "remote",
    width: "wide",
    presentation: "detailed",
  });
  expect(layout.tiles[0]).toMatchObject({
    scope: "all",
    width: "medium",
    presentation: "summary",
  });
  expect(duplicate.id).not.toBe(layout.tiles[0].id);
  const copied = page.locator(`[data-tile="${duplicate.id}"]`);
  await expect(copied.locator('[data-id="local"]')).toHaveCount(0);
  await expect(tile(page, "hosts").locator('[data-id="local"]')).toHaveCount(1);
  await expect(copied).toHaveAttribute("data-width", "wide");
});

test("a failed save retains the old layout and the editable draft", async ({
  page,
  baseURL,
}) => {
  await openDashboard(page, baseURL);
  await action(page, "edit").click();
  await tile(page, "hosts").locator('[data-action="configure"]').click();
  await page.locator("#dashboard-title").fill("My machines");
  await page.getByLabel("Position", { exact: true }).selectOption("end");
  await page.evaluate(() => {
    window.__FAIL_SAVE__ = true;
  });
  await action(page, "apply").click();
  await expect(page.locator(".db-live-note")).toHaveText(
    "Could not save the dashboard: disk unavailable.",
  );
  await expect(page.locator("#dashboard-title")).toHaveValue("My machines");
  await expect(page.locator("#dashboard-title")).toBeEnabled();
  await expect(page.getByLabel("Position", { exact: true })).toHaveValue("end");
  await expect(tile(page, "hosts").locator("h2")).toHaveText("Machines");
  await expect(action(page, "undo")).toBeDisabled();
  expect(await savedLayout(page)).toBeNull();
  await page.evaluate(() => {
    window.__FAIL_SAVE__ = false;
  });
  await action(page, "apply").click();
  await expect(tile(page, "hosts").locator("h2")).toHaveText("My machines");
  expect((await savedLayout(page)).tiles.at(-1).source).toBe("hosts");
});

test("keyboard ordering, undo and dragging persist the visible order", async ({
  page,
  baseURL,
}) => {
  const original = await openDashboard(page, baseURL);
  await action(page, "edit").click();
  await tile(page, "hosts").locator('[data-action="later"]').click();
  await expect(page.locator(".db-tile-title").first()).toHaveText(
    "GitHub repos",
  );
  await action(page, "undo").click();
  await expect(page.locator(".db-tile-title")).toHaveText(
    original.tiles.map((t) => t.title),
  );
  await tile(page, "services")
    .locator(".db-drag")
    .dragTo(tile(page, "hosts").locator(".db-drag"));
  await expect(page.locator(".db-tile-title").first()).toHaveText(
    "Service health",
  );
  expect((await savedLayout(page)).tiles[0].source).toBe("services");
});

test("dragging downward reaches the last position and Undo restores the order", async ({
  page,
  baseURL,
}) => {
  const original = await openDashboard(page, baseURL);
  await action(page, "edit").click();
  await tile(page, "hosts")
    .locator(".db-drag")
    .dragTo(tile(page, "sentryCrons").locator(".db-drag"));
  const moved = [...original.tiles.slice(1), original.tiles[0]];
  await expect(page.locator(".db-tile-title")).toHaveText(
    moved.map((t) => t.title),
  );
  expect((await savedLayout(page)).tiles.map((t) => t.id)).toEqual(
    moved.map((t) => t.id),
  );
  await action(page, "undo").click();
  await expect(page.locator(".db-tile-title")).toHaveText(
    original.tiles.map((t) => t.title),
  );
});

test("clicking or cancelling a move handle leaves the saved layout alone", async ({
  page,
  baseURL,
}) => {
  const original = await openDashboard(page, baseURL);
  await action(page, "edit").click();
  const handle = tile(page, "services").locator(".db-drag");
  await handle.click();
  expect(await savedLayout(page)).toBeNull();
  const from = await handle.boundingBox();
  const to = await tile(page, "hosts").boundingBox();
  await page.mouse.move(from.x + from.width / 2, from.y + from.height / 2);
  await page.mouse.down();
  await page.mouse.move(to.x + 20, to.y + 20, { steps: 6 });
  await expect(tile(page, "hosts")).toHaveClass(/db-dragover/);
  await page.keyboard.press("Escape");
  await expect(page.locator(".db-dragover")).toHaveCount(0);
  await page.mouse.up();
  await expect(page.locator(".db-tile-title")).toHaveText(
    original.tiles.map((t) => t.title),
  );
  expect(await savedLayout(page)).toBeNull();
});

test("a pre-save poll arriving late cannot bring back the old layout", async ({
  page,
  baseURL,
}) => {
  await openDashboard(page, baseURL);
  await action(page, "edit").click();
  await page.evaluate(() => {
    window.__HOLD_READ__ = true;
  });
  await expect
    .poll(() => page.evaluate(() => typeof window.__RELEASE_READ__))
    .toBe("function");
  await tile(page, "hosts").locator('[data-action="hide"]').click();
  await expect(tile(page, "hosts")).toHaveCount(0);
  await page.evaluate(() => window.__RELEASE_READ__());
  await expect(tile(page, "hosts")).toHaveCount(0);
  await expect(action(page, "undo")).toBeEnabled();
});

test("details reach the existing full panel and the source's connection settings", async ({
  page,
  baseURL,
}) => {
  await openDashboard(page, baseURL);
  await tile(page, "hosts").locator('[data-action="details"]').click();
  await expect(page.locator(".db-detail-resource")).toHaveCount(4);
  await action(page, "full").click();
  await expect(page.locator("#dashboardOverview")).toBeHidden();
  await expect(page.locator("#cockpit")).toBeVisible();
  await expect(page.locator("#reposPanel")).toBeHidden();
  await page.locator("#dashboardBack").click();
  await expect(page.locator("#dashboardOverview")).toBeVisible();
  await action(page, "close").click();
  await tile(page, "ghWorkflows")
    .locator('.db-tile-footer [data-action="details"]')
    .click();
  await action(page, "manage").click();
  await expect(page.locator("#settings")).toBeVisible();
  await expect(
    page.locator('#settings .tab[data-tab="connections"]'),
  ).toHaveAttribute("data-active", "true");
  await expect(page.locator('#settings .connection-row[data-kind="account"]').first()).toBeVisible();
  await expect(page.locator("#dashboardOverview")).toBeHidden();
  await page.locator("#settingsClose").click();
  await expect(page.locator("#dashboardOverview")).toBeVisible();
  await expect(page.locator("#cockpitView")).toBeHidden();
  await action(page, "allPanels").click();
  await expect(page.locator("#cockpitView")).toBeVisible();
  await expect(page.locator("#reposPanel")).toBeVisible();
  await expect(page.locator("#containersPanel")).toBeVisible();
});

test("narrow layouts wrap tiles and render saved names as text", async ({
  page,
  baseURL,
}) => {
  await page.setViewportSize({ width: 375, height: 812 });
  await openDashboard(page, baseURL);
  await action(page, "edit").click();
  await tile(page, "hosts").locator('[data-action="configure"]').click();
  const title = '<img src=x onerror="alert(1)"> $& Long machine name';
  await page.locator("#dashboard-title").fill(title);
  await expect(page.locator('.db-placement-tile[aria-current="true"] .db-placement-title')).toHaveText(title);
  await expect(page.locator(".db-placement-tile img")).toHaveCount(0);
  const previewXs = await page.locator(".db-placement-tile").evaluateAll(els => els.map(el => el.getBoundingClientRect().x));
  expect(new Set(previewXs).size).toBe(1);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(375);
  await action(page, "apply").click();
  await expect(tile(page, "hosts").locator("h2")).toHaveText(title);
  await expect(tile(page, "hosts").locator("img")).toHaveCount(0);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(
    375,
  );
  const xs = await page
    .locator(".db-tile")
    .evaluateAll((els) => els.map((el) => el.getBoundingClientRect().x));
  expect(new Set(xs).size).toBe(1);
  await tile(page, "ghWorkflows").locator('[data-action="configure"]').click();
  await expect(page.locator('#dashboard-position option[value="after:overview-hosts"]')).toHaveText(`After ${title}`);
});

test("a failed refresh keeps the last view and clears its warning on recovery", async ({
  page,
  baseURL,
}) => {
  const model = await openDashboard(page, baseURL);
  await page.evaluate(() => {
    window.__FAIL_READ__ = true;
  });
  await expect(page.locator(".db-live-note")).toHaveText(
    model.labels.loadFailed,
  );
  await expect(page.locator(".db-tile")).toHaveCount(5);
  await page.evaluate(() => {
    window.__FAIL_READ__ = false;
  });
  await expect(page.locator(".db-live-note")).toHaveText("");
  await expect(page.locator(".db-tile-title")).toHaveText(
    model.tiles.map((t) => t.title),
  );
});
