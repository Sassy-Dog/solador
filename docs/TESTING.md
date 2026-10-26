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
device that appears later than that is not covered. A re-run of the SAME
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
simulator runtime images never match). `diskutil image attach`, the
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
