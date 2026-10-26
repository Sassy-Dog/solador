import { test, expect } from "@playwright/test";

// Real UI and Rust-dumped readings under Tauri's CSP. The IPC double projects
// edited tiles; Rust tests separately cover policy and actual disk persistence.
async function openDashboard(page, baseURL, expanded = false, prepare = null) {
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
  prepare?.(fixtures.dashboard_view);
  await page.addInitScript(
    ({ fixtures }) => {
      const original = fixtures.dashboard_view;
      let layout =
        JSON.parse(localStorage.getItem("test-dashboard") || "null") ||
        original.layout;
      window.__CALLS__ = [];
      window.__SET_LAYOUT__ = (next) => { layout = structuredClone(next); };
      window.__SET_DASHBOARD__ = (next) => { Object.assign(original, structuredClone(next)); };
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
            const scopeLabel =
              source.scopes.find((s) => s.value === t.scope)?.label ||
              "Resource no longer available";
            // dashboard::scope_note's projection, for a tile the test re-scoped;
            // Rust's own test pins the wording.
            const picked = t.selectedRepos?.length || 0;
            const fullNote = t.scope === "all" && !picked ? null :
              `Scoped to ${t.scope !== "all" ? scopeLabel : picked === 1 ? "1 selected repo" : `${picked} selected repos`}. Totals still cover all of ${source.title}.`;
            return {
              ...base,
              ...t,
              rows: rows.slice(0, limit),
              warnings: source.warnings,
              scopeLabel,
              moreCount: Math.max(0, rows.length - limit),
              footer: `${Math.min(rows.length, limit)} shown`,
              empty: source.message,
              views: base.views ?? original.tiles[0].views,
              fullNote,
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
            if (command === "machine_acknowledge_warning") {
              if (window.__HOLD_ACK__) await new Promise(resolve => { window.__RELEASE_ACK__ = resolve; });
              // Result<Value, String> rejects with a string across the Tauri boundary.
              if (window.__FAIL_ACK__) throw "Could not save the warning acknowledgement.";
              if (!window.__ACK_RESPONSE__) throw new Error("Missing acknowledgement response fixture");
              Object.assign(original, structuredClone(window.__ACK_RESPONSE__));
              return project();
            }
            if (command === "machine_set_volume_hidden") {
              if (window.__FAIL_VOLUME__) throw "Could not save the hidden volumes.";
              // What Rust does to the row: the mount leaves `volumes` for
              // `hiddenVolumes`, or comes back. Readings are not re-derived here.
              const row = original.sources.find(s => s.id === "hosts").rows.find(r => r.id === args.hostId);
              const all = [...(row.volumes || []), ...(window.__HIDDEN_VOLUMES__ ||= [])];
              const volume = all.find(v => v.mount === args.mount);
              if (args.hidden) {
                row.volumes = row.volumes.filter(v => v.mount !== args.mount);
                window.__HIDDEN_VOLUMES__.push(volume);
                row.hiddenVolumes = [...(row.hiddenVolumes || []), args.mount];
              } else {
                row.volumes = [...row.volumes, volume].sort((a, b) => a.mount.localeCompare(b.mount));
                row.hiddenVolumes = row.hiddenVolumes.filter(m => m !== args.mount);
              }
              return project();
            }
            if (command === "machine_save_alerts") {
              if (window.__FAIL_ALERTS__) throw "Could not save the machine alert limits.";
              // The limits edit only; validation is Rust's (store::machine_alerts).
              const g = original.sources.find(s => s.id === "hosts").machineAlerts;
              const initial = (window.__INITIAL_LIMITS__ ||= structuredClone(g.defaults.fields));
              const paint = (editor, values) => editor.fields.forEach(f => { f.value = values[f.id]; });
              const shared = Object.fromEntries(g.defaults.fields.map(f => [f.id, f.value]));
              if (!args.hostId) {
                const next = args.thresholds || Object.fromEntries(initial.map(f => [f.id, f.value]));
                paint(g.defaults, next);
                g.defaults.defaults = next;
                for (const editor of Object.values(g.byHost)) {
                  editor.defaults = next;
                  if (editor.inherited) paint(editor, next);
                }
              } else {
                const editor = g.byHost[args.hostId];
                editor.inherited = args.thresholds === null;
                paint(editor, args.thresholds || shared);
              }
              return project();
            }
            if (command.startsWith("runner_group_label_")) {
              if (window.__FAIL_GROUP_LABEL__) throw "Could not save the group labels.";
              // The list edit only; carriers counts and grouping are Rust's
              // and are not re-derived here.
              const editor = original.sources.find(s => s.id === "ghRunners").groupLabels;
              const labels = editor.rows.map(r => r.label);
              if (command === "runner_group_label_add") labels.push(args.label.trim());
              if (command === "runner_group_label_remove") labels.splice(labels.indexOf(args.label), 1);
              if (command === "runner_group_label_move") {
                const at = labels.indexOf(args.label), to = at + (args.up ? -1 : 1);
                [labels[at], labels[to]] = [labels[to], labels[at]];
              }
              editor.rows = labels.map((label, i) => ({
                label, first: i === 0, last: i === labels.length - 1,
                carriers: editor.rows.find(r => r.label === label)?.carriers || "carried by 1 runner",
              }));
              editor.seen = editor.seen.filter(seen => !labels.some(l => l.toLowerCase() === seen.toLowerCase()));
              return project();
            }
            if (command === "panel_interval_save" || command === "panel_interval_clear") {
              if (window.__FAIL_CADENCE__) throw "Could not save the poll cadence.";
              // Every source carrying the id, as in Rust: one cadence, one value
              // (GitHub's is on both the Repos and the Runners tile).
              const all = original.sources.flatMap(x => x.cadences || []).filter(x => x.id === args.panel);
              const [c] = all;
              if (command === "panel_interval_save") {
                // The floor is Rust's (`PanelInterval::check_secs`); this double
                // only refuses the way it does, with a sentence naming the floor.
                if (args.secs < c.min) throw `${c.label}: ${args.secs}s is below the ${c.min}s floor. ${c.help}`;
                for (const x of all) {
                  x.value = args.secs;
                  x.configured = true;
                  x.status = `Set to ${args.secs}s.`;
                }
              } else {
                for (const x of all) {
                  x.configured = false;
                  x.status = "Using the default.";
                }
              }
              return project();
            }
            if (command.startsWith("container_rule_")) {
              if (window.__FAIL_CONTAINER_RULE__) throw "Could not save the container rules.";
              // The list edit only; host options and which fields a row has
              // are Rust's and are not re-derived here.
              const editor = original.sources.find(s => s.id === "containers").ruleEditor;
              if (command === "container_rule_add")
                editor.rows.push({ index: editor.rows.length, action: "collapse", pattern: "", label: "", expected: "",
                  host: "", collapseOnly: true, hostOptions: editor.rows[0].hostOptions });
              if (command === "container_rule_remove")
                editor.rows = editor.rows.filter(r => r.index !== args.index).map((r, index) => ({ ...r, index }));
              if (command === "container_rule_set") {
                const row = editor.rows[args.index];
                row[args.field] = args.value;
                if (args.field === "action") row.collapseOnly = args.value === "collapse";
              }
              return project();
            }
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
            if (command === "window_fit_height" && window.__FAIL_FIT__) throw "Couldn't fit the window: exit full screen first";
            if (command === "plugin:opener|open_url") return null;
            // A panel command asked for a tile refuses as tile_scope::find_tile
            // does. The payload itself is the unscoped fixture: Rust's tests
            // cover what a scope keeps.
            const panelSource = { cockpit:"hosts", repos:"ghWorkflows", runners:"ghRunners", containers:"containers",
              services:"services", crons:"sentryCrons", usage:"claudeUsage", azure_cost:"azureCost", openclaw:"openclawAgents" };
            if (args?.tile !== undefined && Object.hasOwn(panelSource, command)) {
              const asked = layout.tiles.find(t => t.id === args.tile);
              if (!asked) throw "That tile is no longer on the overview.";
              if (asked.hidden) throw "That tile is hidden, so it has no views.";
              if (asked.source !== panelSource[command]) throw "That tile shows a different source.";
            }
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

// These are IPC response fixtures, not a test-side alert evaluator. Rust tests
// exercise severity, scoping and disk persistence; these exercise the real UI.
function warningFrame(view, acknowledged, critical = false) {
  const result = structuredClone(view), source = result.sources.find(s => s.id === 'hosts');
  const row = structuredClone(source.rows[0]);
  row.label = 'mac-n87k';
  row.attention = critical || !acknowledged;
  row.metrics[1] = {
    ...row.metrics[1], value: '25.2 / 32 GB', fraction: .7875,
    color: critical ? '#ef6b5b' : '#e9b44c',
    acknowledgement: {
      metric:'ram', acknowledged, available:acknowledged || !critical,
      badge: acknowledged && !critical ? 'Acknowledged' : '',
      actionLabel: acknowledged ? 'Clear RAM acknowledgement' : 'Acknowledge RAM warning',
      help: critical ? 'RAM is critical and needs attention. Only warnings are acknowledged.' :
        'RAM warnings acknowledged until cleared; critical alerts remain enabled.',
    },
  };
  row.color = row.valueColor = row.metrics[1].color;
  source.rows = [row];
  source.warnings = [];
  source.attentionCount = row.attention ? 1 : 0;
  result.attention = result.attention.filter(s => s.source !== 'hosts');
  if (row.attention) result.attention.push({source:'hosts', label:'Machines · 1', color:row.metrics[1].color});
  return result;
}

test('machine warnings can be acknowledged and cleared from details without changing the reading', async ({page, baseURL}) => {
  const initial = await openDashboard(page, baseURL, false, view => Object.assign(view, warningFrame(view, false)));
  const acknowledged = warningFrame(initial, true);
  await page.evaluate(next => { window.__ACK_RESPONSE__ = next; }, acknowledged);
  await tile(page, 'hosts').getByRole('button', {name:/mac-n87k/}).click();
  const inspector = page.locator('.db-inspector');
  const acknowledge = inspector.getByRole('button', {name:'Acknowledge RAM warning', exact:true});
  await acknowledge.click();
  await expect(inspector.getByRole('button', {name:'Clear RAM acknowledgement'})).toBeVisible();
  await expect(tile(page, 'hosts').getByRole('img', {name:'RAM: Acknowledged', exact:true})).toBeVisible();
  await expect(page.locator('.db-end')).toHaveCount(0);
  expect(await page.locator('.db-live-note').evaluate(el => el.getBoundingClientRect().height)).toBeLessThanOrEqual(1);
  // A ✓ beside the value, never a standing line of text under the meter.
  await expect(tile(page, 'hosts').getByRole('img', {name:'RAM: Acknowledged', exact:true})).toHaveText('✓');
  await expect(tile(page, 'hosts').getByText('Acknowledged', {exact:true})).toHaveCount(0);
  await expect(tile(page, 'hosts').getByRole('meter', {name:'mac-n87k RAM'})).toHaveAttribute('aria-valuenow', '78.75');
  await expect(action(page, 'attention').filter({hasText:'Machines'})).toHaveCount(0);
  expect(await page.evaluate(() => window.__CALLS__.filter(c => c.command === 'machine_acknowledge_warning'))).toEqual([
    {command:'machine_acknowledge_warning', args:{hostId:initial.sources.find(s=>s.id==='hosts').rows[0].id, metric:'ram', acknowledged:true, width:expect.any(Number)}},
  ]);
  await page.evaluate(next => { window.__ACK_RESPONSE__ = next; }, initial);
  await inspector.getByRole('button', {name:'Clear RAM acknowledgement'}).click();
  await expect(inspector.getByRole('button', {name:'Acknowledge RAM warning'})).toBeVisible();
  await expect(action(page, 'attention').filter({hasText:'Machines'})).toBeVisible();
  expect(await page.evaluate(() => window.__CALLS__.filter(c => c.command === 'machine_acknowledge_warning').at(-1).args.acknowledged)).toBe(false);
});

test('failed acknowledgement keeps the warning actionable and allows retry', async ({page, baseURL}) => {
  const initial = await openDashboard(page, baseURL, false, view => Object.assign(view, warningFrame(view, false)));
  await page.evaluate(() => { window.__FAIL_ACK__ = true; });
  await tile(page, 'hosts').getByRole('button', {name:/mac-n87k/}).click();
  const acknowledge = page.getByRole('button', {name:'Acknowledge RAM warning',exact:true});
  await acknowledge.click();
  const failure = page.locator('.db-warning-control .db-action-error');
  await expect(failure).toHaveText('Could not save the warning acknowledgement.');
  // The persistent live region announces once; repainting a reading must not
  // insert fresh live alerts or mutate that announcement on every poll.
  await expect(failure).not.toHaveAttribute('role', 'alert');
  await page.evaluate(() => {
    window.__ANNOUNCEMENTS__ = 0;
    new MutationObserver(() => window.__ANNOUNCEMENTS__++)
      .observe(document.querySelector('.db-live-note'), {subtree:true,childList:true,characterData:true});
  });
  await page.evaluate(() => refreshPanels());
  await expect(failure).toBeVisible();
  expect(await page.evaluate(() => window.__ANNOUNCEMENTS__)).toBe(0);
  await expect(acknowledge).toBeEnabled();
  await expect(action(page, 'attention').filter({hasText:'Machines'})).toBeVisible();
  await expect(tile(page, 'hosts').getByRole('img', {name:'RAM: Acknowledged', exact:true})).toHaveCount(0);
  await page.evaluate(next => { window.__FAIL_ACK__ = false; window.__ACK_RESPONSE__ = next; }, warningFrame(initial, true));
  await acknowledge.click();
  await expect(page.getByRole('button', {name:'Clear RAM acknowledgement'})).toBeEnabled();
  await expect(tile(page, 'hosts').getByRole('img', {name:'RAM: Acknowledged', exact:true})).toBeVisible();
});

test('an acknowledgement failure stays with its machine when changing inspectors', async ({page, baseURL}) => {
  await openDashboard(page, baseURL, false, view => Object.assign(view, warningFrame(view, false)));
  await page.evaluate(() => { window.__FAIL_ACK__ = true; });
  await tile(page, 'hosts').getByRole('button', {name:/mac-n87k/}).click();
  await page.getByRole('button', {name:'Acknowledge RAM warning',exact:true}).click();
  await expect(page.locator('.db-action-error')).toBeVisible();
  await tile(page, 'ghWorkflows').locator('.db-tile-footer [data-action="details"]').click();
  await page.evaluate(() => refreshPanels());
  await expect(page.locator('.db-action-error')).toHaveCount(0);
});

test('an old poll cannot undo an acknowledgement and duplicate clicks cannot submit twice', async ({page, baseURL}) => {
  const initial = await openDashboard(page, baseURL, false, view => Object.assign(view, warningFrame(view, false)));
  await tile(page, 'hosts').getByRole('button', {name:/mac-n87k/}).click();
  await page.evaluate(next => {
    window.__ACK_RESPONSE__ = next;
    window.__HOLD_ACK__ = true;
    window.__HOLD_READ__ = true;
  }, warningFrame(initial, true));
  await expect.poll(() => page.evaluate(() => typeof window.__RELEASE_READ__)).toBe('function');
  const acknowledge = page.getByRole('button', {name:'Acknowledge RAM warning',exact:true});
  await acknowledge.press('Enter');
  await expect(acknowledge).toBeDisabled();
  await page.evaluate(() => window.__RELEASE_ACK__());
  const clear = page.getByRole('button', {name:'Clear RAM acknowledgement'});
  await expect(clear).toBeEnabled();
  await expect(clear).toBeFocused();
  await page.evaluate(() => window.__RELEASE_READ__());
  await expect(clear).toBeVisible();
  await expect(action(page, 'attention').filter({hasText:'Machines'})).toHaveCount(0);
  expect(await page.evaluate(() => window.__CALLS__.filter(c => c.command === 'machine_acknowledge_warning').length)).toBe(1);
});

test('critical attention stays visible with a saved acknowledgement in table and list views', async ({page, baseURL}) => {
  const initial = await openDashboard(page, baseURL, false, view => Object.assign(view, warningFrame(view, true, true)));
  await tile(page, 'hosts').getByRole('button', {name:/mac-n87k/}).click();
  const inspector = page.locator('.db-inspector');
  for (const view of ['Table', 'List']) {
    await inspector.getByRole('button', {name:view,exact:true}).click();
    await expect(inspector.getByText('RAM is critical and needs attention. Only warnings are acknowledged.', {exact:true})).toBeVisible();
    await expect(inspector.getByRole('button', {name:'Clear RAM acknowledgement'})).toBeVisible();
    await expect(inspector.getByRole('button', {name:'Acknowledge RAM warning',exact:true})).toHaveCount(0);
    await expect(action(page, 'attention').filter({hasText:'Machines'})).toBeVisible();
    await expect(tile(page, 'hosts').getByRole('img', {name:'RAM: Acknowledged', exact:true})).toHaveCount(0);
  }
  await page.evaluate(next => window.__SET_DASHBOARD__(next), warningFrame(initial, true));
  await expect(tile(page, 'hosts').getByRole('img', {name:'RAM: Acknowledged', exact:true})).toBeVisible();
  for (const width of [390, 240, 1280]) {
    await page.setViewportSize({width, height:900});
    await expect(inspector.getByRole('button', {name:'Clear RAM acknowledgement'})).toBeVisible();
    const controls = inspector.locator('.db-warning-controls');
    expect(await controls.evaluate(el => el.getBoundingClientRect().right)).toBeLessThanOrEqual(width);
    expect(await controls.evaluate(el => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(1);
    // Existing repository columns elsewhere have a minimum width below 390px.
    if (width >= 390)
      expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
  }
});

test("dashboard toolbar icons keep accessible labels and keyboard tooltips through editing", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL);
  const toolbar = page.locator(".db-chrome");
  for (const name of ["Full view", "Fit window to tiles", "Edit dashboard", "Settings"]) {
    const button = toolbar.getByRole("button", { name, exact: true });
    await expect(button).toBeVisible();
    await button.focus();
    await expect(button.getByRole("tooltip")).toBeVisible();
  }
  const edit = toolbar.locator('[data-action="edit"]');
  await edit.press("Enter");
  for (const name of ["Add tile", "Undo"]) {
    const button = toolbar.getByRole("button", { name, exact: true });
    await expect(button.locator("svg")).toHaveCount(1);
    await expect(button.getByRole("tooltip", {includeHidden:true})).toHaveText(name);
  }
  await expect(edit).toHaveAccessibleName("Done editing");
  await expect(edit.getByRole("tooltip")).toHaveText("Done editing");
  await expect(edit).toHaveAttribute("aria-pressed", "true");
  await edit.press("Enter");
  await expect(edit).toHaveAccessibleName("Edit dashboard");
  await expect(edit).toHaveAttribute("aria-pressed", "false");
  await toolbar.getByRole("button", { name: "Full view", exact: true }).click();
  await expect(page.locator("#cockpitView")).toBeVisible();
  await expect(toolbar.getByRole("heading", {name:"Solador", exact:true})).toBeVisible();
  await expect(toolbar.locator('[data-action="allPanels"]')).toHaveAttribute("aria-pressed", "true");
  await expect(toolbar.getByRole("button", {name:"Settings", exact:true}).locator("svg")).toHaveCount(1);
  await toolbar.getByRole("button", {name:"Settings", exact:true}).click();
  await page.locator('#settingsClose').click();
  await expect(page.locator('#cockpitView')).toBeVisible();
  await edit.focus();
  await edit.press('Enter');
  await expect(edit).toBeFocused();
  await expect(edit).toHaveAccessibleName('Done editing');
  await expect(toolbar.locator('[data-action="allPanels"]')).toHaveAttribute("aria-pressed", "false");
  await toolbar.getByRole("button", { name: "Settings", exact: true }).click();
  await expect(page.locator("#settings")).toBeVisible();
});

test("Fit window to tiles sends the measured content height from either view, and a failure sits under the toolbar", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL);
  // A window much taller than its tiles: the button must measure the tiles,
  // never the viewport.
  await page.setViewportSize({ width: 1280, height: 3000 });
  const toolbar = page.locator(".db-chrome");
  const fit = toolbar.getByRole("button", { name: "Fit window to tiles", exact: true });
  const fits = () => page.evaluate(() => window.__CALLS__.filter(c => c.command === "window_fit_height").map(c => c.args));
  const measured = () => page.evaluate(() => ({
    contentHeight: Math.ceil(document.body.getBoundingClientRect().height),
    viewportHeight: document.documentElement.clientHeight,
  }));

  await fit.click();
  const overview = await measured();
  expect(overview.contentHeight).toBeLessThan(3000);
  expect(overview.viewportHeight).toBe(3000);
  expect(await fits()).toEqual([overview]);

  await toolbar.getByRole("button", { name: "Full view", exact: true }).click();
  await expect(page.locator("#cockpitView")).toBeVisible();
  await fit.click();
  expect((await fits())[1]).toEqual(await measured());

  await page.evaluate(() => { window.__FAIL_FIT__ = true; });
  await fit.click();
  const notice = page.locator("#cockpitView > .db-chrome + .db-action-error");
  await expect(notice).toHaveText("Couldn't fit the window: exit full screen first");
  await page.evaluate(() => { window.__FAIL_FIT__ = false; });
  await fit.click();
  await expect(page.locator(".db-action-error")).toHaveCount(0);
});

test('Full runners share Detail tables and the saved view choice', async ({page, baseURL}) => {
  await openDashboard(page, baseURL, true);
  await tile(page, 'ghRunners').getByRole('button', {name:/MACOS · ARM64/}).click();
  await action(page, 'tileFull').click();
  const panel = page.locator('#runnersPanel');
  await expect(page.locator('#cockpitView > .topbar')).toBeHidden();
  await expect(panel.locator('.db-detail-table')).toBeVisible();
  await expect(panel.locator('thead th')).toHaveText(['Runner', 'Organization', 'OS', 'Architecture', 'Labels', 'Status']);
  const list = panel.getByRole('button', {name:'List', exact:true});
  await list.click();
  await expect(list).toHaveAttribute('aria-pressed', 'true');
  expect((await savedLayout(page)).detailViews.ghRunners).toBe('list');
  await page.locator('.db-chrome [data-action="allPanels"]').click();
  await tile(page,'ghRunners').locator('.db-tile-footer [data-action="details"]').click();
  await expect(page.getByRole('button',{name:'List',exact:true})).toHaveAttribute('aria-pressed','true');
  await page.getByRole('button',{name:'Table',exact:true}).click();
  await tile(page,'ghRunners').getByRole('button',{name:/MACOS · ARM64/}).click();
  await action(page, 'tileFull').click();
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

test('a half-width Full runners table keeps Status in view, labels on one line', async ({page, baseURL}) => {
  await page.setViewportSize({width:1800, height:1200});
  await openDashboard(page, baseURL, true);
  await page.locator('.db-chrome [data-action="allPanels"]').click();
  const panel = page.locator('#runnersPanel');
  await expect(panel.locator('.db-detail-table')).toBeVisible();
  const edge = await panel.evaluate(el => el.getBoundingClientRect().right);
  expect(await panel.locator('thead th').last().evaluate(el => el.getBoundingClientRect().right)).toBeLessThanOrEqual(edge);
  const row = panel.locator('tbody tr', {hasText:'mac-s1'});
  expect(await row.evaluate(el => el.getBoundingClientRect().height)).toBeLessThanOrEqual(36);
  // The custom label leads; GitHub's own, which OS and Architecture already
  // say, are the ones a narrow cell gives up — and the tooltip still has all.
  await expect(row.locator('.db-label-chip').first()).toHaveText('xcode-16');
  await expect(row.locator('td.gh-runner-labels')).toHaveAttribute('title', 'self-hosted, macOS, ARM64, xcode-16');
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
    await expect(volumes.locator('thead th')).toHaveText(['Volume','Used','Total','Use','']);
    const columns = await volumes.locator('tbody tr').evaluateAll(rows=>rows.map(row=>[...row.cells].map(c=>c.getBoundingClientRect().right)));
    for (const positions of columns) expect(positions).toEqual(columns[0]);
  }
});

test('a volume hidden from Detail leaves the table and can be unhidden there', async ({page,baseURL}) => {
  const model = await openDashboard(page,baseURL);
  const host = model.sources.find(s=>s.id==='hosts').rows.find(r=>r.volumes?.length>1);
  const mount = host.volumes[0].mount;
  await page.locator(`[data-action="row"][data-source="hosts"][data-id="${host.id}"]`).first().click();
  const inspector = page.locator('.db-inspector');
  const hide = inspector.getByRole('button',{name:`Hide ${mount}`,exact:true});
  // A refused save keeps the volume where it was and says why beside the
  // volumes, never inside the button it came from.
  await page.evaluate(() => { window.__FAIL_VOLUME__ = true; });
  await hide.click();
  const error = inspector.locator('.db-volumes > .db-action-error');
  await expect(error).toHaveText('Could not save the hidden volumes.');
  await expect(inspector.locator('button .db-action-error')).toHaveCount(0);
  await expect(hide).toHaveAccessibleName(`Hide ${mount}`);
  await expect(hide).toBeFocused();
  await page.evaluate(() => { window.__FAIL_VOLUME__ = false; });
  await hide.click();
  await expect(error).toHaveCount(0);
  await expect(inspector.locator('.db-volume-table tbody th')).toHaveText(host.volumes.map(v=>v.mount).filter(m=>m!==mount).sort());
  const line = inspector.locator('.db-hidden-volume');
  await expect(line).toHaveText(`Hidden: ${mount}Unhide`);
  const unhide = line.getByRole('button',{name:`Unhide ${mount}`});
  await expect(unhide).toBeFocused();
  await unhide.click();
  await expect(inspector.locator('.db-hidden-volume')).toHaveCount(0);
  await expect(inspector.locator('.db-volume-table tbody th')).toHaveText(host.volumes.map(v=>v.mount).sort());
  await expect(hide).toBeFocused();
  expect(await page.evaluate(() => window.__CALLS__.filter(c => c.command === 'machine_set_volume_hidden').map(c => c.args))).toEqual([
    {hostId:host.id, mount, hidden:true, width:expect.any(Number)},
    {hostId:host.id, mount, hidden:true, width:expect.any(Number)},
    {hostId:host.id, mount, hidden:false, width:expect.any(Number)},
  ]);
});

test('the Machines tile hides its Disk strip volume from the strip itself', async ({page,baseURL}) => {
  const model = await openDashboard(page,baseURL);
  const host = model.sources.find(s=>s.id==='hosts').rows.find(r=>r.metrics?.[4]?.mount);
  const mount = host.metrics[4].mount;
  const card = tile(page,'hosts').locator('.db-host').filter({has:page.locator(`[data-action="row"][data-id="${host.id}"]`)});
  const hide = card.getByRole('button',{name:`Hide ${mount}`,exact:true});
  // Held in place but invisible until the card is hovered or focused, so the
  // percentage never moves.
  await page.mouse.move(0,0);
  await expect(hide).toBeHidden();
  await card.hover();
  await expect(hide).toBeVisible();
  await page.evaluate(() => { window.__FAIL_VOLUME__ = true; });
  await hide.click();
  await expect(card.locator('.db-metric > .db-action-error')).toHaveText('Could not save the hidden volumes.');
  await page.evaluate(() => { window.__FAIL_VOLUME__ = false; });
  await hide.click();
  await expect(card.locator('.db-action-error')).toHaveCount(0);
  await expect(page.locator('.db-inspector')).toBeHidden();
  expect(await page.evaluate(() => window.__CALLS__.filter(c => c.command === 'machine_set_volume_hidden').map(c => c.args))).toEqual([
    {hostId:host.id, mount, hidden:true, width:expect.any(Number)},
    {hostId:host.id, mount, hidden:true, width:expect.any(Number)},
  ]);
});

test('each machine on the Machines tile sits in its own card', async ({page,baseURL}) => {
  await openDashboard(page,baseURL);
  const cards = await tile(page,'hosts').locator('.db-host').evaluateAll(els => els.map(el => {
    const style = getComputedStyle(el), box = el.getBoundingClientRect();
    return {border:style.borderTopWidth, radius:parseFloat(style.borderTopLeftRadius), top:box.top, bottom:box.bottom};
  }));
  expect(cards.length).toBeGreaterThan(1);
  for (const [i, c] of cards.entries()) {
    expect(c.border).toBe('1px');
    expect(c.radius).toBeGreaterThan(0);
    if (i) expect(c.top - cards[i-1].bottom).toBeGreaterThanOrEqual(6);
  }
});

test('GPU memory reads VRAM, and a unified machine says so on RAM and VRAM', async ({page,baseURL}) => {
  // Rust authors the tooltip (only where the producer reports unified memory);
  // this holds the tile to painting it on exactly those two meters.
  const unified = 'Unified memory: the CPU and GPU share one pool, so VRAM is part of RAM';
  await openDashboard(page, baseURL, false, view => {
    const rows = view.sources.find(s => s.id === 'hosts').rows;
    for (const i of [1, 3]) rows[0].metrics[i].title = unified;
  });
  const cards = tile(page,'hosts').locator('.db-host');
  const metrics = cards.nth(0).locator('.db-metric');
  await expect(metrics.nth(3).locator('.db-stat-label')).toContainText('VRAM');
  await expect(metrics.nth(1)).toHaveAttribute('title', unified);
  await expect(metrics.nth(3)).toHaveAttribute('title', unified);
  for (const i of [0, 2]) await expect(metrics.nth(i)).not.toHaveAttribute('title', /./);
  await expect(cards.nth(1).locator('.db-metric').nth(3)).not.toHaveAttribute('title', /./);
  await expect(cards.nth(1).locator('.db-metric').nth(3)).toContainText('22.1 / 24 GB');
});

test('a Machines tile can drop its Disk strip, and only Machines tiles offer it', async ({page,baseURL}) => {
  await openDashboard(page,baseURL);
  await action(page,'edit').click();
  await tile(page,'ghWorkflows').locator('[data-action="configure"]').click();
  await expect(page.getByLabel('Disk strip',{exact:true})).toHaveCount(0);
  await tile(page,'hosts').locator('[data-action="configure"]').click();
  const option = page.getByLabel('Disk strip',{exact:true});
  await expect(option).toHaveValue('true');
  await option.selectOption('false');
  await action(page,'apply').click();
  expect((await savedLayout(page)).tiles.find(t => t.source === 'hosts').showDisk).toBe(false);
  expect((await savedLayout(page)).tiles.find(t => t.source === 'ghWorkflows').showDisk).toBe(true);
  await page.reload();
  await action(page,'edit').click();
  await tile(page,'hosts').locator('[data-action="configure"]').click();
  await expect(page.getByLabel('Disk strip',{exact:true})).toHaveValue('false');
});

test('Settings header keeps space around the logo and Done button', async ({page,baseURL}) => {
  await openDashboard(page,baseURL);
  for (const width of [375,1200]) {
    await page.setViewportSize({width,height:900});
    await action(page,'settings').click();
    const settings = await page.locator('#settings > .topbar').evaluate(el=>({left:el.querySelector('img').getBoundingClientRect().left-el.getBoundingClientRect().left,top:el.querySelector('button').getBoundingClientRect().top-el.getBoundingClientRect().top,right:el.getBoundingClientRect().right-el.querySelector('button').getBoundingClientRect().right}));
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
  // Dependabot's share of PRS is a Detail column of its own, right after PRS.
  const table = page.locator('#dashboardOverview .db-detail-table');
  const headers = await table.locator('thead th').allTextContents();
  expect(headers.filter(h => h === 'DEPENDABOT')).toHaveLength(1);
  const dependabotColumn = headers.indexOf('DEPENDABOT');
  expect(headers[dependabotColumn - 1]).toBe('PRS');
  await expect(table.locator('tr.db-detail-resource[data-resource="acme/gadget"] > *').nth(dependabotColumn)).toHaveText('3');
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
  await page.getByLabel("Runner view", { exact: true }).selectOption("list");
  await page.getByLabel("Rows to show", { exact: true }).selectOption("all");
  await action(page, "apply").click();
  expect((await savedLayout(page)).tiles.find(t => t.source === "ghRunners").runnerView).toBe("list");
  await page.reload();
  await action(page, "edit").click();
  await tile(page, "ghRunners").locator('[data-action="configure"]').click();
  await expect(page.getByLabel("Runner view", { exact: true })).toHaveValue("list");
  await expect(page.getByLabel("Rows to show", { exact: true })).toHaveValue("all");
});

const commandArgs = (page, command) =>
  page.evaluate((c) => window.__CALLS__.filter(call => call.command === c).map(call => call.args), command);
async function configureRunners(page) {
  await page.locator('#dashboardOverview [data-action="edit"]').click();
  await tile(page, "ghRunners").locator('[data-action="configure"]').click();
  return page.locator("[data-group-labels]");
}

test("the Runners Configure lists group labels with counts and the shared note", async ({ page, baseURL }) => {
  const model = await openDashboard(page, baseURL);
  const editor = model.sources.find(s => s.id === "ghRunners").groupLabels;
  const section = await configureRunners(page);
  await expect(section.locator("legend")).toHaveText(editor.heading);
  const row = section.locator('[data-group-label="runner-large"]');
  await expect(row).toContainText("runner-large");
  await expect(row).toContainText(editor.rows[0].carriers);
  await expect(section).toContainText(editor.firstMatch);
  await expect(section).toContainText("Shared · saved immediately · applies everywhere runners appear: every Runners tile and the Full view.");
  await expect(section.locator('[data-action="group-label-seen"]')).toHaveText(editor.seen);  // The labels decide Detail's and Full's groups too, so a list tile shows them.
  await page.getByLabel("Runner view", { exact: true }).selectOption("list");
  await expect(section).toBeVisible();
  await tile(page, "hosts").locator('[data-action="configure"]').click();
  await expect(page.locator("[data-group-labels]")).toHaveCount(0);
});

test("group label buttons round-trip through Rust and save on the click", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL);
  const section = await configureRunners(page);
  const input = page.locator("#dashboard-group-label");
  const add = section.locator('[data-action="group-label-add"]');
  await expect(add).toBeDisabled();
  await input.fill("gpu");
  await expect(add).toBeEnabled();
  await input.press("Enter");
  await expect.poll(() => commandArgs(page, "runner_group_label_add"))
    .toEqual([{ label: "gpu", width: expect.any(Number) }]);
  await expect(input).toHaveValue("");
  const names = section.locator(".db-group-label-name");
  await expect(names).toHaveText(["runner-large", "gpu"]);
  await expect(section.locator('[data-group-label="runner-large"] [data-direction="up"]')).toBeDisabled();
  await expect(section.locator('[data-group-label="gpu"] [data-direction="down"]')).toBeDisabled();
  await section.locator('[data-group-label="gpu"] [data-direction="up"]').click();
  await expect.poll(() => commandArgs(page, "runner_group_label_move"))
    .toEqual([{ label: "gpu", up: true, width: expect.any(Number) }]);
  await expect(names).toHaveText(["gpu", "runner-large"]);
  await section.locator('[data-action="group-label-seen"]', { hasText: "xcode-16" }).click();
  await expect.poll(async () => (await commandArgs(page, "runner_group_label_add")).at(-1))
    .toEqual({ label: "xcode-16", width: expect.any(Number) });
  await section.locator('[data-group-label="runner-large"] [data-action="group-label-remove"]').click();
  await expect.poll(() => commandArgs(page, "runner_group_label_remove"))
    .toEqual([{ label: "runner-large", width: expect.any(Number) }]);
  await expect(names).toHaveText(["gpu", "xcode-16"]);
  // Saved by Rust on the click: never part of the tile's Apply.
  expect(await commandArgs(page, "dashboard_save")).toEqual([]);
});

test("moving a group label to an end keeps keyboard focus in its row", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL);
  const section = await configureRunners(page);
  await page.locator("#dashboard-group-label").fill("gpu");
  await page.locator("#dashboard-group-label").press("Enter");
  const names = section.locator(".db-group-label-name");
  await expect(names).toHaveText(["runner-large", "gpu"]);
  await section.locator('[data-group-label="gpu"] [data-direction="up"]').focus();
  await page.keyboard.press("Enter");
  await expect(names).toHaveText(["gpu", "runner-large"]);
  // Its ↑ is now disabled, so focus moves to the row's ↓ rather than to <body>.
  await expect(section.locator('[data-group-label="gpu"] [data-direction="down"]')).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(names).toHaveText(["runner-large", "gpu"]);
  await expect(section.locator('[data-group-label="gpu"] [data-direction="up"]')).toBeFocused();
});

