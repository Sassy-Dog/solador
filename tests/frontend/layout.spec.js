import { test, expect } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  await page.goto("/index.html");
  await page.waitForFunction(() => document.querySelectorAll("#cores .core").length > 0);
});

test("core grid uses only column counts that leave a full last row", async ({ page }) => {
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
    expect(h, `block height at ${width}px`).toBe(220);
  }
});

test("charts widen their time window instead of stretching", async ({ page }) => {
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
  await expect(page.locator("#gpuValue")).toHaveText("—");
  await expect(page.locator("#vramText")).toHaveText("VRAM: —");
});
