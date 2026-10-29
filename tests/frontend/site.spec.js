import { test, expect } from "@playwright/test";
import fs from "node:fs";
import path from "node:path";

/**
 * solador.app (`site/`), served the way GitHub Pages serves it.
 *
 * No second web server: every request to the fake origin below is answered
 * from `site/` by `page.route`, so this suite cannot meet the reverse-DNS bind
 * stall csp_server.py exists to avoid (#401), and needs no port. A directory
 * path serves its `index.html`, and anything missing serves `404.html` with a
 * 404 status, which is what Pages does.
 *
 * GitHub's API is routed too. The download buttons are the one place the site
 * talks to anything, and their rule is the repo's: a link GitHub returned for
 * an asset found by exact name, or the releases page. Never a URL built from a
 * version string. Every routed API response carries CORS headers, the failures
 * included; without them a fallback case would pass because the browser
 * refused the response, not because the script handled it.
 */

const SITE = path.resolve(__dirname, "..", "..", "site");
const ORIGIN = "https://solador.test";
const API = "https://api.github.com/repos/Sassy-Dog/solador/releases/latest";
const RELEASES_PAGE = "https://github.com/Sassy-Dog/solador/releases/latest";
const PAGES = ["/", "/download/", "/setup/"];

const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".txt": "text/plain; charset=utf-8",
  ".xml": "application/xml",
};

/** The file Pages would serve for a URL path, or null for a 404. */
function resolveSitePath(urlPath) {
  const decoded = decodeURIComponent(urlPath);
  const file = path.join(SITE, decoded.endsWith("/") ? `${decoded}index.html` : decoded);
  if (!file.startsWith(SITE + path.sep)) return null;
  return fs.existsSync(file) && fs.statSync(file).isFile() ? file : null;
}

const asset = (name) => ({
  name,
  browser_download_url: `https://github.com/Sassy-Dog/solador/releases/download/v2026.10.14/${name}`,
});

const GOOD_RELEASE = {
  tag_name: "v2026.10.14",
  html_url: "https://github.com/Sassy-Dog/solador/releases/tag/v2026.10.14",
  published_at: "2026-10-02T01:13:24Z",
  assets: [
    asset("agent-latest.json"),
    asset("latest.json"),
    asset("Solador-2026.10.14.app.tar.gz"),
    asset("Solador-2026.10.14.app.tar.gz.sig"),
    asset("Solador-2026.10.14.dmg"),
    asset("Solador_2026.10.14_x64-setup.exe"),
    asset("solador-agent-2026.10.14-aarch64-apple-darwin"),
  ],
};

const CORS = { "access-control-allow-origin": "*" };

/**
 * Opens a page of the site.
 *
 * `release` is what the API answers: a release object (200), a number (that
 * status, with GitHub's error body), or "abort" for a network failure.
 * `platform` is what the browser reports, for the primary-button choice.
 */
async function openSite(page, urlPath, { release = GOOD_RELEASE, platform = "Linux" } = {}) {
  await page.addInitScript((platform) => {
    window.__csp = [];
    document.addEventListener("securitypolicyviolation", (e) => {
      window.__csp.push(`${e.violatedDirective} ${e.blockedURI}`);
    });
    Object.defineProperty(Navigator.prototype, "userAgentData", { get: () => ({ platform }) });
    Object.defineProperty(Navigator.prototype, "platform", { get: () => platform });
  }, platform);

  await page.route(`${ORIGIN}/**`, async (route) => {
    const { pathname } = new URL(route.request().url());
    const file = resolveSitePath(pathname);
    if (!file) {
      return route.fulfill({
        status: 404,
        contentType: TYPES[".html"],
        body: fs.readFileSync(path.join(SITE, "404.html")),
      });
    }
    return route.fulfill({
      status: 200,
      contentType: TYPES[path.extname(file)] ?? "application/octet-stream",
      body: fs.readFileSync(file),
    });
  });

  await page.route(API, async (route) => {
    if (release === "abort") return route.abort("internetdisconnected");
    if (typeof release === "number") {
      return route.fulfill({
        status: release,
        headers: CORS,
        contentType: "application/json",
        body: JSON.stringify({ message: "API rate limit exceeded" }),
      });
    }
    return route.fulfill({
      status: 200,
      headers: CORS,
      contentType: "application/json",
      body: JSON.stringify(release),
    });
  });

  await page.goto(`${ORIGIN}${urlPath}`);
  // The API call is routed, so it counts as network: idle means download.js
  // has had its answer (or its failure) and finished with it.
  await page.waitForLoadState("networkidle");
}

