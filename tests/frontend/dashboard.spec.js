import { test, expect } from "@playwright/test";

// Real UI and Rust-dumped readings under Tauri's CSP. The IPC double projects
// edited tiles; Rust tests separately cover policy and actual disk persistence.
async function openDashboard(page, baseURL, expanded = false) {
  const names = {
    dashboard_view: expanded ? "dashboard-expanded" : "dashboard",
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
      window.__SET_SOURCE_ROWS__ = (id, rows) => { original.sources.find(s => s.id === id).rows = structuredClone(rows); };
      window.__SET_RUNNER_SOURCE__ = (source) => { Object.assign(original.sources.find(s => s.id === 'ghRunners'), structuredClone(source)); };
      window.__SET_RUNNER_PAYLOAD__ = (payload) => { fixtures.runners = payload; };
      function project(previewTile) {
        const view = structuredClone(original);
        view.layout = structuredClone(layout);
        const projectTile = (t) => {
            const source = view.sources.find((s) => s.id === t.source);
            const base =
              original.tiles.find((old) => old.source === t.source) || {};
            const scopedRows = source.rows.filter(r => !t.selectedRepos?.length || t.selectedRepos.includes(r.id)).filter(
              (r) =>
                t.scope === "all" ||
                (t.scope === "attention" && r.attention) ||
                t.scope === `item:${r.id}` ||
                r.scopes.includes(t.scope),
            );
            // Expanded group rows are produced by Rust; no test-side aggregator.
            const rows = t.runnerView === "grouped" && base.runnerView === "grouped" ? base.rows : scopedRows;
            const limit = t.rowLimit === "all" ? rows.length :
              Number(t.rowLimit) || (t.presentation === "detailed" ? 12 : t.source === "hosts" ? 4 : 5);
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
              if (window.__HOLD_SAVE__) {
                window.__HOLD_SAVE__ = false;
                await new Promise(resolve => { window.__RELEASE_SAVE__ = resolve; });
              }
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

test('Full runners share Detail tables and the saved view choice', async ({page, baseURL}) => {
  await openDashboard(page, baseURL, true);
  await tile(page, 'ghRunners').getByRole('button', {name:/MACOS · ARM64/}).click();
  await action(page, 'full').click();
  const panel = page.locator('#runnersPanel');
  await expect(panel.locator('.db-detail-table')).toBeVisible();
  await expect(panel.locator('thead th')).toHaveText(['Runner', 'Organization', 'OS', 'Architecture', 'Status']);
  const list = panel.getByRole('button', {name:'List', exact:true});
  await list.click();
  await expect(list).toHaveAttribute('aria-pressed', 'true');
  expect((await savedLayout(page)).detailViews.ghRunners).toBe('list');
  await page.locator('#dashboardBack').click();
  await tile(page,'ghRunners').locator('.db-tile-footer [data-action="details"]').click();
  await expect(page.getByRole('button',{name:'List',exact:true})).toHaveAttribute('aria-pressed','true');
  await page.getByRole('button',{name:'Table',exact:true}).click();
  await tile(page,'ghRunners').getByRole('button',{name:/MACOS · ARM64/}).click();
  await action(page, 'full').click();
  await expect(panel.locator('.db-detail-table')).toBeVisible();
  for (const width of [375,1200]) {
    await page.setViewportSize({width,height:900});
    expect(await panel.locator('tbody tr').first().evaluate(el=>el.getBoundingClientRect().height)).toBeLessThanOrEqual(36);
    expect(await page.evaluate(()=>document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
  }
  const absent = panel.locator('tr[data-kind="absent"]').first();
  await absent.focus();
  await page.evaluate(()=>refreshPanels());
  await expect(absent).toBeFocused();
  await absent.press('Shift+F10');
  await expect(page.locator('.ctx-menu')).toBeVisible();
  await page.keyboard.press('Escape');
  await panel.getByRole('button',{name:'List',exact:true}).click();
  for (const status of await panel.locator('.gh-runner-status').all())
    expect(await status.evaluate(el=>el.scrollWidth<=el.clientWidth)).toBe(true);
});

test('machine volumes use sorted aligned columns in both Detail views', async ({page,baseURL}) => {
  const model = await openDashboard(page,baseURL);
  const host = model.sources.find(s=>s.id==='hosts').rows.find(r=>r.volumes?.length>1);
  await page.locator(`[data-action="row"][data-source="hosts"][data-id="${host.id}"]`).first().click();
  const volumes = page.locator('.db-volume-table');
  const mounts = host.volumes.map(v=>v.mount).sort();
  for (const view of ['Table','List']) {
    await page.getByRole('button',{name:view,exact:true}).click();
    await expect(volumes.locator('tbody th')).toHaveText(mounts);
    await expect(volumes.locator('thead th')).toHaveText(['Volume','Used','Total','Use']);
    const columns = await volumes.locator('tbody tr').evaluateAll(rows=>rows.map(row=>[...row.cells].map(c=>c.getBoundingClientRect().right)));
    for (const positions of columns) expect(positions).toEqual(columns[0]);
  }
});

test('Settings header has the same inset as the overview header', async ({page,baseURL}) => {
  await openDashboard(page,baseURL);
  for (const width of [375,1200]) {
    await page.setViewportSize({width,height:900});
    const overview = await page.locator('.db-chrome').evaluate(el=>({padding:getComputedStyle(el).padding}));
    await action(page,'settings').click();
    const settings = await page.locator('#settings > .topbar').evaluate(el=>({padding:getComputedStyle(el).padding,left:el.querySelector('img').getBoundingClientRect().left-el.getBoundingClientRect().left,top:el.querySelector('button').getBoundingClientRect().top-el.getBoundingClientRect().top,right:el.getBoundingClientRect().right-el.querySelector('button').getBoundingClientRect().right}));
    expect(settings.padding).toBe(overview.padding);
    expect(Math.min(settings.left,settings.top,settings.right)).toBeGreaterThanOrEqual(12);
    await page.locator('#settingsClose').click();
  }
});

test('Detail column positions stay fixed as readings and expanded content change', async ({page,baseURL}) => {
  const model = await openDashboard(page,baseURL);
  await tile(page,'hosts').locator('.db-tile-footer [data-action="details"]').click();
  const table = page.locator('#dashboardInspector > .db-inspector-body > .db-table-scroll > table');
  const positions = () => table.locator(':scope > thead th').evaluateAll(cells=>cells.map(c=>({x:c.getBoundingClientRect().x,width:c.getBoundingClientRect().width})));
  const before = await positions();
  const rows = structuredClone(model.sources.find(s=>s.id==='hosts').rows);
  rows[0].metrics[0].value = '100%';
  rows[0].metrics[1].value = '999.9 / 1024 GB';
  rows[0].value = 'Disconnected: waiting for a fresh reading';
  rows[0].label = 'a-machine-with-a-much-longer-name';
  await page.evaluate(rows=>window.__SET_SOURCE_ROWS__('hosts',rows),rows);
  await expect(table.locator(':scope > tbody')).toContainText('999.9 / 1024 GB');
  expect(await positions()).toEqual(before);
  await table.locator('[data-action="detail-toggle"]').first().click();
  expect(await positions()).toEqual(before);
});

test("attention chips toggle their inspector and keep expanded state through refresh", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL);
  const chips = action(page, "attention");
  const first = chips.first(), second = chips.nth(1);
  await expect(first).toHaveAttribute("aria-expanded", "false");
  await first.click();
  await expect(first).toHaveAttribute("aria-expanded", "true");
  await expect(page.locator("#dashboardInspector")).toBeVisible();
  await page.evaluate(() => refreshPanels());
  await expect(first).toHaveAttribute("aria-expanded", "true");
  await first.click();
  await expect(page.locator("#dashboardInspector")).toBeHidden();
  await expect(first).toHaveAttribute("aria-expanded", "false");
  await first.focus();
  await page.keyboard.press("Enter");
  await second.click();
  await expect(first).toHaveAttribute("aria-expanded", "false");
  await expect(second).toHaveAttribute("aria-expanded", "true");
  await action(page, "close").click();
  await expect(second).toHaveAttribute("aria-expanded", "false");
});

test("Detail uses compact aligned table rows and preserves focus through live readings", async ({ page, baseURL }) => {
  const model = await openDashboard(page, baseURL);
  await tile(page, "ghWorkflows").locator('.db-tile-footer [data-action="details"]').click();
  const rows = page.locator('.db-detail-resource');
  await expect(rows).toHaveCount(6);
  await expect(page.locator('#dashboardOverview .db-detail-table')).toBeVisible();
  await expect(rows.first().locator('[data-action="detail-toggle"]')).toHaveAttribute('aria-expanded', 'false');
  for (const width of [375, 1440]) {
    await page.setViewportSize({width, height:950});
    const boxes = await rows.evaluateAll(els => els.map(el => { const r = el.getBoundingClientRect(); return {x:r.x,y:r.y,bottom:r.bottom,height:r.height}; }));
    expect(new Set(boxes.map(r => r.x)).size).toBe(1);
    for (let i=1; i<boxes.length; i++) expect(boxes[i].y).toBeGreaterThanOrEqual(boxes[i-1].bottom);
    expect(Math.max(...boxes.map(r=>r.height))).toBeLessThanOrEqual(36);
    const columns = await rows.evaluateAll(els=>els.map(el=>[...el.cells].map(c=>c.getBoundingClientRect().x)));
    for (const positions of columns) expect(positions).toEqual(columns[0]);
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
  }
  await rows.first().locator('[data-action="detail-toggle"]').click();
  await expect(rows.first().locator('[data-action="detail-toggle"]')).toHaveAttribute('aria-expanded', 'true');
  await expect(rows.first().locator('[data-action="detail-toggle"]')).toBeFocused();
  await expect(page.locator('.db-detail-extra [data-action="openRepo"]').first()).toBeVisible();
  await page.setViewportSize({width:375, height:950});
  await page.locator('#dashboardInspector > .db-inspector-body > .db-table-scroll').evaluate(el => { el.scrollLeft = 200; });
  const scrollLeft = await page.locator('#dashboardInspector > .db-inspector-body > .db-table-scroll').evaluate(el => el.scrollLeft);
  expect(scrollLeft).toBeGreaterThan(0);
  const changed = structuredClone(model.sources.find(s => s.id === 'ghWorkflows').rows);
  changed[0].value = 'Running';
  await page.evaluate(rows => window.__SET_SOURCE_ROWS__('ghWorkflows', rows), changed);
  await expect(rows.first()).toContainText('Running');
  await expect(rows.first().locator('[data-action="detail-toggle"]')).toHaveAttribute('aria-expanded', 'true');
  await expect(rows.first().locator('[data-action="detail-toggle"]')).toBeFocused();
  expect(await page.locator('#dashboardInspector > .db-inspector-body > .db-table-scroll').evaluate(el => el.scrollLeft)).toBe(scrollLeft);
  await page.locator('#dashboardInspector > .db-inspector-body > .db-table-scroll').focus();
  changed[0].value = 'Healthy';
  await page.evaluate(rows => window.__SET_SOURCE_ROWS__('ghWorkflows', rows), changed);
  await expect(rows.first()).toContainText('Healthy');
  await expect(page.locator('#dashboardInspector > .db-inspector-body > .db-table-scroll')).toBeFocused();
  expect(await page.locator('#dashboardInspector > .db-inspector-body > .db-table-scroll').evaluate(el => el.scrollLeft)).toBe(scrollLeft);
  const expandedHeight = (await page.locator('.db-inspector-body').boundingBox()).height;
  await rows.first().locator('[data-action="detail-toggle"]').click();
  await expect(rows.first().locator('[data-action="detail-toggle"]')).toHaveAttribute('aria-expanded', 'false');
  await expect.poll(async () => (await page.locator('.db-inspector-body').boundingBox()).height).toBeLessThan(expandedHeight);
});

test("Detail Table/List choice persists per source and failed saves keep the selected view", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL);
  await tile(page, 'ghWorkflows').locator('.db-tile-footer [data-action="details"]').click();
  const table = page.locator('#dashboardOverview').getByRole('button', {name:'Table', exact:true});
  const list = page.locator('#dashboardOverview').getByRole('button', {name:'List', exact:true});
  await expect(table).toHaveAttribute('aria-pressed', 'true');
  await page.evaluate(() => { window.__FAIL_SAVE__ = true; });
  await list.click();
  await expect(table).toHaveAttribute('aria-pressed', 'true');
  await expect(page.locator('#dashboardOverview .db-detail-table')).toBeVisible();
  await page.evaluate(() => { window.__FAIL_SAVE__ = false; });
  await list.click();
  await expect(list).toHaveAttribute('aria-pressed', 'true');
  await expect(page.locator('.db-detail-grid details')).toHaveCount(6);
  expect((await savedLayout(page)).detailViews.ghWorkflows).toBe('list');
  await page.reload();
  await tile(page, 'ghWorkflows').locator('.db-tile-footer [data-action="details"]').click();
  await expect(list).toHaveAttribute('aria-pressed', 'true');
  await tile(page, 'hosts').locator('.db-tile-footer [data-action="details"]').click();
  await expect(table).toHaveAttribute('aria-pressed', 'true');
  await tile(page, 'ghWorkflows').locator('.db-tile-footer [data-action="details"]').click();
  await table.click();
  await expect(table).toHaveAttribute('aria-pressed', 'true');
  expect((await savedLayout(page)).detailViews.ghWorkflows).toBe('table');
});

test("delayed Detail view saves preserve keyboard focus without stealing it after navigation", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL);
  await tile(page, 'ghWorkflows').locator('.db-tile-footer [data-action="details"]').click();
  const table = page.locator('#dashboardOverview').getByRole('button', {name:'Table', exact:true});
  const list = page.locator('#dashboardOverview').getByRole('button', {name:'List', exact:true});
  const hold = async fail => {
    await page.evaluate(fail => { window.__HOLD_SAVE__ = true; window.__FAIL_SAVE__ = fail; }, fail);
  };
  const release = async () => {
    await page.evaluate(async () => {
      await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      window.__RELEASE_SAVE__();
    });
  };
  await hold(false);
  await list.focus();
  await list.press('Enter');
  await expect(list).toBeDisabled();
  await release();
  await expect(list).toHaveAttribute('aria-pressed', 'true');
  await expect(list).toBeFocused();
  await hold(true);
  await table.press('Enter');
  await expect(table).toBeDisabled();
  await release();
  await expect(table).toBeEnabled();
  await expect(list).toHaveAttribute('aria-pressed', 'true');
  await expect(table).toBeFocused();
  await hold(false);
  await table.press('Enter');
  await expect(table).toBeDisabled();
  await action(page, 'attention').first().click();
  await expect(action(page, 'close')).toBeFocused();
  await release();
  await expect(action(page, 'close')).toBeFocused();
  await expect(action(page, 'attention').first()).toHaveAttribute('aria-expanded', 'true');
});

