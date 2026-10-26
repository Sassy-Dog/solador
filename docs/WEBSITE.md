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
from `tests/frontend/og-card.html`. Its cockpit stub refuses any grid width it
has no dump for, rather than paint a layout computed for another one.