const button = (page, platform) => page.locator(`[data-platform="${platform}"]`).first();

test.describe("the download buttons", () => {
  test("a release with both installers resolves both buttons to GitHub's own asset URLs", async ({ page }) => {
    await openSite(page, "/");
    await expect(button(page, "mac")).toHaveAttribute(
      "href",
      asset("Solador-2026.10.14.dmg").browser_download_url,
    );
    await expect(button(page, "windows")).toHaveAttribute(
      "href",
      asset("Solador_2026.10.14_x64-setup.exe").browser_download_url,
    );
    await expect(page.locator("[data-release-version]").first()).toContainText("v2026.10.14");
  });

  test("a rate-limited API leaves both buttons on the releases page and names no version", async ({ page }) => {
    await openSite(page, "/", { release: 403 });
    for (const platform of ["mac", "windows"]) {
      await expect(button(page, platform)).toHaveAttribute("href", RELEASES_PAGE);
    }
    await expect(page.locator("[data-release-version]").first()).toHaveText("Latest release");
  });

  test("a network failure leaves both buttons on the releases page", async ({ page }) => {
    await openSite(page, "/", { release: "abort" });
    for (const platform of ["mac", "windows"]) {
      await expect(button(page, platform)).toHaveAttribute("href", RELEASES_PAGE);
    }
    await expect(page.locator("[data-release-version]").first()).toHaveText("Latest release");
  });

  test("a release missing one installer resolves the other and leaves that one alone", async ({ page }) => {
    await openSite(page, "/", {
      release: {
        ...GOOD_RELEASE,
        assets: GOOD_RELEASE.assets.filter((a) => !a.name.endsWith("-setup.exe")),
      },
    });
    await expect(button(page, "mac")).toHaveAttribute(
      "href",
      asset("Solador-2026.10.14.dmg").browser_download_url,
    );
    await expect(button(page, "windows")).toHaveAttribute("href", RELEASES_PAGE);
  });

  test("a latest release that is not a desktop v* tag resolves nothing", async ({ page }) => {
    await openSite(page, "/", {
      release: {
        ...GOOD_RELEASE,
        tag_name: "agent-v2026.10.15",
        assets: [asset("Solador-2026.10.15.dmg"), asset("Solador_2026.10.15_x64-setup.exe")],
      },
    });
    for (const platform of ["mac", "windows"]) {
      await expect(button(page, platform)).toHaveAttribute("href", RELEASES_PAGE);
    }
    await expect(page.locator("[data-release-version]").first()).toHaveText("Latest release");
  });

  test("an installer named for a different version than the tag is not taken", async ({ page }) => {
    // Exact names only: a near match is how a stale or mislabelled file would
    // end up behind the button.
    await openSite(page, "/", {
      release: {
        ...GOOD_RELEASE,
        assets: [asset("Solador-2026.10.13.dmg"), asset("Solador_2026.10.13_x64-setup.exe")],
      },
    });
    for (const platform of ["mac", "windows"]) {
      await expect(button(page, platform)).toHaveAttribute("href", RELEASES_PAGE);
    }
  });

  for (const [platform, reported, primary] of [
    ["macOS", "macOS", "mac"],
    ["Windows", "Windows", "windows"],
  ]) {
    test(`a ${platform} visitor gets their installer first and primary, and still sees the other`, async ({ page }) => {
      await openSite(page, "/", { platform: reported });
      const group = page.locator("[data-download]").first();
      await expect(group.locator(".download-buttons .btn").first()).toHaveAttribute("data-platform", primary);
      await expect(group.locator(".btn-primary")).toHaveAttribute("data-platform", primary);
      await expect(group.locator("[data-platform]")).toHaveCount(2);
      await expect(group.locator("[data-download-note]")).toBeHidden();
    });
  }

  test("a visitor neither installer runs on gets no primary button, both links, and a note", async ({ page }) => {
    await openSite(page, "/", { platform: "Linux" });
    const group = page.locator("[data-download]").first();
    await expect(group.locator(".btn-primary")).toHaveCount(0);
    await expect(group.locator("[data-platform]")).toHaveCount(2);
    await expect(group.locator("[data-download-note]")).toBeVisible();
  });
});