test("label edits keep the Configure draft and refresh the preview", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL);
  const section = await configureRunners(page);
  const previews = () => page.evaluate(() => window.__CALLS__.filter(c => c.command === "dashboard_preview").map(c => c.args.tile));
  await page.locator("#dashboard-title").fill("Build fleet");
  await page.locator("#dashboard-width").selectOption("wide");
  await expect.poll(async () => (await previews()).at(-1)).toMatchObject({ title: "Build fleet", width: "wide" });
  const before = (await previews()).length;
  await section.locator('[data-group-label="runner-large"] [data-action="group-label-remove"]').click();
  await expect(section.locator("[data-group-label]")).toHaveCount(0);
  await expect(page.locator("#dashboard-title")).toHaveValue("Build fleet");
  await expect(page.locator("#dashboard-width")).toHaveValue("wide");
  await expect.poll(async () => (await previews()).length).toBeGreaterThan(before);
  expect((await previews()).at(-1)).toMatchObject({ title: "Build fleet", width: "wide" });
});

test("a failed label save says so beside the section", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL);
  await page.evaluate(() => { window.__FAIL_GROUP_LABEL__ = true; });
  const section = await configureRunners(page);
  await section.locator('[data-group-label="runner-large"] [data-action="group-label-remove"]').click();
  await expect(section.locator(".db-action-error")).toHaveText("Could not save the group labels.");
  await expect(section.locator("[data-group-label]")).toHaveCount(1);
  await page.evaluate(() => { window.__FAIL_GROUP_LABEL__ = false; });
  await section.locator('[data-group-label="runner-large"] [data-action="group-label-remove"]').click();
  await expect(section.locator("[data-group-label]")).toHaveCount(0);
  await expect(section.locator(".db-action-error")).toHaveCount(0);
});