test("GitHub tile row limits and runner grouping are saved independently", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL);
  await action(page, "edit").click();
  await tile(page, "ghWorkflows").locator('[data-action="configure"]').click();
  await page.getByLabel("Rows to show", { exact: true }).selectOption("all");
  await action(page, "apply").click();
  expect((await savedLayout(page)).tiles.find(t => t.source === "ghWorkflows").rowLimit).toBe("all");
  await expect(tile(page,"ghWorkflows").locator('.db-item')).toHaveCount(6);
  await tile(page, "ghRunners").locator('[data-action="configure"]').click();
  await page.getByLabel("Runner view", { exact: true }).selectOption("grouped");
  await page.getByLabel("Rows to show", { exact: true }).selectOption("all");
  await action(page, "apply").click();
  expect((await savedLayout(page)).tiles.find(t => t.source === "ghRunners").runnerView).toBe("grouped");
  await page.reload();
  await action(page, "edit").click();
  await tile(page, "ghRunners").locator('[data-action="configure"]').click();
  await expect(page.getByLabel("Runner view", { exact: true })).toHaveValue("grouped");
  await expect(page.getByLabel("Rows to show", { exact: true })).toHaveValue("all");
});

test("repo column sorts save direction, survive refresh, and support the keyboard", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL);
  const repos = tile(page, "ghWorkflows");
  for (const column of ["issues", "ready", "prs", "status", "name"]) {
    const header = repos.locator(`[data-action="sort"][data-column="${column}"]`);
    await header.click();
    const saved = (await savedLayout(page)).tiles.find(t => t.source === "ghWorkflows");
    expect(saved.sortBy).toBe(column);
    expect(saved.sortDescending).toBe(!["name", "status"].includes(column));
    await expect(header).toHaveAttribute("aria-pressed", "true");
    await header.press("Enter");
    expect((await savedLayout(page)).tiles.find(t => t.source === "ghWorkflows").sortDescending).toBe(!saved.sortDescending);
    await expect(header).toBeFocused();
  }
  await page.reload();
  await expect(repos.locator('[data-column="name"]')).toHaveAccessibleName("Repo · Descending");
});

