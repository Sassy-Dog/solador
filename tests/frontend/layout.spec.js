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

/** `#e05a4f` as the browser reports a computed colour. */
const rgb = (hex) => {
  const n = parseInt(hex.slice(1), 16);
  return `rgb(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255})`;
};

/**
 * Stubs the cockpit *and* every panel command, so the cards carry their real
 * contents rather than empty chrome.
 *
 * `stubCockpit` answers every command with the cockpit payload, which the panel
 * scripts happily render as a panel with nothing in it — fine when the question
 * is which row a section lands in, useless when it is how tall the section is
 * or where its edge falls. Panels of identical empty height agree about
 * everything.
 */
async function stubPanels(page, baseURL, cockpit) {
  const named = ["containers", "repos", "runners", "usage", "azure", "openclaw", "services", "crons"];
  const files = {
    containers: "sample-containers.json",
    repos: "sample-repos.json",
    runners: "sample-runners.json",
    usage: "sample-usage.json",
    azure: "sample-azure.json",
    openclaw: "sample-openclaw.json",
    services: "sample-services.json",
    crons: "sample-crons.json",
  };
  const payloads = { cockpit };
  for (const name of named) payloads[name] = await fixture(baseURL, files[name]);
  await page.addInitScript((vms) => {
    window.__TAURI__ = {
      core: {
        // `azure_cost` is the command; `azure` is the fixture. Anything else
        // (`settings_*`) answers null, exactly as `stubCockpit` does.
        invoke: async (command) =>
          vms[command === "azure_cost" ? "azure" : command] ?? null,
      },
    };
  }, payloads);
}

/** The one live host `--dump` writes, as the offline fallback serves it. */
const firstHost = async (page) =>
  page.evaluate(async () => (await (await fetch("sample.json")).json()).hosts[0]);