async function configureContainers(page) {
  // No Containers tile is on the default dashboard: add one, which opens its
  // Configure on a draft. The rules are not the draft's, so nothing below
  // presses Apply.
  await page.locator('#dashboardOverview [data-action="edit"]').click();
  await page.locator('#dashboardOverview [data-action="catalog"]').click();
  await page.locator('[data-action="add"][data-id="containers"]').click();
  return page.locator("[data-container-rules]");
}

test("the Containers Configure lists every rule field, Collapse-only ones only for Collapse, with the shared note (#613)", async ({ page, baseURL }) => {
  const model = await openDashboard(page, baseURL);
  const t = model.sources.find(s => s.id === "containers").ruleEditor;
  const section = await configureContainers(page);
  await expect(section.locator("legend")).toHaveText(t.heading);
  await expect(section).toContainText(t.firstMatch);
  await expect(section).toContainText("Shared \u00b7 saved immediately \u00b7 applies everywhere containers appear: every Containers tile and the Full view.");
  const rows = section.locator("[data-container-rule]");
  await expect(rows).toHaveCount(t.rows.length);
  for (const rule of t.rows) {
    const row = section.locator(`[data-container-rule="${rule.index}"]`);
    await expect(row.locator('[data-field="action"]')).toHaveValue(rule.action);
    await expect(row.locator('[data-field="pattern"]')).toHaveValue(rule.pattern);
    // Including the host that no longer exists: a picker missing its own
    // selection renders blank, which would read as unscoped.
    await expect(row.locator('[data-field="host"]')).toHaveValue(rule.host);
    await expect(row.locator('[data-field="expected"]')).toHaveCount(rule.collapseOnly ? 1 : 0);
    await expect(row.locator('[data-field="label"]')).toHaveCount(rule.collapseOnly ? 1 : 0);
    if (rule.collapseOnly) await expect(row.locator('[data-field="expected"]')).toHaveValue(rule.expected);
  }
  expect(t.rows.some(r => r.collapseOnly)).toBe(true);
  expect(t.rows.some(r => !r.collapseOnly)).toBe(true);
  expect(t.rows.some(r => r.expected !== "")).toBe(true);
  await expect(rows.first().locator('[data-field="action"] option')).toHaveText(t.actions.map(a => a.label));
  // Only Containers carries it.
  await tile(page, "hosts").locator('[data-action="configure"]').click();
  await expect(page.locator("[data-container-rules]")).toHaveCount(0);
});

