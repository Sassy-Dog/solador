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
      function project() {
        const view = structuredClone(original);
        view.layout = structuredClone(layout);
        view.tiles = layout.tiles
          .filter((t) => !t.hidden)
          .map((t) => {
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
          });
        return view;
      }
      window.__TAURI__ = {
        core: {
          invoke: async (command, args) => {
            window.__CALLS__.push({ command, args });
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
  await action(page, "restore").click();
  await expect(tile(page, "ghWorkflows")).toBeVisible();
  expect((await savedLayout(page)).revision).toBe(2);
});

test("a duplicate has an independent scope, width and draft that polling preserves", async ({
  page,
  baseURL,
}) => {
  await openDashboard(page, baseURL);
  await action(page, "edit").click();
  await tile(page, "hosts").locator('[data-action="configure"]').click();
  await action(page, "duplicate").click();
  await expect(page.locator(".db-tile")).toHaveCount(6);
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
  await expect(page.locator("#dashboardInspector")).toBeHidden();
  const layout = await savedLayout(page),
    duplicate = layout.tiles.at(-1);
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
  await page.evaluate(() => {
    window.__FAIL_SAVE__ = true;
  });
  await action(page, "apply").click();
  await expect(page.locator(".db-live-note")).toHaveText(
    "Could not save the dashboard: disk unavailable.",
  );
  await expect(page.locator("#dashboard-title")).toHaveValue("My machines");
  await expect(page.locator("#dashboard-title")).toBeEnabled();
  await expect(tile(page, "hosts").locator("h2")).toHaveText("Machines");
  await expect(action(page, "undo")).toBeDisabled();
  expect(await savedLayout(page)).toBeNull();
  await page.evaluate(() => {
    window.__FAIL_SAVE__ = false;
  });
  await action(page, "apply").click();
  await expect(tile(page, "hosts").locator("h2")).toHaveText("My machines");
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
    page.locator('#settings .tab[data-tab="accounts"]'),
  ).toHaveAttribute("data-active", "true");
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
  const title = '<img src=x onerror="alert(1)"> Long machine name';
  await page.locator("#dashboard-title").fill(title);
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
