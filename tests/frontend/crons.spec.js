import { test, expect } from "@playwright/test";

// Same CSP guard as the other suites: the page is served under the app's real
// policy, so a blocked style surfaces as a console error rather than a thrown
// exception. This panel sets every colour through CSSOM for that reason.
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

/** `#e05a4f` as the browser reports a computed colour. */
const rgb = (hex) => {
  const n = parseInt(hex.slice(1), 16);
  return `rgb(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255})`;
};

const RED = "#e05a4f";
const AMBER = "#e09a26";
const MUTED = "#5a6b60";
const GREEN_DIM = "#1c6b41";

async function gotoWithCrons(page, baseURL, payload) {
  const cockpit = await fixture(baseURL, "sample-cockpit.json");
  await page.addInitScript(
    (vms) => {
      window.__TAURI__ = {
        core: {
          invoke: async (command) =>
            command === "cockpit" ? vms.cockpit : command === "crons" ? vms.crons : null,
        },
      };
    },
    { cockpit, crons: payload }
  );
  await page.goto("/index.html");
  return payload;
}

const rows = (page) => page.locator("#cronsBody .cron-row");
const rowFor = (page, monitor) => rows(page).filter({ hasText: monitor }).first();

test("the panel paints one row per failing environment, in Rust's order", async ({ page, baseURL }) => {
  const vm = await gotoWithCrons(page, baseURL, await fixture(baseURL, "sample-crons.json"));
  await expect(page.locator("#cronsPanel")).toBeVisible();
  await expect(page.locator("#cronsTitle")).toHaveText(vm.title);
  await expect(page.locator("#cronsTrailing")).toHaveText(vm.trailing);

  // Order is Rust's: oldest first, with the entries nobody can put a number on
  // leading. Re-sorting here would be a second implementation of the one rule
  // this panel exists to get right.
  await expect(rows(page)).toHaveCount(vm.rows.length);
  await expect(rows(page).locator(".cron-name")).toHaveText(vm.rows.map((r) => r.label));
  await expect(rows(page).locator(".cron-age")).toHaveText(vm.rows.map((r) => r.age));
  await expect(rows(page).locator(".cron-detail")).toHaveText(vm.rows.map((r) => r.detail));
});

/**
 * The regression the whole panel exists for, at the pixel. The fixture's monitor
 * has an incident a week old and a check-in from this morning; the row must show
 * the week. `0d 22h` here would mean the age was measured from the last
 * check-in — the bug where day 6 of an outage looks exactly like day 1.
 */
test("a failing monitor shows how long it has been broken, not when it last ran", async ({ page, baseURL }) => {
  await gotoWithCrons(page, baseURL, await fixture(baseURL, "sample-crons.json"));
  const row = rowFor(page, "cron-relay-drift-check");
  await expect(row.locator(".cron-age")).toHaveText("7d 22h");
  await expect(row.locator(".cron-age")).not.toHaveText("0d 22h");
  await expect(row.locator(".cron-age")).toHaveCSS("color", rgb(RED));
  await expect(row).toHaveAttribute(
    "title",
    "cron-relay-drift-check (platform/prd) — error for 7d 22h"
  );
});

/**
 * The fallback has to *look* like a weaker claim: an age derived from the last
 * check-in is amber beside a red row, carries a `≈`, and its detail line says
 * there is no incident behind it. Rendering it identically to an incident-derived
 * age would imply precision the panel does not have.
 */
test("a check-in derived age is visibly marked as an approximation", async ({ page, baseURL }) => {
  await gotoWithCrons(page, baseURL, await fixture(baseURL, "sample-crons.json"));
  const row = rowFor(page, "nightly-rollup");
  await expect(row.locator(".cron-age")).toHaveText("≈ 0d 22h");
  await expect(row.locator(".cron-age")).toHaveCSS("color", rgb(AMBER));
  await expect(row.locator(".cron-detail")).toContainText("no incident");
  // The row itself is still red — the monitor is still broken.
  await expect(row.locator(".cron-name")).toHaveCSS("color", rgb(RED));
});

/** Never checked in is words, never a duration — and never a `0d 0h`. */
test("a monitor that never checked in says so instead of showing a duration", async ({ page, baseURL }) => {
  await gotoWithCrons(page, baseURL, await fixture(baseURL, "sample-crons.json"));
  const age = rowFor(page, "brand-new-cron").locator(".cron-age");
  await expect(age).toHaveText("never checked in");
  await expect(age).toHaveCSS("color", rgb(AMBER));
});

/** A suppressed entry is muted and named — counted and shown, never dropped. */
test("a suppressed monitor is still listed, muted, with its reason", async ({ page, baseURL }) => {
  const vm = await fixture(baseURL, "sample-crons.json");
  await gotoWithCrons(page, baseURL, vm);
  const suppressed = vm.rows.find((r) => r.suppressed);
  expect(suppressed, "the fixture must carry a suppressed row").toBeTruthy();

  const row = rowFor(page, suppressed.label);
  await expect(row).toHaveAttribute("data-suppressed", "true");
  await expect(row.locator(".cron-name")).toHaveCSS("color", rgb(MUTED));
  await expect(row.locator(".cron-detail")).toContainText("muted");
  // …and it does not inflate the headline count, which is what suppression is
  // for — but it is named beside it.
  await expect(page.locator("#cronsTrailing")).toHaveText("3 not ok · 1 suppressed");
});

