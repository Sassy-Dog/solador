# Solador testing and dev commands

> Moved verbatim from `AGENTS.md` (#638), which is held under the org's 32 KiB guidance budget. Edit it here; `AGENTS.md` points at this file.

## Quick Commands (detail)

### Quick Commands
- `./dev` — Build and run (debug)
- `./dev run --release` — Run release build
- `./dev build` — Compile the cockpit binary (plain cargo, no bundle).
  `./dev build --bundle` and `./dev build --release` (= `./prd`) go through
  `cargo tauri build` and produce `Solador.app`; see `docs/RELEASING.md`
- `./dev agent` — Cross-compile the metrics agent's four published targets
  (#390). `--targets "<triples>"` narrows it, `--sign` minisigns each artifact;
  see `docs/RELEASING.md`
- `./dev test` — Root Rust workspace (`cargo test --locked --workspace` —
  `crates/*`, `app/src-tauri`), `agent/deploy/lib_test.sh`, the versioning
  suite (`scripts/versioning-test.sh`, #405 — the CalVer derivation, the
  mint and its output contract, the build number, and #404's release-tag
  assertion, plus the agent's derivation, mint, `--agent` assertion and
  `publish --agent` (#490), against temporary bare origins), the agent feed's
  publish-time guards (`scripts/agent-feed-guard-test.sh`, #491 — the freeze and
  latest guards against a stub `gh`), the local run/signing helper
  suite (`scripts/run-test.sh`), the signing-identity refusal test
  (`scripts/signing-identity-test.sh`, #474), the `.dmg` layout test
  (`scripts/dmg-layout-test.sh`, #539 — real images from a dummy bundle: the
  `Applications` symlink must be present and point at `/Applications`; and,
  #586, only `hdiutil attach`'s transient EAGAIN is retried; #629, a failed
  attempt's device is swept, unless it appears more than 2 s after the last
  failure), the
  notarization outcome test (`scripts/notarize-outcome-test.sh`, #551 — a stub
  `xcrun`: a timed-out wait is "still in progress", never "rejected"), the e2e server's bind test
  (`tests/frontend/csp_server_test.py`), plus the `tests/frontend` Playwright
  e2e suite
- `./dev lint` — `cargo fmt --check` + `cargo clippy`, plus `bash -n` and
  `shellcheck -S warning` over every shell source this repo ships
  (`agent/deploy/*.sh`, `scripts/*.sh`, `crates/hostsetup/remote/*.sh`, `dev`, `prd`), `scripts/shellcheck-pin-guard.sh`
  (#548: fails on any local ShellCheck other than `SHELLCHECK_VERSION`, naming both
  and how to install the pinned one; CI downloads that release and verifies
  `SHELLCHECK_SHA256_LINUX_X86_64` rather than using the runner image's) with its
  negative control `scripts/shellcheck-pin-guard-test.sh` (stub binaries), the secrets guard and
  its mutation corpus (`scripts/secrets-guard.sh`, `scripts/secrets-guard-test.sh`),
  `scripts/agent-deps-guard.sh`'s `cargo tree` assertion that `agent/`
  does not resolve `crates/updatefeed`, `crates/agentrelease`, `crates/hostsetup` or `crates/subscription`, and `scripts/tauri-cli-pin-guard.sh`
  (#485: `TAURI_CLI_VERSION` and `Cargo.lock`'s `tauri` must share major.minor,
  text-only) with its negative control `scripts/tauri-cli-pin-guard-test.sh`, and
  `scripts/claude-dir-guard.sh` (nothing named `.claude` is tracked except
  `.claude/sassy-dog/<name>.md`) with its negative control
  `scripts/claude-dir-guard-test.sh`, and `scripts/agent-guide-size-guard.sh`
  (#638: every chain of tracked `AGENTS.md`/`CLAUDE.md` files is at most
  32,768 bytes) with its negative control `scripts/agent-guide-size-guard-test.sh`
  — the scripts CI's `secrets-guard` job runs; mirrors CI
- `./dev format` — `cargo fmt`
- `./dev clean` — `cargo clean` (all of `target/`) plus the pre-Tauri `build/`
  and Playwright's `test-results/`. Cargo never prunes superseded dependency
  versions' artifacts, so `target/` only grows; `./dev clean --over-budget`
  cleans only above `TARGET_BUDGET_GB` (`scripts/config.sh`, 15) and is what
  tidy-repo runs, so the latest build survives a tidy and the accumulation does not
- `./dev publish` — mint the CalVer tag, then build a signed, notarized,
  stapled `.dmg` (#306). **Pushes a real tag**; `--skip-mint` runs the same
  build without one. `./dev publish --agent` mints and pushes the agent's own
  `agent-vYYYY.M.N` tag instead (#490) and stops — no `.dmg`, none of the
  cockpit's credentials, and a **real tag** too

> `./dev lint` is the local mirror of CI's Rust lint gates, which live inside the
> `rust-workspace` and `agent-tests` jobs. `./scripts/install-hooks.sh`
> (one-time) wires it to a pre-push hook so lint failures never reach CI.
>
> `agent/` is a workspace member, so `./dev test` and `./dev lint` cover it.
> Its CI job (`agent-tests`) runs on Linux and is scoped `-p solador-agent`:
> a bare `cargo build` there would resolve the whole workspace, including
> `app/src-tauri`, which needs webkit2gtk libraries the runner does not have.

## Testing

```bash
./dev test
```

Runs `cargo test --locked --workspace` (`crates/*`, `app/src-tauri`),
`agent/deploy/lib_test.sh`, `scripts/versioning-test.sh` (the versioning
scripts — #405 — against temporary bare origins: `get-version-info.sh`'s
derivation and §4 mint with the `action ∈ {create, reuse}` and
reuse-pushes-nothing contract, `get-build-number.sh`, and the release
workflows' tag assertion from #404, and since #490 the agent's derivation, mint, `--agent` assertion and `publish --agent` (`gh` stubbed through `PATH`); ~400 cases, run under bash 5, macOS
`/bin/bash` 3.2 and Git Bash in CI), `scripts/agent-feed-guard-test.sh` (#491:
the agent feed's freeze guard — only the newest published `agent-v*` release may
publish a feed, read from the paginated release list — and latest guard, against
a `gh` stub that serves one page unless asked to paginate; bash 5 and macOS
`/bin/bash` 3.2 in CI), `scripts/run-test.sh` (`./dev run`'s
signing-identity choice: the exact-identity Apple certificate trust gate, and
the warned ad-hoc run where no Apple Development certificate is installed —
only an Apple-issued certificate stops the keychain prompts; see app/README.md,
"Signing for ./dev run"),
`scripts/signing-identity-test.sh`
(#474: `log_error` and `log_warning` write to stderr, so a refusal inside a
`$(fn)` caller such as `resolve_signing_identity` is seen, not captured),
`scripts/dmg-layout-test.sh` (#539: the `.dmg` is built from a staging
directory holding the `ditto`-copied app and an `Applications` symlink, and
`build.sh` mounts the finished image read-only and fails unless its root is
exactly those two; the test builds real images from a dummy bundle and proves
the assertion refuses a missing or mis-targeted link. The mount retries only
`hdiutil attach`'s "Resource temporarily unavailable", 5 tries with a 2-8s
backoff, because that transient error ejected a merge-queue group (#586); any
other failure is final at once, and a stub `hdiutil` proves both. **A failed
attempt's device is detached, not left behind (#629):** `hdiutil attach` can report failure after it
attached, and leaked devices were present both times the suite failed with
EAGAIN locally. Leaks are one observed correlate, not established as the
cause: the same EAGAIN was also seen with no fixture device attached, most
plausibly from concurrent `hdiutil attach` activity, so the retry stays. Every
failed attempt detaches whatever now stands on that image path before the next
one (never a device that stood there before the attach: the release image's
path repeats across builds and an earlier one may still be mounted), which is
what makes retrying safe. The last failure waits 2 s and sweeps once more; a
device that appears later than that is not covered. **Every failed attempt
records what it found (#642)**, before the sweep: one `dmg attach:` line on
stderr with the attempt number, how long `hdiutil attach` took, its exit status,
the (device, image) pairs standing on this image path (any that stood there
before the attach marked `pre-existing`), whether the mountpoint is mounted,
and the *count* of images attached in total (never another image's path), and the
first line of that attempt's error. Two more such lines end a give-up: one
after the final 2 s settle but *before* the last sweep (the sweep detaches
silently, so this is the only trace of a device that appeared during the
pause) and one after it, reading what remains. It exists to answer what macOS 27.0.1's
EAGAIN means: whether `hdiutil attach` reports it for an attach that succeeded,
and whether a device appears after the last failure. The lines are read-only
(they never detach), a read that fails prints `unknown` in its fields and
changes no outcome, and a first-try success prints none; search a log for
`dmg attach:`. The success test and the retry set are unchanged, so nothing
here is a fix. A re-run of the SAME
version names it (it is kept, never detached) on its next attach, successful or
not; once the CalVer moves the image path (`<product>-<version>.dmg`) changes
and nothing revisits the old one, so `hdiutil info` is how to see it. Detach is by the whole-disk device node
(`attach_dmg_readonly` leaves it in `ATTACHED_DEV`; a mount point names nothing
once the unmount succeeds and the eject does not), retried with a backoff,
`-force` only on the last try, then checked against `hdiutil info -plist` (read
with `plutil`). A device is the pair (device, image path), never its number:
macOS hands a freed `/dev/diskN` to the next image at once, so the pair is
re-checked before every detach and none is sent, `-force` included, once it
no longer matches. A listing with no images (a CI runner's usual) is empty; one
that cannot be read is a failure, never "nothing attached". A device that will
not go fails `assert_dmg_layout`, naming it and `hdiutil detach -force <dev>`:
the check runs before signing, so a re-run is cheap, and a release machine that
shrugged would accumulate devices. That holds for a detach that fails after a
successful attach and a passing layout check: on GitHub Actions that case only
warns (the runner is thrown away, and failing would cost a `prd` approval and a
rebuild for a correct artifact). A device that cannot be detached during a
failed attempt fails the build everywhere. The
suite's EXIT trap detaches everything under its temp dir before deleting it,
ends by asserting nothing is left, and at start reaps earlier runs' leaks by
one rule (`dmg_is_leaked_fixture`: `$TMPDIR/tmp.*/<fixture>.dmg` with the
backing file gone — a live run's images, `~/Downloads` installers and the
simulator runtime images never match). **What `hdiutil create` leaves on a
fixture the suite has just written is cleared before anything attaches it
(#683):** under load the create can leave a device standing on the new image,
which the next attach would keep as `pre-existing` and then give up behind.
Every fixture the suite creates with its own `hdiutil create` goes through
`make_image`, which detaches every device on that exact path (no keep list: the
file is new, in the run's own temp dir, and nothing has attached it yet) and
prints one `cleared <device>: attached on <image> right after hdiutil create
wrote it` line per device, the `--probe-die` child's included; a listing it
cannot read, or a device that will not detach, fails the run instead. The
suite's two `make_dmg` cases build their images through `make_dmg`'s own
create, the one the release path runs, and that create is not covered. `diskutil image attach`, the
replacement macOS 27 names, is deliberately not used: it works there but
returns bare `diskN` entries, has no `-noverify` and is unmeasured on the 14.0 floor and the runner,
so one `hdiutil` path serves the runner and the 14.0 floor. macOS only,
skips itself elsewhere), `scripts/notarize-outcome-test.sh` (#551: `notarize_and_staple`
classifies a failed `notarytool submit --wait` by Apple's submission status
from `notarytool info`, never by the exit code — the wait is 60 minutes, and
one that runs out reads "still in progress" and, locally, gives the
`notarytool wait` → staple resume path plus the two caveats that it yields a
`.dmg` only (staple the `.app` too, no updater payload), while with
`GITHUB_ACTIONS=true` it says the runner's artifacts are gone and the remedy is
re-running the release job, never "do NOT resubmit" (both cases are tested; the
suite pins `GITHUB_ACTIONS` itself); a rejection prints Apple's log, a submit with no id says it never
happened, and only Accepted staples; lifted out of `build.sh` and run against a
stub `xcrun`, so nothing is submitted; `NOTARIZE_SOURCE` points it at another
copy of `build.sh` for the negative control), `tests/frontend/csp_server_test.py`,
and the `tests/frontend` Playwright suite. Agent tests run via `cargo test` in
`agent/`.

**The e2e server binds without a name lookup, and the 60s webServer deadline
is not the knob (#401).** `tests/frontend/csp_server.py` serves `app/ui` under
the shipped CSP on a worktree-derived port, and its `LoopbackServer` overrides
`HTTPServer.server_bind` to skip the `socket.getfqdn()` the stdlib performs on
the bind address. On GitHub's hosted macOS runners that reverse lookup of
`127.0.0.1` goes to the VM's NAT resolver, which never answers, and it cost
**35.0s of a 36.6s readiness window** — measured in place and reproduced in
isolation, against 0.06s for the same bind without it. A `getfqdn()` of the
runner's own hostname took 70s on the same resolver, which is the shape of the
two 60s timeouts that opened the issue — consistent with them, not read out of
them, since those runs predate piped server output. The lookup answers in
milliseconds on a laptop, so `csp_server_test.py` **asserts** the bind
resolves no name (every resolver entry point patched to raise, plus a negative
control on the stock server) rather than timing anything; it runs from
`./dev test` and as its own CI step. The server prints a line before the bind
and one after it, each with its elapsed time, so a timeout names the phase it
reached and a creep-back is readable from any green run's `[WebServer]` lines.
Read those before touching the deadline: a larger one with no explanation
converts a loud failure into a slow one.

**A multi-frame cockpit stub is advanced by the test, never by call count
(#407).** `layout.spec.js`'s `stubCockpit(page, [frame1, frame2])` serves
frame 1 to every `invoke` until the test calls the returned `advance()`; only
then does the app's own 1s poll deliver frame 2. The call-counted stub it
replaced handed out frame 2 from the second `invoke` on, so every frame-1
assertion after `gotoApp` was racing the 1000ms cadence against Playwright's
latency, and a loaded merge-group runner lost that race and ejected #406.
The window is intra-test and independent of #401's server-startup one. A
longer timeout on the frame-1 assertion cannot help — once frame 2 has been
served, frame 1 is gone — so do not reach for one; hold the frame instead.

**Per-panel specs enter a tile's Full view (#645); `?view=details` is gone (#647).**
`azure`, `containers`, `crons`, `github`, `openclaw`, `services` and `usage` use
`tests/frontend/tile-full.js`: it stubs the IPC, boots the overview with one
`all`-scoped tile per source, and opens the panel through the tile's View button
then **Full view** (`tileFull`), the one way in; there is no URL or hook that
skips the menu. `status-layout` keeps its own multi-frame stub but builds its
Full-view tiles with the same `dashboardWithTiles` and enters them with the same
`openTileFull`; its clipping test walks all nine sources' Full views. A tile's
Full view is headed by the tile's own name (#640), not the payload's `title`,
so a heading assertion needs a payload title that differs from the tile's name:
several fixtures' titles equal it, and an assertion that cannot tell the two
apart pins neither. The view shows only its own panel, so a spec about two
panels (`github.spec.js`) leaves and enters each tile, and a spec measuring a
panel first asserts it is visible: a panel the Full view hides has no box, and
a width or overflow check on it passes for any content. Each of the seven
tile-full.js specs asserts its renderer sends `{tile: <id>}`;
`{ __frames: [a, b] }` plus `advance(page, command)` holds a multi-frame panel
answer the way `stubCockpit` does. `layout.spec.js`, `settings.spec.js` and
`screenshots.spec.js` enter a tile's Full view the same way (`overview.png` and
the nine `panel-*.png` are the only shots).

**The overview and a tile's Full view are tested with a bare `cockpit`
(#646, #647).** Solador's Full view is gone, so `overview-standalone.spec.js` no
longer has anything to take away but the `cockpit` answer itself: its stub gives
the overview and Settings only what `dashboard_view` carries. `#fullView`, the
host of every panel, stays (`app/README.md`, "The overview and a tile's Full
view stand on one page"). What it asserts, each clause one test: Settings opens and
closes over the overview, and over Machines, GitHub Runners and Sentry Crons Full
views, which Escape then leaves for the overview with focus on the tile's View
button (Escape from the overview is not tested); the Settings button and the
gaps come from `dashboard_view` (`labels.settings`, each tile's `rowGapPx`,
`spacing`); a `cockpit` rejection leaves the overview up with
`labels.cockpitFailed`, and the line clears when `cockpit` answers again; a
Machines tile's Full view whose `cockpit` read failed shows that same sentence
rather than a loading line, and shows its cards once a read succeeds (#667); a Machines tile removed while its Full
view is open shows the tile-gone message and never that line, even when a
`dashboard_view` that still has the tile is in flight, and a `cockpit` read that
keeps failing asks whether the tile is gone once; a rejected `dashboard_view`
shows the `cockpit` payload's `overviewLoadFailed`, taking it the moment it
arrives if the notice is already up with the transport's text; opening a
tile's Full view reads its own source's command and no other, `cockpit`
included (Machines reads `cockpit` with its `tile`), over the reads made on
opening and the next 1s polls, not the slower panel timers; and `cockpit`'s
width falls back to the window's when `#cockpit` measures zero. A `{ __throw }`
answer to `cockpit` is how a spec makes it fail; `stubTileIpc`'s `dashboardKeys`
sets top-level `dashboard_view` keys the fixture does not.
`dashboard.spec.js`'s own stub holds one tile read (`__HOLD_TILE__`), sets
`window.__TILE_PARKED__` once the call is parked (wait on it before Escape or
`__RELEASE_TILE__()`, or fail it with `__FAIL_TILE__()`, #656), refuses a
tile's read with `__REJECT_TILE__`, and parks every tile read of one command
with `__HOLD_ALL_TILE__` (resolvers in `__RELEASE_ALL__`) so a poll cannot land
first.
