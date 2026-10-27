import { test, expect } from "@playwright/test";
import { gotoTileFull, lastArgs, tileIdFor } from "./tile-full.js";

// Same CSP guard as the other suites: the page is served under the app's real
// policy (csp_server.py), so a blocked style surfaces as a console error
// rather than a thrown exception. The panel sets colours through CSSOM for
// exactly that reason — an inline `style=""` would be dropped under
// `style-src 'self'` and every dot would silently render neutral.
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
 * Enters the Containers tile's Full view (View, then Full view) with the IPC
 * stubbed and every call recorded, so a test can assert *which* command painted
 * the panel and what it sent. `cockpit` has to be answered too: app.js replaces
 * the entire document body with an error line when its first `invoke` rejects,
 * which would take the containers panel down with it and make this suite fail
 * for the wrong reason.
 */
async function gotoApp(page, baseURL, { cockpit, containers } = {}) {
  await gotoTileFull(page, baseURL, {
    source: "containers",
    cockpit: cockpit ?? (await fixture(baseURL, "sample-cockpit.json")),
    answers: { containers: containers ?? (await fixture(baseURL, "sample-containers.json")) },
  });
  await expect(page.locator("#containersPanel")).toBeVisible();
}

const rowsOf = (page, host) =>
  page.locator(`.cont-section[data-host="${host}"] .cont-row`);

/** `#33d17a` as the browser reports a computed colour. */
const rgb = (hex) => {
  const n = parseInt(hex.slice(1), 16);
  return `rgb(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255})`;
};

test("the Containers panel is headed by its tile's name and paints Rust's trailing counts and section order", async ({ page, baseURL }) => {
  const payload = { ...(await fixture(baseURL, "sample-containers.json")), title: "Rust's own title" };
  await gotoApp(page, baseURL, { containers: payload });

  // Headed by the tile's own name (#640), not the payload's `title`. The
  // fixture's title is the tile's name too, so it is changed here.
  await expect(page.locator("#containersTitle")).toHaveText("Containers / VMs");
  // The rollup counts every container, including the ones rules collapsed —
  // asserted against Rust's own string so a re-derivation in JS would fail here.
  await expect(page.locator("#containersTrailing")).toHaveText(payload.trailing);
  expect(payload.trailing).toContain("missing");

  const labels = await page.locator(".cont-section .lbl").allTextContents();
  expect(labels).toEqual(payload.sections.map((s) => s.label));
  expect(labels[0]).toBe("THIS MACHINE");
});

test("present, absent and aggregate rows carry Rust's strings and colours", async ({ page, baseURL }) => {
  await gotoApp(page, baseURL);
  const payload = await fixture(baseURL, "sample-containers.json");
  const local = payload.sections.find((s) => s.host === "this machine");

  // Every row, in the order Rust ordered them (name-sorted across present and
  // absent, so a VM keeps its place as it flips between the two).
  const names = await rowsOf(page, "this machine").locator(".cont-name").allTextContents();
  expect(names).toEqual(local.rows.map((r) => r.name));

  for (const [index, expected] of local.rows.entries()) {
    const row = rowsOf(page, "this machine").nth(index);
    await expect(row).toHaveAttribute("data-kind", expected.kind);
    await expect(row.locator(".cont-status")).toHaveText(expected.status);
    await expect(row.locator(".dot")).toHaveCSS("background-color", rgb(expected.dotColor));
    await expect(row.locator(".cont-status")).toHaveCSS("color", rgb(expected.statusColor));
  }

  // The presence semantics the panel exists for: amber while recycling,
  // red once absence passes grace.
  const recycling = local.rows.find((r) => r.status.startsWith("recycling"));
  const missing = local.rows.find((r) => r.status.startsWith("missing"));
  expect(recycling.dotColor).not.toBe(missing.dotColor);
  await expect(rowsOf(page, "this machine").filter({ hasText: missing.name }).locator(".dot"))
    .toHaveCSS("background-color", rgb(missing.dotColor));

  // The collapsed group renders last in its section, with its match count in
  // the name and its running count where a status would be.
  const remote = payload.sections.find((s) => s.host === "ubu-01");
  const aggregate = remote.rows.at(-1);
  expect(aggregate.kind).toBe("aggregate");
  const lastRemoteRow = rowsOf(page, "ubu-01").last();
  await expect(lastRemoteRow).toHaveAttribute("data-kind", "aggregate");
  await expect(lastRemoteRow.locator(".cont-name")).toHaveText(aggregate.name);
  await expect(lastRemoteRow.locator(".cont-status")).toHaveText(aggregate.status);
});

test("a healthy panel shows no warning", async ({ page, baseURL }) => {
  await gotoApp(page, baseURL);
  expect((await fixture(baseURL, "sample-containers.json")).footer).toBeNull();
  await expect(page.locator("#containersStale")).toBeHidden();
});

test("the empty state is one sentence plus the warning, not an empty grid", async ({ page, baseURL }) => {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  const containers = await fixture(baseURL, "sample-containers-empty.json");
  await gotoApp(page, baseURL, { cockpit, containers });

  await expect(page.locator("#containersBody .cont-empty")).toHaveText(containers.empty.message);
  await expect(page.locator(".cont-section")).toHaveCount(0);
  // A failed runtime names itself, and says how long ago the last good
  // reading was, rather than letting stale data pass as current.
  const stale = page.locator("#containersStale");
  await expect(stale).toBeVisible();
  await expect(stale).toHaveText(containers.footer.text);
  await expect(stale).toHaveCSS("color", rgb(containers.footer.color));

  const commands = await page.evaluate(() => window.__CALLS__.map((c) => c.command));
  expect(commands).toContain("containers");
});