/**
 * The coding-agent prompt on /setup/, which README.md also carries. The two are
 * one text in two files, so the page's copy is held to the README's, and the
 * copy button is held to the page's text: what a visitor pastes is what the
 * README says.
 */
function readmePrompt() {
  const readme = fs.readFileSync(path.resolve(__dirname, "..", "..", "README.md"), "utf8");
  const match = readme.match(/### Let a coding agent add a host[\s\S]*?```text\n([\s\S]*?)```/);
  if (!match) throw new Error("README.md no longer has the coding-agent prompt this suite compares");
  return match[1].trimEnd();
}

test.describe("the coding-agent prompt", () => {
  const copyButton = (page) => page.locator('button[data-copy="agent-prompt"]');
  const status = (page) => page.locator('[data-copy-status="agent-prompt"]');

  test("the prompt names only flags `host add` takes and output the command prints", () => {
    // The prompt steers an agent by the CLI's own words, so a renamed flag or
    // reworded line would leave it following instructions that no longer
    // apply. Read both from the sources rather than restating them here.
    const repo = path.resolve(__dirname, "..", "..");
    const cli = fs.readFileSync(path.join(repo, "crates/hostsetup/src/cli.rs"), "utf8");
    const remote = fs.readFileSync(path.join(repo, "crates/hostsetup/src/remote.rs"), "utf8");
    const usage = cli.match(/solador-app host add [\s\S]*?\n\n/);
    expect(usage, "cli.rs's HELP no longer has a `host add` usage block").not.toBeNull();
    // Whole flags on both sides, so `--address` is not satisfied by a usage
    // that only has `--address-foo`.
    const flagsIn = (text) => new Set(text.match(/--[a-z][a-z-]*/g));
    const takes = flagsIn(usage[0]);
    const flags = [...flagsIn(readmePrompt())];
    expect(flags.length).toBeGreaterThan(5);
    for (const flag of flags) {
      expect(takes.has(flag), `the prompt names ${flag}, which \`host add\` does not take`).toBe(true);
    }
    // Only what remote.rs prints: string literals outside its tests and
    // comments, so a comment or a test that keeps an old wording cannot
    // satisfy the check after the output itself changes.
    const shipped = remote
      .split(/\n#\[cfg\(test\)\]\nmod tests/)[0]
      .split("\n")
      .filter((line) => !line.trim().startsWith("//"))
      .join("\n");
    // `\\[\s\S]`, not `\\.`: a Rust literal continues across lines with a
    // trailing backslash, and `.` does not match the newline after it.
    const literals = [...shipped.matchAll(/"((?:[^"\\]|\\[\s\S])*)"/g)].map((m) => m[1]);
    const prompt = readmePrompt();
    const quoted = [...prompt.matchAll(/"([^"]+)"/g)].map((m) => m[1].replace(/\s*\.\.\.$/, ""));
    expect(quoted.length).toBeGreaterThan(2);
    for (const line of quoted) {
      expect(
        literals.some((literal) => literal.includes(line)),
        `the prompt quotes "${line}", which remote.rs does not print`,
      ).toBe(true);
    }
  });

  test("the setup page carries the README's prompt word for word", async ({ page }) => {
    await openSite(page, "/setup/", { platform: "macOS" });
    expect((await page.locator("#agent-prompt").textContent()).trimEnd()).toBe(readmePrompt());
  });

  test("Copy prompt puts exactly that text on the clipboard and says so", async ({ page, context }) => {
    await context.grantPermissions(["clipboard-read", "clipboard-write"], { origin: ORIGIN });
    await openSite(page, "/setup/", { platform: "macOS" });
    await copyButton(page).click();
    await expect(status(page)).toHaveText("Copied to the clipboard.");
    await expect(copyButton(page)).toHaveText("Copied");
    expect((await page.evaluate(() => navigator.clipboard.readText())).trimEnd()).toBe(readmePrompt());
    await expect(copyButton(page)).toHaveText("Copy prompt", { timeout: 5000 });
    await expect(status(page)).toHaveText("");
  });

  test("a clipboard that refuses is not reported as a copy, and the text is selected instead", async ({ page }) => {
    await page.addInitScript(() => {
      navigator.clipboard.writeText = () => Promise.reject(new DOMException("denied", "NotAllowedError"));
    });
    await openSite(page, "/setup/", { platform: "macOS" });
    await copyButton(page).click();
    await expect(status(page)).toHaveText(/^Could not copy\./);
    await expect(copyButton(page)).toHaveText("Copy prompt");
    expect((await page.evaluate(() => window.getSelection().toString())).trimEnd()).toBe(readmePrompt());
  });

  test("with no clipboard API there is no button, and the prompt is still there to select", async ({ page }) => {
    await page.addInitScript(() => {
      Object.defineProperty(Navigator.prototype, "clipboard", { get: () => undefined });
    });
    await openSite(page, "/setup/", { platform: "macOS" });
    await expect(copyButton(page)).toBeHidden();
    await expect(page.locator(".copy-row")).toBeHidden();
    await expect(page.locator("#agent-prompt")).toBeVisible();
  });
});

/**
 * Every screenshot `site/index.html` declares a size for is that size (#672).
 * `npm run screenshots` rewrites the images and nothing rewrites the markup, so
 * a Full-view layout change once left two of them declaring shapes the files no
 * longer had. Read from the PNG header (IHDR), so it needs no browser.
 */
test("every screenshot's declared width and height is its file's pixel size", () => {
  const html = fs.readFileSync(path.join(SITE, "index.html"), "utf8");
  const imgs = [
    ...html.matchAll(/<img src="(\/assets\/screenshots\/[^"]+\.png)"[^>]*? width="(\d+)" height="(\d+)"/g),
  ];
  expect(imgs.length).toBe(10);
  for (const [, src, width, height] of imgs) {
    const png = fs.readFileSync(path.join(SITE, src));
    expect(png.subarray(0, 8).toString("hex"), `${src} is a PNG`).toBe("89504e470d0a1a0a");
    expect({ src, w: png.readUInt32BE(16), h: png.readUInt32BE(20) }).toEqual({
      src,
      w: Number(width),
      h: Number(height),
    });
  }
});