test("a container rule edits one field at a time, adds and deletes, saving on the change and never through Apply (#613)", async ({ page, baseURL }) => {
  const model = await openDashboard(page, baseURL);
  const t = model.sources.find(s => s.id === "containers").ruleEditor;
  const section = await configureContainers(page);
  const collapse = t.rows.find(r => r.collapseOnly && r.expected !== "");
  expect(collapse, "the fixture must carry a collapse rule with a count").toBeTruthy();
  const row = () => section.locator(`[data-container-rule="${collapse.index}"]`);
  const width = expect.any(Number);

  await row().locator('[data-field="pattern"]').fill("worker-*");
  await row().locator('[data-field="pattern"]').blur();
  await expect.poll(() => commandArgs(page, "container_rule_set"))
    .toEqual([{ index: collapse.index, field: "pattern", value: "worker-*", width }]);
  await expect(row().locator('[data-field="pattern"]')).toHaveValue("worker-*");
  await row().locator('[data-field="expected"]').fill("6");
  await row().locator('[data-field="expected"]').blur();
  await row().locator('[data-field="host"]').selectOption("");
  await row().locator('[data-field="action"]').selectOption("hide");
  await expect.poll(async () => (await commandArgs(page, "container_rule_set")).map(a => [a.field, a.value]))
    .toEqual([["pattern", "worker-*"], ["expected", "6"], ["host", ""], ["action", "hide"]]);
  // Rust says a Hide rule has no aggregate, so the Collapse-only fields go.
  await expect(row().locator('[data-field="expected"]')).toHaveCount(0);

  const before = t.rows.length;
  await section.locator('[data-action="container-rule-add"]').click();
  await expect.poll(() => commandArgs(page, "container_rule_add")).toEqual([{ width }]);
  await expect(section.locator("[data-container-rule]")).toHaveCount(before + 1);
  await section.locator(`[data-container-rule="${collapse.index}"] [data-action="container-rule-remove"]`).click();
  await expect.poll(() => commandArgs(page, "container_rule_remove")).toEqual([{ index: collapse.index, width }]);
  await expect(section.locator("[data-container-rule]")).toHaveCount(before);
  expect(await commandArgs(page, "dashboard_save")).toEqual([]);
});

test("a failed container rule save says so beside the section (#613)", async ({ page, baseURL }) => {
  const model = await openDashboard(page, baseURL);
  const t = model.sources.find(s => s.id === "containers").ruleEditor;
  await page.evaluate(() => { window.__FAIL_CONTAINER_RULE__ = true; });
  const section = await configureContainers(page);
  await section.locator('[data-container-rule="0"] [data-action="container-rule-remove"]').click();
  await expect(section.locator(".db-action-error")).toHaveText("Could not save the container rules.");
  await expect(section.locator("[data-container-rule]")).toHaveCount(t.rows.length);
});

async function configureMachines(page) {
  await page.locator('#dashboardOverview [data-action="edit"]').click();
  await tile(page, "hosts").locator('[data-action="configure"]').click();
  return page.locator("[data-machine-alerts]");
}
const machine = (section, id) => section.locator(`[data-machine="${id}"]`);
const limit = (block, label) => block.getByLabel(label, { exact: true });

test("the Machines Configure edits the shared limits, every machine's override and its volumes, and says they are shared", async ({ page, baseURL }) => {
  const model = await openDashboard(page, baseURL);
  const hosts = model.sources.find(s => s.id === "hosts");
  const editor = hosts.machineAlerts;
  const section = await configureMachines(page);
  await expect(section.locator("legend")).toHaveText(editor.heading);
  await expect(section).toContainText("Shared · saved immediately · applies to every Machines tile, Detail and the Full view.");
  const shared = machine(section, "defaults");
  for (const field of editor.defaults.fields)
    await expect(limit(shared, field.label)).toHaveValue(String(field.value));
  for (const row of hosts.rows) {
    const block = machine(section, row.id);
    await expect(block.locator("h4")).toHaveText(row.label);
    await expect(block.getByLabel("Use shared defaults", { exact: true })).toBeChecked();
    await expect(limit(block, "RAM warning (%)")).toBeDisabled();
    const mounts = [...(row.volumes || []).map(v => v.mount), ...(row.hiddenVolumes || [])].sort();
    await expect(block.locator(".db-machine-mount")).toHaveText(mounts);
  }
  // Another tile's Configure has none of it.
  await tile(page, "ghWorkflows").locator('[data-action="configure"]').click();
  await expect(page.locator("[data-machine-alerts]")).toHaveCount(0);
});

test("shared limits, a machine's override and its reset save on the click, never with the tile's Apply", async ({ page, baseURL }) => {
  const model = await openDashboard(page, baseURL);
  const row = model.sources.find(s => s.id === "hosts").rows[0];
  const section = await configureMachines(page);
  const shared = machine(section, "defaults");
  await limit(shared, "RAM warning (%)").fill("80");
  await limit(shared, "RAM critical (%)").fill("95");
  await shared.getByRole("button", { name: "Apply thresholds", exact: true }).click();
  await expect.poll(() => commandArgs(page, "machine_save_alerts")).toEqual([
    { hostId: null, thresholds: expect.objectContaining({ ramWarning: 80, ramCritical: 95 }), width: expect.any(Number) },
  ]);
  // A machine that follows the shared limits shows the new ones.
  await expect(limit(machine(section, row.id), "RAM warning (%)")).toHaveValue("80");
  const block = machine(section, row.id);
  await block.getByLabel("Use shared defaults", { exact: true }).uncheck();
  await limit(block, "RAM warning (%)").fill("50");
  await limit(block, "RAM critical (%)").fill("60");
  await block.getByRole("button", { name: "Apply thresholds", exact: true }).click();
  await expect.poll(async () => (await commandArgs(page, "machine_save_alerts")).at(-1)).toEqual(
    { hostId: row.id, thresholds: expect.objectContaining({ ramWarning: 50, ramCritical: 60 }), width: expect.any(Number) },
  );
  await expect(block.getByLabel("Use shared defaults", { exact: true })).not.toBeChecked();
  await block.getByLabel("Use shared defaults", { exact: true }).check();
  await block.getByRole("button", { name: "Apply thresholds", exact: true }).click();
  await expect.poll(async () => (await commandArgs(page, "machine_save_alerts")).at(-1))
    .toEqual({ hostId: row.id, thresholds: null, width: expect.any(Number) });
  await shared.getByRole("button", { name: "Reset defaults", exact: true }).click();
  await expect.poll(async () => (await commandArgs(page, "machine_save_alerts")).at(-1))
    .toEqual({ hostId: null, thresholds: null, width: expect.any(Number) });
  await expect(limit(shared, "RAM warning (%)")).toHaveValue("70");
  expect(await commandArgs(page, "dashboard_save")).toEqual([]);
});