test("a section with no rows says which kind of empty it is", async ({ page, baseURL }) => {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  const containers = await fixture(baseURL, "sample-containers.json");
  // The two sentences are Rust's; this only proves the panel renders whichever
  // it is handed, alongside the standing aggregate that survives an empty
  // section.
  containers.sections = [
    {
      host: "this machine",
      label: "THIS MACHINE",
      empty: { message: "no container runtimes" },
      rows: [
        {
          kind: "aggregate",
          name: "workflow jobs ×0/4",
          runtime: null,
          dotColor: "#e09a26",
          status: "0 running",
          statusColor: "#5a6b60",
        },
      ],
    },
  ];
  await gotoApp(page, baseURL, { cockpit, containers });

  await expect(page.locator('.cont-section[data-host="this machine"] .cont-empty'))
    .toHaveText("no container runtimes");
  const aggregate = rowsOf(page, "this machine").first();
  await expect(aggregate.locator(".cont-name")).toHaveText("workflow jobs ×0/4");
  await expect(aggregate.locator(".cont-runtime")).toHaveCount(
    0,
    "an empty group has no runtime, so no tag may be rendered"
  );
});

test("container names reach the DOM as text, never as markup", async ({ page, baseURL }) => {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  const containers = await fixture(baseURL, "sample-containers.json");
  // Names come from a remote agent, and in Tauri the DOM can call `invoke`.
  const hostile = '<img src=x onerror="window.__PWNED__=1">';
  containers.sections = [
    {
      host: "this machine",
      label: "THIS MACHINE",
      empty: null,
      rows: [
        {
          kind: "present",
          name: hostile,
          runtime: "docker",
          dotColor: "#33d17a",
          status: "Up 1 hour",
          statusColor: "#33d17a",
        },
      ],
    },
  ];
  await gotoApp(page, baseURL, { cockpit, containers });

  await expect(rowsOf(page, "this machine").first().locator(".cont-name")).toHaveText(hostile);
  expect(await page.locator("#containersBody img").count()).toBe(0);
  expect(await page.evaluate(() => window.__PWNED__)).toBeUndefined();
});

test("a failed load leaves no invented panel chrome on screen", async ({ page, baseURL }) => {
  await gotoTileFull(page, baseURL, {
    source: "containers",
    cockpit: await fixture(baseURL, "sample-cockpit.json"),
    answers: { containers: { __throw: "containers unavailable" } },
  });

  // The Full view is up, so the page is alive...
  await expect(page.locator("#cockpitView")).toBeVisible();
  // ...and the panel stayed hidden rather than showing an empty heading and a
  // blank count, which would read as "nothing is running" instead of "we
  // don't know".
  await expect(page.locator("#containersPanel")).toBeHidden();
  // The heading is the tile's own name (#640), set before any reading, not
  // anything the failed reading supplied; the panel is hidden under it and the
  // count stays blank.
  await expect(page.locator("#containersTitle")).toHaveText("Containers / VMs");
  await expect(page.locator("#containersTrailing")).toHaveText("");
});

test("host groups sit side by side once Rust says the panel is wide enough", async ({ page, baseURL }) => {
  // Whole groups become the columns — THIS MACHINE fills one, a remote the
  // next — so no group is ever split across a boundary.
  const wide = await fixture(baseURL, "sample-cockpit.json");
  const containers = await fixture(baseURL, "sample-containers.json");
  expect(
    wide.panelRows.flat().find((p) => p.id === "containers").columns,
    "fixture must be wide enough for 2"
  ).toBe(2);

  await gotoApp(page, baseURL, { cockpit: wide, containers });

  await expect(page.locator("#containersPanel")).toHaveAttribute("data-cols", "2");
  const tracks = await page.locator("#containersBody").evaluate((el) =>
    getComputedStyle(el).gridTemplateColumns.trim().split(/\s+/).length
  );
  expect(tracks, "two grid tracks").toBe(2);
  // Every section still renders, whole: the split is CSS placing them, not JS
  // re-chunking their rows.
  await expect(page.locator("#containersBody .cont-section")).toHaveCount(
    containers.sections.length
  );
});

test("a narrow panel keeps the host groups stacked", async ({ page, baseURL }) => {
  const narrow = await fixture(baseURL, "sample-cockpit-narrow.json");
  const containers = await fixture(baseURL, "sample-containers.json");
  expect(narrow.panelRows.flat().find((p) => p.id === "containers").columns).toBe(1);

  await gotoApp(page, baseURL, { cockpit: narrow, containers });

  await expect(page.locator("#containersPanel")).toHaveAttribute("data-cols", "1");
  const tracks = await page.locator("#containersBody").evaluate((el) =>
    getComputedStyle(el).gridTemplateColumns.trim().split(/\s+/).length
  );
  expect(tracks).toBe(1);
});

// The renderer asks for its own tile's reading (#639): `{}` would be the
// source-wide payload, wrong in a tile's Full view whenever its scope narrows.
test("the panel asks for its tile's reading, not the source's", async ({ page, baseURL }) => {
  await gotoApp(page, baseURL);
  expect(await lastArgs(page, "containers")).toEqual({ tile: tileIdFor("containers") });
});