test("every row wears the colour and the dot Rust chose", async ({ page, baseURL }) => {
  const vm = await fixture(baseURL, "sample-crons.json");
  await gotoWithCrons(page, baseURL, vm);
  for (const [i, row] of vm.rows.entries()) {
    const el = rows(page).nth(i);
    await expect(el).toHaveAttribute("data-monitor", row.id);
    await expect(el.locator(".dot")).toHaveCSS("background-color", rgb(row.color));
    await expect(el.locator(".cron-age")).toHaveCSS("color", rgb(row.ageColor));
  }
});

/** The only rendering entitled to say nothing is wrong. */
test("a measured healthy org says so, in green, with no rows", async ({ page, baseURL }) => {
  const vm = await gotoWithCrons(page, baseURL, await fixture(baseURL, "sample-crons-empty.json"));
  await expect(page.locator("#cronsPanel")).toBeVisible();
  await expect(rows(page)).toHaveCount(0);
  await expect(page.locator("#cronsBody .cron-message")).toHaveText(vm.message.text);
  await expect(page.locator("#cronsBody .cron-message")).toHaveCSS("color", rgb(GREEN_DIM));
  await expect(page.locator("#cronsTrailing")).toHaveText("all ok");
});

/**
 * The blind read. An org with no crons, a mistyped slug and an under-scoped token
 * are indistinguishable, so none of them may render as a calm empty panel — and
 * the trailing label must not contradict the message beside it.
 */
test("an empty monitor list paints an error, never an empty green panel", async ({ page, baseURL }) => {
  const vm = await gotoWithCrons(page, baseURL, await fixture(baseURL, "sample-crons-blind.json"));
  await expect(rows(page)).toHaveCount(0);
  const message = page.locator("#cronsBody .cron-message");
  await expect(message).toHaveText(vm.message.text);
  await expect(message).toHaveCSS("color", rgb(RED));
  await expect(message).not.toHaveCSS("color", rgb(GREEN_DIM));
  await expect(page.locator("#cronsTrailing")).not.toHaveText("all ok");
});

/** A failed read is red and names the failure. */
test("a failed read paints the failure Rust named", async ({ page, baseURL }) => {
  const vm = await gotoWithCrons(page, baseURL, await fixture(baseURL, "sample-crons-error.json"));
  await expect(rows(page)).toHaveCount(0);
  const message = page.locator("#cronsBody .cron-message");
  await expect(message).toHaveText(vm.message.text);
  await expect(message).toHaveCSS("color", rgb(RED));
  await expect(page.locator("#cronsStale")).toBeHidden();
});

/**
 * The footer is Rust's text and Rust's colour, and it lives in the header — a
 * line under the body would make the card taller the moment the panel degraded,
 * and `.panel-row` would stretch every neighbour to match.
 */
test("a degraded read carries its reason in the header, not under the body", async ({ page, baseURL }) => {
  const vm = await fixture(baseURL, "sample-crons.json");
  vm.footer = { text: "⚠ Sentry API request failed (HTTP 503) · last ok 10m ago", color: AMBER };
  await gotoWithCrons(page, baseURL, vm);

  const stale = page.locator("#cronsStale");
  await expect(stale).toBeVisible();
  await expect(stale).toHaveText(vm.footer.text);
  await expect(stale).toHaveAttribute("title", vm.footer.text);
  await expect(stale).toHaveCSS("color", rgb(AMBER));
  // …and the rows Rust carried forward are still on screen under it.
  await expect(rows(page)).toHaveCount(vm.rows.length);
});

/** A fresh, healthy panel renders no warning — that is what makes one mean something. */
test("a fresh panel renders no footer at all", async ({ page, baseURL }) => {
  await gotoWithCrons(page, baseURL, await fixture(baseURL, "sample-crons.json"));
  await expect(page.locator("#cronsStale")).toBeHidden();
});

/**
 * Monitor slugs, project slugs and Sentry status words are remote strings, and a
 * webview parses markup. Every one of them is set with textContent.
 */
test("a monitor name is rendered as text and never as markup", async ({ page, baseURL }) => {
  const vm = await fixture(baseURL, "sample-crons.json");
  vm.rows[0].label = '<img src=x onerror="alert(1)">';
  vm.rows[0].detail = 'platform/<script>alert(1)</script> · error';
  await gotoWithCrons(page, baseURL, vm);

  const el = rows(page).nth(0);
  await expect(el.locator(".cron-name")).toHaveText(vm.rows[0].label);
  expect(await el.evaluate((n) => n.querySelector("img, script"))).toBeNull();
});
