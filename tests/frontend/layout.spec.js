import { test, expect } from "@playwright/test";

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
  await page.waitForFunction(() => document.querySelectorAll("#cores .core").length > 0);
}

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
  const expectedPx = await page.evaluate(async () => {
    const data = await (await fetch("sample.json")).json();
    return `${data.coreBlockHeight}px`;
  });
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
      const pts = document.querySelector("#cpuChart svg polyline")
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

test("a host with no discrete GPU renders an em dash, never zero", async ({ page }) => {
  await gotoApp(page);
  await expect(page.locator("#gpuValue")).toHaveText("—");
  await expect(page.locator("#vramText")).toHaveText("VRAM: —");
});

test("volume bar width is proportional to its fraction, not fixed full", async ({ page }) => {
  await gotoApp(page);
  // Under a CSP that silently blocks `style="width:…;background:…"`, every
  // bar renders at its track's full width with a transparent fill -- a
  // fabricated 100%-full reading regardless of the real fraction. None of
  // the fixture's volumes are actually full, so a correct render must be
  // narrower than its track and actually painted.
  const { fillWidth, trackWidth, background } = await page.evaluate(() => {
    const track = document.querySelector("#volumes .vol .bar");
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
    const data = await (await fetch("sample.json")).json();
    const cell = document.querySelector("#cores .core .cap b");
    const probe = document.createElement("div");
    probe.style.color = data.cores[0].valueColor;
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
  // real Tauri IPC in a browser context) so the first `invoke("snapshot")`
  // returns a live view-model and every call after returns a "stale" one --
  // the whole point is that the numbers must not change, only the connection
  // badge.
  //
  // Both fixtures are dumped by the real Rust binary (`--dump` / `--dump-stale`,
  // see tests/frontend/package.json's pretest and app/src-tauri/src/main.rs),
  // from the identical underlying snapshot/history, rather than the stale one
  // being hand-built here from `live` with a copy-pasted "stale"/message
  // string: a hand-built copy can't notice viewmodel's own strings drifting
  // out from under it (see finding M4).
  const live = await (await fetch(`${baseURL}/sample.json`)).json();
  const stale = await (await fetch(`${baseURL}/sample-stale.json`)).json();

  await page.addInitScript(
    ([liveVm, staleVm]) => {
      let calls = 0;
      window.__TAURI__ = {
        core: { invoke: async () => (calls++ === 0 ? liveVm : staleVm) },
      };
    },
    [live, stale]
  );

  await gotoApp(page);

  // First poll: live and green, real numbers on screen.
  await expect(page.locator("#connDot")).toHaveAttribute("data-state", "live");
  const cpuBefore = await page.locator("#cpuValue").textContent();
  expect(cpuBefore).not.toBe("—");

  // The app's own 2s `setInterval` (only armed when `window.__TAURI__`
  // exists) drives the second poll -- wait for that real transition rather
  // than calling into app.js internals directly.
  await expect(page.locator("#connDot")).toHaveAttribute("data-state", "stale", { timeout: 5000 });

  const cpuAfter = await page.locator("#cpuValue").textContent();
  expect(cpuAfter, "the reading must not change just because the poll failed").toBe(cpuBefore);
  expect(cpuAfter).not.toBe("—");
  await expect(page.locator("#staleMsg")).toContainText("Couldn't reach the agent");
  await expect(page.locator("#staleMsg")).toContainText("ago");
});

test("repeated renders do not leak chart bookkeeping (#cores)", async ({ page, baseURL }) => {
  // Regression test for finding I1: render() used to replace #cores'
  // innerHTML on every single poll without pruning the discarded cells from
  // CHARTS (a strong Map) or unregistering them from chartObserver -- ~32
  // detached nodes leaked per poll, forever, in an app meant to run
  // full-screen indefinitely. app.js exposes a read-only test hook
  // (window.__DEVCANOPY_TEST__) specifically so this can be driven directly
  // rather than waiting on real 2s poll ticks.
  await gotoApp(page);
  const data = await (await fetch(`${baseURL}/sample.json`)).json();
  const coreCount = data.cores.length;
  // 5 fixed top-level charts (cpu/mem/gpu/disk/net) that exist once and are
  // never rebuilt, plus one per core.
  const expectedCharts = 5 + coreCount;

  const counts = await page.evaluate(
    ({ d, n }) => {
      const results = [];
      for (let i = 0; i < n; i++) {
        window.__DEVCANOPY_TEST__.render(d);
        results.push(window.__DEVCANOPY_TEST__.chartCount());
      }
      return results;
    },
    { d: data, n: 20 }
  );

  // Flat at every single render, not just the last one -- if CHARTS grew by
  // even one entry per call, this would climb linearly instead.
  for (const [i, count] of counts.entries()) {
    expect(count, `chart bookkeeping size after render #${i + 1}`).toBe(expectedCharts);
  }

  // The DOM itself must also still show exactly one cell per core, not a
  // pile-up of orphaned ones sitting outside #cores.
  const cells = await page.locator("#cores .core").count();
  expect(cells).toBe(coreCount);
});