/**
 * The Usage card is the one card anchored bottom left (#685): its text names
 * Neon, Sentry and Vercel consumption, which sit at the bottom of the panel.
 * Every other card is a window onto the panel's top-left. This is what ties
 * `site.css` to the bottom-crop assumption `screenshots.spec.js` checks.
 */
test("only the Usage card's screenshot is anchored bottom left", async ({ page }) => {
  await openSite(page, "/");
  const anchors = await page.$$eval(".tile-shot img", (imgs) =>
    imgs.map((img) => ({
      src: new URL(img.src).pathname,
      position: getComputedStyle(img).objectPosition,
    })),
  );
  expect(anchors.length).toBeGreaterThan(1);
  for (const { src, position } of anchors) {
    expect({ src, position }).toEqual({
      src,
      position: src === "/assets/screenshots/panel-usage.png" ? "0% 100%" : "0% 0%",
    });
  }
  expect(anchors.filter((a) => a.src === "/assets/screenshots/panel-usage.png")).toHaveLength(1);
});

test.describe("every page", () => {
  for (const urlPath of PAGES) {
    test(`${urlPath} raises no CSP violation and every local link and asset exists`, async ({ page }) => {
      await openSite(page, urlPath, { platform: "macOS" });
      expect(await page.evaluate(() => window.__csp)).toEqual([]);

      const refs = await page.evaluate(() =>
        [...document.querySelectorAll("[href], [src]")]
          .map((el) => el.getAttribute("href") ?? el.getAttribute("src"))
          .filter((ref) => ref.startsWith("/")),
      );
      expect(refs.length).toBeGreaterThan(5);
      const missing = refs.map((ref) => ref.split("#")[0]).filter((ref) => !resolveSitePath(ref));
      expect(missing).toEqual([]);
    });
  }

  test("an unknown path deep in the tree serves the styled 404", async ({ page }) => {
    await openSite(page, "/no/such/page/", { platform: "macOS" });
    await expect(page.locator("h1")).toHaveText("One tile out of true.");
    // Root-absolute stylesheet: at this depth a relative one would 404 and
    // leave the page unstyled.
    expect(await page.evaluate(() => getComputedStyle(document.body).backgroundColor)).toBe("rgb(7, 11, 19)");
    expect(await page.evaluate(() => window.__csp)).toEqual([]);
  });
});
