import { test, expect } from '@playwright/test';
import { dashboardWithTiles, openTileFull } from './tile-full.js';

test.beforeEach(async ({ page }) => {
  page.statusErrors = [];
  page.on('pageerror', error => page.statusErrors.push(error.message));
  page.on('console', message => {
    if (message.type() === 'error' && /content security policy|refused to (apply|load)/i.test(message.text())) page.statusErrors.push(message.text());
  });
});
test.afterEach(async ({ page }) => { expect(page.statusErrors).toEqual([]); });

const files = { cockpit: 'cockpit', dashboard_view: 'dashboard', containers: 'containers', repos: 'repos', runners: 'runners', services: 'services', crons: 'crons', usage: 'usage', azure_cost: 'azure', openclaw: 'openclaw' };
// `sources` replaces the overview's tiles with one `all`-scoped tile per source
// (ids from tile-full.js), for a test that enters a tile's Full view.
async function open(page, baseURL, sources = null) {
  const frames = {};
  for (const [command, file] of Object.entries(files)) {
    frames[command] = await (await fetch(`${baseURL}/sample-${file}.json`)).json();
  }
  if (sources) frames.dashboard_view = dashboardWithTiles(frames.dashboard_view, sources);
  await page.addInitScript(frames => {
    window.framesForTest = frames;
    window.dashboardReads = 0;
    window.__TAURI__ = { core: { invoke: async command => {
      if (command === 'dashboard_view') {
        window.dashboardReads += 1;
        if (window.failDashboard) throw new Error('Connection interrupted');
      }
      return window.framesForTest[command] ?? null;
    } } };
  }, frames);
  await page.goto('/index.html');
  await expect(page.locator('.db-tile').first()).toBeVisible();
  return frames;
}
const bounds = (page, selector) => page.locator(selector).evaluateAll(elements => elements.map(el => {
  const r = el.getBoundingClientRect();
  return { x:r.x, y:r.y + scrollY, width:r.width, height:r.height };
}));
// app/ui/layout-stability.js records a panel's height floor on the animation
// frame after its content changes, so content replaced before that frame has
// run is measured without the floor the previous content would have set.
// Awaiting two frames (the second covers the pass the first one's min-height
// writes trigger) is the condition the app guarantees: the floors are in place.
const layoutSettled = page => page.evaluate(() => new Promise(done =>
  requestAnimationFrame(() => requestAnimationFrame(done))));
async function changeDashboard(page, model) {
  await page.evaluate(model => { window.framesForTest.dashboard_view = model; }, model);
  await expect(page.locator('.db-value').first()).toHaveText(model.tiles[0].rows[0].value);
}

for (const width of [375, 1024, 1600]) {
  test(`overview stays anchored through status and attention transitions at ${width}px`, async ({ page, baseURL }) => {
    await page.setViewportSize({ width, height:900 });
    const frames = await open(page, baseURL);
    const initial = structuredClone(frames.dashboard_view);
    initial.attention = [];
    initial.tiles[0].rows[0].value = 'Connected';
    await changeDashboard(page, initial);
    await expect(page.locator('.db-attention-items button')).toHaveCount(0);
    const subtitle = page.locator('.db-grid .db-tile-note').first();
    await expect(subtitle).toBeVisible();
    expect(await page.locator('.db-tile').first().evaluate(tile =>
      tile.querySelector('.db-tile-rows').getBoundingClientRect().top -
      tile.querySelector('.db-tile-note').getBoundingClientRect().bottom,
    )).toBeLessThanOrEqual(8);
    const before = await bounds(page, '.db-grid, .db-tile, .db-tile-footer, .db-host-stats, .db-item');
    const failed = structuredClone(initial);
    failed.attention = failed.sources.map(s => ({ source:s.id, label:`${s.title} · 999 need attention`, color:'#ff9384' }));
    for (const tile of failed.tiles) {
      tile.warnings = [{ text:'Refresh failed: ' + 'long diagnostic explanation '.repeat(12), color:'#ff9384' }];
      for (const row of tile.rows) {
        row.value = 'Stale / unavailable';
        for (const metric of row.metrics || []) metric.value = null;
      }
    }
    await changeDashboard(page, failed);
    expect(await bounds(page, '.db-grid, .db-tile, .db-tile-footer, .db-host-stats, .db-item')).toEqual(before);
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
    const warnings = page.locator('.db-grid .db-warnings').first();
    await expect(subtitle).toBeHidden();
    await expect(warnings).toBeVisible();
    await expect(page.locator('.db-grid .db-tile-title').first()).toHaveAccessibleDescription('All resources · Summary');
    await warnings.focus();
    await expect(warnings).toBeFocused();
    await expect(warnings).toHaveAttribute('title', failed.tiles[0].warnings[0].text);
    expect(await warnings.evaluate(el => getComputedStyle(el).overflowX)).toBe('hidden');
    const reads = await page.evaluate(() => window.dashboardReads);
    await expect.poll(() => page.evaluate(() => window.dashboardReads)).toBeGreaterThan(reads);
    await expect(warnings).toBeFocused();
    await expect(warnings).toHaveAttribute('title', failed.tiles[0].warnings[0].text);
    await changeDashboard(page, initial);
    await expect(subtitle).toBeVisible();
    expect(await bounds(page, '.db-grid, .db-tile, .db-tile-footer, .db-host-stats, .db-item')).toEqual(before);
  });
}