test("saving one machine's limits keeps another machine's unsaved draft", async ({ page, baseURL }) => {
  const model = await openDashboard(page, baseURL);
  const [first, second] = model.sources.find(s => s.id === "hosts").rows;
  const section = await configureMachines(page);
  const other = machine(section, second.id);
  await other.getByLabel("Use shared defaults", { exact: true }).uncheck();
  await limit(other, "RAM warning (%)").fill("42");
  const block = machine(section, first.id);
  await block.getByLabel("Use shared defaults", { exact: true }).uncheck();
  await block.getByRole("button", { name: "Apply thresholds", exact: true }).click();
  await expect.poll(() => commandArgs(page, "machine_save_alerts")).toHaveLength(1);
  await expect(limit(other, "RAM warning (%)")).toHaveValue("42");
  await expect(other.getByLabel("Use shared defaults", { exact: true })).not.toBeChecked();
});

test("saving the shared limits keeps a machine's unsaved draft", async ({ page, baseURL }) => {
  const model = await openDashboard(page, baseURL);
  const [, second] = model.sources.find(s => s.id === "hosts").rows;
  const section = await configureMachines(page);
  const other = machine(section, second.id);
  await other.getByLabel("Use shared defaults", { exact: true }).uncheck();
  await limit(other, "RAM warning (%)").fill("42");
  const shared = machine(section, "defaults");
  await limit(shared, "RAM warning (%)").fill("80");
  await limit(shared, "RAM critical (%)").fill("95");
  await shared.getByRole("button", { name: "Apply thresholds", exact: true }).click();
  await expect.poll(() => commandArgs(page, "machine_save_alerts")).toHaveLength(1);
  await shared.getByRole("button", { name: "Reset defaults", exact: true }).click();
  await expect.poll(() => commandArgs(page, "machine_save_alerts")).toHaveLength(2);
  await expect(other.getByLabel("Use shared defaults", { exact: true })).not.toBeChecked();
  await expect(limit(other, "RAM warning (%)")).toHaveValue("42");
  await expect(limit(other, "RAM warning (%)")).toBeEnabled();
});

test("an invalid unsaved limit does not block the tile's own Apply", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL);
  const section = await configureMachines(page);
  const shared = machine(section, "defaults");
  await limit(shared, "RAM warning (%)").fill("150");
  await page.locator("#dashboard-title").fill("Machines now");
  await action(page, "apply").click();
  await expect.poll(() => commandArgs(page, "dashboard_save")).toHaveLength(1);
  expect(await commandArgs(page, "machine_save_alerts")).toEqual([]);
  // The block's own Apply is still what gates a limit.
  await tile(page, "hosts").locator('[data-action="configure"]').click();
  await limit(machine(page.locator("[data-machine-alerts]"), "defaults"), "RAM warning (%)").fill("150");
  await machine(page.locator("[data-machine-alerts]"), "defaults").getByRole("button", { name: "Apply thresholds", exact: true }).click();
  expect(await commandArgs(page, "machine_save_alerts")).toEqual([]);
});

test("a refused limits save says so beside the machine and keeps the draft", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL);
  await page.evaluate(() => { window.__FAIL_ALERTS__ = true; });
  const section = await configureMachines(page);
  const shared = machine(section, "defaults");
  await limit(shared, "RAM warning (%)").fill("99");
  await shared.getByRole("button", { name: "Apply thresholds", exact: true }).click();
  await expect(section.locator(".db-action-error")).toHaveText("Could not save the machine alert limits.");
  await expect(limit(shared, "RAM warning (%)")).toHaveValue("99");
});

test("the Configure volume list hides and shows a mount through the same command as the Disk strip", async ({ page, baseURL }) => {
  const model = await openDashboard(page, baseURL);
  const row = model.sources.find(s => s.id === "hosts").rows.find(r => (r.volumes || []).length);
  const mount = row.volumes[0].mount;
  const section = await configureMachines(page);
  const box = () => machine(section, row.id).locator(`input[data-mount="${mount}"]`);
  await expect(box()).toBeChecked();
  await box().click();
  await expect.poll(() => commandArgs(page, "machine_set_volume_hidden"))
    .toEqual([{ hostId: row.id, mount, hidden: true, width: expect.any(Number) }]);
  await expect(box()).not.toBeChecked();
  await box().click();
  await expect.poll(async () => (await commandArgs(page, "machine_set_volume_hidden")).at(-1))
    .toEqual({ hostId: row.id, mount, hidden: false, width: expect.any(Number) });
  await expect(box()).toBeChecked();
  expect(await commandArgs(page, "dashboard_save")).toEqual([]);
});

test("a refused volume save puts the tick back", async ({ page, baseURL }) => {
  const model = await openDashboard(page, baseURL);
  const row = model.sources.find(s => s.id === "hosts").rows.find(r => (r.volumes || []).length);
  const mount = row.volumes[0].mount;
  await page.evaluate(() => { window.__FAIL_VOLUME__ = true; });
  const section = await configureMachines(page);
  const box = machine(section, row.id).locator(`input[data-mount="${mount}"]`);
  await box.click();
  await expect(section.locator(".db-action-error")).toHaveText("Could not save the hidden volumes.");
  await expect(box).toBeChecked();
});

test("ticking a volume, or a poll adding and removing a mount, keeps the machine's typed limits (#620)", async ({ page, baseURL }) => {
  const model = await openDashboard(page, baseURL);
  const hosts = model.sources.find(s => s.id === "hosts");
  const row = hosts.rows.find(r => (r.volumes || []).length);
  const mount = row.volumes[0].mount;
  const section = await configureMachines(page);
  const block = machine(section, row.id);
  await block.getByLabel("Use shared defaults", { exact: true }).uncheck();
  await limit(block, "RAM warning (%)").fill("42");
  // A poll that brings a mount (removable media) and one that takes it away.
  const withUsb = hosts.rows.map(r => r.id === row.id
    ? { ...r, volumes: [...r.volumes, { ...r.volumes[0], mount: "/Volumes/USB" }] }
    : r);
  await page.evaluate(next => window.__SET_SOURCE_ROWS__("hosts", next), withUsb);
  await expect(block.locator('input[data-mount="/Volumes/USB"]')).toHaveCount(1);
  await expect(limit(block, "RAM warning (%)")).toHaveValue("42");
  await page.evaluate(next => window.__SET_SOURCE_ROWS__("hosts", next), hosts.rows);
  await expect(block.locator('input[data-mount="/Volumes/USB"]')).toHaveCount(0);
  await expect(limit(block, "RAM warning (%)")).toHaveValue("42");
  // Ticking a volume repaints the list from Rust's answer, not the limits.
  await block.locator(`input[data-mount="${mount}"]`).click();
  await expect.poll(() => commandArgs(page, "machine_set_volume_hidden")).toHaveLength(1);
  await expect(block.locator(`input[data-mount="${mount}"]`)).not.toBeChecked();
  await expect(limit(block, "RAM warning (%)")).toHaveValue("42");
  await expect(limit(block, "RAM warning (%)")).toBeEnabled();
  await expect(block.getByLabel("Use shared defaults", { exact: true })).not.toBeChecked();
});

test("after a shared save, unticking Use shared defaults prefills the new defaults, and a ticked machine shows them (#620)", async ({ page, baseURL }) => {
  const model = await openDashboard(page, baseURL);
  const [first, second] = model.sources.find(s => s.id === "hosts").rows;
  const section = await configureMachines(page);
  const shared = machine(section, "defaults");
  const inheriting = machine(section, first.id);
  // The second machine holds a saved override, then ticks the box locally.
  const overridden = machine(section, second.id);
  await overridden.getByLabel("Use shared defaults", { exact: true }).uncheck();
  await limit(overridden, "RAM warning (%)").fill("50");
  await limit(overridden, "RAM critical (%)").fill("60");
  await overridden.getByRole("button", { name: "Apply thresholds", exact: true }).click();
  await expect.poll(() => commandArgs(page, "machine_save_alerts")).toHaveLength(1);
  await overridden.getByLabel("Use shared defaults", { exact: true }).check();
  await limit(shared, "RAM warning (%)").fill("80");
  await limit(shared, "RAM critical (%)").fill("95");
  await shared.getByRole("button", { name: "Apply thresholds", exact: true }).click();
  await expect.poll(() => commandArgs(page, "machine_save_alerts")).toHaveLength(2);
  await expect(limit(overridden, "RAM warning (%)")).toHaveValue("80");
  await expect(limit(overridden, "RAM warning (%)")).toBeDisabled();
  await inheriting.getByLabel("Use shared defaults", { exact: true }).uncheck();
  await expect(limit(inheriting, "RAM warning (%)")).toHaveValue("80");
  await expect(limit(inheriting, "RAM critical (%)")).toHaveValue("95");
  // Apply sends what is shown: nothing for the ticked block, 80/95 for the unticked.
  await overridden.getByRole("button", { name: "Apply thresholds", exact: true }).click();
  await expect.poll(async () => (await commandArgs(page, "machine_save_alerts")).at(-1))
    .toEqual({ hostId: second.id, thresholds: null, width: expect.any(Number) });
  await inheriting.getByRole("button", { name: "Apply thresholds", exact: true }).click();
  await expect.poll(async () => (await commandArgs(page, "machine_save_alerts")).at(-1))
    .toEqual({ hostId: first.id, thresholds: expect.objectContaining({ ramWarning: 80, ramCritical: 95 }), width: expect.any(Number) });
  await overridden.getByLabel("Use shared defaults", { exact: true }).uncheck();
  await expect(limit(overridden, "RAM warning (%)")).toBeEnabled();
});

// Several 1s polls: every render calls refreshDefaults on a kept block.
const afterRepaints = async (page) => {
  const count = () => page.evaluate(() => window.__CALLS__.length);
  const start = await count();
  await expect.poll(count, { timeout: 8000 }).toBeGreaterThan(start + 2);
};

test("a typed limit survives tick, a repaint and untick (#620)", async ({ page, baseURL }) => {
  const model = await openDashboard(page, baseURL);
  const [first] = model.sources.find(s => s.id === "hosts").rows;
  const block = machine(await configureMachines(page), first.id);
  const toggle = block.getByLabel("Use shared defaults", { exact: true });
  await toggle.uncheck();
  await limit(block, "RAM warning (%)").fill("42");
  await toggle.check();
  await afterRepaints(page);
  await toggle.uncheck();
  await expect(limit(block, "RAM warning (%)")).toHaveValue("42");
});

test("a saved override survives a local tick, a repaint and untick, and Apply sends it (#620)", async ({ page, baseURL }) => {
  const model = await openDashboard(page, baseURL);
  const [first] = model.sources.find(s => s.id === "hosts").rows;
  const block = machine(await configureMachines(page), first.id);
  const toggle = block.getByLabel("Use shared defaults", { exact: true });
  await toggle.uncheck();
  await limit(block, "RAM warning (%)").fill("50");
  await limit(block, "RAM critical (%)").fill("60");
  await block.getByRole("button", { name: "Apply thresholds", exact: true }).click();
  await expect.poll(() => commandArgs(page, "machine_save_alerts")).toHaveLength(1);
  await toggle.check();
  await afterRepaints(page);
  await toggle.uncheck();
  await expect(limit(block, "RAM warning (%)")).toHaveValue("50");
  await block.getByRole("button", { name: "Apply thresholds", exact: true }).click();
  await expect.poll(async () => (await commandArgs(page, "machine_save_alerts")).at(-1)).toEqual(
    { hostId: first.id, thresholds: expect.objectContaining({ ramWarning: 50, ramCritical: 60 }), width: expect.any(Number) });
});

