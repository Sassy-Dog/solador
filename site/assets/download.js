// Points the download buttons at the current signed release's installers.
//
// The never-fabricate rule, applied to a button. Every `[data-platform]` link
// ships with the releases page as its href, which is correct with JavaScript
// off, with the API down and when the visitor is rate-limited. This script only
// ever *replaces* that with a URL GitHub itself returned for an asset it found
// by exact name. It never builds a download URL out of a version string: a
// guessed link is a 404 the moment a release is shaped differently, and a
// fallback that still works is better than a direct link that might not.
//
// `/releases/latest` is the desktop release by construction: agent releases are
// created with `--latest=false`, and `scripts/agent-feed-guard.sh` asserts that
// "latest" names a `v*` tag. The tag check in `pickAsset` holds the line anyway.

const REPO = "Sassy-Dog/solador";
const LATEST_API = `https://api.github.com/repos/${REPO}/releases/latest`;

/** The exact asset name each platform's button wants, for a CalVer like 2026.10.14. */
export const ASSET_NAMES = {
  mac: (version) => `Solador-${version}.dmg`,
  windows: (version) => `Solador_${version}_x64-setup.exe`,
};

/**
 * The release asset this platform's button should point at, or `null` to leave
 * the button on the releases page.
 *
 * `release` is the GitHub API's latest-release object: `tag_name` (e.g.
 * "v2026.10.14"), `published_at`, and `assets`, each with a `name` and a
 * `browser_download_url`. `platform` is a key of `ASSET_NAMES`.
 *
 * @param {object} release
 * @param {"mac" | "windows"} platform
 * @returns {{ name: string, browser_download_url: string } | null}
 */
export function pickAsset(release, platform) {
  const nameFor = ASSET_NAMES[platform];
  // A strict desktop CalVer tag (docs/VERSIONING.md: non-padded month, patch
  // floored at 1), the same shape the agent's installer insists on. Anything
  // else, an agent-v tag included, is not this button's release.
  const version = /^v(\d{4}\.[1-9]\d?\.[1-9]\d*)$/.exec(release?.tag_name ?? "")?.[1];
  if (!nameFor || !version || !Array.isArray(release.assets)) return null;
  // Exact name only: a near match is how a stale or mislabelled file would end
  // up behind the button, and the releases page is a fallback that still works.
  const asset = release.assets.find((a) => a?.name === nameFor(version));
  const url = asset?.browser_download_url;
  return typeof url === "string" && url.startsWith(`https://github.com/${REPO}/releases/download/`)
    ? asset
    : null;
}

/** "mac", "windows", or null where neither installer runs (Linux, phones). */
function detectPlatform() {
  const platform = (navigator.userAgentData?.platform || navigator.platform || "").toLowerCase();
  // iPadOS reports itself as a Mac; a touch screen is how it gives itself away.
  if (platform.includes("mac") && navigator.maxTouchPoints <= 1) return "mac";
  if (platform.includes("win")) return "windows";
  return null;
}

/** "Oct 2, 2026" from an ISO timestamp, or null when there is none to read. */
function releaseDate(iso) {
  const date = iso ? new Date(iso) : null;
  if (!date || Number.isNaN(date.getTime())) return null;
  return date.toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric" });
}

async function latestRelease() {
  // No custom headers: any would turn this into a preflighted CORS request.
  const response = await fetch(LATEST_API);
  if (!response.ok) return null;
  return response.json();
}

function emphasise(platform) {
  for (const group of document.querySelectorAll("[data-download]")) {
    const buttons = group.querySelector(".download-buttons");
    const match = platform && group.querySelector(`[data-platform="${platform}"]`);
    for (const link of group.querySelectorAll("[data-platform]")) {
      link.classList.toggle("btn-primary", link === match);
    }
    // The visitor's installer first; both are always there.
    if (match && buttons) buttons.prepend(match);
    const note = group.querySelector("[data-download-note]");
    if (note) note.hidden = Boolean(platform);
  }
}

async function resolve() {
  let release = null;
  try {
    release = await latestRelease();
  } catch {
    // Offline, blocked, or rate-limited at the network layer: the static
    // links already point somewhere correct.
    return;
  }
  if (!release) return;

  let resolved = false;
  for (const link of document.querySelectorAll("[data-platform]")) {
    const asset = pickAsset(release, link.dataset.platform);
    if (!asset) continue;
    link.href = asset.browser_download_url;
    link.dataset.resolved = "true";
    resolved = true;
  }
  if (!resolved) return;

  const date = releaseDate(release.published_at);
  const text = date ? `${release.tag_name} · ${date}` : release.tag_name;
  for (const slot of document.querySelectorAll("[data-release-version]")) {
    slot.textContent = text;
  }
  for (const link of document.querySelectorAll("[data-release-notes]")) {
    link.href = release.html_url ?? link.href;
  }
}

emphasise(detectPlatform());
resolve();