for (const width of [375, 1024, 1600]) {
  test(`the attention strip is one fixed-height row at ${width}px`, async ({ page, baseURL }) => {
    await page.setViewportSize({ width, height:900 });
    const frames = await open(page, baseURL);
    const strip = page.locator('.db-attention');
    const model = structuredClone(frames.dashboard_view);
    const heights = [];
    for (const attention of [[], model.attention.slice(-2, -1), model.attention]) {
      model.attention = attention;
      model.tiles[0].rows[0].value = `step ${heights.length}`;
      await changeDashboard(page, model);
      await expect(page.locator('.db-attention-items button')).toHaveCount(attention.length);
      heights.push(await strip.evaluate(el => el.getBoundingClientRect().height));
      expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
    }
    expect(new Set(heights).size).toBe(1);
    if (width === 375) {
      // Every source at once overflows a phone-width row: the chips scroll
      // inside it, on their own line under the title and note.
      const row = page.locator('.db-attention-items');
      expect(await row.evaluate(el => el.scrollWidth > el.clientWidth)).toBe(true);
      const [frame, head, chips, note] = await page.locator('.db-attention, .db-attention > h2, .db-attention-items, .db-attention-note')
        .evaluateAll(els => els.map(el => el.getBoundingClientRect()));
      expect(chips.y).toBeGreaterThanOrEqual(head.y + head.height);
      expect(chips.y).toBeGreaterThanOrEqual(note.y + note.height);
      expect(Math.round(chips.width)).toBe(Math.round(frame.width - 24 - 2));
    }
    // One item: packed at the start of the row and sized to its label — not
    // parked in a reserved per-source slot.
    const [one] = model.attention = frames.dashboard_view.attention.slice(-2, -1);
    model.tiles[0].rows[0].value = 'single';
    await changeDashboard(page, model);
    const [row, chip] = await page.locator('.db-attention-items, .db-attention-items button')
      .evaluateAll(els => els.map(el => el.getBoundingClientRect()));
    await expect(page.locator('.db-attention-items button')).toHaveText(one.label);
    expect(Math.round(row.x)).toBe(Math.round(chip.x));
    expect(chip.width).toBeLessThan(row.width / 2);
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
  });
}

test('a long repo name ellipsizes before the count strip does, and the numbers stay in columns', async ({ page, baseURL }) => {
  await page.setViewportSize({ width:1024, height:900 });
  const frames = await open(page, baseURL);
  const model = structuredClone(frames.dashboard_view);
  const repos = model.tiles.find(t => t.source === 'ghWorkflows');
  repos.rows[0].label = 'mission-control-tower-and-then-some';
  model.tiles[0].rows[0].value = 'long name';
  await changeDashboard(page, model);
  const tile = page.locator('.db-tile[data-tile="overview-ghWorkflows"]');
  await expect(tile.locator('.db-item-name').first()).toHaveText(repos.rows[0].label);
  const strips = await tile.locator('.db-item .db-row-counts').evaluateAll(els => els.map(el => ({
    clipped: el.scrollWidth > el.clientWidth,
    edges: [...el.querySelectorAll('strong')].map(n => Math.round(n.getBoundingClientRect().right)),
  })));
  expect(strips.length).toBe(repos.rows.length);
  for (const s of strips) {
    expect(s.clipped).toBe(false);
    expect(s.edges).toEqual(strips[0].edges);
  }
  const name = tile.locator('.db-item-name').first();
  expect(await name.evaluate(el => el.scrollWidth > el.clientWidth)).toBe(true);
});