test("a fresh dashboard groups Runners by OS and architecture, and an added Runners tile starts grouped with List one click away", async ({ page, baseURL }) => {
  const original = await openDashboard(page, baseURL);
  expect(original.tiles.find(t => t.source === "ghRunners").runnerView).toBe("grouped");
  await expect(tile(page, "ghRunners").locator('.db-item-name')).toHaveText(["LINUX · ARM64", "MACOS · ARM64"]);
  await action(page, "edit").click();
  await action(page, "catalog").click();
  await page.locator('[data-action="add"][data-id="ghRunners"]').click();
  await expect(page.getByLabel("Runner view", { exact: true })).toHaveValue("grouped");
  await page.getByLabel("Runner view", { exact: true }).selectOption("list");
  await expect(page.getByLabel("Runner view", { exact: true })).toHaveValue("list");
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
  // Measured meters only: mac-studio has no GPU (CPU, RAM, Disk) and ubu-01
  // serves a model (CPU, RAM, GPU, VRAM, Disk). An unmeasured slot is a track
  // with no `meter` role, never a 0% one.
  await expect(tile(page,"hosts").getByRole("meter")).toHaveCount(8);
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
  // Hiding a volume is a machine's own control; the shared panel is the rest.
  expect(controls).toEqual(machineControls.filter(action => !['manage-resource', 'volume-hidden'].includes(action)));
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
  linux.value = '0/2 busy';
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
  await expect(inspector).toContainText('No runners currently match this group.');
  await expect(subtitle).toBeEmpty();
  await expect(page.locator('#dashboardOverview')).toBeVisible();
  await inspector.getByRole('button', {name:'List', exact:true}).click();
  await expect(inspector.locator('.db-detail-resource')).toHaveCount(0);
  await expect(inspector).toContainText('No runners currently match this group.');
  // A connection problem must not look like a successfully read empty group.
  source.rows = [];
  source.groups = [];
  source.message = 'Configure a GitHub token in Settings to see runners.';
  await page.evaluate(source => window.__SET_RUNNER_SOURCE__(source), source);
  await expect(inspector).toContainText(source.message);
  await expect(inspector).not.toContainText('No runners currently match this group.');
});

test("runner group Full view preserves the filter, tracks membership, and clears it", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL, true);
  const payload = await (await fetch(`${baseURL}/sample-runners.json`)).json();
  await tile(page,"ghRunners").getByRole("button",{name:/LINUX · ARM64/}).click();
  await action(page, 'tileFull').click();
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
  await expect(page.locator('#runnersBody')).toContainText('No runners currently match this group.');
  await page.evaluate(async payload => { window.__SET_RUNNER_PAYLOAD__(payload); await refreshPanels(); }, payload);
  await page.getByRole('button', {name:'All runners', exact:true}).click();
  await expect(runners).toHaveCount(6);
  await expect(page.locator('#runnersTitle')).toBeFocused();
  await page.locator('.db-chrome [data-action="allPanels"]').click();
  await expect(page.locator('#dashboardOverview')).toBeVisible();
});

test("label-split runner groups open Detail and the full panel onto exactly their runners", async ({ page, baseURL }) => {
  // Rust stamps every runner row with its group; here two LINUX · ARM64
  // runners carry runner-large and the third carries none. The frontend must
  // follow the stamp — never parse an id or regroup by OS · architecture.
  const large = { id: "group:LINUX:ARM64:label:runner-large", label: "LINUX · ARM64 · runner-large" };
  const other = { id: "group:LINUX:ARM64:other", label: "LINUX · ARM64 · other" };
  const largeNames = ["ubu-01", "ubu-1"];
  const stamp = (rows) => rows.filter((r) => r.groupId === "group:LINUX:ARM64").forEach((r) => {
    const group = largeNames.includes(r.label ?? r.name) ? large : other;
    Object.assign(r, { groupId: group.id, groupLabel: group.label });
  });
  const split = (groups) => groups.flatMap((g) =>
    g.id === "group:LINUX:ARM64" ? [{ ...g, ...large }, { ...g, ...other }] : [g]);
  await openDashboard(page, baseURL, true, (view) => {
    const source = view.sources.find((s) => s.id === "ghRunners");
    stamp(source.rows);
    source.groups = split(source.groups);
    const runners = view.tiles.find((t) => t.source === "ghRunners");
    runners.rows = split(runners.rows);
  });
  const inspector = page.locator("#dashboardInspector");
  await tile(page, "ghRunners").getByRole("button", { name: /runner-large/ }).click();
  await expect(inspector.getByRole("heading")).toHaveText(large.label);
  await expect(inspector.locator(".db-table-name")).toHaveText(largeNames);
  await tile(page, "ghRunners").getByRole("button", { name: /LINUX · ARM64 · other/ }).click();
  await expect(inspector.getByRole("heading")).toHaveText(other.label);
  await expect(inspector.locator(".db-table-name")).toHaveText(["ubu-spare"]);

  const payload = await (await fetch(`${baseURL}/sample-runners.json`)).json();
  stamp(payload.rows);
  payload.groups = split(payload.groups);
  await page.evaluate((payload) => window.__SET_RUNNER_PAYLOAD__(payload), payload);
  await action(page, "tileFull").click();
  await page.evaluate(() => refreshPanels());
  await expect(page.locator("#runnersBody .gh-runner-name")).toHaveText(["ubu-spare"]);
});

// ubu-01 carries runner-large, stamped as Rust would: the row's group, its
// chips with the one that matched marked, and each group's match line. The
// frontend paints these and derives none of them. Returns `groups` split.
function stampRunnerLarge(rows, groups) {
  const large = { id: "group:LINUX:ARM64:label:runner-large", label: "LINUX · ARM64 · runner-large", match: "matched by runner-large" };
  const other = { id: "group:LINUX:ARM64:other", label: "LINUX · ARM64 · other", match: "carries none of your group labels · 1 not recorded" };
  const chips = ["self-hosted", "Linux", "ARM64"].map(text => ({ text, kind: "standard" }))
    .concat([{ text: "runner-large", kind: "match" }]);
  for (const r of rows.filter(r => r.groupId === "group:LINUX:ARM64")) {
    const carries = (r.label ?? r.name) === "ubu-01", group = carries ? large : other;
    Object.assign(r, { groupId: group.id, groupLabel: group.label, groupMatch: carries ? "runner-large" : null, groupSplit: true });
    if (!carries) continue;
    Object.assign(r, { labelChips: chips, labelsText: chips.map(c => c.text).join(", ") });
    const field = r.details?.find(d => d.label === "Labels");
    if (field) Object.assign(field, { chips, value: r.labelsText });
  }
  return groups.flatMap(g => g.id === "group:LINUX:ARM64" ? [{ ...g, ...large }, { ...g, ...other }] : [g]);
}
function splitRunnerFixture(view) {
  const source = view.sources.find(s => s.id === "ghRunners");
  source.groups = stampRunnerLarge(source.rows, source.groups);
  const runners = view.tiles.find(t => t.source === "ghRunners");
  runners.rows = stampRunnerLarge([], runners.rows);
}

test("Detail shows each runner's labels with the match highlighted", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL, true, splitRunnerFixture);
  const inspector = page.locator("#dashboardInspector");
  await tile(page, "ghRunners").locator('.db-tile-footer [data-action="details"]').click();
  await expect(inspector.locator("thead th")).toContainText(["Labels"]);
  for (const view of ["Table", "List"]) {
    await inspector.getByRole("button", { name: view, exact: true }).click();
    const large = inspector.locator('[data-resource="acme/ubu-01"] .db-label-chips').first();
    await expect(large.locator('.db-label-chip[data-kind="match"]')).toHaveText("runner-large");
    await expect(large.locator('.db-label-chip[data-kind="standard"]').first()).toHaveText("self-hosted");
    await expect(inspector.locator('[data-resource="acme/ubu-1"]').first()).toContainText("— not recorded");
    await expect(inspector.locator('[data-resource="acme/ubu-1"] .db-label-chips')).toHaveCount(0);
  }
});

test("Detail opened on a group says what matched", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL, true, splitRunnerFixture);
  const subtitle = page.locator("#dashboardInspector .db-inspector-head .db-sub");
  await tile(page, "ghRunners").getByRole("button", { name: /runner-large/ }).click();
  await expect(subtitle).toContainText("matched by runner-large");
  await tile(page, "ghRunners").getByRole("button", { name: /LINUX · ARM64 · other/ }).click();
  await expect(subtitle).toContainText("carries none of your group labels · 1 not recorded");
  // A plain, unsplit group has no match line.
  await tile(page, "ghRunners").getByRole("button", { name: /MACOS · ARM64/ }).click();
  await expect(subtitle).not.toContainText("matched");
  await expect(subtitle).not.toContainText("carries none");
});

test("a long group label wraps inside a small tile", async ({ page, baseURL }) => {
  const long = `LINUX · ARM64 · ${"x".repeat(48)}`;
  expect(long).toHaveLength(64);
  await openDashboard(page, baseURL, false, (view) => {
    view.layout.tiles.find(t => t.source === "ghRunners").width = "small";
    view.tiles.find(t => t.source === "ghRunners").rows[0].label = long;
  });
  await page.setViewportSize({ width: 1200, height: 900 });
  const runners = tile(page, "ghRunners");
  const name = runners.locator(".db-item-name").first();
  await expect(name).toHaveText(long);
  const box = await name.evaluate(el => ({ height: el.getBoundingClientRect().height, line: parseFloat(getComputedStyle(el).lineHeight) }));
  expect(box.height).toBeGreaterThan(box.line * 1.5);
  expect(await runners.evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
});

test("Full runners table and list show labels", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL, true);
  const payload = await (await fetch(`${baseURL}/sample-runners.json`)).json();
  const mac = payload.rows.find(r => r.name === "mac-s1");
  await page.locator('.db-chrome [data-action="allPanels"]').click();
  const panel = page.locator("#runnersPanel");
  await expect(panel.locator("thead th")).toContainText(["Labels"]);
  const cell = panel.locator(`tr[data-runner='${JSON.stringify(["acme", "mac-s1"])}'] .db-label-chip`);
  await expect(cell).toHaveText(mac.labelChips.map(c => c.text));
  await expect(cell.first()).toHaveAttribute("data-kind", "custom");
  await expect(cell.last()).toHaveAttribute("data-kind", "standard");
  await expect(panel.locator(`tr[data-runner='${JSON.stringify(["acme", "mac-s3"])}']`)).toContainText("— not recorded");
  await panel.getByRole("button", { name: "List", exact: true }).click();
  const row = panel.locator("#runnersBody .gh-row", { has: page.locator(".gh-runner-name", { hasText: "mac-s1" }) });
  await expect(row.locator(".db-label-chips .db-label-chip")).toHaveText(mac.labelChips.map(c => c.text));
});

test("Full filtered to a group says what matched", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL, true, splitRunnerFixture);
  const payload = await (await fetch(`${baseURL}/sample-runners.json`)).json();
  payload.groups = stampRunnerLarge(payload.rows, payload.groups);
  await page.evaluate((payload) => window.__SET_RUNNER_PAYLOAD__(payload), payload);
  await tile(page, "ghRunners").getByRole("button", { name: /runner-large/ }).click();
  await action(page, "tileFull").click();
  await page.evaluate(() => refreshPanels());
  await expect(page.locator("#runnersBody .gh-group-match")).toHaveText("matched by runner-large");
  await page.locator('.db-chrome [data-action="allPanels"]').click();
  await tile(page, "ghRunners").getByRole("button", { name: /MACOS · ARM64/ }).click();
  await action(page, "tileFull").click();
  await expect(page.locator("#runnersBody .gh-runner-filter")).toContainText("MACOS · ARM64");
  await expect(page.locator("#runnersBody .gh-group-match")).toHaveCount(0);
});

