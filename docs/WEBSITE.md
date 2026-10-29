# Solador website (`site/`)

> Moved verbatim from `AGENTS.md` (#638), which is held under the org's 32 KiB guidance budget. Edit it here; `AGENTS.md` points at this file.

## Website (`site/`, solador.app)

The marketing site is **static, in this repo, and makes product claims**, so the
docs rule applies to it like any README: a change that alters something the site
says (a panel, a permission, an install step, a platform floor) updates `site/`
in the same PR. It is plain HTML/CSS and two modules (`site/assets/download.js`
for the download buttons, `site/assets/copy.js` for copy buttons that ship
`hidden` and appear only where the clipboard API exists), no bundler, under a
`<meta>` CSP as strict as the app's — GitHub Pages cannot
send headers, so `frame-ancestors` is the one directive it does not carry.
Paths are **root-absolute**, because Pages serves `404.html` at any missing
path and a relative stylesheet breaks below `/`. There are **no analytics**:
the README's "no telemetry, ever" is the product's promise, and the site keeps
it. Conversion is the release download counts.

**Deploy:** `.github/workflows/pages.yml`, on a push to `main` touching `site/`
(or by hand). `deploy-pages` authenticates with the job's OIDC token, scoped to
the `github-pages` environment, so it holds **no credential** and
`scripts/secrets-guard.sh` needs no allowance for it. The custom domain lives in
the repository's Pages settings, not a `CNAME` file (an Actions-deployed site
ignores one); DNS is on Cloudflare, DNS-only, so GitHub issues the certificate.

**Download buttons never fabricate a URL.** Each link's static href is the
releases page; `download.js` replaces it only with the `browser_download_url`
GitHub returned for an asset found by **exact name** in a `v*` latest release
(`Solador-<v>.dmg`, `Solador_<v>_x64-setup.exe`). The API down, a rate limit, a
missing asset or an unexpected tag all leave the releases page in place, and
`tests/frontend/site.spec.js` holds each of those cases — every routed API
response carries CORS headers, so a fallback cannot pass because the browser
refused the response. The same suite serves `site/` through `page.route` (no
second server, so no #401 bind stall) and asserts no CSP violation and no
missing local link or asset on every page.

**The coding-agent prompt lives in two files.** README.md's **Let a coding agent
add a host** and `/setup/#coding-agent` carry the same text, and `site.spec.js`
reads the README's block and requires the page's to match it word for word, and
the copy button to put exactly that on the clipboard. Change the prompt in the
README and the page together; a root `README.md` change selects the browser
suite in CI (`scripts/ci_changes.py`'s `FRONTEND_SOURCES`), so a README-only edit
cannot skip that comparison. It steers an agent by the CLI's own words, so the
same suite also requires every flag it names to be in `host add`'s usage
(`crates/hostsetup/src/cli.rs`'s `HELP`) and every line it quotes ("Would
pair", "first:", "note:") to be one `crates/hostsetup/src/remote.rs` prints.
What it cannot check is the flow: `--dry-run` refuses, rather than plans, while
a flag the host needs is missing, so the prompt loops on the refusal. A change
to how `remote::plan` refuses is a change to the prompt.

**Screenshots are the showcase fixtures.** `npm run screenshots` renders the
`--showcase` dumps — a working morning with exactly one tile out of true, held by
`the_showcase_dashboard_has_exactly_one_tile_out_of_true` — into
`site/assets/screenshots/`, which the README shares, plus `site/assets/og.png`
from `tests/frontend/og-card.html`. The nine `panel-*.png` are a tile's **Full
view** (View, then Full view) of the showcase readings. Full view lays a panel out
in columns across the whole window, so the window is sized per panel in
`tests/frontend/screenshots.spec.js` to the shape its card needs: `.tile-shot img`
crops each image into a 311x195 box (330x195 at 2-up, 328x195 at 1-up) with
`object-fit: cover`, so the image's shape decides what the card shows. 908pt for
Repos and Runners, 462pt for the panels that stack (Containers, Azure Cost,
Usage, Crons, Services, OpenClaw), 900pt for Hosts. Every card is anchored top
left except Usage, which `.tile-shot-bottom` anchors bottom left: its panel is a
stack of independent sections, its top-left (the Claude rollups) is already
claimed under "Useful before you configure anything", and its card text names
the Neon, Sentry and Vercel consumption at the bottom. The card's words no
longer name the Claude and Codex plan limits; the Codex plan rows still show,
unnamed, at the top edge of the crop. The limits are claimed on `/setup/` and in
the README. The showcase Usage payload is `usage::Fixture::Showcase`: the
measured fixture plus a Vercel reading, which the e2e suite's `Measured`
deliberately lacks. Containers keeps the remote host under this machine's
(the generator asserts it is inside the crop), the generator asserts that Usage's
Neon, Sentry and Vercel sections sit inside its bottom-anchored crop at all three
boxes with the panel's full width (a two-column Usage at 908pt fails it), and
`site.spec.js` asserts only Usage's image is anchored bottom, which ties the CSS
to that assumption. The Hosts alt names only the CPU section ("overall and
per-core CPU sparklines"), because that is all every layout's crop shows (memory,
disk and network sit lower), and the generator asserts the overall chart and every
core sparkline lie inside the crop at all three boxes. Every screenshot's
`width`/`height` in `site/index.html` is the file's pixel size (`site.spec.js`
compares them; `mark.svg`'s sizes are not checked). The wide end of the 2-up and
1-up layouts (near 960 and 640px) shows less of every tall card, Usage's Neon
rows included; 1280, 800 and 390 are the checked widths. Azure
Cost's "Azure Database for PostgreSQL" is still cut by the panel's own ellipsis at
462pt. There are no other cockpit shots: `cockpit.png` and `cockpit-narrow.png`
went with Solador's Full view (#647).
Its cockpit stub refuses any grid width it has no dump for, rather than paint a
layout computed for another one; each viewport has its own `--width` dump in
`fixtures:showcase` (a tile's Full view asks at the window's own width as well as the
overview's, so Hosts' 900pt needs a 900 dump beside the 868 one).