for (const presentation of ['summary', 'detailed']) test(`all overview sources retain their ${presentation} footprint when status filters empty their rows`, async ({ page, baseURL }) => {
  const frames = await open(page, baseURL);
  const next = structuredClone(frames.dashboard_view);
  next.tiles = next.sources.map(source => ({ ...next.tiles[0], id:source.id, source:source.id, title:source.title, presentation, rows:source.rows.slice(0, 5), warnings:source.warnings }));
  next.layout.tiles = next.tiles.map(({id, source, title}) => ({ id, source, title, presentation, width:'medium', scope:'all', hidden:false }));
  next.layout.revision += 1;
  await page.evaluate(model => { window.framesForTest.dashboard_view = model; }, next);
  await expect(page.locator('.db-tile')).toHaveCount(next.sources.length);
  const before = await bounds(page, '.db-tile, .db-tile-footer');
  next.tiles.forEach(t => { t.rows = []; t.warnings = []; });
  await page.evaluate(model => { window.framesForTest.dashboard_view = model; }, next);
  await expect(page.locator('.db-grid .db-item')).toHaveCount(0);
  expect(await bounds(page, '.db-tile, .db-tile-footer')).toEqual(before);
});

// Each panel in its own tile's Full view: [source, command, fixture file].
const panelTiles = [
  ['containers', 'containers', 'containers'], ['ghWorkflows', 'repos', 'repos'], ['ghRunners', 'runners', 'runners'],
  ['services', 'services', 'services'], ['sentryCrons', 'crons', 'crons'], ['claudeUsage', 'usage', 'usage'],
  ['azureCost', 'azure_cost', 'azure'], ['openclawAgents', 'openclaw', 'openclaw'],
];
for (const [source, command, file] of panelTiles) {
  test(`${source}'s Full view retains its footprint through failure, empty and recovery frames`, async ({ page, baseURL }) => {
    const frames = await open(page, baseURL, [source]);
    await openTileFull(page, source);
    await page.evaluate(() => refreshPanels());
    await layoutSettled(page);
    const before = await bounds(page, '.panel');
    // The open tile's panel is laid out; a hidden one would hold still trivially.
    expect(before.some(b => b.width > 0 && b.height > 0), "the tile's own panel is measured").toBe(true);
    const variants = { empty:['containers','repos','runners','services','crons','usage','azure','openclaw'], error:['crons','azure','openclaw'], stale:['crons','usage','azure'] };
    for (const suffix of ['empty', 'error', 'stale']) {
      const changed = structuredClone(frames);
      if (variants[suffix].includes(file)) changed[command] = await (await fetch(`${baseURL}/sample-${file}-${suffix}.json`)).json();
      await page.evaluate(async changed => { window.framesForTest = changed; await refreshPanels(); }, changed);
      await layoutSettled(page);
      expect(await bounds(page, '.panel'), suffix).toEqual(before);
    }
    await page.evaluate(async frames => { window.framesForTest = frames; await refreshPanels(); }, frames);
    await layoutSettled(page);
    expect(await bounds(page, '.panel')).toEqual(before);
  });
}

test('a disconnected detailed host hides old readings without moving the next panel', async ({ page, baseURL }) => {
  await open(page, baseURL, ['hosts']);
  await openTileFull(page, 'hosts');
  const live = await (await fetch(`${baseURL}/sample.json`)).json();
  const down = await (await fetch(`${baseURL}/sample-unreachable.json`)).json();
  await page.evaluate(async model => { window.framesForTest.cockpit = model; await refreshCockpit(); }, live);
  await expect.poll(() => page.locator('.card').first().evaluate(el => parseFloat(el.style.minHeight) === el.getBoundingClientRect().height)).toBe(true);
  const before = await bounds(page, '.card, #panelRows');
  await page.evaluate(async model => { window.framesForTest.cockpit = model; await refreshCockpit(); }, down);
  await expect(page.locator('.card-down')).toBeVisible();
  await expect(page.locator('.cpuChart')).toBeHidden();
  expect(await bounds(page, '.card, #panelRows')).toEqual(before);
  await page.evaluate(async model => { window.framesForTest.cockpit = model; await refreshCockpit(); }, live);
  expect(await bounds(page, '.card, #panelRows')).toEqual(before);
});

