import { expect } from "@playwright/test";

/**
 * How a per-panel spec reaches a panel: as a tile's **Full view**, entered the
 * way an operator enters it — the tile's View button, then "Full view" — which
 * is `tileFull` in app/ui/dashboard.js. There is no URL or test hook that skips
 * the menu, so a spec cannot be on a path the product has no way to take.
 *
 * (`dashboard.spec.js` carries its own richer double for the dashboard's own
 * behaviour; this one is the smallest that lets a panel spec answer its panel's
 * command and nothing else.)
 */

/** The id of the tile a panel spec adds for a source. */
export const tileIdFor = (source) => `full-${source}`;

/**
 * The dashboard fixture with exactly one tile per source in `sources`, each scoped to `all` so
 * no row a spec expects is filtered out by a fixture scope. The fixture's
 * overview has no Containers, Usage, Azure Cost or OpenClaw tile, so the tile is
 * built from the Repos tile's shape rather than found.
 */
export function dashboardWithTiles(dashboard, sources) {
  const view = structuredClone(dashboard);
  const [template] = view.tiles.filter((t) => t.source === "ghWorkflows");
  const [layoutTemplate] = view.layout.tiles.filter((t) => t.source === "ghWorkflows");
  view.tiles = sources.map((source) => ({
    ...template, id: tileIdFor(source), source, title: view.sources.find((s) => s.id === source).title,
    scopeLabel: "All resources", rows: [], moreCount: 0, moreLabel: "", footer: "", warnings: [],
  }));
  view.layout.tiles = view.tiles.map(({ id, source, title }) => ({ ...layoutTemplate, id, source, title, scope: "all" }));
  view.hiddenTiles = [];
  view.attention = [];
  return view;
}

/**
 * Answers the IPC surface. `answers` maps a command to its payload; a value of
 * `{ __throw: "text" }` makes that command reject, as Rust's `Err(String)` does
 * across the boundary. Every other command answers `null`. Calls are recorded in
 * `window.__CALLS__` (the time of each in `window.__CALL_TIMES__`) so a spec can assert what a
 * renderer sent. `{ __frames: [first, second] }` serves `first` to every ask
 * until the test calls `advance(page, command)`, then `second`: the frame is
 * moved by the test, never by counting calls (#407; docs/TESTING.md).
 *
 * `cockpit` and `dashboard_view` are always given an answer here, so a spec
 * about a panel never has to think about them. A spec about a failed
 * `cockpit` passes `{ __throw }` as `cockpit`; the overview survives it (#646).
 */
export async function stubTileIpc(page, baseURL, { source, sources = [source], cockpit, answers, dashboardKeys = {} }) {
  // `dashboardKeys` are top-level keys of the `dashboard_view` payload a spec
  // sets itself (the operator's `rowGapPx`, say), over the fixture's.
  const dashboard = {
    ...dashboardWithTiles(await (await fetch(`${baseURL}/sample-dashboard.json`)).json(), sources),
    ...dashboardKeys,
  };
  await page.addInitScript(
    ({ answers, cockpit, dashboard }) => {
      window.__CALLS__ = [];
      window.__CALL_TIMES__ = []; // parallel to __CALLS__, so a call stays {command, args}
      window.__ANSWERS__ = { ...answers, cockpit, dashboard_view: dashboard };
      window.__TAURI__ = {
        core: {
          invoke: async (command, args) => {
            window.__CALLS__.push({ command, args });
            window.__CALL_TIMES__.push(Date.now());
            let answer = window.__ANSWERS__[command] ?? null;
            // `{ __frames: [a, b] }` serves `a` until the test calls advance().
            if (answer && typeof answer === "object" && "__frames" in answer) {
              answer = answer.__frames[(window.__FRAME__ ||= {})[command] ?? 0];
            }
            if (answer && typeof answer === "object" && "__throw" in answer) throw answer.__throw;
            return answer;
          },
        },
      };
    },
    { answers, cockpit, dashboard },
  );
}

/** View → Full view on the spec's tile, then waits for the Full view to be up. */
export async function openTileFull(page, source, tileId = tileIdFor(source)) {
  const tile = page.locator(`[data-tile="${tileId}"]`);
  await tile.getByRole("button", { name: "View", exact: true }).click();
  await page.getByRole("menuitem", { name: "Full view", exact: true }).click();
  await expect(page.locator("#fullView")).toBeVisible();
  await expect(page.locator("#fullView")).toHaveAttribute("data-detail-source", source);
}

/**
 * Stubs the IPC, boots the app on its overview and enters `source`'s tile's Full
 * view. `sources` adds more tiles to the overview, for a spec about two panels.
 */
export async function gotoTileFull(page, baseURL, options) {
  await stubTileIpc(page, baseURL, options);
  await page.goto("/index.html");
  await expect(page.locator(`[data-tile="${tileIdFor(options.source)}"]`)).toBeVisible();
  await openTileFull(page, options.source);
}

/** The arguments of the last `command` the page sent. */
export const lastArgs = (page, command) =>
  page.evaluate((command) => window.__CALLS__.filter((c) => c.command === command).at(-1)?.args, command);

/** Steps `command`'s `__frames` answer to its next frame. */
export const advance = (page, command) =>
  page.evaluate((command) => {
    const frames = window.__ANSWERS__[command].__frames;
    const next = ((window.__FRAME__ ||= {})[command] ?? 0) + 1;
    if (next >= frames.length) throw new Error(`advance(${command}) past the last frame`);
    window.__FRAME__[command] = next;
  }, command);
