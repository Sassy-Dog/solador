import { test, expect } from "@playwright/test";
import { readFileSync } from "node:fs";

test.beforeEach(async ({ page }) => {
  // Served under the app's real CSP (csp_server.py) rather than no policy at
  // all. A blocked inline style or stylesheet surfaces as a console error
  // naming the policy, not as a thrown exception -- collect those so every
  // test in this file fails loudly on a regression, instead of the app
  // quietly falling back to unstyled/default markup while assertions that
  // don't happen to probe the broken bit keep passing.
  //
  // Navigation itself is NOT done here: the connection-state test below has
  // to install a `window.__TAURI__` mock via `addInitScript` before its
  // first navigation, and a beforeEach at file scope always runs before a
  // describe-scoped one, so navigating here would run the real (un-mocked)
  // page load first. Every test calls `gotoApp(page)` itself instead.
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

async function gotoApp(page) {
  await page.goto("/index.html");
  await page.waitForFunction(() => document.querySelectorAll(".cores .core").length > 0);
}

/**
 * Stubs `window.__TAURI__.core.invoke("cockpit")` with Rust-dumped payloads
 * (there is no real Tauri IPC in a browser context), returning each in turn
 * and repeating the last one forever.
 */
async function stubCockpit(page, payloads) {
  await page.addInitScript((vms) => {
    let calls = 0;
    window.__TAURI__ = {
      core: { invoke: async () => vms[Math.min(calls++, vms.length - 1)] },
    };
  }, payloads);
}

const fixture = async (baseURL, name) => (await fetch(`${baseURL}/${name}`)).json();

/** The one live host `--dump` writes, as the offline fallback serves it. */
const firstHost = async (page) =>
  page.evaluate(async () => (await (await fetch("sample.json")).json()).hosts[0]);

test("core grid uses only column counts that leave a full last row", async ({ page }) => {
  await gotoApp(page);
  // 16 cores -> divisors 1,2,4,8,16. Any other count would orphan the last row.
  for (const [width, expected] of [[1900, 16], [900, 8], [500, 4], [300, 2], [150, 1]]) {
    const got = await page.evaluate((w) => {
      const wrap = document.querySelector(".cores-wrap");
      const grid = document.querySelector(".cores");
      wrap.style.width = w + "px";
      void wrap.offsetWidth;
      const cols = getComputedStyle(grid).gridTemplateColumns.trim().split(/\s+/).length;
      const cells = grid.children.length;
      wrap.style.width = "";
      return { cols, cells };
    }, width);
    expect(got.cols, `at ${width}px`).toBe(expected);
    expect(got.cells % got.cols, `orphan row at ${width}px`).toBe(0);
  }
});

test("cores block holds a fixed height at every width", async ({ page }) => {
  await gotoApp(page);
  const expectedPx = `${(await firstHost(page)).coreBlockHeight}px`;
  // app.css's fallback is `var(--core-block-h, 220px)`: if render() silently
  // failed to set the property (exactly the failure mode a blocked style
  // sink produces), the fallback would still make the height assertions
  // below pass. Assert the property itself landed, and against Rust's own
  // number, before trusting the height it produces.
  const actualProp = await page.evaluate(() =>
    getComputedStyle(document.documentElement).getPropertyValue("--core-block-h").trim()
  );
  expect(actualProp, "--core-block-h custom property").toBe(expectedPx);

  const expectedHeight = parseFloat(expectedPx);
  for (const width of [1900, 900, 500, 300, 150]) {
    const h = await page.evaluate((w) => {
      const wrap = document.querySelector(".cores-wrap");
      wrap.style.width = w + "px";
      void wrap.offsetWidth;
      const px = Math.round(wrap.getBoundingClientRect().height);
      wrap.style.width = "";
      return px;
    }, width);
    // core_block_height(CORE_ROW_SPAN_DEFAULT) = 2 * 110
    expect(h, `block height at ${width}px`).toBe(expectedHeight);
  }
});

test("charts widen their time window instead of stretching", async ({ page }) => {
  await gotoApp(page);
  const density = async (w) =>
    page.evaluate(async (width) => {
      document.body.style.width = width + "px";
      void document.body.offsetWidth;
      // The chart repaints from a ResizeObserver callback, which the browser
      // schedules as a rendering-pipeline step, not synchronously with the
      // style change above — give it two animation frames to land before
      // reading the SVG it produces.
      await new Promise(requestAnimationFrame);
      await new Promise(requestAnimationFrame);
      const pts = document.querySelector(".cpuChart svg polyline")
        .getAttribute("points").trim().split(" ");
      const xs = pts.map((p) => parseFloat(p.split(",")[0]));
      document.body.style.width = "";
      return { n: xs.length, px: (xs[xs.length - 1] - xs[0]) / (xs.length - 1) };
    }, w);

  const narrow = await density(500);
  const wide = await density(1500);
  // three times the width shows about three times the samples...
  expect(wide.n).toBeGreaterThan(narrow.n * 2.5);
  // ...at unchanged on-screen density, which is what "not stretched" means
  expect(Math.abs(wide.px - narrow.px)).toBeLessThan(0.5);
});

test("a filling history hugs the right edge at fixed density, never stretching", async ({ page }) => {
  // Swift parity (Sparkline.swift:54-59): the newest sample is pinned at the
  // right edge and points step left by a FIXED step, so a history that is
  // still filling occupies only the right side of the chart. Stretching the
  // few present points across the full width — then switching to sliding at
  // capacity — is the compact-then-slide artifact this guards against.
  const vm = JSON.parse(readFileSync("../../app/ui/sample.json", "utf8"));
  vm.hosts[0].cpuHistory = vm.hosts[0].cpuHistory.slice(0, 6);
  await stubCockpit(page, [vm]);
  await gotoApp(page);
  const got = await page.evaluate(async () => {
    await new Promise(requestAnimationFrame);
    await new Promise(requestAnimationFrame);
    const el = document.querySelector(".cpuChart");
    const pts = el.querySelector("svg polyline").getAttribute("points").trim().split(" ");
    return { w: el.clientWidth, xs: pts.map((p) => parseFloat(p.split(",")[0])) };
  });
  expect(got.xs.length).toBe(6);
  // Newest sample at the right edge...
  expect(Math.abs(got.xs[got.xs.length - 1] - got.w)).toBeLessThan(1.5);
  // ...and the sparse history hugs the right side instead of spanning the
  // chart (6 samples at ~4px density on a wide chart sit within the last few
  // percent of the width; the stretch bug puts the first point at x=0).
  expect(got.xs[0]).toBeGreaterThan(got.w * 0.9);
});

test("a host with no discrete GPU renders an em dash, never zero", async ({ page }) => {
  await gotoApp(page);
  await expect(page.locator(".gpuValue")).toHaveText("—");
  await expect(page.locator(".vramText")).toHaveText("VRAM: —");
});

test("volume bar width is proportional to its fraction, not fixed full", async ({ page }) => {
  await gotoApp(page);
  // Under a CSP that silently blocks `style="width:…;background:…"`, every
  // bar renders at its track's full width with a transparent fill -- a
  // fabricated 100%-full reading regardless of the real fraction. None of
  // the fixture's volumes are actually full, so a correct render must be
  // narrower than its track and actually painted.
  const { fillWidth, trackWidth, background } = await page.evaluate(() => {
    const track = document.querySelector(".volumes .vol .bar");
    const fill = track.querySelector("span");
    return {
      fillWidth: fill.getBoundingClientRect().width,
      trackWidth: track.getBoundingClientRect().width,
      background: getComputedStyle(fill).backgroundColor,
    };
  });
  expect(fillWidth).toBeGreaterThan(0);
  expect(fillWidth).toBeLessThan(trackWidth - 1);
  expect(background).not.toBe("rgba(0, 0, 0, 0)");
});

test("a core cell's value text renders its usage colour", async ({ page }) => {
  await gotoApp(page);
  // Under the broken CSP this fell back to `.cap`'s inherited --muted grey
  // (the attribute carrying the real colour was blocked outright, and it
  // still sat unused in the markup, so nothing about the DOM's structure
  // caught it). Compare against the same value the fixture actually shipped,
  // normalised through the browser's own colour parser so hex vs. rgb()
  // formatting can't produce a false pass or fail.
  const { computed, expected } = await page.evaluate(async () => {
    const host = (await (await fetch("sample.json")).json()).hosts[0];
    const cell = document.querySelector(".cores .core .cap b");
    const probe = document.createElement("div");
    probe.style.color = host.cores[0].valueColor;
    document.body.appendChild(probe);
    const expected = getComputedStyle(probe).color;
    probe.remove();
    return { computed: getComputedStyle(cell).color, expected };
  });
  expect(computed).toBe(expected);
});

test("a host that fails after connecting keeps its last-known data, with an unmissable stale indicator", async ({ page, baseURL }) => {
  // Regression test for the "stale value presented as current" defect: a
  // host that dies after a good poll used to render a fully live-looking
  // card forever. Simulates that sequence by stubbing `window.__TAURI__` (no
  // real Tauri IPC in a browser context) so the first `invoke("cockpit")`
  // returns a live payload and every call after returns a "stale" one --
  // the whole point is that the numbers must not change, only the connection
  // badge.
  //
  // Both fixtures are dumped by the real Rust binary (`--dump` / `--dump-stale`,
  // see tests/frontend/package.json's fixtures script and
  // app/src-tauri/src/main.rs), from the identical underlying
  // snapshot/history, rather than the stale one being hand-built here from
  // `live` with a copy-pasted "stale"/message string: a hand-built copy can't
  // notice viewmodel's own strings drifting out from under it (see finding M4).
  await stubCockpit(page, [
    await fixture(baseURL, "sample.json"),
    await fixture(baseURL, "sample-stale.json"),
  ]);

  await gotoApp(page);

  // First poll: live and green, real numbers on screen.
  await expect(page.locator(".connDot")).toHaveAttribute("data-state", "live");
  const cpuBefore = await page.locator(".cpuValue").textContent();
  expect(cpuBefore).not.toBe("—");

  // The app's own poll `setInterval` (only armed when `window.__TAURI__`
  // exists) drives the second poll -- wait for that real transition rather
  // than calling into app.js internals directly.
  await expect(page.locator(".connDot")).toHaveAttribute("data-state", "stale", { timeout: 5000 });

  const cpuAfter = await page.locator(".cpuValue").textContent();
  expect(cpuAfter, "the reading must not change just because the poll failed").toBe(cpuBefore);
  expect(cpuAfter).not.toBe("—");
  await expect(page.locator(".staleMsg")).toContainText("Couldn't reach the agent");
  await expect(page.locator(".staleMsg")).toContainText("ago");
});

test("every configured host gets its own card, in payload order", async ({ page, baseURL }) => {
  const vm = await fixture(baseURL, "sample-cockpit.json");
  await stubCockpit(page, [vm]);
  await gotoApp(page);

  const cards = page.locator(".cockpit .card");
  await expect(cards).toHaveCount(vm.hosts.length);
  // Names, in order: one card painting another host's name is the failure a
  // count-only assertion would sail past.
  await expect(cards.locator(".hostName")).toHaveText(
    vm.hosts.map((h) => h.hostName ?? h.error.hostName)
  );
});

test("one unreachable host shows its error card while the others stay live", async ({ page, baseURL }) => {
  // Per-host failure isolation, at the DOM. The fixture is deliberately mixed
  // (live / stale / failed, dumped together by `--dump-cockpit`) so a shared
  // error path -- one connection badge for the page, one `cpuValue` id shared
  // across cards -- shows up as cards agreeing when they must not.
  const vm = await fixture(baseURL, "sample-cockpit.json");
  await stubCockpit(page, [vm]);
  await gotoApp(page);

  // Four cards: this machine leads, then the three remotes.
  const cards = page.locator(".cockpit .card");
  await expect(cards).toHaveCount(4);
  expect(await cards.evaluateAll((els) => els.map((e) => e.dataset.state)))
    .toEqual(["live", "live", "stale", "failed"]);

  // The live host is untouched by its neighbours' trouble.
  await expect(cards.nth(1).locator(".cpuValue")).toHaveText(vm.hosts[1].cpuValue);
  await expect(cards.nth(1).locator(".staleMsg")).toHaveText("");

  // The stale host keeps the numbers it last heard, and says how old they are.
  await expect(cards.nth(2).locator(".cpuValue")).toHaveText(vm.hosts[2].cpuValue);
  await expect(cards.nth(2).locator(".staleMsg")).toContainText("Couldn't reach the agent");

  // The host that never connected shows the cause, never a fabricated number.
  await expect(cards.nth(3).locator(".cpuValue")).toHaveText("—");
  await expect(cards.nth(3).locator(".cpuModel")).toHaveText(vm.hosts[3].error.message);
});

test("this machine leads the grid and admits what it could not measure", async ({ page, baseURL }) => {
  // The local card is not a remote host with a shorter address: it is collected
  // in-process, so its connection dot is green by construction, and the figures
  // the platform declines to answer (memory pressure has no portable source,
  // the GPU has no dependency-free read) must render "—" rather than the 0.0
  // the wire contract would lower them to. Both are Rust's decisions
  // (`local::lower_unknowns`) and both are asserted against Rust's own dump.
  const vm = await fixture(baseURL, "sample-cockpit.json");
  await stubCockpit(page, [vm]);
  await gotoApp(page);

  const local = page.locator(".cockpit .card").first();
  expect(vm.hosts[0].id, "the local card leads the payload").toBe("local");
  await expect(local.locator(".hostName")).toHaveText(vm.hosts[0].hostName);
  await expect(local.locator(".connDot")).toHaveAttribute("data-state", "live");

  await expect(local.locator(".pressureText")).toHaveText("Pressure: —");
  await expect(local.locator(".gpuValue")).toHaveText("—");
  await expect(local.locator(".vramText")).toHaveText("VRAM: —");

  // …and what it DID measure is still a number: an em dash everywhere would
  // pass the assertions above while saying nothing.
  await expect(local.locator(".cpuValue")).toHaveText(vm.hosts[0].cpuValue);
  await expect(local.locator(".cpuValue")).not.toHaveText("—");
  await expect(local.locator(".diskRead")).toHaveText(vm.hosts[0].diskRead);
});

test("the panel rows below the grid are the ones Rust reflowed", async ({ page, baseURL }) => {
  // The pairing is `viewmodel::cockpit::reflow`'s, applied here and not decided
  // here — a CSS `auto-fit` would be a second implementation of every panel's
  // `min_width`. The fixture is dumped wide enough for every authored pair, so
  // Usage and Azure Cost share the last row.
  const vm = await fixture(baseURL, "sample-cockpit.json");
  await stubCockpit(page, [vm]);
  await gotoApp(page);

  // Rows carrying only panels this frontend has no section for (`hosts`, which
  // is the grid above) are skipped.
  const known = [
    "containers",
    "openclawAgents",
    "ghWorkflows",
    "ghRunners",
    "claudeUsage",
    "azureCost",
  ];
  const expected = vm.panelRows
    .map((row) => row.map((p) => p.id).filter((id) => known.includes(id)))
    .filter((row) => row.length);

  const rows = page.locator("#panelRows .panel-row");
  await expect(rows).toHaveCount(expected.length);
  const tracks = await rows.evaluateAll((els) =>
    els.map((el) => getComputedStyle(el).gridTemplateColumns.trim().split(/\s+/).length)
  );
  expect(tracks).toEqual(expected.map((row) => row.length));

  // Usage and Azure Cost are the pair the whole per-panel breakpoint model
  // exists for: they stay side by side at widths where the hungrier pairs split.
  const last = expected[expected.length - 1];
  expect(last).toEqual(["claudeUsage", "azureCost"]);
  await expect(rows.last().locator("section")).toHaveCount(2);
});

test("a reflow re-parents every panel without losing one", async ({ page, baseURL }) => {
  // The row containers are rebuilt when the shape changes, and rebuilding them
  // MOVES the existing sections. Get the order wrong — replaceChildren before
  // the moves — and the panels are destroyed with their old containers, which
  // no single-render test can see because the memo skips the rebuild entirely
  // when the shape is unchanged. So this drives a real shape change: a wide
  // payload with every authored pair, then a 700pt one where every row splits.
  const wide = await fixture(baseURL, "sample-cockpit.json");
  const narrow = await fixture(baseURL, "sample-cockpit-narrow.json");
  expect(
    narrow.panelRows.map((r) => r.length),
    "the narrow fixture must actually reflow, or this test proves nothing"
  ).not.toEqual(wide.panelRows.map((r) => r.length));

  await stubCockpit(page, [wide, narrow]);
  await gotoApp(page);

  const sections = [
    "containersPanel",
    "openclawPanel",
    "reposPanel",
    "runnersPanel",
    "usagePanel",
    "azurePanel",
  ];
  const rows = page.locator("#panelRows .panel-row");
  // Wide: three rendered rows (the `hosts` row is the grid above). Narrow: one
  // per section.
  await expect(rows).toHaveCount(3);

  // The app's own 1s poll delivers the narrow payload; wait for the reflow.
  await expect(rows).toHaveCount(sections.length, { timeout: 5000 });
  for (const id of sections) {
    await expect(page.locator(`#${id}`), `${id} survived the reflow`).toHaveCount(1);
    await expect(page.locator(`#panelRows .panel-row > #${id}`)).toHaveCount(1);
  }
  // …and each is alone in its row now, one track apiece.
  const tracks = await rows.evaluateAll((els) =>
    els.map((el) => getComputedStyle(el).gridTemplateColumns.trim().split(/\s+/).length)
  );
  expect(tracks).toEqual(sections.map(() => 1));
});

test("the grid lays out exactly the columns the view-model asked for", async ({ page, baseURL }) => {
  // Both fixtures hold the SAME three hosts and differ only in the width Rust
  // computed them for (`--dump-cockpit` vs `--width 1000`), so this is purely
  // about the frontend applying `hostColumns` rather than deciding it. A CSS
  // `repeat(auto-fit, minmax(900px, 1fr))` here would pass the wide case in a
  // wide browser and fail the stacked one -- which is the point.
  for (const name of ["sample-cockpit.json", "sample-cockpit-stacked.json"]) {
    const vm = await fixture(baseURL, name);
    await stubCockpit(page, [vm]);
    await gotoApp(page);

    const { tracks, gap } = await page.evaluate(() => {
      const style = getComputedStyle(document.querySelector(".cockpit"));
      return {
        tracks: style.gridTemplateColumns.trim().split(/\s+/).length,
        gap: parseFloat(style.columnGap),
      };
    });
    expect(tracks, `${name}: grid tracks`).toBe(vm.hostColumns);
    expect(gap, `${name}: grid gap`).toBe(vm.spacing);
  }
});

test("the tabs overflow mode shows one host at a time, and the bar switches between them", async ({ page, baseURL }) => {
  // The two fixtures hold the SAME four cards at the SAME width (1000pt, where
  // 900pt cards cannot pair) and differ only in the General tab's overflow
  // preference — so this is purely about the frontend applying `hostTabs`
  // rather than deciding it. A JS `columns <= 1 && hosts > 1` here would pass
  // the tabbed case and quietly tab the stacked one too.
  const stacked = await fixture(baseURL, "sample-cockpit-stacked.json");
  const tabbed = await fixture(baseURL, "sample-cockpit-tabs.json");
  expect(stacked.hostColumns, "both fixtures must be below the breakpoint").toBe(1);
  expect(tabbed.hostColumns).toBe(1);
  expect(stacked.hostTabs, "stack is the default and produces no tab bar").toBeNull();

  await stubCockpit(page, [tabbed]);
  await gotoApp(page);

  // One tab per card, labelled and ordered by Rust — this machine leads the
  // bar exactly as it leads the grid.
  const bar = page.locator("#hostTabs");
  await expect(bar).toBeVisible();
  await expect(bar.locator(".tab")).toHaveText(tabbed.hostTabs.tabs.map((t) => t.label));

  // Every card is still in the DOM (hiding, not tearing down, is what keeps
  // each host's sparkline history), but exactly one is on screen.
  const cards = page.locator(".cockpit .card");
  await expect(cards).toHaveCount(tabbed.hosts.length);
  await expect(cards.filter({ visible: true })).toHaveCount(1);
  await expect(cards.first()).toBeVisible();

  // The container carries Rust's floor, or it would collapse to the height of
  // the tab bar with one card on screen.
  const minHeight = await page.evaluate(
    () => getComputedStyle(document.querySelector(".cockpit")).minHeight
  );
  expect(minHeight).toBe(`${tabbed.hostTabs.minHeight}px`);

  // Switching tabs swaps which card is visible, without waiting on a poll.
  const second = tabbed.hostTabs.tabs[1];
  await bar.locator(`.tab[data-host="${second.id}"]`).click();
  await expect(bar.locator(`.tab[data-host="${second.id}"]`)).toHaveAttribute("data-active", "true");
  const shown = cards.filter({ visible: true });
  await expect(shown).toHaveCount(1);
  await expect(shown.locator(".hostName")).toHaveText(second.label);
});

test("stack keeps every host on screen, and leaves no tab bar behind", async ({ page, baseURL }) => {
  // The default, and the regression this guards: a payload that turns tabs OFF
  // (the preference changed, or the window widened past the breakpoint) must
  // put every card back and drop the height floor — a tab bar left on screen
  // over a full grid is worse than never having had one.
  const tabbed = await fixture(baseURL, "sample-cockpit-tabs.json");
  const stacked = await fixture(baseURL, "sample-cockpit-stacked.json");
  await stubCockpit(page, [tabbed, stacked]);
  await gotoApp(page);

  await expect(page.locator("#hostTabs")).toBeVisible();
  // The app's own 1s poll delivers the stacked payload.
  await expect(page.locator("#hostTabs")).toBeHidden({ timeout: 5000 });

  const cards = page.locator(".cockpit .card");
  await expect(cards).toHaveCount(stacked.hosts.length);
  await expect(cards.filter({ visible: true })).toHaveCount(stacked.hosts.length);
  const minHeight = await page.evaluate(
    () => document.querySelector(".cockpit").style.minHeight
  );
  expect(minHeight, "the tabbed floor must not outlive the tab bar").toBe("");
});

test("the Hosts panel title comes from Rust's panel table", async ({ page, baseURL }) => {
  const vm = await fixture(baseURL, "sample-cockpit.json");
  await stubCockpit(page, [vm]);
  await gotoApp(page);
  const hosts = vm.panels.find((p) => p.id === "hosts");
  await expect(page.locator("#hostsTitle")).toHaveText(hosts.title);
});

test("a cockpit with no hosts says so instead of rendering an empty page", async ({ page, baseURL }) => {
  // The message is Rust's (`cockpit_payload`, dumped by `--hosts 0`), not a
  // string invented here: an unconfigured app must read as unconfigured, never
  // as broken.
  const empty = await fixture(baseURL, "sample-cockpit-empty.json");
  await stubCockpit(page, [await fixture(baseURL, "sample.json"), empty]);
  await gotoApp(page);

  // Every *remote* card is torn down when its host leaves the payload. The
  // local one stays: this machine is always there, so "nothing configured" is a
  // statement about monitored hosts, not about the page being empty.
  await expect(page.locator(".cockpit .card")).toHaveCount(1, { timeout: 5000 });
  await expect(page.locator(".cockpit .card .hostName")).toHaveText(empty.hosts[0].hostName);
  // ...and the placeholder takes over rather than leaving a blank page.
  await expect(page.locator("#emptyMsg")).toHaveText(empty.empty.message);
  await expect(page.locator("#emptyMsg")).toBeVisible();
});

test("repeated renders do not leak chart bookkeeping (multi-host)", async ({ page, baseURL }) => {
  // Regression test for finding I1: render() used to replace #cores'
  // innerHTML on every single poll without pruning the discarded cells from
  // CHARTS (a strong Map) or unregistering them from chartObserver -- ~32
  // detached nodes leaked per poll, forever, in an app meant to run
  // full-screen indefinitely. Driven against the three-host payload so the
  // per-card core-count bookkeeping is covered too: a single module-level
  // counter matches on the first card and then rebuilds every other card's
  // cells on every poll, which is the same leak with more cards.
  //
  // app.js exposes a read-only test hook (window.__DEVCANOPY_TEST__)
  // specifically so this can be driven directly rather than waiting on real
  // poll ticks.
  const vm = await fixture(baseURL, "sample-cockpit.json");
  await stubCockpit(page, [vm]);
  await gotoApp(page);

  // 5 fixed top-level charts (cpu/mem/gpu/disk/net) that exist once and are
  // never rebuilt, plus one per core -- for each host that actually has data.
  const expectedCharts = vm.hosts
    .filter((h) => !h.error)
    .reduce((n, h) => n + 5 + h.cores.length, 0);

  const counts = await page.evaluate(
    ({ d, n }) => {
      const results = [];
      for (let i = 0; i < n; i++) {
        window.__DEVCANOPY_TEST__.render(d);
        results.push(window.__DEVCANOPY_TEST__.chartCount());
      }
      return results;
    },
    { d: vm, n: 20 }
  );

  // Flat at every single render, not just the last one -- if CHARTS grew by
  // even one entry per call, this would climb linearly instead.
  for (const [i, count] of counts.entries()) {
    expect(count, `chart bookkeeping size after render #${i + 1}`).toBe(expectedCharts);
  }

  // The DOM itself must also still show exactly one cell per core per card,
  // not a pile-up of orphaned ones.
  for (const [i, host] of vm.hosts.entries()) {
    const cells = await page.locator(".cockpit .card").nth(i).locator(".cores .core").count();
    expect(cells, `core cells on card ${i}`).toBe(host.cores ? host.cores.length : 0);
  }
});

test("a host that leaves the payload takes its chart bookkeeping with it", async ({ page, baseURL }) => {
  // The multi-host counterpart of the leak above: removing a card must prune
  // CHARTS and unobserve its plots, or every host ever configured stays
  // pinned in memory with a live ResizeObserver target for the life of the
  // process.
  //
  // Driven through the real poll path (the stub serves three hosts, then one)
  // rather than the test hook: the app's own interval keeps re-rendering the
  // last stubbed payload, so a hook-driven teardown would be undone within a
  // tick.
  const many = await fixture(baseURL, "sample-cockpit.json");
  const one = await fixture(baseURL, "sample.json");
  await stubCockpit(page, [many, one]);
  await gotoApp(page);

  const cards = page.locator(".cockpit .card");
  await expect(cards).toHaveCount(many.hosts.length);
  const before = await page.evaluate(() => window.__DEVCANOPY_TEST__.chartCount());

  await expect(cards).toHaveCount(1, { timeout: 5000 });
  const after = await page.evaluate(() => window.__DEVCANOPY_TEST__.chartCount());

  expect(after).toBe(5 + one.hosts[0].cores.length);
  expect(after).toBeLessThan(before);
});