test('a host first connecting after a cold failure keeps the surrounding layout anchored', async ({ page, baseURL }) => {
  await open(page, baseURL, ['hosts']);
  await openTileFull(page, 'hosts');
  const live = await (await fetch(`${baseURL}/sample.json`)).json();
  const down = await (await fetch(`${baseURL}/sample-unreachable.json`)).json();
  // Remove existing cards so this is a cold failure, not cached live markup.
  await page.evaluate(async down => {
    window.framesForTest.cockpit = { ...down, hosts:[] };
    await refreshCockpit();
    window.framesForTest.cockpit = down;
    await refreshCockpit();
  }, down);
  await expect.poll(() => page.locator('.card').first().evaluate(el => parseFloat(el.style.minHeight) === el.getBoundingClientRect().height)).toBe(true);
  const before = await bounds(page, '.card, #panelRows');
  await page.evaluate(async live => { window.framesForTest.cockpit = live; await refreshCockpit(); }, live);
  expect(await bounds(page, '.card, #panelRows')).toEqual(before);
});

test('the Machines tile warns when every remote host is unreachable, and clears once one answers', async ({ page, baseURL }) => {
  const frames = await open(page, baseURL);
  const hostsTile = () => page.locator('.db-tile[data-source="hosts"]');
  const warningText = "All remote hosts unreachable — is this machine's network or VPN up?";
  const model = structuredClone(frames.dashboard_view);
  model.tiles[0].rows[0].value = 'baseline';
  await changeDashboard(page, model);
  await expect(hostsTile().locator('.db-warning')).toHaveCount(0);
  const before = await bounds(page, '.db-grid, .db-tile, .db-tile-footer, .db-host-stats, .db-item');

  // Every remote host down at once (#446): the Machines tile's fixed-height
  // subtitle carries the hint, without moving anything around it.
  // `all_remote_hosts_unreachable` itself is exercised by the Rust unit tests
  // beside `host_rows`; this frame only proves the frontend paints the
  // warning `dashboard_view` would hand it and keeps the grid anchored.
  const down = structuredClone(model);
  down.tiles.find(t => t.source === 'hosts').warnings = [{ text: warningText, color: '#e0a03a' }];
  down.attention = [...down.attention, { source: 'hosts', label: 'Machines · check readings', color: '#e0a03a' }];
  down.tiles[0].rows[0].value = 'unreachable';
  await page.evaluate(down => { window.framesForTest.dashboard_view = down; }, down);
  await expect(hostsTile().locator('.db-warning')).toHaveText(warningText);
  expect(await bounds(page, '.db-grid, .db-tile, .db-tile-footer, .db-host-stats, .db-item')).toEqual(before);

  // The next frame in which any remote host answers clears it — recomputed
  // per frame, with no state carried forward.
  const recovered = structuredClone(down);
  recovered.tiles.find(t => t.source === 'hosts').warnings = [];
  recovered.attention = recovered.attention.filter(a => a.source !== 'hosts');
  recovered.tiles[0].rows[0].value = 'recovered';
  await page.evaluate(recovered => { window.framesForTest.dashboard_view = recovered; }, recovered);
  await expect(hostsTile().locator('.db-warning')).toHaveCount(0);
  expect(await bounds(page, '.db-grid, .db-tile, .db-tile-footer, .db-host-stats, .db-item')).toEqual(before);
});

test('live resource details keep the inspector and its actions anchored', async ({ page, baseURL }) => {
  const frames = await open(page, baseURL);
  await page.locator('.db-tile .db-item').first().click();
  const before = await bounds(page, '.db-inspector, .db-inspector > .db-form-actions, .db-grid');
  const next = structuredClone(frames.dashboard_view);
  const hosts = next.sources.find(s => s.id === 'hosts');
  hosts.rows = [];
  hosts.trailing = '';
  hosts.warnings = [{text:'Long diagnostic '.repeat(40),color:'#ff9384'}];
  await page.evaluate(next => { window.framesForTest.dashboard_view = next; }, next);
  await expect(page.locator('.db-detail-resource')).toHaveCount(0);
  expect(await bounds(page, '.db-inspector, .db-inspector > .db-form-actions, .db-grid')).toEqual(before);
});