test("core grid uses only column counts core_columns would accept", async ({ page }) => {
  await gotoApp(page);
  // 16 cores -> 1, 2, 4, 8. The 16-column rung used to be here too, and a
  // 1900px card took it: one row of sixteen cells stretched over the whole
  // block. A full last row was only half the rule -- the rung must also stay
  // under the 10-column cap and leave at least 2 rows, so 8 x 2 is as wide as
  // this grid goes no matter how much room it is given.
  for (const [width, expected] of [[1900, 8], [900, 8], [500, 4], [300, 2], [150, 1]]) {
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

test("cores block keeps the 2-row height until cells hit the squeeze floor, then grows", async ({ page }) => {
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

  // 16 cores: rungs of 8/4 columns give 2/4 rows, which fit the fixed
  // 220px block at or above the Swift squeeze floor (49px cells — the
  // 4-row case of core_cell_height). The 2- and 1-column rungs would need
  // 8 and 16 rows, where 220px leaves the plots literally 0px — there the
  // block grows to rows*49 + (rows-1)*8 instead of erasing the charts.
  for (const [width, expected] of [[1900, 220], [900, 220], [500, 220], [300, 448], [150, 904]]) {
    const h = await page.evaluate((w) => {
      const wrap = document.querySelector(".cores-wrap");
      wrap.style.width = w + "px";
      void wrap.offsetWidth;
      const px = Math.round(wrap.getBoundingClientRect().height);
      wrap.style.width = "";
      return px;
    }, width);
    expect(h, `block height at ${width}px`).toBe(expected);
  }
});

test("a many-core host keeps its core sparklines visible at narrow rungs", async ({ page }) => {
  // 36 cores at a ~990px container sit on the 6-column rung -> 6 rows. The
  // fixed 220px block gave each tile ~30px, which padding and the label
  // consumed whole: the plot flexed to 0 and the charts silently vanished
  // (the bug this guards). Past the squeeze floor the block must grow so
  // every tile keeps at least the tightest cell Swift itself renders
  // (49px: HostMetricsPanel's 36-core, 9-column, 4-row case).
  const vm = JSON.parse(readFileSync("../../app/ui/sample.json", "utf8"));
  const host = vm.hosts[0];
  const base = host.cores;
  host.cores = Array.from({ length: 36 }, (_, i) => ({
    ...base[i % base.length], label: `Core ${i}`,
  }));
  host.coreLadder = [1, 2, 3, 4, 6, 9, 12, 18, 36].map((cols) => {
    const rows = Math.ceil(36 / cols);
    return {
      minWidth: cols * 104 + (cols - 1) * 8,
      cols,
      height: Math.max(220, rows * 49 + (rows - 1) * 8),
    };
  });
  await stubCockpit(page, [vm]);
  await gotoApp(page);
  const got = await page.evaluate(async () => {
    const wrap = document.querySelector(".cores-wrap");
    wrap.style.width = "990px";
    void wrap.offsetWidth;
    await new Promise(requestAnimationFrame);
    await new Promise(requestAnimationFrame);
    const cols = getComputedStyle(document.querySelector(".cores"))
      .gridTemplateColumns.split(" ").length;
    const plots = [...document.querySelectorAll(".core .plot")]
      .map((p) => p.getBoundingClientRect().height);
    wrap.style.width = "";
    return { n: plots.length, cols, minPlot: Math.min(...plots) };
  });
  expect(got.n).toBe(36);
  expect(got.cols).toBe(6);
  // A chart needs real room: the 49px squeeze-floor cell leaves ~17px of plot.
  expect(got.minPlot).toBeGreaterThan(12);
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

test("sparklines carry the Swift gradient fade under the line", async ({ page }) => {
  // Swift parity (Sparkline.swift:27-30): the area under the line fades from
  // 0.28 of the series colour at the top to transparent at the bottom, over
  // the FULL chart height. userSpaceOnUse is the load-bearing detail:
  // SwiftUI's LinearGradient spans the view frame, so an idle-flat line must
  // not compress the fade into its own bounding box and paint a solid band.
  await gotoApp(page);
  const got = await page.evaluate(async () => {
    await new Promise(requestAnimationFrame);
    await new Promise(requestAnimationFrame);
    const read = (sel) => {
      const svg = document.querySelector(sel + " svg");
      const lines = [...svg.querySelectorAll("polyline")];
      return [...svg.querySelectorAll("polygon")].map((pg, i) => {
        const gid = (pg.getAttribute("fill").match(/^url\(#(.+)\)$/) || [])[1];
        const grad = svg.querySelector(`linearGradient[id="${gid}"]`);
        return {
          inSameSvg: !!grad,
          units: grad && grad.getAttribute("gradientUnits"),
          y2: grad && grad.getAttribute("y2"),
          stops: grad && [...grad.querySelectorAll("stop")].map((s) => s.getAttribute("stop-opacity")),
          sharesLine: pg.getAttribute("points").includes(lines[i].getAttribute("points")),
        };
      });
    };
    return { cpu: read(".cpuChart"), net: read(".netChart") };
  });
  expect(got.cpu.length).toBe(1);
  expect(got.net.length).toBe(2); // one fade per series, down and up
  for (const g of [...got.cpu, ...got.net]) {
    expect(g.inSameSvg).toBe(true);
    expect(g.units).toBe("userSpaceOnUse");
    expect(g.y2).toBe("100");
    expect(g.stops).toEqual(["0.28", "0"]);
    // The fill is the stroke's own point run closed down to the baseline
    expect(g.sharesLine).toBe(true);
  }
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

test("a host that fails after connecting blanks its card and says it cannot be contacted", async ({ page, baseURL }) => {
  // Regression test for "a stale value presented as current", now taken to its
  // conclusion. A host that dies after a good poll used to render its last
  // reading behind a red badge — and on 2026-08-06 ubu-01 sat like that
  // through a GitHub outage, showing four-minute-old numbers as if they were
  // now. A card is read at a glance, and at a glance a card is its figures.
  //
  // Both fixtures are dumped by the real Rust binary (`--dump` /
  // `--dump-unreachable`, see tests/frontend/package.json's fixtures script)
  // rather than the second being hand-built here from the first: a hand-built
  // copy can't notice viewmodel's own state string or message format drifting
  // out from under it (see finding M4).
  await stubCockpit(page, [
    await fixture(baseURL, "sample.json"),
    await fixture(baseURL, "sample-unreachable.json"),
  ]);

  await gotoApp(page);

  // First poll: live and green, real numbers on screen.
  await expect(page.locator(".connDot")).toHaveAttribute("data-state", "live");
  const cpuBefore = await page.locator(".cpuValue").textContent();
  expect(cpuBefore).not.toBe("—");
  await expect(page.locator(".cores .core").first()).toBeVisible();

  // The app's own poll `setInterval` (only armed when `window.__TAURI__`
  // exists) drives the second poll -- wait for that real transition rather
  // than calling into app.js internals directly.
  await expect(page.locator(".connDot")).toHaveAttribute("data-state", "unreachable", {
    timeout: 5000,
  });

  // Not one figure survives, and the core grid is gone rather than left
  // showing the last shape it had. Cards are reused across polls to keep their
  // chart history, so this is the assertion that catches stale markup nobody
  // cleared.
  await expect(page.locator(".cpuValue")).toHaveText("—");
  await expect(page.locator(".cores .core").first()).toBeHidden();
  await expect(page.locator(".chart").first()).toBeHidden();

  // What is left is the host's name and one sentence dating the outage.
  const down = page.locator(".card-down");
  await expect(down).toBeVisible();
  await expect(down).toContainText("Couldn't reach the agent");
  await expect(down).toContainText("last update");
  await expect(down).toContainText("ago");
  await expect(page.locator(".hostName")).toHaveText("ubu-01");
});

test("a host whose agent stopped sampling loses the green dot even though every poll succeeded", async ({ page, baseURL }) => {
  // Issue #182: the agent answers `/v1/snapshot` with whatever its sampler last
  // produced -- or `empty_snapshot()`'s zeros before it ever produced one -- so
  // every poll succeeds and nothing on the app's side can tell. Only the
  // agent's own `/v1/health` (`samplerStale`) can, and the card must land on
  // the SAME red badge a failed poll gets rather than a live green one over
  // frozen numbers.
  //
  // Both fixtures come from the real binary (`--dump` / `--dump-sampler-stale`)
  // over the identical snapshot, so this asserts the "only the badge changes"
  // rule against a state whose badge nothing coordinator-side produced.
  await stubCockpit(page, [
    await fixture(baseURL, "sample.json"),
    await fixture(baseURL, "sample-sampler-stale.json"),
  ]);

  await gotoApp(page);

  await expect(page.locator(".connDot")).toHaveAttribute("data-state", "live");
  const cpuBefore = await page.locator(".cpuValue").textContent();
  expect(cpuBefore).not.toBe("—");

  // The app's own poll interval drives the second frame.
  await expect(page.locator(".connDot")).toHaveAttribute("data-state", "stale", { timeout: 5000 });

  // Real data, kept.
  expect(await page.locator(".cpuValue").textContent()).toBe(cpuBefore);

  // The message names the agent rather than the link -- a stalled sampler and
  // an unreachable host send an operator to different places.
  await expect(page.locator(".staleMsg")).toContainText("sampler");
  await expect(page.locator(".staleMsg")).not.toContainText("Couldn't reach");
  // …dated by the agent's own clock (`sampleAgeSeconds`, 300 in the fixture).
  // This side's last successful request is a second old, and a badge measuring
  // that would say five-minute-old numbers are current.
  await expect(page.locator(".staleMsg")).toContainText("5m ago");
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
  // (live / unreachable / never-connected, dumped together by
  // `--dump-cockpit`) so a shared error path -- one connection badge for the
  // page, one `cpuValue` id shared across cards -- shows up as cards agreeing
  // when they must not.
  const vm = await fixture(baseURL, "sample-cockpit.json");
  await stubCockpit(page, [vm]);
  await gotoApp(page);

  // Four cards: this machine leads, then the three remotes.
  const cards = page.locator(".cockpit .card");
  await expect(cards).toHaveCount(4);
  expect(await cards.evaluateAll((els) => els.map((e) => e.dataset.state)))
    .toEqual(["live", "live", "unreachable", "failed"]);

  // The live host is untouched by its neighbours' trouble.
  await expect(cards.nth(1).locator(".cpuValue")).toHaveText(vm.hosts[1].cpuValue);
  await expect(cards.nth(1).locator(".staleMsg")).toHaveText("");

  // The unreachable host shows nothing but its name and the reason -- and
  // crucially not its neighbour's figures, which a shared `cpuValue` selector
  // would produce.
  await expect(cards.nth(2).locator(".cpuValue")).toHaveText("—");
  await expect(cards.nth(2).locator(".card-down")).toContainText("Couldn't reach the agent");
  await expect(cards.nth(2).locator(".cores .core").first()).toBeHidden();

  // The host that never connected shows the cause, never a fabricated number.
  await expect(cards.nth(3).locator(".cpuValue")).toHaveText("—");
  await expect(cards.nth(3).locator(".card-down")).toHaveText(vm.hosts[3].error.message);
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

/**
 * Vertical overlap between two boxes, in px. Two things "share a line" when
 * they overlap by most of the shorter one's height — same-line is not
 * same-top: an 18px CPU model centred beside a 26px percentage has neither the
 * same top nor the same bottom.
 */
const sharesLine = (a, b) =>
  Math.min(a.y + a.height, b.y + b.height) - Math.max(a.y, b.y) >= Math.min(a.height, b.height) * 0.8;

test("a card's header names the host and its CPU on one line, ellipsizing the model", async ({ page, baseURL }) => {
  // The header used to be two rows: the name (with its dot) above the CPU
  // model / thermal badge / percentage. They are one row now, which is worth
  // ~30px of every card. The model is the only member allowed to give ground,
  // so a workstation-length model string must ellipsize instead of shoving the
  // percentage past the card's edge.
  const vm = await fixture(baseURL, "sample-cockpit.json");
  vm.hosts[0].cpuModel = "Intel(R) Core(TM) i9-10980XE CPU @ 3.00GHz".padEnd(120, " Turbo");
  expect(vm.hosts[0].cpuModel.length).toBeGreaterThanOrEqual(120);
  await stubCockpit(page, [vm]);
  await gotoApp(page);

  const card = page.locator(".cockpit .card").first();
  const box = async (sel) => await card.locator(sel).boundingBox();
  const [name, model, badge, pct, header, cardBox] = await Promise.all([
    box(".hostName"), box(".cpuModel"), box(".thermal"), box(".cpuValue"),
    box(".hdr"), card.boundingBox(),
  ]);

  // One row, and everything that matters is on it.
  expect(sharesLine(name, model), "the CPU model shares the host name's line").toBe(true);
  expect(sharesLine(name, badge), "the thermal badge shares it too").toBe(true);
  expect(sharesLine(name, pct), "so does the percentage").toBe(true);
  expect(header.height).toBeLessThan(name.height + pct.height);

  // The percentage is pushed right, still inside the card, and still readable.
  expect(pct.x).toBeGreaterThan(model.x + model.width);
  expect(pct.x + pct.width).toBeLessThanOrEqual(cardBox.x + cardBox.width);
  await expect(card.locator(".cpuValue")).toHaveText(vm.hosts[0].cpuValue);

  // …because the model clipped rather than the row growing to fit it.
  const clipped = await card
    .locator(".cpuModel")
    .evaluate((el) => el.scrollWidth > el.clientWidth && getComputedStyle(el).textOverflow === "ellipsis");
  expect(clipped, "a 120-character CPU model must ellipsize").toBe(true);
  // The name never does: it is how you tell the cards apart.
  const nameClipped = await card.locator(".hostName").evaluate((el) => el.scrollWidth > el.clientWidth);
  expect(nameClipped).toBe(false);

  // The stale message shares this row too, and `sample-cockpit.json` carries a
  // host that has one. It is the last thing to give — the model beside it goes
  // to nothing first — but neither of them may cost the percentage its place
  // on the card.
  const overflowing = await page.locator(".cockpit .card").evaluateAll((cards) =>
    cards
      .filter((c) => c.querySelector(".cpuValue").getBoundingClientRect().right >
        c.getBoundingClientRect().right)
      .map((c) => c.querySelector(".hostName").textContent)
  );
  expect(overflowing, "no card's reading may be pushed past its own edge").toEqual([]);
  // …and the unreachable card in the same fixture carries its reason on its
  // own line rather than in the header, where a long message would compete
  // with the very ellipsis rule this test exists to pin.
  const downCard = page.locator(".cockpit .card").filter({ hasText: "Couldn't reach the agent" }).first();
  await expect(downCard.locator(".card-down")).toContainText("Couldn't reach the agent");
  await expect(downCard.locator(".staleMsg")).toHaveText("");
  await expect(downCard.locator(".cpuValue")).toHaveText("—");
});

test("the panel rows below the grid are the ones Rust reflowed", async ({ page, baseURL }) => {
  // The rows AND the track widths are `viewmodel::cockpit`'s — `reflow` decides
  // who shares a row, each placement's `PanelSpan` weight decides how much of it
  // they get — applied here and not decided here. A CSS `auto-fit` would be a
  // second implementation of every panel's own `min_width`.
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
    "services",
    "sentryCrons",
  ];
  const expected = vm.panelRows
    .map((row) => row.map((p) => p.id).filter((id) => known.includes(id)))
    .filter((row) => row.length);

  const rows = page.locator("#panelRows .panel-row");
  await expect(rows).toHaveCount(expected.length);
  // Four quarter tracks on every row, whatever it holds — the grid is the same
  // one everywhere, and a panel claims its share with `grid-column`.
  const tracks = await rows.evaluateAll((els) =>
    els.map((el) => getComputedStyle(el).gridTemplateColumns.trim().split(/\s+/).length)
  );
  expect(tracks).toEqual(expected.map(() => 4));

  // The row the span system exists for: Containers takes half, OpenClaw and
  // Usage a quarter each.
  const quarter = expected.findIndex(
    (row) => row.join() === "containers,openclawAgents,claudeUsage"
  );
  expect(quarter, "the wide fixture must carry the authored quarter row").toBeGreaterThan(-1);
  const spans = await rows
    .nth(quarter)
    .evaluate((el) =>
      [...el.children].map((c) => getComputedStyle(c).gridColumnStart.trim())
    );
  expect(spans).toEqual(["1", "3", "4"]);

  // …and the last row is Azure Cost beside the two lean panels: a half still
  // buys the cost breakdowns their own column, and a row of its own for either
  // short list would leave most of a full-width band empty. Same shape as the
  // quarter row above it, so the two land on the same gridlines.
  expect(expected[expected.length - 1]).toEqual(["azureCost", "services", "sentryCrons"]);
  await expect(rows.last().locator("section")).toHaveCount(3);
  const lastSpans = await rows
    .last()
    .evaluate((el) => [...el.children].map((c) => getComputedStyle(c).gridColumnStart.trim()));
  expect(lastSpans).toEqual(["1", "3", "4"]);
});

/**
 * The bug the four-quarter grid was introduced for. Each row used to size its
 * own tracks with the weights it happened to contain — `2fr 1fr 1fr` beside
 * `2fr 2fr` — and `fr` divides what is left after *that row's* gutters. So a
 * Half beside two Quarters came out half a gutter narrower than a Half beside
 * one Half, and the vertical edge between Repos and Runners missed the edge
 * between Containers and OpenClaw directly below it by 8pt on the shipped
 * cockpit. Both are `half`; they must land on the same gridline.
 */
test("a half is the same width in every row", async ({ page, baseURL }) => {
  const vm = await fixture(baseURL, "sample-cockpit.json");
  // Both cards have to be on screen to be measured, and `stubCockpit` leaves
  // Repos hidden — it never answers the `repos` command with a repos payload.
  await stubPanels(page, baseURL, vm);
  await gotoApp(page);
  await expect(page.locator("#reposPanel")).toBeVisible();
  await expect(page.locator("#containersPanel")).toBeVisible();

  const spanOf = (id) =>
    vm.panelRows.flat().find((p) => p.id === id)?.span;
  expect(spanOf("ghWorkflows"), "the fixture must carry two halves in different rows").toBe("half");
  expect(spanOf("containers")).toBe("half");

  const [repos, containers] = await Promise.all([
    page.locator("#reposPanel").boundingBox(),
    page.locator("#containersPanel").boundingBox(),
  ]);
  expect(containers.width).toBeCloseTo(repos.width, 1);
  expect(
    containers.x + containers.width,
    "the edge under Repos and the edge under Containers are one line"
  ).toBeCloseTo(repos.x + repos.width, 1);
});

test("every card sharing a row is the same height", async ({ page, baseURL }) => {
  // The ragged edge this replaces: cards were `align-items:start`, so a short
  // panel beside a long one left a gap the row read as damage. Content stays
  // top-aligned inside the stretched card — the extra height is trailing space,
  // not stretched rows — so this asserts the CARD heights match AND that at
  // least one card is genuinely taller than its own content. Without the second
  // half, a fixture whose panels happened to be equally tall would pass with
  // `align-items:start` back in place.
  const vm = await fixture(baseURL, "sample-cockpit.json");
  // Real panel contents, not the empty chrome `stubCockpit` produces: cards of
  // identical empty height pass a height-equality test without proving
  // anything, and there is no trailing space in one for the slack check below
  // to find.
  await stubPanels(page, baseURL, vm);
  await gotoApp(page);
  await expect(page.locator("#reposPanel")).toBeVisible();
  await expect(page.locator("#openclawPanel .oc-agent").first()).toBeVisible();

  const rows = page.locator("#panelRows .panel-row");
  const shared = await rows.evaluateAll((els) =>
    els
      .map((el) =>
        [...el.querySelectorAll(":scope > section")].map((section) => {
          const box = section.getBoundingClientRect();
          const content = section.lastElementChild.getBoundingClientRect().bottom;
          const padding = parseFloat(getComputedStyle(section).paddingBottom);
          return { height: box.height, slack: box.bottom - content - padding };
        })
      )
      .filter((cards) => cards.length > 1)
  );
  expect(shared.length, "the fixture must have at least one shared row").toBeGreaterThan(0);
  for (const cards of shared) {
    const heights = cards.map((c) => c.height);
    for (const height of heights) {
      expect(Math.abs(height - heights[0]), `heights ${heights.join("/")}`).toBeLessThan(1);
    }
  }
  const slack = Math.max(...shared.flat().map((c) => c.slack));
  expect(slack, "a shorter card is stretched to its row, not merely equal").toBeGreaterThan(24);
});

test("a reflow re-parents every panel without losing one", async ({ page, baseURL }) => {
  // The row containers are rebuilt when the shape changes, and rebuilding them
  // MOVES the existing sections. Get the order wrong — replaceChildren before
  // the moves — and the panels are destroyed with their old containers, which
  // no single-render test can see because the memo skips the rebuild entirely
  // when the shape is unchanged. So this drives a real shape change: a wide
  // payload with every authored row, then a 700pt one where the halves and the
  // quarter row both break up.
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
    "servicesPanel",
    "cronsPanel",
  ];
  // Both shapes are the payload's, not this test's: the sectionless `hosts` row
  // is the grid above, so it contributes no rendered row.
  const shapeOf = (vm) =>
    vm.panelRows
      .map((row) => row.filter((p) => p.id !== "hosts").length)
      .filter((count) => count);
  const rows = page.locator("#panelRows .panel-row");
  await expect(rows).toHaveCount(shapeOf(wide).length);

  // The app's own 1s poll delivers the narrow payload; wait for the reflow.
  await expect(rows).toHaveCount(shapeOf(narrow).length, { timeout: 5000 });
  for (const id of sections) {
    await expect(page.locator(`#${id}`), `${id} survived the reflow`).toHaveCount(1);
    await expect(page.locator(`#panelRows .panel-row > #${id}`)).toHaveCount(1);
  }
  // …and every section is accounted for by the rows Rust asked for. Counted as
  // sections rather than as tracks: every row is the same four quarter tracks,
  // so a track count says nothing about how many panels landed in it.
  const perRow = await rows.evaluateAll((els) =>
    els.map((el) => el.querySelectorAll(":scope > section").length)
  );
  expect(perRow).toEqual(shapeOf(narrow));
  expect(perRow.reduce((a, b) => a + b, 0)).toBe(sections.length);
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

test("side-by-side cards reserve volume slots so the sections below them line up", async ({ page, baseURL }) => {
  // The fixture's four cards report 1, 3, 3 and 0 volumes, so before the
  // reservation the 1- and 0-volume cards ended their Volumes block two rows
  // short and dragged TOP CPU / TOP RAM up with it.
  const vm = await fixture(baseURL, "sample-cockpit.json");
  expect(vm.hostColumns, "fixture must be side by side").toBeGreaterThan(1);
  expect(vm.volumeSlots, "Rust reserves the busiest card's count").toBe(
    Math.max(...vm.hosts.map((h) => (h.volumes || []).length))
  );

  await stubCockpit(page, [vm]);
  await gotoApp(page);

  const measured = await page.locator(".cockpit .card").evaluateAll((cards) =>
    cards.map((card) => {
      const vols = card.querySelector(".volumes");
      const label = [...card.querySelectorAll(".cols2 > section > .lbl")]
        .find((e) => e.textContent.trim() === "TOP CPU");
      return {
        tiles: vols.querySelectorAll(".vol").length,
        height: Math.round(vols.getBoundingClientRect().height),
        // The heading's y measured from the top of the CORES block — every
        // section between the two now aligns across the row, so this spans
        // the whole stack rather than just Volumes → TOP CPU.
        labelY: Math.round(
          label.getBoundingClientRect().y -
            card.querySelector(".cores-wrap").getBoundingClientRect().y
        ),
      };
    })
  );

  // A host that has never answered renders no data at all — `drawCard` returns
  // before `render`, deliberately, so the card shows the cause instead of
  // fabricated numbers. It has no Volumes block to align, and it contributes 0
  // to the maximum rather than dragging it down.
  const live = vm.hosts.map((h, i) => (h.error ? null : i)).filter((i) => i !== null);
  expect(live.length, "fixture needs at least two live cards").toBeGreaterThan(1);

  // Every live card renders the same number of tiles — the mechanism. Equal
  // counts at equal card widths give the same auto-fit column count, hence the
  // same number of rows.
  expect(measured.map((m, i) => (vm.hosts[i].error ? 0 : m.tiles)))
    .toEqual(vm.hosts.map((h) => (h.error ? 0 : vm.volumeSlots)));

  // …and the result: equal block heights.
  const heights = live.map((i) => measured[i].height);
  expect(new Set(heights).size, `volume block heights ${heights}`).toBe(1);

  // …so the whole stack from the cores block down shares one baseline. This
  // used to be measured from the *Volumes* block, because the cores block
  // above was its own axis of variation: a card whose cores hit the squeeze
  // floor (`core_rung_height`) was legitimately taller than one whose don't.
  // `aligned_core_ladders` closed that axis, so the span widens to cover
  // Cores → Memory → Disk → Volumes → TOP CPU; cores drifting out of
  // alignment now fails here too.
  //
  // It stops at the cores block rather than the card's top edge because the
  // header genuinely varies: a host that has gone unreachable while still
  // showing its last-known data carries a "Couldn't reach the agent" banner
  // there, making its header 21px taller. That difference is the card doing
  // its job — padding every healthy card to match would be alignment for its
  // own sake.
  const baselines = live.map((i) => measured[i].labelY);
  expect(new Set(baselines).size, `TOP CPU baselines ${baselines}`).toBe(1);
});

test("a stacked column reserves nothing, so a short card keeps its own height", async ({ page, baseURL }) => {
  // Alignment is meaningless once the cards stack, and reserving there would
  // pad a 1-volume card with dead space under it. Rust says 0; the frontend
  // must render no padding tiles at all.
  const vm = await fixture(baseURL, "sample-cockpit-stacked.json");
  expect(vm.hostColumns).toBe(1);
  expect(vm.volumeSlots, "nothing reserved when cards never share a row").toBe(0);

  await stubCockpit(page, [vm]);
  await gotoApp(page);

  await expect(page.locator(".cockpit .card .volumes .vol.pad")).toHaveCount(0);
  const tiles = await page.locator(".cockpit .card .volumes").evaluateAll((els) =>
    els.map((el) => el.querySelectorAll(".vol").length)
  );
  expect(tiles).toEqual(vm.hosts.map((h) => (h.volumes || []).length));
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

/**
 * A tab bar shows one card and hides the rest, so a host that drops while you
 * are looking at another one has nothing on screen but its button. On
 * 2026-08-06 ubu-01 went down mid-outage and stayed unnoticed for exactly
 * that reason — the alarm has to live on the tab.
 */
test("a tab whose host cannot be contacted is red and pulses", async ({ page, baseURL }) => {
  const tabbed = await fixture(baseURL, "sample-cockpit-tabs.json");
  const down = tabbed.hostTabs.tabs.filter((t) => t.alert);
  expect(down.length, "the fixture must carry an unreachable host").toBeGreaterThan(0);

  await stubCockpit(page, [tabbed]);
  await gotoApp(page);

  const alerting = page.locator("#hostTabs .tab[data-alert]");
  await expect(alerting).toHaveCount(down.length);
  await expect(alerting.first()).toHaveText(down[0].label);
  await expect(alerting.first()).toHaveCSS("color", rgb(down[0].color));
  // The pulse is what makes it findable in peripheral vision; a red tab that
  // sits still reads as a style, not an alarm.
  expect(
    await alerting.first().evaluate((el) => getComputedStyle(el).animationName)
  ).not.toBe("none");

  // …and the healthy tabs are untouched, or the bar would read as all-alarm.
  const calm = page.locator("#hostTabs .tab:not([data-alert])");
  await expect(calm).toHaveCount(tabbed.hostTabs.tabs.length - down.length);
  expect(
    await calm.first().evaluate((el) => getComputedStyle(el).animationName)
  ).toBe("none");
});

test("the host tab bar rides the Hosts title line instead of costing a row", async ({ page, baseURL }) => {
  // The tabs mode only ever appears below the side-by-side breakpoint — the
  // narrowest, shortest windows — so a switcher on a row of its own spent
  // vertical space exactly where there is none. It now sits inside `.topbar`
  // between the title and the Settings button, and the topbar must not have
  // grown taller to hold it.
  const tabbed = await fixture(baseURL, "sample-cockpit-tabs.json");
  await stubCockpit(page, [tabbed]);
  await gotoApp(page);

  const topbar = page.locator("#cockpitView > .topbar");
  const bar = page.locator("#hostTabs");
  await expect(bar).toBeVisible();

  const [tb, barBox, title, button, firstTab] = await Promise.all([
    topbar.boundingBox(),
    bar.boundingBox(),
    page.locator("#hostsTitle").boundingBox(),
    page.locator("#settingsToggle").boundingBox(),
    bar.locator(".tab").first().boundingBox(),
  ]);

  // Same visual line as the title, and inside the topbar's box.
  expect(sharesLine(title, firstTab), "the tabs sit on the Hosts title's line").toBe(true);
  expect(barBox.y).toBeGreaterThanOrEqual(tb.y);
  expect(barBox.y + barBox.height).toBeLessThanOrEqual(tb.y + tb.height + 0.5);
  // Between the title and the button, in that order.
  expect(barBox.x).toBeGreaterThanOrEqual(title.x + title.width);
  expect(barBox.x + barBox.width).toBeLessThanOrEqual(button.x);
  // …and the row is still exactly as tall as its tallest fixed member.
  expect(tb.height).toBeCloseTo(button.height, 1);

  // The strip scrolls rather than wrapping: wrapping would put the overflow
  // tabs back on a second line, which is the line this move reclaimed.
  const wrap = await bar.evaluate((el) => getComputedStyle(el).flexWrap);
  expect(wrap).toBe("nowrap");
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
  // app.js exposes a read-only test hook (window.__SOLADOR_TEST__)
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
        window.__SOLADOR_TEST__.render(d);
        results.push(window.__SOLADOR_TEST__.chartCount());
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
  const before = await page.evaluate(() => window.__SOLADOR_TEST__.chartCount());

  await expect(cards).toHaveCount(1, { timeout: 5000 });
  const after = await page.evaluate(() => window.__SOLADOR_TEST__.chartCount());

  expect(after).toBe(5 + one.hosts[0].cores.length);
  expect(after).toBeLessThan(before);
});

test("cards sharing a row share one core-block height", async ({ page, baseURL }) => {
  // The skew this fixes: at a ~900pt card a 36-core host takes 6 rows (334px)
  // where a 10-core host takes 2 (220px), and every section below the block —
  // Memory, Disk, Volumes, the process lists — inherits the 114px difference.
  // Rust's `aligned_core_ladders` gives each count its own columns but the
  // busiest card's height; this asserts the frontend actually paints that.
  const vm = await fixture(baseURL, "sample-cockpit.json");
  expect(vm.hostColumns, "fixture must be side by side").toBeGreaterThan(1);

  // Two live hosts with genuinely different core counts, each carrying the
  // ladder Rust emits for the {10, 36} set: own cols, shared height.
  const [a, b] = vm.hosts.filter((h) => !h.error).slice(0, 2);
  const reshape = (host, n) => {
    const base = host.cores;
    host.cores = Array.from({ length: n }, (_, i) => ({
      ...base[i % base.length], label: `Core ${i}`,
    }));
    const divisors = (m) => [...Array(m).keys()].map((i) => i + 1).filter((d) => m % d === 0);
    const rung = (count, cols) => Math.max(220, Math.ceil(count / cols) * 49 + (Math.ceil(count / cols) - 1) * 8);
    // Union of both ladders' boundaries, height = max across both counts —
    // the JS mirror of the Rust function, exactly as the 36-core test above
    // mirrors `core_rung_height`.
    const widths = [...new Set([10, 36].flatMap((m) => divisors(m).map((d) => d * 104 + (d - 1) * 8)))]
      .sort((x, y) => x - y);
    const colsAt = (count, w) =>
      divisors(count).filter((d) => d * 104 + (d - 1) * 8 <= w).pop() || 1;
    host.coreLadder = widths.map((w) => ({
      minWidth: w,
      cols: colsAt(n, w),
      height: Math.max(...[10, 36].map((m) => rung(m, colsAt(m, w)))),
    }));
  };
  reshape(a, 10);
  reshape(b, 36);

  await stubCockpit(page, [vm]);
  await gotoApp(page);

  const measured = await page.evaluate(async () => {
    const wraps = [...document.querySelectorAll(".card:not([hidden]) .cores-wrap")];
    // 899px: the band where a 36-core host is on its 6-column rung (334px)
    // while a 10-core host would naturally sit at 220.
    for (const w of wraps) w.style.width = "899px";
    void wraps[0].offsetWidth;
    await new Promise(requestAnimationFrame);
    const out = wraps.map((w) => ({
      height: Math.round(w.getBoundingClientRect().height),
      cols: getComputedStyle(w.querySelector(".cores")).gridTemplateColumns.split(/\s+/).length,
      cells: w.querySelectorAll(".core").length,
    }));
    for (const w of wraps) w.style.width = "";
    return out;
  });

  const live = measured.filter((m) => m.cells > 0);
  expect(live.length, "at least two live cards").toBeGreaterThan(1);
  const [ten, thirtySix] = live;
  expect(ten.cells).toBe(10);
  expect(thirtySix.cells).toBe(36);
  // Each keeps a column count its own core count divides evenly into —
  // sharing the height must never orphan a last row.
  expect(ten.cells % ten.cols, "10 cores divide evenly").toBe(0);
  expect(thirtySix.cells % thirtySix.cols, "36 cores divide evenly").toBe(0);
  // …and the blocks match, at the taller card's height.
  expect(ten.height, `heights ${live.map((m) => m.height)}`).toBe(thirtySix.height);
  expect(thirtySix.height).toBe(334);
});
