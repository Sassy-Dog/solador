import { test, expect } from "@playwright/test";

// Same CSP guard as the other suites: the page is served under the app's real
// policy (csp_server.py), so a blocked style surfaces as a console error rather
// than a thrown exception. This panel sets every dot's colour AND its opacity
// through CSSOM for exactly that reason — an inline `style=""` would be dropped
// under `style-src 'self'`, and a disabled channel would then render
// indistinguishable from a live one.
test.beforeEach(async ({ page }) => {
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

const fixture = async (baseURL, name) => (await fetch(`${baseURL}/${name}`)).json();

/**
 * Stubs the whole IPC surface and records every call, so a test can assert the
 * panel was painted by the `openclaw` command rather than by a fixture the page
 * happened to fetch.
 *
 * `cockpit` has to be answered too: app.js replaces the entire document body
 * with an error line when its first `invoke` rejects, which would take this
 * panel down with it and fail the suite for the wrong reason.
 */
async function stubIpc(page, { cockpit, openclaw }) {
  await page.addInitScript(
    ({ cockpit, openclaw }) => {
      window.__CALLS__ = [];
      window.__TAURI__ = {
        core: {
          invoke: async (command, args) => {
            window.__CALLS__.push({ command, args });
            if (command === "cockpit") return cockpit;
            if (command === "openclaw") return openclaw;
            return null;
          },
        },
      };
    },
    { cockpit, openclaw }
  );
}

async function gotoWith(page, baseURL, name = "sample-openclaw.json") {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  const openclaw = await fixture(baseURL, name);
  await stubIpc(page, { cockpit, openclaw });
  await page.goto("/index.html");
  return openclaw;
}

/** `#33d17a` as the browser reports a computed colour. */
const rgb = (hex) => {
  const n = parseInt(hex.slice(1), 16);
  return `rgb(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255})`;
};

const runtime = (page) => page.locator("#openclawBody .oc-runtime");
const section = (page, id) => page.locator(`#openclawBody .oc-section[data-section="${id}"]`);

test("the panel paints Rust's title, trailing label and agent rows", async ({ page, baseURL }) => {
  const vm = await gotoWith(page, baseURL);
  await expect(page.locator("#openclawPanel")).toBeVisible();

  await expect(page.locator("#openclawTitle")).toHaveText(vm.title);
  await expect(page.locator("#openclawTrailing")).toHaveText(vm.trailing);

  const agents = vm.runtimes[0].agents;
  await expect(section(page, "agents").locator(".oc-hdr")).toHaveText(agents.header);
  const rows = section(page, "agents").locator(".oc-agent");
  await expect(rows).toHaveCount(agents.rows.length);
  await expect(rows.locator(".oc-name")).toHaveText(agents.rows.map((r) => r.name));

  // The model ref sits on its own line UNDER the name rather than beside it, so
  // neither has to give up characters on a quarter-width card. It starts on the
  // next line, and its TEXT is indented past the status light (the indent is
  // padding, so the box itself still starts at the card's left edge).
  const withDetail = agents.rows.findIndex((r) => r.detail);
  expect(withDetail, "the fixture must carry a model ref").toBeGreaterThan(-1);
  const agent = rows.nth(withDetail);
  const name = await agent.locator(".oc-name").boundingBox();
  const detail = await agent.locator(".oc-detail").boundingBox();
  await expect(agent.locator(".oc-detail")).toHaveText(agents.rows[withDetail].detail);
  expect(detail.y).toBeGreaterThanOrEqual(name.y + name.height - 1);

  const dot = await agent.locator(".dot").boundingBox();
  const textStart =
    detail.x +
    (await agent
      .locator(".oc-detail")
      .evaluate((el) => parseFloat(getComputedStyle(el).paddingLeft)));
  expect(textStart).toBeGreaterThan(dot.x + dot.width);
  expect(textStart).toBeLessThanOrEqual(name.x + 1);

  // The panel is confirmed to be the `openclaw` command's output.
  const calls = await page.evaluate(() => window.__CALLS__.map((c) => c.command));
  expect(calls).toContain("openclaw");
});

test("only a running agent is badged, and only some carry an emoji", async ({ page, baseURL }) => {
  // Both are Rust decisions arriving as `null` or a string. Deriving either
  // here — "badge it if status is running", "always reserve an emoji slot" —
  // would put the rule where no Rust test can see it, and the second would
  // knock every name out of alignment.
  const vm = await gotoWith(page, baseURL);
  const agents = vm.runtimes[0].agents.rows;
  expect(agents.filter((a) => a.trailing).length, "the fixture has exactly one").toBe(1);
  expect(agents.filter((a) => a.emoji).length).toBe(1);

  await expect(section(page, "agents").locator(".oc-badge")).toHaveCount(1);
  await expect(section(page, "agents").locator(".oc-badge")).toHaveText("running");
  await expect(section(page, "agents").locator(".oc-emoji")).toHaveCount(1);
});

test("a disabled dot is dimmed where an unknown one is not", async ({ page, baseURL }) => {
  // `unknown` and `disabled` are the SAME muted colour, so the opacity is the
  // only thing separating "switched off" from "nobody has heard from it". A
  // frontend that painted only `dot.color` would render them identically —
  // which is why both travel and both are applied.
  const vm = await gotoWith(page, baseURL);
  const channels = vm.runtimes[0].channels.rows;
  const unknown = channels.find((c) => c.dot.opacity === 1 && c.dot.color !== channels[0].dot.color);
  const disabled = channels.find((c) => c.dot.opacity !== 1);
  expect(unknown.dot.color).toBe(disabled.dot.color);

  const dots = section(page, "channels").locator(".oc-chip .dot");
  await expect(dots).toHaveCount(channels.length);
  await expect(dots.nth(channels.indexOf(unknown))).toHaveCSS("opacity", "1");
  await expect(dots.nth(channels.indexOf(disabled))).toHaveCSS("opacity", String(disabled.dot.opacity));
  await expect(dots.nth(channels.indexOf(disabled))).toHaveCSS(
    "background-color",
    rgb(disabled.dot.color)
  );
});

test("the cron line is Rust's summary, with the error only when there is one", async ({ page, baseURL }) => {
  const vm = await gotoWith(page, baseURL);
  const cron = vm.runtimes[0].cron;

  await expect(section(page, "cron").locator(".oc-hdr")).toHaveText(cron.header);
  // Counts joined in Rust. A `parts.join(" · ")` here would be a second
  // implementation of which buckets are worth naming.
  await expect(section(page, "cron").locator(".oc-summary")).toHaveText(cron.summary);
  await expect(section(page, "cron").locator(".oc-error")).toHaveText(cron.error.text);
  await expect(section(page, "cron").locator(".oc-error")).toHaveCSS(
    "color",
    rgb(cron.error.color)
  );

  // …and a healthy farm renders no error line at all rather than an empty one.
  const idle = await gotoWith(page, baseURL, "sample-openclaw-idle.json");
  expect(idle.runtimes[0].cron).toBeNull();
  await expect(section(page, "cron")).toHaveCount(0);
});

test("the usage line is abbreviated by Rust", async ({ page, baseURL }) => {
  const vm = await gotoWith(page, baseURL);
  await expect(page.locator("#openclawBody .oc-usage")).toHaveText(vm.runtimes[0].usage.text);
  // `1.2M`, not `1234567` — a `toLocaleString` here would be a second
  // implementation of `OpenClawPanel.formatTokens`.
  expect(vm.runtimes[0].usage.text).toMatch(/^\d+(\.\d)?[kM]? tokens · ctx \d+(\.\d)?[kM]?$/);
});

test("a session that reported no counters shows em dashes, not zeros", async ({ page, baseURL }) => {
  // #184. Both fixtures are the SAME live farm — same agents, same cron, same
  // channels — and differ only in whether the session reported its token
  // counts. So a panel that printed a falsy number as `0` would pass the
  // measured case and paint "0 tokens · ctx 0" here for a farm nobody measured.
  const measured = await gotoWith(page, baseURL);
  expect(measured.runtimes[0].usage.text).not.toContain("—");

  const vm = await gotoWith(page, baseURL, "sample-openclaw-unmeasured.json");
  const line = page.locator("#openclawBody .oc-usage");
  await expect(line).toHaveText(vm.runtimes[0].usage.text);
  await expect(line).toHaveText("— tokens · ctx —");
  expect(vm.runtimes[0].usage.text, "no fabricated figure anywhere").not.toMatch(/\d/);

  // The line survives: it is still one live session, and dropping it would be
  // indistinguishable from the runtime having no session at all — which is a
  // different fact, and the one the idle fixture renders.
  await expect(line).toHaveCount(1);
  await expect(section(page, "agents").locator(".oc-agent")).toHaveCount(
    vm.runtimes[0].agents.rows.length
  );
  const idle = await gotoWith(page, baseURL, "sample-openclaw-idle.json");
  expect(idle.runtimes[0].usage).toBeNull();
  await expect(page.locator("#openclawBody .oc-usage")).toHaveCount(0);
});

test("the pairing banner carries the approve command verbatim and pulses", async ({ page, baseURL }) => {
  const vm = await gotoWith(page, baseURL, "sample-openclaw-pairing.json");
  const pairing = vm.runtimes[0].pairing;
  const banner = page.locator("#openclawBody .oc-pairing");

  await expect(banner.locator(".oc-pairing-title")).toHaveText(pairing.title);
  await expect(banner.locator(".oc-pairing-title")).toHaveCSS("color", rgb(pairing.titleColor));
  // The whole point of the banner: the literal line to paste, selectable.
  await expect(banner.locator(".oc-command")).toHaveText(pairing.command);
  await expect(banner.locator(".oc-command")).toHaveCSS("user-select", "text");
  await expect(banner.locator(".oc-device")).toHaveText(pairing.device);

  // The dot pulses because a human, not a retry, is what clears this — the
  // same class (and the same reduced-motion opt-out) the Repos panel uses.
  await expect(banner.locator(".dot.blink")).toHaveCount(1);
  await expect(page.locator("#openclawTrailing")).toHaveText("pairing required");

  // And nothing else claims the banner slot: a Settings hint stacked under a
  // pairing banner would read as two unrelated problems.
  await expect(runtime(page).locator(".oc-message")).toHaveCount(0);
});

test("a failed connect is red and an unconfigured one is a muted hint", async ({ page, baseURL }) => {
  // The distinction that keeps an operator from hunting a break on a machine
  // nobody set up. Both are `{text, color}` pairs from Rust; choosing between
  // them here would put that judgement beyond every Rust test.
  const down = await gotoWith(page, baseURL, "sample-openclaw-error.json");
  const line = runtime(page).locator(".oc-connection");
  await expect(line).toHaveText(down.runtimes[0].connection.text);
  await expect(line.locator("xpath=preceding-sibling::span[1]")).toHaveCSS(
    "background-color",
    rgb(down.runtimes[0].connection.dotColor)
  );
  await expect(page.locator("#openclawTrailing")).toHaveText("disconnected");

  const idle = await gotoWith(page, baseURL, "sample-openclaw-idle.json");
  await expect(runtime(page).locator(".oc-connection")).toHaveCount(0);
  await expect(runtime(page).locator(".oc-message")).toHaveText(idle.runtimes[0].hint.text);
  // Nothing was attempted, so nothing is claimed in the header either.
  await expect(page.locator("#openclawTrailing")).toHaveText("");
});

test("with no runtime the panel says so and renders no runtime block", async ({ page, baseURL }) => {
  const vm = await gotoWith(page, baseURL, "sample-openclaw-empty.json");
  expect(vm.runtimes).toEqual([]);
  await expect(page.locator("#openclawBody .oc-message")).toHaveText(vm.message.text);
  await expect(runtime(page)).toHaveCount(0);
  await expect(page.locator("#openclawTrailing")).toHaveText("");
});

test("the panel renders no staleness warning, because it is event-driven", async ({ page, baseURL }) => {
  // Every other panel polls and can therefore be stale. This one is fed by a
  // live socket and its connection line already answers "is this current" —
  // a staleness warning would be a second, weaker answer to the same question.
  const vm = await gotoWith(page, baseURL);
  expect(vm.footer).toBeUndefined();
  await expect(page.locator("#openclawPanel .panel-stale")).toHaveCount(0);
});