test("repo selections persist and filter the tile and its Details", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL);
  await action(page,"edit").click();
  await tile(page,"ghWorkflows").locator('[data-action="configure"]').click();
  await page.getByLabel("acme/pipe-fitting", {exact:true}).check();
  await page.getByLabel("acme/widget", {exact:true}).check();
  await page.getByLabel("Rows to show", {exact:true}).selectOption("all");
  await action(page,"apply").click();
  expect((await savedLayout(page)).tiles.find(t => t.source === "ghWorkflows").selectedRepos).toEqual(["acme/pipe-fitting","acme/widget"]);
  await expect(tile(page,"ghWorkflows").locator('.db-item-name')).toHaveText(["pipe-fitting","widget"]);
  await tile(page,"ghWorkflows").locator('[data-action="details"]').click();
  await expect(page.locator('.db-detail-resource')).toHaveCount(2);
  await page.reload();
  await action(page,"edit").click();
  await tile(page,"ghWorkflows").locator('[data-action="configure"]').click();
  await expect(page.getByLabel("acme/widget", {exact:true})).toBeChecked();
});

test("Rust expanded view renders sorted repos, grouped runners and live meters at narrow and wide sizes", async ({ page, baseURL }) => {
  const expanded = await (await fetch(`${baseURL}/sample-dashboard-expanded.json`)).json();
  await page.route("**/sample-dashboard.json", route => route.fulfill({json:expanded}));
  await page.goto("/index.html");
  await expect(tile(page,"ghWorkflows").locator('.db-item-name')).toHaveText(["gadget","pipe-fitting","widget","flywheel","cogwheel","toolkit"]);
  await expect(tile(page,"ghRunners").locator('.db-item-name')).toHaveText(["LINUX · ARM64","MACOS · ARM64"]);
  await expect(tile(page,"ghRunners").locator('.db-row-description')).toHaveText(["0 busy · 1 idle · 1 offline · 1 missing","1 busy · 1 idle · 0 offline · 1 recycling"]);
  await expect(tile(page,"hosts").getByRole("meter")).toHaveCount(4);
  for (const width of [375, 1024, 1440]) {
    await page.setViewportSize({width,height:900});
    const rects = await tile(page,"ghWorkflows").locator('.db-repo-head button').evaluateAll(els=>els.map(e=>({x:e.getBoundingClientRect().x,right:e.getBoundingClientRect().right})));
    for(let i=1;i<rects.length;i++) expect(rects[i].x).toBeGreaterThanOrEqual(rects[i-1].right);
    expect(await page.evaluate(()=>document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
  }
  await tile(page,"ghRunners").getByRole("button",{name:/LINUX · ARM64/}).click();
  await expect(page.locator('#dashboardOverview')).toBeVisible();
  await expect(page.locator('#cockpitView')).toBeHidden();
  await expect(page.locator('#dashboardInspector .db-table-name')).toHaveText(['ubu-01', 'ubu-1', 'ubu-spare']);
});

test('runner groups open the shared inline Detail panel like machines', async ({page, baseURL}) => {
  const model = await openDashboard(page, baseURL, true);
  const inspector = page.locator('#dashboardInspector');
  await tile(page, 'hosts').locator('[data-action="row"]').first().click();
  const machineControls = await inspector.locator('button[data-action]').evaluateAll(buttons =>
    buttons.map(b => b.dataset.action).filter(action => action !== 'detail-toggle'));
  await tile(page, 'ghRunners').getByRole('button', {name:/LINUX · ARM64/}).click();
  await expect(page.locator('#dashboardOverview')).toBeVisible();
  await expect(page.locator('#cockpitView')).toBeHidden();
  await expect(tile(page, 'hosts')).toBeVisible();
  await expect(inspector.getByRole('heading')).toHaveText('LINUX · ARM64');
  await expect(inspector.locator('.db-table-name')).toHaveText(['ubu-01', 'ubu-1', 'ubu-spare']);
  const controls = await inspector.locator('button[data-action]').evaluateAll(buttons =>
    buttons.map(b => b.dataset.action).filter(action => !['detail-toggle', 'manage-resource'].includes(action)));
  expect(controls).toEqual(machineControls.filter(action => action !== 'manage-resource'));
  await inspector.getByRole('button', {name:'List', exact:true}).click();
  await expect(inspector.locator('.db-detail-resource')).toHaveCount(3);
  await tile(page, 'ghRunners').getByRole('button', {name:/MACOS · ARM64/}).click();
  await expect(inspector.getByRole('heading')).toHaveText('MACOS · ARM64');
  await expect(inspector.getByRole('button', {name:'List', exact:true})).toHaveAttribute('aria-pressed', 'true');
  const macs = model.sources.find(s => s.id === 'ghRunners').rows.filter(r => r.os === 'MACOS');
  await expect(inspector.locator('.db-detail-title strong')).toHaveText(macs.map(r => r.label));
  await inspector.getByRole('button', {name:'Close', exact:true}).click();
  await expect(inspector).toBeHidden();
  await tile(page, 'ghRunners').locator('.db-tile-footer [data-action="details"]').click();
  await expect(inspector.getByRole('heading')).toHaveText('Runners');
  await expect(inspector.locator('.db-detail-resource')).toHaveCount(6);
});

test('inline runner groups follow live membership and keep an empty group selected', async ({page, baseURL}) => {
  const model = await openDashboard(page, baseURL, true);
  const source = structuredClone(model.sources.find(s => s.id === 'ghRunners'));
  const linux = source.groups.find(g => g.label === 'LINUX · ARM64');
  const mac = source.groups.find(g => g.label === 'MACOS · ARM64');
  const inspector = page.locator('#dashboardInspector');
  const names = inspector.locator('.db-table-name');
  const subtitle = inspector.locator('.db-inspector-head .db-sub');
  await tile(page, 'ghRunners').getByRole('button', {name:/LINUX · ARM64/}).click();
  await expect(subtitle).toHaveText(`${linux.value} · ${linux.detail}`);
  const added = {...source.rows.find(r => r.groupId === linux.id), id:'acme/new-runner', label:'new-runner'};
  source.rows.push(added);
  linux.value = '2 online / 4';
  linux.detail = '0 busy · 2 idle · 1 offline · 1 missing';
  await page.evaluate(source => window.__SET_RUNNER_SOURCE__(source), source);
  await expect(names).toHaveText(['ubu-01', 'ubu-1', 'ubu-spare', 'new-runner']);
  await expect(subtitle).toHaveText(`${linux.value} · ${linux.detail}`);
  // A runner moving type must leave the open group, even though its ID stays.
  added.groupId = mac.id;
  const originalGroup = model.sources.find(s => s.id === 'ghRunners').groups.find(g => g.id === linux.id);
  Object.assign(linux, originalGroup);
  await page.evaluate(source => window.__SET_RUNNER_SOURCE__(source), source);
  await expect(names).toHaveText(['ubu-01', 'ubu-1', 'ubu-spare']);
  await expect(subtitle).toHaveText(`${linux.value} · ${linux.detail}`);
  source.groups = source.groups.filter(g => g.id !== linux.id);
  source.rows = source.rows.filter(r => r.groupId !== linux.id);
  await page.evaluate(source => window.__SET_RUNNER_SOURCE__(source), source);
  await expect(names).toHaveCount(0);
  await expect(inspector.getByRole('heading')).toHaveText('LINUX · ARM64');
  await expect(inspector).toContainText('No runners currently match this type.');
  await expect(subtitle).toBeEmpty();
  await expect(page.locator('#dashboardOverview')).toBeVisible();
  await inspector.getByRole('button', {name:'List', exact:true}).click();
  await expect(inspector.locator('.db-detail-resource')).toHaveCount(0);
  await expect(inspector).toContainText('No runners currently match this type.');
  // A connection problem must not look like a successfully read empty group.
  source.rows = [];
  source.groups = [];
  source.message = 'Configure a GitHub token in Settings to see runners.';
  await page.evaluate(source => window.__SET_RUNNER_SOURCE__(source), source);
  await expect(inspector).toContainText(source.message);
  await expect(inspector).not.toContainText('No runners currently match this type.');
});

test("runner group Open full panel preserves the filter, tracks membership, and clears it", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL, true);
  const payload = await (await fetch(`${baseURL}/sample-runners.json`)).json();
  await tile(page,"ghRunners").getByRole("button",{name:/LINUX · ARM64/}).click();
  await action(page, 'full').click();
  await expect(page.locator('#dashboardOverview')).toBeHidden();
  await expect(page.locator('#runnersPanel')).toBeVisible();
  await expect(page.locator('#reposPanel')).toBeHidden();
  const runners = page.locator('#runnersBody .gh-runner-name');
  await expect(runners).toHaveText(['ubu-01', 'ubu-1', 'ubu-spare']);
  const all = page.getByRole('button', {name:'All runners', exact:true});
  await all.focus();
  await page.evaluate(() => refreshPanels());
  await expect(all).toBeFocused();
  const added = {...payload.rows.find(r=>r.os==='LINUX'), name:'new-runner'};
  const changed = structuredClone(payload);
  changed.rows.push(added);
  await page.evaluate(async payload => { window.__SET_RUNNER_PAYLOAD__(payload); await refreshPanels(); }, changed);
  await expect(runners).toHaveCount(4);
  added.groupId = 'group:MACOS:ARM64';
  await page.evaluate(async payload => { window.__SET_RUNNER_PAYLOAD__(payload); await refreshPanels(); }, changed);
  await expect(runners).toHaveCount(3);
  for (const width of [375, 1440]) {
    await page.setViewportSize({width, height:950});
    expect(new Set(await runners.evaluateAll(els=>els.map(el=>el.getBoundingClientRect().x))).size).toBe(1);
    expect(await page.evaluate(()=>document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
  }
  changed.rows = [];
  changed.groups = [];
  await page.evaluate(async payload => { window.__SET_RUNNER_PAYLOAD__(payload); await refreshPanels(); }, changed);
  await expect(runners).toHaveCount(0);
  await expect(page.locator('#runnersBody')).toContainText('No runners currently match this type.');
  await page.evaluate(async payload => { window.__SET_RUNNER_PAYLOAD__(payload); await refreshPanels(); }, payload);
  await page.getByRole('button', {name:'All runners', exact:true}).click();
  await expect(runners).toHaveCount(6);
  await expect(page.locator('#runnersTitle')).toBeFocused();
  await page.locator('#dashboardBack').click();
  await expect(page.locator('#dashboardOverview')).toBeVisible();
});

test("repo checklist reconciles new readings without discarding the Configure draft", async ({ page, baseURL }) => {
  const model = await openDashboard(page, baseURL);
  await action(page,"edit").click();
  await tile(page,"ghWorkflows").locator('[data-action="configure"]').click();
  await page.getByLabel("acme/widget",{exact:true}).check();
  await page.getByLabel("Tile name",{exact:true}).fill("Chosen repos");
  await page.evaluate(() => { window.__SETTINGS_ROUTE__ = {editor:null,kinds:["account"]}; });
  await action(page,"manage").click();
  await expect(page.locator("#settings")).toBeVisible();
  const rows = model.sources.find(s=>s.id==="ghWorkflows").rows;
  await page.evaluate(rows=>window.__SET_SOURCE_ROWS__("ghWorkflows",rows),[...rows,{...rows[0],id:"acme/new-repo",label:"new-repo"}]);
  await page.locator("#settingsClose").click();
  await expect(page.getByLabel("acme/new-repo",{exact:true})).toBeVisible();
  await expect(page.getByLabel("acme/widget",{exact:true})).toBeChecked();
  await expect(page.getByLabel("Tile name",{exact:true})).toHaveValue("Chosen repos");
  await page.getByLabel("acme/new-repo",{exact:true}).check();
  await action(page,"apply").click();
  expect((await savedLayout(page)).tiles.find(t=>t.source==="ghWorkflows").selectedRepos).toEqual(["acme/widget","acme/new-repo"]);
});

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
  const order = ["Machines", "GitHub Repos", "Build machines", "Runners", "Service Health", "Scheduled Jobs"];
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

test("the default overview uses page scrolling on a laptop and keeps all source alerts", async ({
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
    await page.locator(".db-tile-content").evaluateAll(els => els.every(el => el.scrollHeight <= el.clientHeight + 1)),
  ).toBe(true);
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
    "GitHub Repos",
  );
  await action(page, "undo").click();
  await expect(page.locator(".db-tile-title")).toHaveText(
    original.tiles.map((t) => t.title),
  );
  await tile(page, "services")
    .locator(".db-drag")
    .dragTo(tile(page, "hosts").locator(".db-drag"));
  await expect(page.locator(".db-tile-title").first()).toHaveText(
    "Service Health",
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

test("repo rows carry issues, ready and PRs on the row, in summary and in detail", async ({
  page,
  baseURL,
}) => {
  await openDashboard(page, baseURL);
  const repos = tile(page, "ghWorkflows");
  const item = (id) => repos.locator(`[data-id="${id}"]`);
  // Every row carries the three counts verbatim from the Rust cells: a real
  // zero is "0", an unreadable count is the em dash.
  await expect(item("acme/pipe-fitting").locator(".db-row-counts")).toHaveText("7 issues · 3 ready · 2 PRs");
  // Exactly one is singular; `ready` has no plural.
  await expect(item("acme/flywheel").locator(".db-row-counts")).toHaveText("1 issue · 0 ready · 1 PR");
  await expect(item("acme/cogwheel").locator(".db-row-counts")).toHaveText("— issues · — ready · — PRs");
  // The numbers are columns: every row's three numbers end at the same x as
  // every other row's, whether the row reads `1 PR`, `18 issues` or `—`.
  const rightEdges = await repos
    .locator(".db-item .db-row-counts")
    .evaluateAll((strips) =>
      strips.map((strip) =>
        [...strip.querySelectorAll("strong")].map((n) => Math.round(n.getBoundingClientRect().right)),
      ),
    );
  expect(rightEdges.length).toBe(5);
  for (const edges of rightEdges) expect(edges).toEqual(rightEdges[0]);
  // …on the row itself: name, counts and status share one line, and the
  // status is not displaced by the strip.
  const [name, counts, status] = await item("acme/pipe-fitting")
    .locator(".db-item-name, .db-row-counts, .db-value")
    .evaluateAll((els) => els.map((el) => el.getBoundingClientRect()));
  const sameLine = (a, b) => Math.abs(a.y - b.y) <= 2 && Math.abs(a.y + a.height - (b.y + b.height)) <= 2;
  expect(sameLine(counts, name)).toBe(true);
  expect(sameLine(status, name)).toBe(true);
  expect(counts.x).toBeGreaterThan(name.x + name.width);
  expect(status.x).toBeGreaterThanOrEqual(counts.x + counts.width);
  await expect(item("acme/pipe-fitting").locator(".db-value")).toHaveText("Running");
  // Detailed adds the remaining columns beneath, without repeating the three.
  await action(page, "edit").click();
  await repos.locator('[data-action="configure"]').click();
  await page.locator("#dashboard-presentation").selectOption("detailed");
  await action(page, "apply").click();
  const row = item("acme/pipe-fitting").locator("xpath=..");
  await expect(row.locator(".db-extra-metrics .db-muted")).toHaveText(["REMOTE", "LOCAL", "WT", "JOBS", "LONGEST"]);
  await expect(row.locator(".db-row-counts")).toHaveCount(1);
  expect(page.dashboardErrors).toEqual([]);
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