test('live hidden previews keep Restore and Remove anchored', async ({ page, baseURL }) => {
  const frames = await open(page, baseURL);
  const next = structuredClone(frames.dashboard_view);
  const hidden = next.tiles.shift();
  next.hiddenTiles = [hidden];
  next.layout.tiles.find(t => t.id === hidden.id).hidden = true;
  next.layout.revision += 1;
  await page.evaluate(next => { window.framesForTest.dashboard_view = next; }, next);
  await expect(page.locator('.db-tile')).toHaveCount(next.tiles.length);
  await page.locator('[data-action="edit"]').click();
  await page.locator('.db-hidden-count').click();
  const before = await bounds(page, '.db-inspector, .db-hidden-preview .db-form-actions');
  hidden.rows = [];
  hidden.warnings = [{text:'Long diagnostic '.repeat(40),color:'#ff9384'}];
  await page.evaluate(next => { window.framesForTest.dashboard_view = next; }, next);
  await expect(page.locator('.db-hidden-preview .db-item')).toHaveCount(0);
  expect(await bounds(page, '.db-inspector, .db-hidden-preview .db-form-actions')).toEqual(before);
});

test('refresh failures stay beside attention without a dashboard footer', async ({ page, baseURL }) => {
  await page.setViewportSize({width:375,height:812});
  const frames = await open(page, baseURL);
  await page.evaluate(() => { window.failDashboard = true; });
  await expect(page.locator('.db-attention + .db-action-error')).toHaveText(frames.dashboard_view.labels.loadFailed);
  await expect(page.locator('.db-end')).toHaveCount(0);
  await page.evaluate(() => { window.failDashboard = false; });
  await expect(page.locator('.db-action-error')).toHaveCount(0);
});

for (const width of [375, 1024, 1600]) test(`tile content is fully visible without internal scrolling at ${width}px`, async ({page, baseURL}) => {
  await page.setViewportSize({width, height:900});
  const frames = await open(page, baseURL);
  const next = structuredClone(frames.dashboard_view);
  const runners = next.tiles.find(t => t.source === 'ghRunners');
  // Individual rows: the shipped default is grouped, but stored List tiles still paint these.
  const individual = next.sources.find(s => s.id === 'ghRunners').rows[0];
  runners.runnerView = 'list';
  runners.rows = Array.from({length:12}, (_, i) => ({...individual, id:`runner-${i}`, name:`runner-${i}`}));
  await page.evaluate(next => { window.framesForTest.dashboard_view = next; }, next);
  await expect(page.locator('[data-source="ghRunners"] .db-item')).toHaveCount(12);
  const clipped = () => page.locator('.db-tile-content, .card, .panel-body').evaluateAll(elements => elements.filter(el => el.getClientRects().length && el.scrollHeight > el.clientHeight + 1).map(el => el.className));
  await expect.poll(clipped).toEqual([]);
  const padding = await page.locator('.db-attention').evaluate(el => [getComputedStyle(el).paddingLeft, getComputedStyle(el).paddingRight]);
  expect(padding).toEqual(await page.locator('.db-tile').first().evaluate(el => [getComputedStyle(el).paddingLeft, getComputedStyle(el).paddingRight]));
  // Every source's Full view, as an operator reaches it. The fixture's overview
  // carries five of the nine sources, so the Full views come from one
  // `all`-scoped tile per source: all nine panels with every row, which is what
  // `?view=details` measured before #645.
  const full = dashboardWithTiles(frames.dashboard_view, next.sources.map(s => s.id));
  full.layout.revision += 1;
  await page.evaluate(view => { window.framesForTest.dashboard_view = view; }, full);
  await expect(page.locator('.db-tile')).toHaveCount(full.tiles.length);
  for (const { source } of full.tiles) {
    await openTileFull(page, source);
    await expect(page.locator('#cockpitView')).not.toHaveAttribute('data-tile-loading');
    // `clipped` skips what has no box, so a body the Full view hid would pass unmeasured.
    const body = source === 'hosts' ? '#cockpit .card' : '#cockpitView .panel[data-detail-active] .panel-body';
    await expect(page.locator(body).first(), source).toBeVisible();
    await expect.poll(clipped, source).toEqual([]);
    await page.keyboard.press('Escape');
    await expect(page.locator('#dashboardOverview')).toBeVisible();
  }
});

