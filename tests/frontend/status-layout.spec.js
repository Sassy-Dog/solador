import { test, expect } from '@playwright/test';

test.beforeEach(async ({ page }) => {
  page.statusErrors = [];
  page.on('pageerror', error => page.statusErrors.push(error.message));
  page.on('console', message => {
    if (message.type() === 'error' && /content security policy|refused to (apply|load)/i.test(message.text())) page.statusErrors.push(message.text());
  });
});
test.afterEach(async ({ page }) => { expect(page.statusErrors).toEqual([]); });

const files = { cockpit: 'cockpit', dashboard_view: 'dashboard', containers: 'containers', repos: 'repos', runners: 'runners', services: 'services', crons: 'crons', usage: 'usage', azure_cost: 'azure', openclaw: 'openclaw' };
async function open(page, baseURL, details = false) {
  const frames = {};
  for (const [command, file] of Object.entries(files)) {
    frames[command] = await (await fetch(`${baseURL}/sample-${file}.json`)).json();
  }
  await page.addInitScript(frames => {
    window.framesForTest = frames;
    window.dashboardReads = 0;
    window.__TAURI__ = { core: { invoke: async command => {
      if (command === 'dashboard_view') window.dashboardReads += 1;
      return window.framesForTest[command] ?? null;
    } } };
  }, frames);
  await page.goto(`/index.html${details ? '?view=details' : ''}`);
  await expect(page.locator(details ? '#openclawBody .oc-runtime' : '.db-tile').first()).toBeVisible();
  return frames;
}
const bounds = (page, selector) => page.locator(selector).evaluateAll(elements => elements.map(el => {
  const r = el.getBoundingClientRect();
  return { x:r.x, y:r.y + scrollY, width:r.width, height:r.height };
}));
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
    await warnings.focus();
    await expect(warnings).toBeFocused();
    await page.keyboard.press('ArrowRight');
    await expect.poll(() => warnings.evaluate(el => el.scrollLeft)).toBeGreaterThan(0);
    const reads = await page.evaluate(() => window.dashboardReads);
    await expect.poll(() => page.evaluate(() => window.dashboardReads)).toBeGreaterThan(reads);
    await expect(warnings).toBeFocused();
    expect(await warnings.evaluate(el => el.scrollLeft)).toBeGreaterThan(0);
    await changeDashboard(page, initial);
    expect(await bounds(page, '.db-grid, .db-tile, .db-tile-footer, .db-host-stats, .db-item')).toEqual(before);
  });
}

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

test('detailed panels retain their footprint through failure, empty and recovery frames', async ({ page, baseURL }) => {
  const frames = await open(page, baseURL, true);
  await page.evaluate(() => refreshPanels());
  const before = await bounds(page, '.panel');
  for (const suffix of ['empty', 'error', 'stale']) {
    const changed = structuredClone(frames);
    for (const [command, file] of Object.entries(files)) {
      if (['cockpit', 'dashboard_view'].includes(command)) continue;
      const variants = { empty:['containers','repos','runners','services','crons','usage','azure','openclaw'], error:['crons','azure','openclaw'], stale:['crons','usage','azure'] };
      if (variants[suffix].includes(file)) changed[command] = await (await fetch(`${baseURL}/sample-${file}-${suffix}.json`)).json();
    }
    await page.evaluate(async changed => { window.framesForTest = changed; await refreshPanels(); }, changed);
    expect(await bounds(page, '.panel'), suffix).toEqual(before);
  }
  await page.evaluate(async frames => { window.framesForTest = frames; await refreshPanels(); }, frames);
  expect(await bounds(page, '.panel')).toEqual(before);
});

test('a disconnected detailed host hides old readings without moving the next panel', async ({ page, baseURL }) => {
  const frames = await open(page, baseURL, true);
  const live = await (await fetch(`${baseURL}/sample.json`)).json();
  const down = await (await fetch(`${baseURL}/sample-unreachable.json`)).json();
  await page.evaluate(async model => { window.framesForTest.cockpit = model; await refreshCockpit(); }, live);
  const before = await bounds(page, '.card, #panelRows');
  await page.evaluate(async model => { window.framesForTest.cockpit = model; await refreshCockpit(); }, down);
  await expect(page.locator('.card-down')).toBeVisible();
  await expect(page.locator('.cpuChart')).toBeHidden();
  expect(await bounds(page, '.card, #panelRows')).toEqual(before);
  await page.evaluate(async model => { window.framesForTest.cockpit = model; await refreshCockpit(); }, live);
  expect(await bounds(page, '.card, #panelRows')).toEqual(before);
});

test('a host first connecting after a cold failure keeps the surrounding layout anchored', async ({ page, baseURL }) => {
  await open(page, baseURL, true);
  const live = await (await fetch(`${baseURL}/sample.json`)).json();
  const down = await (await fetch(`${baseURL}/sample-unreachable.json`)).json();
  // Remove existing cards so this is a cold failure, not cached live markup.
  await page.evaluate(async down => {
    window.framesForTest.cockpit = { ...down, hosts:[] };
    await refreshCockpit();
    window.framesForTest.cockpit = down;
    await refreshCockpit();
  }, down);
  const before = await bounds(page, '.card, #panelRows');
  await page.evaluate(async live => { window.framesForTest.cockpit = live; await refreshCockpit(); }, live);
  expect(await bounds(page, '.card, #panelRows')).toEqual(before);
});