test("a vanished group in Full shows the empty-group message", async ({ page, baseURL }) => {
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await openDashboard(page, baseURL, true);
  const payload = await (await fetch(`${baseURL}/sample-runners.json`)).json();
  await tile(page, "ghRunners").getByRole("button", { name: /LINUX · ARM64/ }).click();
  await action(page, "tileFull").click();
  await expect(page.locator("#runnersBody .gh-runner-name")).toHaveText(["ubu-01", "ubu-1", "ubu-spare"]);
  // The group's identity moved (a label now splits it), so the stored filter
  // names a group this frame does not have.
  const moved = structuredClone(payload);
  moved.groups = moved.groups.filter(g => g.id !== "group:LINUX:ARM64");
  moved.rows = moved.rows.filter(r => r.groupId !== "group:LINUX:ARM64");
  await page.evaluate(async (payload) => { window.__SET_RUNNER_PAYLOAD__(payload); await refreshPanels(); }, moved);
  await expect(page.locator("#runnersBody .gh-runner-filter strong")).toHaveText("LINUX · ARM64");
  await expect(page.locator("#runnersBody")).toContainText(payload.emptyGroupMessage);
  await expect(page.locator("#runnersBody .gh-runner-name")).toHaveCount(0);
  expect(errors).toEqual([]);
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
    if (preset.tile.runnerView === "grouped") {
      // Group rows are Rust's (the IPC double serves the fixture's own groups; scope filtering of them is Rust-tested).
      await expect(page.locator(".db-preview-tile .db-item-name")).toHaveText(["LINUX · ARM64", "MACOS · ARM64"]); // the double does not scope-filter group rows
      await expect(page.getByLabel("Runner view", { exact: true })).toHaveValue("grouped");
    } else {
      await expect(page.locator(".db-preview-tile .db-item-name")).toHaveText(rows.slice(0, preset.tile.source === "hosts" ? 4 : 5).map(r => r.label));
    }
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
  await expect(copied.locator('[data-action="row"][data-id="local"]')).toHaveCount(0);
  await expect(tile(page, "hosts").locator('[data-action="row"][data-id="local"]')).toHaveCount(1);
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
  // Tall enough that the second tile row — the services handle — is on
  // screen: a drag that starts outside the viewport presses nothing.
  await page.setViewportSize({ width: 1280, height: 1100 });
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

test("details reach the tile's Full view and the source's connection settings", async ({
  page,
  baseURL,
}) => {
  await openDashboard(page, baseURL);
  await tile(page, "hosts").locator('[data-action="details"]').click();
  await expect(page.locator(".db-detail-resource")).toHaveCount(4);
  await expect(action(page, "tileFull")).toHaveText("Full view");
  await action(page, "tileFull").click();
  await expect(page.locator("#dashboardOverview")).toBeHidden();
  await expect(page.locator("#cockpit")).toBeVisible();
  await expect(page.locator("#reposPanel")).toBeHidden();
  await expect.poll(() => page.evaluate(() => window.__CALLS__.filter(c => c.command === "cockpit").at(-1).args))
    .toEqual({ width: expect.any(Number), tile: "overview-hosts" });
  // The toolbar's Full view toggle leaves a tile's Full view as it leaves Solador's.
  await page.locator('.db-chrome [data-action="allPanels"]').click();
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

// The Repos tile scoped to Healthy repos; Rust's note for it, word for word.
const HEALTHY = { title: "Healthy Repos", note: "Scoped to Healthy repos. Totals still cover all of GitHub Repos." };
function healthyRepos(view) {
  Object.assign(view.layout.tiles.find((t) => t.source === "ghWorkflows"), { scope: "healthy", title: HEALTHY.title });
}
const detailFull = (page) =>
  page.locator("#dashboardInspector").getByRole("button", { name: "Full view", exact: true });
const lastArgs = (page, command) =>
  page.evaluate((command) => window.__CALLS__.filter((c) => c.command === command).at(-1)?.args, command);
const callCount = (page, command) =>
  page.evaluate((command) => window.__CALLS__.filter((c) => c.command === command).length, command);
async function openTileFull(page, tileId) {
  await page.locator(`[data-tile="${tileId}"] .db-tile-footer [data-action="details"]`).click();
  await detailFull(page).click();
  await expect(page.locator("#cockpitView")).toBeVisible();
}

test("Detail's Full view opens the tile's own scoped panel", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL, false, healthyRepos);
  await tile(page, "ghWorkflows").locator('.db-tile-footer [data-action="details"]').click();
  await detailFull(page).click();
  await expect(page.locator("#dashboardOverview")).toBeHidden();
  await expect(page.locator("#cockpitView")).toHaveAttribute("data-detail-source", "ghWorkflows");
  await expect(page.locator("#reposPanel")).toBeVisible();
  await expect(page.locator("#runnersPanel")).toBeHidden();
  expect(await page.evaluate(() => window.__CALLS__)).toContainEqual({ command: "repos", args: { tile: "overview-ghWorkflows" } });
  // Only the tile's own source is asked for the tile.
  expect(await lastArgs(page, "runners")).toEqual({});
  await expect(page.locator("#reposTitle")).toHaveText(HEALTHY.title);
  await expect(page.locator("#cockpitView .db-full-note")).toHaveCount(1);
  await expect(page.locator("#reposPanel > .panel-hdr + .db-full-note")).toHaveText(HEALTHY.note);
  // The panel's own poll repaints its header; the tile's name stays. Read in
  // the same task as that repaint, so the overview's next read cannot be what
  // puts the name back.
  expect(await page.evaluate(async () => {
    await window.__SOLADOR_GITHUB_TEST__.refresh();
    return document.getElementById("reposTitle").textContent;
  })).toBe(HEALTHY.title);
  await expect(page.locator("#reposPanel > .panel-hdr + .db-full-note")).toHaveText(HEALTHY.note);
});

test("Escape returns to the overview and restores the panel", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL, false, healthyRepos);
  const back = tile(page, "ghWorkflows").locator('.db-tile-footer [data-action="details"]');
  for (const blur of [false, true]) {
    await openTileFull(page, "overview-ghWorkflows");
    await expect(page.locator("#reposTitle")).toHaveText(HEALTHY.title);
    // From a control in the view, and from nothing focused at all.
    if (blur) await page.evaluate(() => document.activeElement.blur());
    await page.keyboard.press("Escape");
    await expect(page.locator("#dashboardOverview")).toBeVisible();
    await expect(page.locator("#cockpitView")).toBeHidden();
    await expect(page.locator("#reposTitle")).toHaveText("GitHub Repos");
    await expect(page.locator(".db-full-note")).toHaveCount(0);
    await expect(back).toBeFocused();
  }
  // Solador's own Full view has no tile to leave; Escape there changes nothing.
  await page.locator('.db-chrome [data-action="allPanels"]').click();
  await expect(page.locator("#cockpitView")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.locator("#cockpitView")).toBeVisible();
});

test("Detail from an attention chip opens the source-wide panel", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL, false, healthyRepos);
  await action(page, "attention").filter({ hasText: "GitHub Repos" }).click();
  const before = await callCount(page, "repos");
  await detailFull(page).click();
  await expect(page.locator("#cockpitView")).toHaveAttribute("data-detail-source", "ghWorkflows");
  await expect(page.locator("#reposPanel")).toBeVisible();
  await expect.poll(() => callCount(page, "repos")).toBeGreaterThan(before);
  expect(await lastArgs(page, "repos")).toEqual({});
  await expect(page.locator("#reposTitle")).toHaveText("GitHub Repos");
  await expect(page.locator(".db-full-note")).toHaveCount(0);
});

test("a tile that disappears while its Full view is open returns to the overview", async ({ page, baseURL }) => {
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const view = await openDashboard(page, baseURL, false, healthyRepos);
  await openTileFull(page, "overview-ghWorkflows");
  await expect(page.locator("#reposTitle")).toHaveText(HEALTHY.title);
  // Hidden elsewhere: the next overview read no longer has the tile. Hold that
  // read, so the panel's own poll lands first and is refused.
  await page.evaluate((layout) => {
    const next = structuredClone(layout);
    next.tiles.find((t) => t.id === "overview-ghWorkflows").hidden = true;
    window.__SET_LAYOUT__(next);
    window.__HOLD_READ__ = true;
  }, view.layout);
  await expect.poll(() => page.evaluate(() => typeof window.__RELEASE_READ__)).toBe("function");
  await page.evaluate(() => window.__SOLADOR_GITHUB_TEST__.refresh());
  expect(await lastArgs(page, "repos")).toEqual({ tile: "overview-ghWorkflows" });
  // The refusal paints nothing under the tile's name.
  await expect(page.locator("#reposTitle")).toHaveText(HEALTHY.title);
  await expect(page.locator("#cockpitView")).not.toContainText("That tile");
  await page.evaluate(() => window.__RELEASE_READ__());
  await expect(page.locator("#dashboardOverview")).toBeVisible();
  await expect(page.locator("#cockpitView")).toBeHidden();
  await expect(page.locator(".db-action-error")).toHaveText("That tile is no longer on the overview.");
  await expect(page.locator(".db-live-note")).toHaveText("That tile is no longer on the overview.");
  await expect(page.locator("#reposTitle")).toHaveText("GitHub Repos");
  await expect(page.locator(".db-full-note")).toHaveCount(0);
  // Detail is still open on the tile. Out to Solador's Full view and back
  // clears the status; then Detail's Full view must say it again, and ask nothing.
  await page.locator('.db-chrome [data-action="allPanels"]').click();
  await expect(page.locator("#cockpitView")).toBeVisible();
  await page.locator('.db-chrome [data-action="allPanels"]').click();
  await expect(page.locator(".db-live-note")).toHaveText("");
  const asked = await callCount(page, "repos");
  await detailFull(page).click();
  await expect(page.locator(".db-live-note")).toHaveText("That tile is no longer on the overview.");
  await expect(page.locator("#dashboardOverview")).toBeVisible();
  expect(await callCount(page, "repos")).toBe(asked);
  expect(errors).toEqual([]);
});

test("two tiles of one source each open their own scope", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL, false, (view) => {
    healthyRepos(view);
    const tiles = view.layout.tiles, repos = tiles.find((t) => t.source === "ghWorkflows");
    tiles.find((t) => t.source === "services").hidden = true;
    tiles.push({ ...repos, id: "repos-attention", title: "Repos Needing Attention", scope: "attention" });
  });
  await openTileFull(page, "overview-ghWorkflows");
  await expect(page.locator("#reposTitle")).toHaveText(HEALTHY.title);
  await page.keyboard.press("Escape");
  await expect(page.locator("#dashboardOverview")).toBeVisible();
  await openTileFull(page, "repos-attention");
  await expect(page.locator("#reposTitle")).toHaveText("Repos Needing Attention");
  await expect(page.locator(".db-full-note")).toHaveText("Scoped to Needs Attention. Totals still cover all of GitHub Repos.");
  await page.evaluate(() => refreshPanels());
  const asked = await page.evaluate(() =>
    window.__CALLS__.filter((c) => c.command === "repos" && c.args?.tile).map((c) => c.args.tile));
  const firstB = asked.indexOf("repos-attention");
  expect(firstB).toBeGreaterThan(0);
  expect(asked.slice(0, firstB)).toContain("overview-ghWorkflows");
  expect(asked.slice(firstB)).not.toContain("overview-ghWorkflows");
});

test("a runner group carries into the tile's Full view", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL, true);
  await tile(page, "ghRunners").getByRole("button", { name: /LINUX · ARM64/ }).click();
  await detailFull(page).click();
  await expect(page.locator("#runnersPanel")).toBeVisible();
  await expect.poll(() => lastArgs(page, "runners")).toEqual({ tile: "overview-ghRunners" });
  await expect(page.locator("#runnersBody .gh-runner-filter")).toContainText("LINUX · ARM64");
  await expect(page.locator("#runnersBody .gh-runner-name")).toHaveText(["ubu-01", "ubu-1", "ubu-spare"]);
  await expect(page.locator("#runnersTitle")).toHaveText("Runners");
  // The group filter repaints the panel before the tile's name is painted;
  // leaving still restores the panel's own title.
  await page.keyboard.press("Escape");
  await expect(page.locator("#dashboardOverview")).toBeVisible();
  await expect(page.locator("#runnersTitle")).toHaveText("GitHub Runners");
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
  // Who opened the PRs is the count's tooltip, in Rust's words, and absent
  // over an unknown count.
  const prsCount = (id) => item(id).locator('.db-count[data-header="PRS"]');
  await expect(prsCount("acme/gadget")).toHaveAttribute("title", "5 open PRs · 3 from Dependabot");
  await expect(prsCount("acme/flywheel")).toHaveAttribute("title", "1 open PR · none from Dependabot");
  await expect(prsCount("acme/cogwheel")).not.toHaveAttribute("title");
  await expect(item("acme/gadget")).toHaveAttribute("aria-label", /5 open PRs · 3 from Dependabot/);
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
  await expect(row.locator(".db-extra-metrics .db-muted")).toHaveText(["DEPENDABOT", "REMOTE", "LOCAL", "WT", "JOBS", "LONGEST"]);
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