test('resizing releases obsolete height reservations without clipping content', async ({page, baseURL}) => {
  await page.setViewportSize({width:375,height:900});
  const frames = await open(page, baseURL);
  const next = structuredClone(frames.dashboard_view);
  const runners = next.tiles.find(t => t.source === 'ghRunners');
  runners.runnerView = 'list';
  runners.rows = Array.from({length:20}, (_, i) => ({...next.sources.find(s => s.id === 'ghRunners').rows[0], id:`runner-${i}`}));
  await page.evaluate(next => { window.framesForTest.dashboard_view = next; }, next);
  const body = page.locator('.db-tile[data-source="ghRunners"] .db-tile-content');
  await expect.poll(() => body.evaluate(el => parseFloat(el.style.minHeight))).toBeGreaterThan(400);
  const tall = await body.boundingBox();
  runners.rows = [];
  await page.evaluate(next => { window.framesForTest.dashboard_view = next; }, next);
  await expect(page.locator('.db-tile[data-source="ghRunners"] .db-item')).toHaveCount(0);
  expect((await body.boundingBox()).height).toEqual(tall.height);
  await page.setViewportSize({width:1600,height:900});
  await expect.poll(() => body.evaluate(el => el.getBoundingClientRect().height)).toBeLessThan(tall.height);
  expect(await body.evaluate(el => el.scrollHeight <= el.clientHeight + 1)).toBe(true);
});

test('changing Detailed to Summary releases the old tile height', async ({page, baseURL}) => {
  const frames = await open(page, baseURL);
  const next = structuredClone(frames.dashboard_view);
  next.tiles[0].presentation = 'detailed';
  next.layout.revision += 1;
  await page.evaluate(next => { window.framesForTest.dashboard_view = next; }, next);
  const content = page.locator('.db-tile').first().locator('.db-tile-content');
  // Wait for the presentation itself: a Machines summary is already taller
  // than Detailed's 360px floor, so the floor alone no longer proves the
  // Detailed frame was painted.
  await expect(page.locator('.db-tile').first()).toHaveAttribute('data-presentation', 'detailed');
  await expect.poll(() => content.evaluate(el => parseFloat(el.style.minHeight))).toBeGreaterThanOrEqual(360);
  const detailed = (await content.boundingBox()).height;
  next.tiles[0].presentation = 'summary';
  next.layout.revision += 1;
  await page.evaluate(next => { window.framesForTest.dashboard_view = next; }, next);
  await expect.poll(() => content.evaluate(el => el.getBoundingClientRect().height)).toBeLessThan(detailed);
});

test('resizing releases shared row heights and immediately establishes new reservations', async ({page, baseURL}) => {
  await page.setViewportSize({width:1600,height:900});
  const frames = await open(page, baseURL);
  const next = structuredClone(frames.dashboard_view);
  const hosts = next.tiles[0];
  hosts.rows = Array.from({length:20}, (_, i) => ({...hosts.rows[0], id:`host-${i}`}));
  await page.evaluate(next => { window.framesForTest.dashboard_view = next; }, next);
  const bodies = page.locator('.db-tile-content');
  await expect.poll(() => bodies.nth(1).evaluate(el => parseFloat(el.style.minHeight))).toBeGreaterThan(1000);
  hosts.rows = [];
  await page.evaluate(next => { window.framesForTest.dashboard_view = next; }, next);
  await expect(page.locator('.db-tile').first().locator('.db-item')).toHaveCount(0);
  await page.setViewportSize({width:1500,height:900});
  await expect.poll(() => bodies.nth(1).evaluate(el => el.getBoundingClientRect().height)).toBeLessThan(500);
  await expect.poll(() => bodies.evaluateAll(els => els.every(el => parseFloat(el.style.minHeight) > 0))).toBe(true);
});