test("a repo waiting on approval reads Waiting, pulses, and opens GitHub at Rust's URL", async ({ page, baseURL }) => {
  const model = await openDashboard(page, baseURL);
  const rows = model.sources.find((s) => s.id === "ghWorkflows").rows;
  const waiting = rows.find((r) => r.value === "Waiting");
  expect(waiting.approvalUrl).toMatch(/^https:\/\/github\.com\/[^/]+\/[^/]+\/actions\/runs\/\d+$/);
  const item = (label) => tile(page, "ghWorkflows").locator(".db-item", { has: page.locator(".db-item-name", { hasText: new RegExp(`^${label}$`) }) });
  const row = item(waiting.label);
  await expect(row.locator(".db-value")).toHaveText("Waiting");
  await expect(row).toHaveAttribute("title", /Approval needed · acme\/flywheel · longest running/);
  // The label fits its column at every width the tile renders.
  for (const width of [375, 768, 1440]) {
    await page.setViewportSize({ width, height: 950 });
    const fits = await row.locator(".db-value").evaluate((el) => el.scrollWidth <= el.clientWidth);
    expect(fits, `${width}px`).toBe(true);
  }
  // Only the waiting row pulses.
  await expect(row.locator(".db-dot")).toHaveClass(/db-dot-blink/);
  const running = rows.find((r) => r.value === "Running");
  await expect(item(running.label).locator(".db-dot")).not.toHaveClass(/db-dot-blink/);
  expect(await row.locator(".db-dot").evaluate((el) => getComputedStyle(el).animationName)).toBe("gh-blink");
  expect(await item(running.label).locator(".db-dot").evaluate((el) => getComputedStyle(el).animationName)).toBe("none");
  await page.emulateMedia({ reducedMotion: "reduce" });
  expect(await row.locator(".db-dot").evaluate((el) => getComputedStyle(el).animationName)).toBe("none");

  // Clicking it hands Rust's string, byte for byte, to the opener, not to Detail.
  await row.click();
  await expect(page.locator("#dashboardInspector")).toBeHidden();
  const opened = () => page.evaluate(() => window.__CALLS__.filter((c) => c.command === "plugin:opener|open_url"));
  expect(await opened()).toEqual([{ command: "plugin:opener|open_url", args: { url: waiting.approvalUrl } }]);

  // Every other row still opens Detail and opens nothing.
  await item(running.label).click();
  await expect(page.locator("#dashboardInspector")).toBeVisible();
  expect(await opened()).toHaveLength(1);
});

// The poll cadences moved from Settings into the Configure of the tile(s) they
// feed (#614, #615). Seven sources carry them, Usage three (its Claude
// rollups, metered providers and plans' limit windows); GitHub's one cadence is
// on both its Repos and Runners tiles; Machines carries none.
const CADENCE_SOURCES = ["containers", "ghWorkflows", "ghRunners", "services", "claudeUsage", "azureCost", "sentryCrons"];
const cadencesOf = (model, id) => model.sources.find(s => s.id === id).cadences;
async function configureCadence(page, source, panel = null) {
  await page.locator('#dashboardOverview [data-action="edit"]').click();
  await page.locator('#dashboardOverview [data-action="catalog"]').click();
  await page.locator(`[data-action="add"][data-id="${source}"]`).click();
  return page.locator(panel ? `[data-cadence="${panel}"]` : "[data-cadence]");
}

test("each source's Configure shows its poll cadence with the floor, the reason and the shared note (#614)", async ({ page, baseURL }) => {
  const model = await openDashboard(page, baseURL);
  expect(CADENCE_SOURCES.every(id => cadencesOf(model, id)?.length)).toBe(true);
  expect(cadencesOf(model, "hosts")).toBeUndefined();
  expect(cadencesOf(model, "claudeUsage").map(c => c.id)).toEqual(["claude_rollups", "usage_providers", "plan_usage"]);
  expect(cadencesOf(model, "ghWorkflows").map(c => c.id)).toEqual(["github"]);
  expect(cadencesOf(model, "ghRunners").map(c => c.id)).toEqual(["github"]);
  expect(cadencesOf(model, "services").map(c => c.id)).toEqual(["services"]);
  const cadences = CADENCE_SOURCES.flatMap(id => cadencesOf(model, id).map(c => [id, c]));
  expect(cadences.some(([, c]) => c.configured) && cadences.some(([, c]) => !c.configured),
    "the fixture needs both renderings").toBe(true);
  for (const [id, c] of cadences) {
    const section = await configureCadence(page, id, c.id);
    await expect(page.locator("[data-cadence]")).toHaveCount(cadencesOf(model, id).length);
    await expect(section.locator("legend")).toHaveText(c.heading);
    await expect(section.locator("input")).toHaveValue(String(c.value));
    await expect(section.locator("input")).toHaveAttribute("min", String(c.min));
    await expect(section.locator(".db-cadence-status")).toHaveText(c.status);
    await expect(section.locator(".db-cadence-help")).toHaveText(c.help);
    await expect(section.locator(".db-cadence-shared")).toHaveText(c.shared);
    await expect(section.locator(".db-cadence-shared")).toContainText("applies to every");
    await expect(section.locator('[data-action="cadence-reset"]')).toBeEnabled({ enabled: c.configured });
    await page.reload();
  }
  await page.locator('#dashboardOverview [data-action="edit"]').click();
  await tile(page, "hosts").locator('[data-action="configure"]').click();
  await expect(page.locator("[data-cadence]")).toHaveCount(0);
});

test("a valid cadence saves through panel_interval_save on the click, never through the tile's Apply (#614)", async ({ page, baseURL }) => {
  const model = await openDashboard(page, baseURL);
  const [c] = cadencesOf(model, "containers");
  const section = await configureCadence(page, "containers");
  await section.locator("input").fill("45");
  await section.locator('[data-action="cadence-apply"]').click();
  await expect.poll(() => commandArgs(page, "panel_interval_save"))
    .toEqual([{ panel: c.id, secs: 45, width: expect.any(Number) }]);
  await expect(section.locator(".db-cadence-status")).toHaveText("Set to 45s.");
  await section.locator('[data-action="cadence-reset"]').click();
  await expect.poll(() => commandArgs(page, "panel_interval_clear"))
    .toEqual([{ panel: c.id, width: expect.any(Number) }]);
  expect(await commandArgs(page, "dashboard_save")).toEqual([]);
});

test("a below-floor cadence is refused with the floor's reason, not clamped, and the typed value stays (#614)", async ({ page, baseURL }) => {
  const model = await openDashboard(page, baseURL);
  const [c] = cadencesOf(model, "azureCost");
  const section = await configureCadence(page, "azureCost");
  await section.locator("input").fill("30");
  await section.locator('[data-action="cadence-apply"]').click();
  // Beside the section, never inside the button it came from.
  const error = page.locator("[data-cadence] > .db-action-error");
  await expect(error).toBeVisible();
  await expect(error).toContainText(`${c.min}s floor`);
  await expect(error).toContainText(c.help);
  await expect(page.locator("[data-cadence] button .db-action-error")).toHaveCount(0);
  await expect(section.locator('[data-action="cadence-apply"]')).toHaveAccessibleName(c.applyLabel);
  // Not clamped to the floor, and the saved state is untouched.
  await expect(section.locator("input")).toHaveValue("30");
  await expect(section.locator(".db-cadence-status")).toHaveText(c.status);
});

test("a typed cadence survives the dashboard's repaint (#614)", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL);
  const section = await configureCadence(page, "containers");
  await section.locator("input").fill("77");
  const reads = () => page.evaluate(() => window.__CALLS__.filter(c => c.command === "dashboard_view").length);
  const before = await reads();
  await expect.poll(reads, { timeout: 8000 }).toBeGreaterThan(before + 1);
  await expect(section.locator("input")).toHaveValue("77");
});

test("a negative cadence is sent as 0 so Rust refuses it in the floor's words (#614)", async ({ page, baseURL }) => {
  await openDashboard(page, baseURL);
  const section = await configureCadence(page, "containers");
  await section.locator("input").fill("-5");
  await section.locator('[data-action="cadence-apply"]').click();
  await expect.poll(() => commandArgs(page, "panel_interval_save"))
    .toEqual([{ panel: "containers", secs: 0, width: expect.any(Number) }]);
});

test("a failed cadence save says so beside the section, keeps the typed value and the status (#614)", async ({ page, baseURL }) => {
  const model = await openDashboard(page, baseURL);
  const [c] = cadencesOf(model, "containers");
  await page.evaluate(() => { window.__FAIL_CADENCE__ = true; });
  const section = await configureCadence(page, "containers");
  await section.locator("input").fill("45");
  await section.locator('[data-action="cadence-apply"]').click();
  await expect(page.locator("[data-cadence] > .db-action-error")).toHaveText("Could not save the poll cadence.");
  await expect(page.locator("[data-cadence] button .db-action-error")).toHaveCount(0);
  await expect(section.locator("input")).toHaveValue("45");
  await expect(section.locator(".db-cadence-status")).toHaveText(c.status);
});

test("one GitHub cadence is on both of its tiles, and a save from either moves both (#615)", async ({ page, baseURL }) => {
  const model = await openDashboard(page, baseURL);
  const [repos] = cadencesOf(model, "ghWorkflows");
  const [runners] = cadencesOf(model, "ghRunners");
  expect(runners).toEqual(repos);
  expect(repos.shared).toContain("every GitHub Repos tile and every Runners tile");
  for (const [saveIn, readIn] of [["ghWorkflows", "ghRunners"], ["ghRunners", "ghWorkflows"]]) {
    await page.goto(baseURL);
    const section = await configureCadence(page, saveIn, "github");
    await section.locator("input").fill("300");
    await section.locator('[data-action="cadence-apply"]').click();
    await expect.poll(() => commandArgs(page, "panel_interval_save"))
      .toEqual([{ panel: "github", secs: 300, width: expect.any(Number) }]);
    await expect(section.locator(".db-cadence-status")).toHaveText("Set to 300s.");
    // The other tile, opened in the same session, now carries the same value:
    // one key, not a copy per tile.
    await page.locator('#dashboardOverview [data-action="catalog"]').click();
    await page.locator(`[data-action="add"][data-id="${readIn}"]`).click();
    const other = page.locator('[data-cadence="github"]');
    await expect(other).toHaveCount(1);
    await expect(other.locator("input")).toHaveValue("300");
    await expect(other.locator(".db-cadence-status")).toHaveText("Set to 300s.");
  }
});

test("the Usage tile's three cadences each save, and refuse, on their own box", async ({ page, baseURL }) => {
  const model = await openDashboard(page, baseURL);
  const [rollups, providers, plans] = cadencesOf(model, "claudeUsage");
  expect(rollups.id).toBe("claude_rollups");
  await configureCadence(page, "claudeUsage");
  await expect(page.locator("[data-cadence]")).toHaveCount(3);
  const providerBox = page.locator(`[data-cadence="${providers.id}"]`);
  const planBox = page.locator(`[data-cadence="${plans.id}"]`);
  // Enter applies the box it was pressed in, not the first one on the page.
  await planBox.locator("input").fill("600");
  await planBox.locator("input").press("Enter");
  await expect.poll(() => commandArgs(page, "panel_interval_save"))
    .toEqual([{ panel: plans.id, secs: 600, width: expect.any(Number) }]);
  await expect(planBox.locator(".db-cadence-status")).toHaveText("Set to 600s.");
  await expect(providerBox.locator(".db-cadence-status")).toHaveText(providers.status);
  // A refusal lands beside the box it came from, never its neighbour.
  await planBox.locator("input").fill("30");
  await planBox.locator('[data-action="cadence-apply"]').click();
  await expect(planBox.locator(":scope > .db-action-error")).toContainText(`${plans.min}s floor`);
  await expect(providerBox.locator(".db-action-error")).toHaveCount(0);
});
