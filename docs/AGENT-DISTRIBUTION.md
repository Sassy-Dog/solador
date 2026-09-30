# Agent Distribution — design

**Status:** implemented. Agreed 2026-08-24; tracked by
[#381](https://github.com/Sassy-Dog/solador/issues/381), split into five
children on 2026-09-07, the last of which (#394, unattended scheduling)
shipped 2026-09-12.

- **§1 Build and publish — SHIPPED** ([#390](https://github.com/Sassy-Dog/solador/issues/390)).
- **§3 Versioning — SHIPPED** with it; `docs/VERSIONING.md` carries the
  reclassification.
- **§2 The feed — SHIPPED, both sides** (producer
  [#391](https://github.com/Sassy-Dog/solador/issues/391), consumer
  [#393](https://github.com/Sassy-Dog/solador/issues/393)):
  `agent-latest.json` and its signature are generated, verified and published
  by `publish-feed.yml` when a release is published, and `solador-agent
  update` consumes them. `install.sh` deliberately does not (§6).
- **§6 Installing — SHIPPED** ([#392](https://github.com/Sassy-Dog/solador/issues/392)):
  `install.sh` downloads and verifies a published binary and has a macOS
  LaunchAgent path. The installer-side half of §5's verification shipped with
  it.
- **§4 Updating — SHIPPED, both halves** ([#393](https://github.com/Sassy-Dog/solador/issues/393),
  [#394](https://github.com/Sassy-Dog/solador/issues/394)):
  `solador-agent update` and `solador-agent rollback` are in the binary, with
  the automatic restore on a failed verification; **unattended checking** is
  `install.sh --enable-timer` — a systemd user timer + oneshot on Linux, a
  second LaunchAgent on macOS — off by default, daily, no catch-up, with the
  scheduling policy recorded in §4's last paragraph.
- **§5 Signing and trust — SHIPPED** (#393): both public keys are compiled
  into the agent. The standby, `agent/release-signing-key-next.pub`, was
  provisioned by `scripts/agent-standby-key.sh` and is committed (it landed
  with #410), so every build since carries the two-key trust set; the
  private half's custody is `docs/SECRETS.md`'s. (The tests still cover the
  one-key shape, which is what a checkout with the file removed builds.)

How the Solador metrics agent reaches machines that are not ours, and how the
people running it stay up to date.

This was written as a **design for a change**. Every section above is marked
SHIPPED and now describes what exists; where the built thing settled
something the design left open (the cadence and missed-check policy in §4,
the unprivileged-ownership rule in §6), the paragraph says which decision
and when.

## Why

The desktop app has a real public distribution story. Release `v2026.8.118`
ships `.dmg`, `_x64-setup.exe` and `.app.tar.gz`, each with a minisign `.sig`,
plus a `latest.json` update feed that Tauri's updater consumes.

The agent shipped **nothing** — zero binary assets on any release — until #390.
It now ships four minisigned binaries beside the app's, and since #392
`install.sh` installs from them. The three consequences below were the
*reason*; all three are addressed, the third by #393 (the command) and #394
(the opt-in schedule that runs it).

The state this was written against, and how much of it still holds: the only way
to install or update the agent was to clone this repository and run
`cargo build --release` on the target host (`agent/deploy/install.sh`). Three
consequences, in order of how much they hurt:

1. **Every monitored machine needs a Rust toolchain.** For an Apache-2.0
   project asking strangers to run an agent on their servers, this is the
   barrier that matters. Nobody installs `cargo` on a NAS to try a dashboard.
   *(Addressed by #392: `install.sh` needs `curl` and `minisign`.)*
2. **macOS has no install path at all.** `install.sh` hard-fails without
   `systemctl` (`"Linux + systemd required"`), while `agent/README.md` states
   the agent "Runs on Linux … and macOS". Half the stated platform support is
   undeliverable today. *(Addressed by #392: a LaunchAgent, §6.)*
3. **Updating is a manual `git pull` + rebuild** on each host. There is no
   mechanism by which a user learns a new version exists. *(Addressed by
   #393: `solador-agent update` fetches, verifies, swaps and verifies the
   restart, and restores the previous binary itself if that fails; #394
   schedules that check daily for hosts that opt in with `--enable-timer`,
   and a host that does not opt in is still told nothing — by design.)*

## Non-goals

- Auto-update enabled by default. Opt-in only.
- Telemetry, analytics, or any phone-home beyond the update check the user
  asked for.
- Distribution via OS package managers (Homebrew tap, apt repo). Defensible
  later; disproportionate now.
- Changing how the desktop app is built, signed, or updated.

## What today already gets right

`agent/deploy/redeploy.sh` encodes three primitives worth preserving verbatim.
They were learned the hard way and the design keeps all of them:

- **Atomic swap around `ETXTBSY`.** A running binary cannot be overwritten in
  place; Linux returns "Text file busy". Write `<bin>.new` and `rename()` it
  over the live path, which the kernel permits while it runs.
- **`<bin>.prev` retained**, so a bad version is one rollback away rather than
  a from-source rebuild on a remote machine while monitoring is down.
- **Post-restart verification.** Poll `/v1/health` and assert it reports the
  expected version — "a running unit alone does not prove the new binary is the
  one serving."

The design changes what *drives* these primitives (a downloaded, signed
artifact instead of a local source build), not the primitives themselves.

## Design

### 1. Build and publish

Four targets:

| Target | Why |
|---|---|
| `x86_64-unknown-linux-musl` | static; runs on any distro |
| `aarch64-unknown-linux-musl` | static; ARM servers, Pi-class hosts |
| `aarch64-apple-darwin` | Apple Silicon |
| `x86_64-apple-darwin` | Intel Macs |

**musl, not gnu, for Linux.** A dynamically linked gnu build fails on hosts
older than the builder with `GLIBC_2.xx not found`. That failure is invisible
to us and fatal to a stranger's first install — the exact class of problem
public distribution exists to solve. The agent's data sources are `/proc` reads
and subprocess calls, so static linking is viable.

Artifacts attach to the **same GitHub Release the app already publishes to** —
one tag, one release, both products. No second release train to keep in sync.

**Raw binaries, not archives**, named `solador-agent-<version>-<triple>` with a
detached `<artifact>.minisig` beside each. That is a consequence of §2's content
hash rather than a packaging preference: the updater compares the published
artifact's hash against the **installed binary**, and an archive hashes
differently from the file inside it — so a tarball would force every host to
download and unpack before it could answer the question the hash exists to
answer *without* downloading.

**Cross-compilation uses `cargo-zigbuild`** (decided 2026-09-07, resolving what
was an open item below). Zig as the linker needs no Docker on the runner, is
faster per target, and musl is precisely its strength. **Both** Linux targets go
through it, not only the cross one: one code path for Linux is what keeps "it
worked on the x86 runner" from being a different build than the ARM artifact.
The two darwin targets use plain `cargo build` — Apple's own toolchain
cross-links between them natively, so zig would only add SDK handling to a path
that does not need it. Pinned as `CARGO_ZIGBUILD_VERSION` in
`scripts/config.sh`, installed from PyPI so one pin brings the driver *and* the
zig it links with.

**Every built binary must have `--version` executed on a matching runner before
publish.** An artifact that does not start is worse than no artifact — and a
cross-compiled binary links perfectly well on a machine that cannot execute one
instruction of it, so this is a claim only a matching machine can make.
`release.yml` therefore runs it on four: `ubuntu-latest`, `ubuntu-24.04-arm`,
`macos-latest` and `macos-15-intel`, against the files the build uploaded rather
than a rebuild. `solador-agent --version` prints the version and nothing else,
so that comparison needs no parsing.

Staticness is asserted out of the ELF, never inferred from the target name:
`file` must say `statically linked` (or `static-pie linked` — rustc's musl
targets can produce either) *and*, where `readelf` is available, the binary must
carry no `PT_INTERP` segment. "Needs no dynamic loader" is the property that
actually matters, and it is the one a target-name check does not verify.

### 2. The feed

A separate `agent-latest.json`, signed, **not** the app's `latest.json`.

Tauri's updater owns `latest.json` on a schema it controls. Adding agent
entries risks breaking desktop updates for a change that has nothing to do with
the app.

Each entry carries the download URL, the signature, and the **content hash** of
the binary. The hash is load-bearing — see versioning below.

**Shipped in #391 — the producer.** The wire contract, which #393's consumer
and this producer are both held to:

- **Discovery.** `https://github.com/Sassy-Dog/solador/releases/latest/download/agent-latest.json`,
  with the detached signature at the same URL plus `.minisig`. A consumer
  resolves the *concrete* release behind `latest` first and downloads both
  files from that tag's `releases/download/<tag>/` URL, so a redirect that
  moves between two GETs cannot pair one release's feed with another's
  signature. Tag-specific assets stay on the existing GitHub release; there is
  no second release train and no hosting service.
- **Document.** Top-level `version` (the release's `YYYY.M.P` CalVer, no `v`)
  and `targets`, an object keyed by the full Rust triple. Each target carries
  `url` (absolute `https://`, naming `solador-agent-<version>-<triple>` on that
  tag), `signature` (the binary's `.minisig` text verbatim, JSON-escaped) and
  `sha256` (64 lowercase hex over the raw executable bytes). Exactly the four
  §1 targets, no more and no fewer; no archives, no app-style `darwin-*`
  aliases, no Windows agent. Pretty-printed, two-space indent, one trailing
  newline, byte-stable across runs.
- **Signature.** `agent-latest.json.minisig` is a plain detached minisign
  signature over the **exact bytes served**, final newline included, under the
  same key as the binaries (§5). A consumer verifies those bytes *before*
  decoding JSON and never verifies a re-serialised object; each binary is then
  independently signature-verified and hashed. The trust root is the committed
  `agent/release-signing-key.pub` — never a key fetched from the release, and
  never `tauri.conf.json`'s. Every signature's **trusted comment is the
  asset's own file name** (`agent-latest.json`, or
  `solador-agent-<version>-<triple>`): the signer sets it, the producer refuses
  a signature that names anything else, and a consumer may hold a download to
  the same rule — a signature that verifies over its bytes but was made for
  another file is a mislabelled artifact.
- **The skip rule is hash equality**, even when the feed's version differs. It
  is *not* a promise that an app-only release yields identical agent bytes —
  the build embeds the CalVer — only that equal bytes mean no swap (§3 says
  what that leaves of the mitigation).
- **The version is checked too, before the hash rule applies.** A consumer
  must refuse a feed whose `version` is not the release it resolved
  (`v<version>` must be the tag it downloaded from) and one that is not newer
  than the CalVer it is running: every release's feed signature is valid on
  its own, so an older, validly signed pair copied onto a newer release would
  otherwise read as "the newest release wants these bytes". The producer's
  `solador-agent-feed verify --version` is the first of those two checks;
  the second needs the running agent's own version and is the consumer's.
- **The fixtures are the contract's executable form.**
  `tests/fixtures/agent/agent-latest.json`, its `.minisig` and
  `test-agent-key.pub` are a complete, signed instance of everything above; a
  consumer's parser must accept that pair under that key, and refuse it after
  any single byte moves.
- **Additions are the only change the producer will make.** A future producer
  may add keys (a rotation-window key id is the obvious one) and will never
  rename, remove or re-type the ones above. The producer's own `verify` is
  strict about unknown keys because it checks the document it just wrote; a
  consumer that wants to update *past* the release that adds a key must not
  be. **The consumer tolerates them** (#393): `agent/src/update.rs`'s
  `Feed` carries no `deny_unknown_fields`, a test adds a key at each level and
  the document still parses, and a malformed *known* field is still refused.
  Extra target triples are likewise ignored — a host reads its own entry —
  provided every entry is well-formed; a malformed entry for *any* triple
  refuses the whole document, because a feed the producer could not have
  written is not one to pick the good parts out of.

Producer-side, `crates/updatefeed::agent` builds the document **only from
verified inputs**: every binary's signature is checked under the committed key
before its hash is computed, and a missing, duplicate or unknown target, a
foreign or lifted signature, a byte that moved after signing, or a URL naming
the wrong asset is a refusal — nothing is written. `publish-feed.yml`'s
`agent-feed` job runs the `solador-agent-feed` binary from the tagged commit,
signs the result with `scripts/agent-signing.sh` (the same signer and key as
the binaries), re-verifies the pair as a consumer would — exact bytes, then
shape, then every binary against its entry — has the reference C `minisign`
read it too, and only then uploads both halves. The job runs when the release
is **published**, never at build time: a draft's assets are not public, and
the feed is assembled from the public URLs. A `release` event runs the
workflow file **at the tagged commit**, so a tag cut before #391 publishes
with its own older file and gets no agent leg at all; the first feed comes
from the first tag cut after #391 merged. See
`.github/workflows/publish-feed.yml` for why that job, and only that job,
holds a credential outside `release.yml`.

### 3. Versioning: shared CalVer

The agent adopts the repo's existing marketing version
(`scripts/get-version-info.sh --version`, CalVer `YYYY.M.<commits-this-month>`).
Agent and app in a given release carry the same number, so "what version are you
on?" has one answer and app/agent compatibility is mostly a non-question.

**The accepted cost, and its mitigation.** The agent gets a new version on every
app-only release, including releases where the agent did not change. A naive
updater would download and restart for an identical binary.

So the updater compares the **content hash**, not the version. Same hash means
stop — no download, no swap, no restart. The version moves; nothing happens.

**How much of that mitigation survives, honestly:** the agent compiles its
CalVer in (`agent/build.rs`, described just below), so today an app-only release *does*
produce different agent bytes, and the hash rule fires only when a release is
rebuilt with no change at all. The rule is still the right one — it is what a
consumer can check without downloading, and it is what makes a genuinely
identical binary free — but "the version moves; nothing happens" is a property
of a build that does not embed the version, and this build does. Making the
agent's bytes version-independent is a separate decision, not implied here.

**This tripped `docs/VERSIONING.md`'s own revisit clause, and #390 made the
edit.** That document classified the agent as N/A — "an internal artifact
hand-deployed to our own hosts … never published to a registry or distributed
externally" — and said to "Revisit at the first artifact that leaves our
machines." These binaries are that artifact, so it now carries **two shipping
tiers on one number**, and the details of how the agent learns its version
(`agent/build.rs` → `crates/buildversion` → `scripts/get-version-info.sh`) live
there rather than here.

Two consequences worth stating where an implementer will hit them:

- `agent/Cargo.toml`'s semver **stopped naming a release**. It survives as the
  wire-contract marker its comment block always was, and nothing reads it at
  runtime — both deploy scripts now ask the built binary (`--version`) instead
  of parsing the manifest.
- A build made outside a full git checkout **carries no version**, and says so:
  `--version` exits non-zero, `/v1/health` omits the key, and the cockpit
  renders `agent version —`. The deploy helper fails closed on it rather than
  verifying against an empty string that `/v1/health` would then "match".

### 4. Updating — SHIPPED (#393)

`solador-agent update`, a subcommand of the binary — not a shell script.
`redeploy.sh` is bash and systemd-shaped; an in-binary command behaves
identically on macOS and Linux, needs no shell, and is testable in Rust.
`agent/src/update.rs` is the implementation; `agent/README.md` is the
operator reference. What follows is the contract as built, in the order it
runs, and every step refuses before the next one changes anything:

1. **Resolve the installed service through #392's contract** — reading
   only: the executable from the systemd user unit's `ExecStart=` (Linux)
   or the LaunchAgent plist's `ProgramArguments` (macOS, read back through
   `plutil`; its fourth argument, the log file, is kept for the failure
   messages), the env file beside it, and the token/bind/port from that
   file — read with `EnvironmentFile=` semantics, never `source`d. Running
   as root is refused outright. An install directory this user cannot write
   to (the pre-#392 root-owned `/opt` layout is the usual case) is refused
   with the installer's `--migrate-from-opt` step; the command never takes
   a privilege it does not have.
2. **Take the transaction lock** — `<bin>.update.lock` beside the resolved
   binary, `flock`-style, non-blocking. A competing `update` or `rollback`
   on the same install (a manual run racing the scheduled one) reports
   *busy*, exits **75**, and changes nothing; the lock dies with the
   process, so a crashed run cannot wedge the next. The one busy that
   is waited out (2 s, bounded) is a lock whose note names *this very
   process*: a `flock` outlives its `File` while any child forked in the
   window between fork and exec still holds an inherited reference, and
   that is a stale reference to our own lock, not another transaction — a
   note naming any other pid is busy at once. It serialises these two
   commands, and — since #439's follow-up review — `install.sh --uninstall`
   (§6), which now holds the same lock for the whole run rather than merely
   checking it once; a normal, no-flag `install.sh` and `redeploy.sh` still
   write the same `.new`/`.prev` without taking it, so they are not to be
   run during an update. Then the **service manager must answer**
   (`systemctl --user
   show-environment` / `launchctl print gui/<uid>`) and must not name this
   process as the service: a manager discovered unreachable at the restart,
   after the swap, is the half-applied update this design exists to prevent,
   and restarting the service must never kill the process that owes the
   health check and the rollback.
3. **Resolve the concrete release** behind `/releases/latest` and fetch
   *that tag's* `agent-latest.json` and `.minisig`, so two independently
   moving redirects can never pair one release's feed with another's
   signature. The feed's **exact served bytes** are verified under the
   compiled-in trust set (§5) before they are decoded, and its `version`
   must be the tag's.
4. **Hash the installed executable** and compare it with the feed's entry for
   this host's triple. Equal bytes mean *already current*: no binary
   download, no `.new`, no `.prev`, no restart — even when the feed's version
   differs (§3 explains why that can happen). Bytes on disk are not a running
   service, though: the endpoint is asked for the version those bytes claim
   (their own `--version`, not the feed's), and "current *and* serving" is
   exit 0, while "current but the service reports something else" — an
   earlier run interrupted between its swap and its restart — is a distinct
   non-zero outcome whose remedy is a restart, never a download.
5. Otherwise **the feed must be newer** than the installed CalVer (read by
   executing the installed binary's `--version`). Every release's feed
   signature is valid on its own, so an older, validly signed pair replayed
   onto a newer release would otherwise read as "the newest release wants
   these bytes". An installed binary that carries no version cannot be
   compared and is **refused**, not assumed older — re-running `install.sh`
   is the way onto a published release from there. A deliberate downgrade is
   `install.sh`'s pinned form, never this command's.
6. **Download into memory** and verify the binary's plain minisign signature
   under either trusted key **and** its SHA-256 against the authenticated
   entry, before a byte reaches disk. Both: the signature says we published
   these bytes, the hash says they are the bytes this feed entry is about,
   and a valid signature over the wrong binary fails the second.
7. **Stage** `<bin>.new` (mode 0755, beside the live path) and execute the
   staged candidate's `--version`; it must be the feed's CalVer. A candidate
   that cannot name it is removed, never installed.
8. **Copy the live executable to `<bin>.prev`** (through a sibling and a
   rename, so a crash mid-copy cannot leave a truncated rollback anchor), then
   **`rename()` `.new` over the live path.** A running executable is never
   overwritten in place — Linux answers `ETXTBSY`, and arm64 macOS kills a
   process whose signed pages change under it — and the live path is never
   absent for even an instant, so a `KeepAlive`/`Restart=always` respawn in
   that window cannot fail to exec.
9. **Restart the metrics service** — `systemctl --user restart solador-agent`
   or `launchctl kickstart -k gui/<uid>/app.solador.agent` — and **poll the
   authenticated `/v1/health`** (the same wildcard→loopback and bracketed-IPv6
   rules as `lib.sh`, fifteen one-second polls) until it reports the
   installed CalVer. A service-manager success, or an HTTP 200 carrying a
   stale or absent `version`, is **not** an installed update.
10. **On any failure after step 8, restore `.prev`** through the same
    stage-and-rename, restart, and require the *previous* version back. The
    command exits non-zero **either way** — **5** when recovery worked (the
    host is on its previous binary and needs a human to look at why), **3**
    when recovery also failed (both failures named, and what is at the live
    path now — the candidate, or the previous binary restored but not
    verified; the service may be down). A recovery that failed is never
    reported as a rollback.

`solador-agent rollback` is the explicit, **offline** form of step 10: no
feed request, no key needed. It refuses when there is no `.prev`, touching
nothing; otherwise it swaps the live binary and `.prev` (so the binary rolled
back over becomes the new `.prev`, and a second `rollback` rolls forward),
restarts, and verifies the restored version where the previous binary can
name one — and **liveness only** where it cannot, saying so, because the one
case rollback exists for is a source-built `.prev` from a shallow checkout
that carries no version.

Exit codes are a contract for the scheduled job: `0` updated, or
already current and serving; `1` failed with nothing changed (and a
`rollback` that did not come back or was left half done — both its own,
named states); `3` failed *and* not restored; `4` no applicable release —
the feed is not newer than what is installed, which a from-source host
running ahead of the last tag answers every day and is not an alert; `5`
failed with the previous binary back and serving; `75` busy (naming the
holder's pid and start time); `2` usage.

**The release base is compiled in** (`RELEASE_BASE`, `agent/src/update.rs`)
and every URL — discovery, feed, binary — is constructed from it and
checked against it, so a feed cannot send a download elsewhere. The
corollary is that **renaming the repository or the organisation strands
every installed agent** at discovery until it is reinstalled from a
checkout; that has happened once already (`devcanopy` → `solador`), and a
second time is a fleet recall, not a rename.

**What is fed to `update` never reaches a shell**: the env file is parsed, the
token goes into exactly one `Authorization` header (the local health probe),
and it appears in no output. From the host's own environment the command
reads `HOME` (where the install is resolved under), the standard proxy
variables for the github.com client only (the health probe of this host's
own service is deliberately proxy-free, so an `HTTP_PROXY` without a
`NO_PROXY` cannot roll back every update), and `SOLADOR_AGENT_LAUNCHD_LABEL`
— the same throwaway-label seam `install.sh` honours, validated the same
way — so the test harness can update a disposable LaunchAgent beside a real
one. Every failure that leaves a service to look at ends with the manager's
status command and the log path.

**Unattended checking ships off by default**, opt-in at install
(`--enable-timer`, SHIPPED, #394): a systemd user timer + oneshot on Linux
(`solador-agent-update.timer` / `.service`), a second LaunchAgent on macOS
(`<metrics label>.update`). People running a monitoring agent on their own
servers should not get surprise restarts; those who want hands-off can ask.
A default install creates no job and makes no check; a re-run without the
flag leaves an earlier opt-in exactly as it is, and the documented
disable/remove commands (`agent/README.md`, **Unattended updates**) revoke
it on its own — `install.sh --uninstall` (#439) revokes it along with
everything else. The job is **separate from the metrics service** on both
platforms, because `update` restarts that service and then verifies and,
on failure, restores it — a job inside the service's cgroup or launchd job
would be killed by its own restart. It runs the installed binary's `update`
with no `sudo`, no checkout, no prompt and no second copy of the token
(the oneshot has no `EnvironmentFile=`, the plist no secret; the updater
reads the env file itself), and is refused before anything is created as
root, on an install directory this user cannot write to, and on an
unmigrated `/opt` host (both flags together migrate and opt in). Its exit
codes are `update`'s: `4` (nothing newer) is `SuccessExitStatus` on Linux
and documented as normal on macOS; every other non-zero exit is a visibly
failed run, never retried before the next day. On macOS the launcher adds
two of its own for the firings it does not hand to `update`: `0` for one it
discarded by design, `6` for one it **held** because it could not read a
clock or its stamp — distinct from every code the agent uses, so a
permanently held job cannot read as a good day in `launchctl print`. The
installer itself exits `3`, not `1`, when the metrics install verified but
the opt-in failed, and prints the install's `Done` block before the opt-in
runs, so a scripted caller can tell the two apart.

**Scheduling policy — decided 2026-09-09 on #394: daily, no catch-up.** One
check per 24 hours while the user session/manager is up; never a check at
enable, boot, login or wake to recover a missed interval; a missed check is
discarded outright — no replay, no coalescing of several misses into one
late run; a session restart begins a fresh day; no boot-without-login
promise on macOS. On Linux the property is the timer's clock:
`OnActiveSec=24h` + `OnUnitActiveSec=24h` are **monotonic**, the first
firing is a day after the timer starts, later ones a day after the last run
started, and the monotonic clock pauses through suspend
(`systemd.timer(5)`) — an `OnCalendar=` timer would fire on resume once its
time had passed during sleep, and `Persistent=` would replay a firing
missed across a stopped manager, so neither is used, nor `WakeSystem=`.
**On Linux the guard is the oneshot's `ExecCondition=`
([#411](https://github.com/Sassy-Dog/solador/issues/411), decided
2026-09-12: guard first, observe when a host allows).** A source trace of
systemd v255's `timer.c` during #394's review found that a `daemon-reload`
after the timer's first day (every installer re-run does one) re-bases the
one-shot `OnActiveSec=` without re-disabling it, and the clock-change
notification a resume delivers then recomputes that deadline from the
timer's original activation — in the past — so the next resume fires the
job once at wake. Nil on a lingering server that never sleeps; real on an
opted-in laptop; ~0.8 confidence from the trace. "Not observed" is not
evidence it does not happen, the guard is cheap, and a check at wake is
exactly the catch-up the policy forbids — so the oneshot runs
`~/.local/bin/solador-agent-update-guard %n` (from
`agent/deploy/update-guard.sh`, installed by `--enable-timer`) before
`ExecStart` on every activation, applying the launcher's two rules: skip a
firing within five minutes of the last resume (or boot) and one within
23 h of the last attempt, recorded in a one-line stamp of the launcher's
format at the same path (the Linux guard writes it 0600; the launcher does
not). The exit mapping is `ExecCondition=`'s own — 0 run, 1 skip cleanly
(unit inactive, `Result=exec-condition`, reason in the journal), 255 fail
the unit (an input unreadable or the stamp unwritable; `Result=exit-code`,
listed by `--failed` until the next activation or a `reset-failed`) — and
each of the three was observed on a real user manager (systemd 256, uid
501) while the guard was designed, along with a fourth fact that shapes
the unit: a condition exit matching `SuccessExitStatus=` *runs*
`ExecStart` (a condition exit of 4 ran the updater), so `lib_test.sh`
asserts the guard's two codes stay off that line. "Seconds since the
last resume" is the difference of two `CLOCK_MONOTONIC` readings, both
unprivileged: this activation's `InactiveExitTimestampMonotonic` from the
user manager and `sleep.target`'s `InactiveEnterTimestampMonotonic` from
the *system* manager (a read-only property fetch over the system bus, which
every user may make; every systemd sleep path pulls that target in and
stops it after the resume). The second reading is **advisory** (decided
2026-09-13, on the review of #416): the
manager garbage-collects `sleep.target` after every cycle (it is
`StopWhenUnneeded=` and nothing else references it on a stock
distribution), so a later `show` reads `0` — measured on systemd 256 in the
same second the journal recorded the target stopping. The kernel's
`/sys/power/suspend_stats/success` tells that `0` apart from a boot that
has not slept, and when the kernel counted a suspend the manager no longer
has, the guard logs one line and falls through to the 23 h rule rather
than hold: the first cut held there, and on a laptop after its first
suspend that was a failed unit on every daily firing until reboot — a job
that never runs, found by the review of #416. Holds are the launcher's own
set (the clock, this activation's time, the stamp, `HOME`), the kernel's
counter when present but unreadable, every usage error, plus an `EXIT` trap
that turns a `set -e` death into a hold, because the status such a death
carries is `1`, the discard. Out, deliberately: the journal (it would
survive the collection, but is readable only in `adm`/`wheel`, which a
dedicated service user is not in) and logind's D-Bus, neither of which is
unprivileged on every host. Two traps found and closed on the
way: a condition binary that is *not there* is exec failure 203, which is
inside the skip range, so a deleted guard would skip every day with
`Result=exec-condition` — the unit's `AssertFileIsExecutable=` on the
guard's path makes that an error line and a failed `start` (no more: an
assertion changes no unit state, so the unit is not in `--failed`), the
installer writes the guard before the unit, the removal recipe takes the
units out first, and a pre-#411 opt-in that a no-flag re-run preserves is
reported `enabled but UNGUARDED` until a flagged re-run retrofits it; and
`ExecCondition=` exists only from systemd 243, so the installer reads the
*running* user manager's `Version` property and refuses the opt-in on an
older one rather than let the key be ignored. The unit also pins `PATH`
to the system directories (the macOS updater plist's decision) and unsets
the guard's test seam. A second, benign effect
of the same re-basing: a reload before the first firing delays that first
check to a day after the reload — later, never sooner. On
macOS the plist's `StartInterval=86400` with no `RunAtLoad` gives the
cadence, and **the launcher is the guard**: `launchd.plist(5)` says a
`StartInterval` firing that falls during sleep is missed, and
`StartCalendarInterval` (which coalesces missed firings into one run at
wake) is not used — but the property is enforced rather than trusted. In
update mode `solador-agent-launchd` refuses, exit 0 with a reason in the
updater's log, any firing within five minutes of the last wake or boot
(`kern.waketime` / `kern.boottime`) or within 23 hours of the last attempt
(a stamp beside the env file, written *before* the attempt), and holds
(exit 6) rather than runs when it cannot read a clock or the stamp, or
cannot write the stamp. Its `PATH` is the four system directories only —
no `/opt/homebrew/bin`, which is group-writable on a stock Homebrew
install, in front of a process that reads the token and renames a binary
over the service. The 23 h is
24 h less the drift a `StartInterval` firing can carry; the five minutes is
generous against launchd's post-wake delivery, and the daily firing it
would occasionally coincide with is exactly the discarded check the policy
allows. This guard never touches the metrics path: three arguments is the
metrics service, four with the literal `update` is the updater, and the
metrics path reads no clock and writes no stamp.

**What has been observed and what has not.** Both guards' every rule is
driven in `agent/deploy/lib_test.sh` with stubbed inputs — the macOS one
with a stubbed clock, wake and boot time; the Linux one with its two
manager reads, the clock and the kernel's counter stubbed (the counter
under an override root, never the suite's own `/sys`), including a server
that never sleeps running on three consecutive days without a hold, and
the installer's actions around it (guard only with the flag, before the
unit, on both unit lines, preserved by a no-flag re-run, refused on
systemd 242 and accepted on 243). The opt-in `SOLADOR_DEPLOY_TEST_LAUNCHD=1`
run bootstraps a real throwaway updater beside a real throwaway agent,
sees it loaded with zero runs, fires it by hand into a read-only `update`
(exit 4 against the published feed, or 1 with no route to github.com —
never a swap; metrics pid unchanged), fires it again to watch the guard
discard it, and removes it while the metrics service keeps running. The
Linux guard's manager readings and its three exit mappings were observed
once, by hand, on a real unprivileged user manager (Fedora CoreOS 41,
systemd 256, kernel 6.12, in a podman machine VM) during #411 — a VM that
proves the *reading* and cannot be suspended. Not observed: a 24-hour
sleep on a real Mac (whether current launchd delivers a `StartInterval`
firing at wake at all — the guard makes the answer immaterial), and a
real systemd user timer taken through `daemon-reload` and a
suspend/resume across its deadline (`journalctl --user -u
solador-agent-update` after such a cycle is the observation still owed;
the suite stubs `systemctl`, CI's Linux runner has no user session, and no
suspend-capable Linux host was available). The reload-then-resume case is
therefore a source trace, and the guard is what holds the cadence whether
or not the trace is right.

### 5. Signing and trust

A self-updating daemon is a remote-code-execution channel into every host that
runs it. If an attacker can convince the agent a binary is legitimate, they own
the machine. This section is the security boundary of the whole design.

**HTTPS is not sufficient.** It authenticates the transport, not the artifact.
It does not survive a compromised release asset, a mirror, or an intercepting
proxy. The signature is what binds the bytes to us.

- The agent verifies a signature over **both the feed and the binary**, before
  writing anything.
- The **public key is compiled into the binary**. Not fetched, not
  trust-on-first-use — a TOFU daemon is defeated by anyone present at install
  time.
- The private key lives in CI secrets. `scripts/build-agent.sh --sign` will use
  a local copy if one is pointed at it — that seam is what lets the signing path
  be exercised without cutting a release — so "never leaves them" is a rule an
  operator keeps, not a property the tooling enforces. A copy made to provision
  the secret should be destroyed once it is in Doppler.

**A separate keypair from the app's.** Compromise of the agent key must not
yield a signed desktop app, and vice versa. The audiences and threat models
differ: the app updates on a person's laptop, the agent runs unattended as a
service on servers, which is the higher-value target.

**Shipped in #390 — the signing half.** The keypair exists: its public half is
committed at `agent/release-signing-key.pub`, its private half is the `prd`
environment secret `SOLADOR_AGENT_SIGNING_PRIVATE_KEY`, and it is not the
cockpit's `TAURI_SIGNING_PRIVATE_KEY`. **The file is the authority on the key's
identity**, not this prose: its id is `B2E5C62B763FD2C4`, read out of the key
bytes by `crates/updatefeed`'s test
`the_committed_agent_key_is_the_provisioned_one_and_its_comment_agrees` and
cross-checked against the comment above them. (An earlier revision of this
paragraph cited an id the file never carried; that is the drift the test
exists to catch.) Signatures are **plain minisign** — the Tauri
signer's extra base64 wrapper is the app's convention and there is no Tauri here
— so anyone can check a download with the reference tool:

```sh
minisign -Vm solador-agent-<version>-<triple> -p agent/release-signing-key.pub
```

Three properties of the release path, each chosen so a failure is loud:

- The signer is the pinned `rsign2` (`RSIGN_VERSION` in `scripts/config.sh`),
  minisign's Rust implementation, through the one implementation in
  `scripts/agent-signing.sh` — sourced by `scripts/build-agent.sh --sign` for
  the binaries, locally and in CI alike, and run directly for the feed (§2).
- **Every signature is re-verified against the committed public key** before
  anything is uploaded. That is what turns a mis-provisioned private key into a
  failed release instead of a release full of signatures nobody can check.
- CI then verifies again with the reference **C** `minisign` from apt — a
  different implementation from the one that signed, so "it verifies" is not
  merely the signer agreeing with itself.

Signing is its own release job and the **only** agent job in `release.yml`
holding a credential, so the key reaches one runner rather than six, and
build/verify start without waiting on `prd`'s reviewer.

**The feed is signed at publish time, by the same key (#391).** The binaries
can be signed during the build because they exist then; `agent-latest.json`
cannot, because it is assembled from the public download URLs of a release that
is still a draft. So `publish-feed.yml` gained one protected job, `agent-feed`,
declaring `environment: prd` and reading exactly that one secret, behind the
same reviewer and the same `v*`-tag-only deployment policy. A credential-free
`agent-eligibility` job in front of it refuses a draft, a prerelease, a release
without the eight agent assets, and — for a manual replay — a run that is not
*at* the tag it was asked about, so a tag typed into an input can never hand a
`main`-ref job the key. `ci.yml`'s `secrets-guard` allows that job by name and
requires its environment line; the desktop feed job beside it stays
credential-free. `scripts/agent-signing.sh` is the one signer for binaries and
feed alike, so the two cannot be signed differently.

**Shipped in #392 — the installer's verifying half.** `install.sh` verifies
every download with the stock `minisign` under `agent/release-signing-key.pub`
*from the checkout it runs from* — never a key fetched beside the binary, and
with no override — before the candidate is made executable, asked its version,
installed, or allowed to stop anything already running. That is §6's contract,
and its test is the proven-to-fail one the Testing section below demands.

**Shipped in #393 — the agent's own verifying half, and the rotation window.**
`agent/build.rs` compiles the trust set in from two files:
`agent/release-signing-key.pub` (its absence is a build failure — an updater
that trusts nothing would call every feed forged) and, when it is committed,
`agent/release-signing-key-next.pub`. `agent/src/update.rs` decodes them with
`minisign-verify` — the crate the producer already uses, so consumer and
producer cannot disagree about what verifies — and holds every signature it
checks (the feed's, the binary's) to the same rules the producer applies: a
prehashed plain-minisign signature whose trusted comment is the asset's own
name, under **either** trusted key. A key listed twice is refused as a trust
set (one key twice is one key), and a signature from a third key names the
ids it did trust in the refusal. Nothing is fetched, nothing is read from disk
at run time, and there is no override: a loopback release base exists for the
tests and cannot be configured into a shipped binary.

**Rotation must ship on day one.** A key compiled into a binary cannot be
rotated by the update path it protects: if it is lost or compromised, every
deployed agent is stranded and every user must reinstall by hand. The agent
therefore accepts **two** valid public keys from the first release, so rotation
is a release rather than a recall.

**The standby, and where its halves live.** `docs/SECRETS.md` is the
authority on the topology; in one line: the active key's private half is
Doppler `solador/prd` → `SOLADOR_AGENT_SIGNING_PRIVATE_KEY`, synced to the
GitHub `prd` environment and read by the two release jobs; the standby's is
Doppler `solador/custody` → `SOLADOR_AGENT_SIGNING_STANDBY_PRIVATE_KEY`, in a
config that syncs **nowhere**, read by nothing. It is provisioned exactly
once by `scripts/agent-standby-key.sh` — non-printing, idempotent, custody
proven with the value retrieved back from Doppler under the stock `minisign`
— and its public half is committed as `agent/release-signing-key-next.pub`.

**Staged rotation, when the day comes** — each step a separate, authorised
decision, and #393 performs only the first:

1. **Ship trust in both keys.** Every agent from this release on verifies
   under the active key *or* the standby. (Done: this is what #393 built.)
2. **Verify fleet uptake.** A host still running a one-key agent will reject
   a release signed under the standby; the update feed's hash rule means it
   will simply stop at "signature does not verify" and keep serving. Confirm
   the hosts that matter report a two-key version (`/v1/health`'s `version`)
   before anything switches.
3. **Switch the active signer, in one release** — separately authorised.
   The trust set has exactly two slots, so a switch that kept the old
   active key trusted "for the window" would need a third; instead the
   release that switches also **replaces the standby**, and hosts that
   updated under the old key still verify it because they already trust the
   (old) standby that is now signing. One PR, naming everything it moves:
   `agent/release-signing-key.pub` ← the old `-next.pub` (the standby S
   becomes the active key); `solador/prd`'s
   `SOLADOR_AGENT_SIGNING_PRIVATE_KEY` ← S's private half, moved out of
   `solador/custody` (delete it there — the custody secret is then empty);
   `agent/release-signing-key-next.pub` removed; then
   `scripts/agent-standby-key.sh` run once to mint S2 into the empty custody
   secret and commit its public half as the new `-next.pub`; and the three
   pins of the old active id updated — `crates/updatefeed`'s
   `the_committed_agent_key_is_the_provisioned_one_and_its_comment_agrees`,
   `agent/src/update.rs`'s trust-set test, and
   `agent/tests/update_flow.rs`'s real-feed assertion. The custody script is
   *unusable* between those steps by design (it refuses a secret without a
   file and a file without a secret), which is why they are one PR.
   `scripts/build-agent.sh --sign`'s re-verification against the committed
   file is what catches a half-done switch at release time.
4. **Confirm the new window.** The release cut from that PR ships trust in
   {S, S2}; hosts that updated under the old active key A before the switch
   trust {A, S} and verify it under S; a host still on a pre-#393 build
   trusts nothing and updates through `install.sh`. A is retired the moment
   no host needs it — which the fleet's `/v1/health` versions tell you.

**What two accepted keys do and do not give, stated so nobody reads more
into them.** They give **continuity**: as long as one of the two private
halves is controlled, a release can be cut that every deployed agent
verifies. They do **not** revoke a compromised key — an agent accepts either
until a release ships that stops listing it, and that release must itself be
signed under a key the agent still trusts. And they are **not** an
anti-replay mechanism: every release's signatures stay valid forever, and it
is §4's *newer-than* rule, not the key count, that stops an older validly
signed feed being replayed onto a host.

**Accepted risk, stated explicitly:** a tag push publishes a release, and
GitHub branch rulesets do not cover tag refs. The release trigger is the least
protected link in this chain. Acceptable for a solo maintainer; it should be a
known acceptance rather than a later discovery.

### 6. Installing — SHIPPED (#392)

`agent/deploy/install.sh` downloads the signed binary for the detected platform
and architecture instead of running `cargo build --release`. This removes the
Rust toolchain from the requirements — the single largest barrier to a
stranger running this. `agent/README.md` is the operator reference; what
follows is the contract, and why each piece is shaped the way it is.

**Discovery is the `/releases/latest` redirect**, validated as a CalVer tag,
or an explicit `SOLADOR_AGENT_RELEASE=vYYYY.M.N`. It needs no API token and no
JSON, and a draft release is invisible to it by construction — the same
property that keeps `latest.json` honest (§2, and `publish-feed.yml`'s
`release: published` trigger). The installer does not read §2's feed and must
not grow a dependency on it: the feed's job is the hash comparison the
*updater* needs, and an install has nothing installed to compare against.

**Discovery is unsigned, and that is an accepted limit of an *install*.**
The `/releases/latest` redirect is authenticated by HTTPS only; an
intercepting proxy could steer a fresh install to an older, validly signed
release, and every check below would pass. That is the downgrade §4's updater
refuses (by its newer-than rule, against the CalVer already installed) — an
install has nothing installed to compare against, and `SOLADOR_AGENT_RELEASE`
is the operator's pin when it matters. A *re-run* does have something to
compare against — the binary already serving — so on the unpinned path it
refuses to install an older CalVer than the one installed, which closes the
steer-to-old-release case for every host past its first install; the
fresh-install window is recorded here beside §5's tag-ruleset acceptance
rather than solved. The two mechanisms should agree that a pulled release is
one marked *prerelease*, which both a feed and this redirect skip.
The public key, meanwhile, must reach the host from `main` — the protected
ref — not from a tag or an archive of one. Until #434, the only path onto
`main` was a full `git clone`; `agent/deploy/bootstrap.sh` is the second one,
and it keeps exactly that property rather than relaxing it: it downloads the
repository **archive at a `main` commit** (or a `--ref` the operator pins to
an exact 40-hex commit SHA) from `codeload.github.com` over HTTPS, extracts
only `agent/deploy/*` and the signing key(s), and runs the extracted
`install.sh` unchanged. **"Reachable from `main`" is checked, not assumed,
for a pinned `--ref`**: `codeload` will archive *any* commit this public
repository holds — an open pull request's head among them, pushed by
anyone — so before a byte downloads, `bootstrap.sh` asks GitHub's
(unauthenticated) compare API whether `main`'s own history already
contains `--ref` (`status` `ahead` or `identical`) and refuses otherwise
(`diverged`, `behind`, or an unanswerable request). Without that check, a
pinned commit off `main` — a fork's own `release-signing-key.pub` and
`install.sh`, both internally consistent — would satisfy every check
downstream and still not be `main`'s key. An archive of a commit `main`'s
history contains is not "a tag or an archive of one" — it travels over the
same GitHub HTTPS a `git clone` of that commit would, so the key arrives by
the same protected path a checkout already gave it; a release *tag* remains
the least-protected ref (§5) and is never where `bootstrap.sh` looks. No key
is embedded in `bootstrap.sh` and none is downloaded from anywhere but that
archive, so there is no key-drift assertion to keep in CI the way #392's
embedded-key proposal would have needed.

**The reachability check above only protects an operator who already has a
trustworthy `bootstrap.sh`.** The check is code *inside* the script, so a
copy fetched from `raw.githubusercontent.com/.../<sha>/agent/deploy/bootstrap.sh`
— a commit not already known to be on `main` — is free to run its own
version of it: skip the check outright, or always answer `identical`, and
trust its own `release-signing-key.pub`. That copy gets no protection from
the check at all, regardless of what `--ref` is then passed to it. Only a
`bootstrap.sh` already known to be on `main` can be trusted to enforce the
guarantee this section describes, which is why `agent/README.md`'s
Prerequisites fetches `bootstrap.sh` itself from `/main/` in both the
unpinned and the pinned form, and passes the pin as `--ref` rather than as
part of the fetch URL.

Fetching the archive is itself no
more and no less authenticated than the checkout it stands in for — HTTPS
only — so it
neither closes nor widens the fresh-install downgrade window described
above; that window is about `install.sh`'s own `/releases/latest`
resolution, which is identical regardless of which path fetched
`install.sh` itself.

**A release without agent binaries is a failure, never a fallback.** Every
release up to and including `v2026.9.3` predates #390 and carries none; the
installer names the tag and the asset it looked for and exits non-zero. There
is no source build behind the download and no quiet selection of some other
version.

**Verification precedes execution, and precedes every installed-state change.**
The binary and its `.minisig` are fetched into a private staging directory
under `~/.cache` (not `/tmp`: a `noexec` `/tmp` would make the verified
binary's own `--version` fail in a way that reads as "carries no version"),
verified with the stock `minisign` under the checkout's
`agent/release-signing-key.pub`, and only then made executable and asked its
version — which must equal the release's. A rejected candidate is never run,
nothing is installed, no service is stopped, and the env file is untouched;
staging is removed on every exit path. Requiring the stock `minisign` rather
than bundling a verifier is deliberate: the installer never installs a
verifier, a package manager or a toolchain on the operator's behalf, and the
key arrives by a path (the checkout) other than the download it verifies.

**Install ownership (decision 2026-09-09).** New installs are user-owned at
`~/.local/bin/solador-agent`, staged as `.new` beside the live path and
renamed over it — the ETXTBSY-safe swap this document already required of
`redeploy.sh` — with the displaced binary kept as `.prev`. Nothing uses
`sudo`. A host installed before #392 has `ExecStart=/opt/solador-agent/…`;
the installer detects that, stops before changing anything, and prints the
explicit step (`--migrate-from-opt`) rather than migrating it silently. The
migration preserves the env file, installs at the user-owned path, regenerates
the existing unit from the template (keeping the displaced file as
`.service.prev`), seeds `.prev` from the `/opt` binary so a rollback has an
anchor, restarts and verifies; the `/opt` binary stays until the operator
removes it. #393 and #394 consume exactly this topology — the
service identities, the binary path, the env path and the no-`sudo` rule —
so an unattended job never needs a privilege it does not have; and #394
records that as the **unprivileged-ownership decision (2026-09-09)**:
`--enable-timer` is refused before creating anything on an unmigrated `/opt`
host, on an install directory or binary the user cannot write to, and as
root, and neither the timer nor the job ever invokes `sudo`, configures it,
or acquires a helper.

**Both services render the actual paths.** The systemd unit is a template
whose `ExecStart` is rendered with the chosen absolute path (double-quoted
when it needs to be), and `EnvironmentFile=%h/.config/solador-agent.env` is
unchanged; a static `Environment=SOLADOR_AGENT_CONFIG_DIR=%h/.config` line
beside it (#447) is what lets `SOLADOR_AGENT_TLS=1` find or create
`solador-agent.tls.key`/`solador-agent.tls.crt` in the SAME directory
`EnvironmentFile=` reads out of, both keyed off the one systemd specifier —
never off this process's own inherited `HOME`. On macOS the service is a
**LaunchAgent** —
`~/Library/LaunchAgents/app.solador.agent.plist`, label `app.solador.agent`,
domain `gui/<uid>`, running as the invoking user, never a LaunchDaemon —
whose `ProgramArguments` is a launcher (`~/.local/bin/solador-agent-launchd`,
a copy of `deploy/run-agent.sh`) plus the binary and env-file paths. The
launcher exists because launchd has no `EnvironmentFile=` and its
`EnvironmentVariables` key would put the token into the plist; it reads the
same mode-0600 env file line by line at every start, exports the documented
keys only, never `source`s the file, and `exec`s the agent. It also exports
`SOLADOR_AGENT_CONFIG_DIR` as that env file's own directory (#447) — derived
from the path argument it was already handed, not from `$HOME` — because
launchd's `HOME` (the target user record's) need not be the `HOME`
`install.sh` ran under. So token rotation
is "edit the file, restart the service" on both platforms, and the token
appears in neither the plist nor the log. The plist does set `PATH`
(`/opt/homebrew/bin:/usr/local/bin:/opt/podman/bin` ahead of the system
directories), and that is load-bearing rather than tidy: launchd starts a job
with its compiled-in `PATH`, which holds none of `docker`, `tart` or `podman`,
and the agent maps a runtime it cannot find to "not installed" — so without
it every macOS install would serve `/v1/containers` as `[]` forever under a
green `/v1/health`, the empty-panel-as-all-clear failure this project exists
to remove. `PATH` is not a secret, so the argument against
`EnvironmentVariables` does not apply to it. A LaunchAgent is login-session
coverage, not boot coverage, and the installer says so: with no `gui/<uid>`
domain to bootstrap into it refuses, actionably, before touching anything.

**The bearer-token prompt, the env-file contract, and the pre-rename
`devcanopy-agent` handover are unchanged**, with one addition: an existing
port in the env file is now kept on a re-run, the way the bind already was.
Success still means the authenticated `/v1/health` reports the verified
artifact's own version, and anything else is non-zero.

**The bind follows the transport, and Tailscale is optional (#449, part 3 of
#445).** The agent and the installer resolve the bind the same way: an
explicit `SOLADOR_AGENT_BIND` always wins; else the env file's existing value
(installer only, and not a bind the installer itself chose — below); else the
detected Tailscale IP; else — and only with TLS on — all interfaces (`0.0.0.0`). With TLS off the last step is still a refusal, as
it always was: over plain HTTP the tailnet is the only thing between the
bearer token and the network, so a host with no tailnet is refused rather than
exposed. With TLS on the token is encrypted and the certificate is pinned by
the cockpit (#448), which is why the tailnet requirement was retired for that
transport and only that one. Because TLS is not final until the staged binary
has been asked whether it supports it (§6, above), the installer refuses a
host with no bind address before any download only when TLS is already known
to be off (`SOLADOR_AGENT_TLS=0`, or an existing env file without it), and
after staging when the staged release predates TLS.

**All-interfaces is a real exposure and the installer says so.** The
authenticated agent is then reachable on every network the host is on — on a
cloud VM, a public interface included. The installer names the interface and the
reason in both its `Binding to` line and its Done block (`Bind: 0.0.0.0:7878 —
ALL interfaces`), and points the operator at the two mitigations:
firewall the port, or set `SOLADOR_AGENT_BIND` to the one interface the
cockpit dials. The agent also logs a warning at every start that binds a
wildcard.

**What keeps the token off an open network is the client's rule, not the
listener's (#449 part 3).** The earlier statement here — that the token plus the
pinned certificate are the whole defence — was true only of a *paired* host. A
host added in the cockpit without pairing is dialled over plain HTTP, and an
agent that may now listen off the tailnet would have been sent the bearer token
in the clear before the connection ever failed. So the cockpit refuses:
**it never sends the bearer token over plain HTTP to an address that is not
loopback or Tailscale** (IPv4 `100.64.0.0/10`, IPv6 `fd7a:115c:a1e0::/48` minus
the 4via6 prefix `fd7a:115c:a1e0:b1a::/64`; an address range, not tailnet
membership — `100.64.0.0/10` is RFC 6598 CGNAT space other networks use; a
name only if every address it resolves to is, connecting to the addresses it
vetted; nothing sent if it does not resolve). The accurate statement is
therefore: a paired host uses pinned TLS, and an unpaired host is only ever
dialled over plain HTTP on loopback or Tailscale. An unpaired host elsewhere is
not polled and reads "unpaired, off-tailnet — pair it in Settings" (its own
state, not *unreachable*); the fix is the pairing flow already there. The
guard lives in `crates/agentclient` (`plain`), where the address table is unit
tested exhaustively and the "no connection is made" property is tested against
a live listener, with a guard-off negative control.

**A bind the installer chose is provisional.** It writes
`SOLADOR_AGENT_BIND_AUTO=1` beside the `0.0.0.0` it picked for a TLS host with no
tailnet; every re-run sets that bind aside and resolves it again (tailnet IP if
one is up, `0.0.0.0` only with TLS on, else the refusal), so it survives neither
TLS being turned off nor Tailscale arriving. A bind without the marker is the
operator's explicit choice and is kept. (This replaces an earlier draft of this
section that pinned the chosen bind so that "a host does not change interface
behind the operator's back": pinning it is what left plain HTTP on every
interface after TLS was turned off.)

**The certificate's SAN list no longer matters to the local probes (#457).**
The list is fixed at first start: the loopback baseline plus the concrete bind
host then; wildcards are dropped. Because the bind can now change after that
(all interfaces to a tailnet address the day Tailscale comes up), the two local
probes stopped depending on it rather than leaving a path where every `update`
rolls back: `verify_health` verifies the pinned certificate as the name
`localhost` — in every certificate's baseline — while `curl --connect-to` dials
the bind address, and `solador-agent update`'s post-restart probe trusts the
pinned certificate as its only root. For an IP or DNS-name bind it dials
`localhost` with the connection pinned to the bind (reqwest
`resolve_to_addrs`, the counterpart of `--connect-to`): the bind's IP, or the
looked-up addresses of a DNS-name bind (before the swap; unresolvable is a
refusal, as is an IPv6 zone id, for `update` and `rollback` alike). A wildcard
bind is not pinned: the probe dials loopback as written (`https://127.0.0.1:P`
for `0.0.0.0` or an empty bind, `https://[::1]:P` for `::` / `[::]`) and
verifies against the baseline loopback IP SANs.
Neither disables certificate or hostname verification, and a different
certificate — including a CA-signed `localhost` one — is still refused. The
tests that guard it: `health_client_reaches_a_dns_name_bind_absent_from_the_san_list`
and `health_client_reaches_a_non_loopback_bind_absent_from_the_san_list` (the
probe works for a bind the certificate does not name, each with a
direct-dial negative control), `loopback_control_verifies_the_hostname_not_the_pin`
(never skips; checks hostname verification only, not the pin), `health_client_refuses_a_localhost_certificate_from_another_ca`
and `health_client_trusts_only_the_pinned_certificate` (the pin). The
non-loopback one skips on a host with no route unless
`SOLADOR_AGENT_TEST_REQUIRE_NONLOOPBACK` is set, which CI does. The cockpit checks no hostname. What #457 still tracks is the list itself
for an external client that verifies by name.

The from-source path is deliberately still there: `redeploy.sh` builds with
`cargo` for our own Linux hosts and is `build_release_binary`'s only caller
now. See "Open items".

**Uninstalling is the mirror of installing, and covers only the invoking
user (#439).** `install.sh --uninstall` — `--uninstall --purge` also removes
the env file, the pre-rename `devcanopy-agent.env` beside it (since
install copied its token out of that file and never deleted it), and the
TLS keypair (`solador-agent.tls.key`/`solador-agent.tls.crt`, #447), on the
same reasoning: purging credentials means a re-pair. Its
refusals run in this order, each untouched: **root** first, for the same
reason `--enable-timer` refuses it; then an **unsupported platform** (this
script supports Linux/systemd and macOS/launchd, named as such); then the
**service manager being unreachable** (the same `systemctl --user
show-environment` / `gui/<uid>` domain check the install path makes in its
own preflight, below: a `sudo -u`/`su` session cannot ask systemd to stop
anything, and discovering that mid-uninstall — after a unit file is already
gone — is exactly the half-changed state this check exists to prevent); then
the **update transaction lock** (`<bin>.update.lock`, §4's own transaction
lock — uninstalling mid-swap would race that transaction's own binary
rename). Two further checks — which lock tool is on PATH, and whether
`~/.local/bin` (`$DEST_BIN`'s own directory) exists at all — run **before**
any `mkdir` or `exec` touches that lock file (a round-4 review fix, #454):
the earlier revision opened, and thereby created, the lock file FIRST and
only then asked either question, so "neither flock(1) nor perl" and an
unsupported platform each exited 1 "Nothing has been changed" while actually
leaving that lock file — and, on a host with no install directory yet, the
directory itself — behind; the very next run then reported "removed: update
transaction lock" for a host nothing was ever installed on. A missing
`~/.local/bin` is the strongest case: nothing can possibly be installed
under a directory that does not exist, so the whole lock section (tool
check, `exec`, the held-note write) is skipped outright rather than
materializing that directory just to find it empty; the removal switch below
still runs unconditionally, so a stray unit/plist an unmigrated `/opt` host
can carry with no local install directory is still found and removed. Where
`~/.local/bin` DOES exist but *neither* lock tool is on PATH, there is no way
left to ask the kernel whether a transaction is running, and guessing "free"
is the wrong direction — so this refuses (busy) before opening anything.
Once the directory exists and a tool is available, `exec 9>>` opens the lock
file — creating it if missing, and never truncating one that exists,
matching `TransactionLock::try_acquire`'s own `create(true).truncate(false)`
— and takes a non-blocking exclusive flock on it, `flock -n 9` where
`flock(1)` is on PATH (`man flock`'s own bash-3.2-safe EXAMPLES idiom for
locking the CALLER's own already-open fd in place), else the stock `perl`'s
Fcntl flock asking the identical question on that SAME fd (`open(my
$fh,"<&=",9)` reopens fd 9 by number rather than dup()ing a new one, so the
lock it takes is the SAME open file description bash's own fd refers to, and
persists after that perl process exits — verified empirically against a
genuinely competing process before this was trusted, not merely assumed) —
the same primitive `TransactionLock` itself uses, per its own comments on
`std::fs::File::try_lock` being `flock()`-based, never `fcntl()`/`F_SETLK`.
**Held for the rest of the run, on both platforms, in every tier that can
check it at all**: a transaction that would otherwise start in the window
this spends stopping the service and removing its unit/plist meets that hold
as busy on its own terms (exit 75) rather than racing anything here. This
closes the window rather than merely narrowing it, which is what the
previous revision did — checking once up front only when the file already
existed and `flock(1)` was on PATH, then re-checking once more right before
the binary's own removal — and what a review of #439's follow-up found could
still return **exit 1** ("refused, nothing changed") **after** the service
had already been stopped and its unit/plist already removed, a false claim;
that second check, and the exit code it produced, no longer exist (see
**Exit status**, below). Whether THIS run's own open is what created the
lock file is decided by an ATOMIC create-if-missing — `( set -C; : >
"$lock_file" )`, noclobber, run BEFORE `exec 9>>` ever touches the file —
never by a separate `[ -e ]` check followed by a later open, which is
exactly the TOCTOU window a second, genuinely competing process can win. A
follow-up review found the previous revision could still `rm -f` a lock
file that PRE-EXISTED on a "lock is busy" refusal, which is precisely how
two processes come to hold "the" lock at once
(`agent/src/update.rs:1349-1351` names the hazard explicitly): `flock()`
locks the open file description, not the path, so unlinking a file another
process holds a lock on and letting a third opener recreate the same name
gives that third opener a lock on a *different* inode — the two are no
longer contending for anything, and both believe they hold the lock. Fixed
by splitting the refusal branches on what they actually are: a **busy**
result (the flock reported held, by anyone) now **never** deletes the lock
file, created by this run or not — deleting it is the one thing a busy
refusal must never do. Of the two **non-busy** failures, only the open
itself failing deletes the file, and only when the atomic create above
proved THIS run is the one that made it (`created_lock`); a file that
pre-existed is left alone there too. A perl reopen failure never deletes
the file, this run's or not — it proves nothing about whether the lock is
free, so it is held to the same rule as a genuinely busy result. Once
held, a note (`pid=<pid> since=<epoch>`, the same
shape `agent/src/update.rs` writes) is left in the file so a racing
`update`/`rollback`'s own busy message names *this* uninstall rather than a
stale previous holder.

`stop` and `disable` are two separate `systemctl` calls, never the single
combined `disable --now` an earlier revision issued (#454 round-4 review's
own follow-up finding). Real systemd's `do_unit_file_disable`
(`src/shared/install.c`) returns `-ENOENT` for a unit file that is not
there, and `disable`'s CLI path (`systemctl-enable.c`) fails at "Failed to
%s unit" BEFORE it ever reaches the `--now` stop — so a combined call on a
unit whose FILE is already gone never stops anything, it just fails
outright. `disable` runs only while the unit's own FILE exists; `stop`
runs when the FILE exists **or** the user manager still holds the unit
([#463](https://github.com/Sassy-Dog/solador/issues/463)). Both calls take the unit's FULL name —
`solador-agent.service`, never bare `solador-agent`: `list-units` does not
append `.service` (`systemctl-list-units.c:275`), and a bare
`solador-agent-update` resolves to the oneshot `.service`, never the
`.timer`. A failed `stop`, or a failed `disable`
while the file exists, both mean exit **4** — an enablement symlink
`disable` could not clear is exactly as unconfirmed as a process `stop`
could not stop, though the two are reported as separate, distinct claims
(see **Exit status**, below): a failed `stop` says the process may still be
running; a failed `disable` with a successful `stop` says only that the
unit's future auto-start is unconfirmed, and does not claim the process is
still running.

**A re-run after exit 4 converges (#463, part of #455).** The first run's
best-effort removal has already deleted the unit files while the manager may
still run a unit, so the re-run asks it, per unit and by full name:
`systemctl --user show -p LoadState -p ActiveState <unit>`, each property
read by key. A unit is **held** when `LoadState != not-found` OR
`ActiveState != inactive`, and a held unit is stopped. Five systemd
behaviours (source citations are v256) shaped that, each found by a review
round of the backed-out #454 and each modelled by the test stub:

1. `disable` fails ENOENT when the unit file is gone
   (`do_unit_file_disable`, `src/shared/install.c`) and
   `systemctl-enable.c` returns before its `--now` stop, so stop and disable
   stay separate calls and a fileless unit is only stopped.
2. `list-units` does not append `.service`, so full names everywhere.
3. `list-units --all` also shows fileless units something merely references
   (a dangling `*.wants` link, the update oneshot's own
   `After=solador-agent.service`, an operator's unit): `not-found` +
   `inactive`. `stop` on one fails "Unit ... not loaded."
   (`dbus-unit.c:1811-1814`), exit 5, so counting them held would be a false
   exit 4 forever. They are left alone.
4. A fileless oneshot left `failed` reads `not-found` + `failed`: held, and
   `stop` works on it.
5. A unit the manager never loaded is not an error: `show` exits 0 with
   `not-found` + `inactive`. Absent: no stop call, and "Nothing installed".

A `show` that itself fails is not evidence of absence, so it is exit 4
naming the unit rather than a quiet "Nothing installed". macOS asks
`launchctl print` directly, with no file-existence gate.

**Before either unit/plist is removed**, the binary
path it currently names is read (`unowned_service_binary`, never assumed to
be `$DEST_BIN`) — reading `ExecStart=` on Linux, the second
`<string>` of the plist's `ProgramArguments` on macOS (the first is the
launcher, handled on its own path). One outside `~/.local/bin` — an
unmigrated `/opt` host, most likely, root-owned since #392's migration is
explicit and never automatic on its own — is reported with the actual
remedy (`sudo rm -rf /opt/solador-agent` for that specific layout; "remove
it as its owner" otherwise) rather than pointed at `--migrate-from-opt`,
which post-uninstall either reinstalls fresh (Linux) or is refused outright
(macOS) and was never a real remedy for a leftover binary — the mirror-of-install
claiming it removed everything IT put there must not go quiet about a
binary it never touched, or send the operator to a flag that does nothing
here. It then removes both unit/plist pairs (Linux additionally
`daemon-reload`s and `reset-failed`s all four unit names once something
changed, clearing any "failed" state a disable/stop that reported an error
left on one of them), and removes the binary with its
`.prev`/`.new`/`.update.lock`/`.rollback-displaced` siblings, the macOS
launcher, the Linux guard, and the update stamp. The env file — the file
that holds the bearer token — and the TLS keypair
(`solador-agent.tls.key`/`solador-agent.tls.crt`, #447, beside it) are both
**kept** and named in the output unless
`--purge` says otherwise, for the same reason: a re-install as this user
reuses them, so an operator's existing cockpit pairing (and an existing TLS pin, #448)
survives. `--purge` alone is refused (it modifies
`--uninstall`, it is never a mode of its own), and neither combines with
`--migrate-from-opt` or `--enable-timer` (remove an install or
create/repoint one, never both in one run). It never runs `loginctl
disable-linger` (another service on this login may depend on it), and is
idempotent: a second run finds nothing left and says so — a lock file THIS
run created purely to check for contention, with nothing else to remove
either, does not itself count as "something removed": only a lock that
already existed, or a run that removed something else too, reports it.

**Exit status is not always 0 or 1 for a run that changed something**: a
manager the reachability check found reachable can still refuse one
specific `stop` or `disable` request (rarer than unreachable, and not
grounds for the refusal above, since the files genuinely can be removed) —
that case still removes every file (best-effort) but exits **4**. And `uninstall_remove`
checks every `rm -f`'s own result, not merely that it ran: a read-only
parent directory or an immutable file (both reproduced with `chmod 555` —
see **Testing**, below) previously still printed `removed:` and ended in
`==> Done` with exit 0 — a failed removal reported as a successful one.
That case now prints `FAILED to remove: <desc> (<path>)`, no `removed:`
line for that file, no `Done`, and exits **6**, which wins over 4 when both
apply (a provably-failed removal is the more severe claim). 4 and 6 are
each distinct from 0 ("Done", earned only when every stop AND disable
request succeeded and every file that should be gone actually is) and 1
("refused, nothing changed" — false in both). **There is no exit 5.** The previous revision's
narrower hold left a real window — a transaction starting between the
service being stopped and the binary being removed, reachable only where
the lock was not held continuously (no pre-existing file, or no `flock(1)`
on PATH) — and that case exited 5, the binary and lock file deliberately
left alone. #439's follow-up review closed the window (every tier that can
check the lock now holds it from before the first service-manager call, and
the one tier that cannot refuses before that call too) rather than
narrowing it further, so nothing reaches that exit any more; the second
check that produced it is gone from the source, not merely undocumented.
`SOLADOR_AGENT_LAUNCHD_LABEL`'s path-safety validation (the same
`[A-Za-z0-9][A-Za-z0-9._-]*` gate the install path always had) is checked
**before** `--uninstall` dispatches rather than only inside the install-only
preflight further down — `run_uninstall` derives `PLIST_DST`/`UPDATE_PLIST_DST`
from that variable too and `rm -f`s whatever they resolve to, so an
unvalidated value could otherwise steer an uninstall's own deletions outside
`~/Library/LaunchAgents`. It is the second half of
moving the agent to another Unix user; `agent/README.md`'s "Moving the agent
to another user" has the ordered procedure — install as the new user first
(checkout or `bootstrap.sh`), re-pair the token, then `--uninstall --purge`
as the old user — for exactly the reason a plain `--uninstall` does not
purge by default: the env file is worth keeping until whatever replaced it
is confirmed working.

## Testing

**The signature-rejection test is the load-bearing one.** A tampered binary
must fail to install, and that test must be proven to fail against an unsigned
or modified artifact — not merely observed to pass. A verification step that
silently accepts everything is indistinguishable from one that works, and a
green test suite is exactly how that survives review.

Also required:

- **Hash-skip — SHIPPED (#393):** same content, different version → no swap,
  no restart. `agent/tests/update_flow.rs` serves a feed whose entry hashes
  to the installed bytes under a newer version and asserts exit 0, no
  request for the binary, no `.new`, no `.prev`, no restart.
- **Rollback — SHIPPED (#393):** a failed post-restart health check restores
  `.prev` and exits non-zero. The same suite drives a stale version after
  restart, a service that never comes up, a manager that refuses the
  restart, and a manager that refuses the *recovery's* restart too —
  asserting the bytes at the live path and at `.prev` afterwards, the
  restart count, and that the last case is a distinct exit code (3) that
  never claims a rollback. The opt-in `launchd_smoke` does the first of
  those on a real throwaway LaunchAgent with the real built agent.
- **Feed parsing** against a locally served fixture; no network in tests.
  The producer half of this is shipped: `crates/updatefeed::agent`'s tests run
  over `tests/fixtures/agent/` — four stand-in binaries signed by the pinned
  `rsign2`, and a feed/signature pair the producer emitted and `rsign` signed —
  and assert exact SHA-256 over known bytes, byte-for-byte reproduction of the
  committed document, plain-minisign acceptance, and refusal of a tampered
  binary, a foreign key, a lifted signature, a missing or fifth target, a feed
  whose served bytes changed by one character (or lost its final newline), and
  the app's base64-wrapped signature form in either direction. **The consumer
  half is shipped too** (#393): the agent's own tests read the same committed
  pair, accept it under its key, refuse it after one character moves or the
  final newline goes, and refuse it under the production key; and the
  end-to-end suite serves feeds it builds and signs in-test from a loopback
  axum server.
- **Platform matrix — SHIPPED.** Each published binary executes `--version` on a
  matching runner before the release is published: `release-agent-verify` is a
  four-way matrix over `ubuntu-latest`, `ubuntu-24.04-arm`, `macos-latest` and
  `macos-15-intel`, and it runs the *uploaded* file rather than a rebuild.
- **Off by default, and no catch-up — SHIPPED (#394).** `lib_test.sh` asserts
  the default install's *actions* on both platforms: no timer, oneshot or
  updater plist written, nothing said to the service manager about one, no
  check made; and, for `--enable-timer`, that the job is created without
  being started or kickstarted, that a no-flag re-run touches neither its
  files nor the manager, that a repeated opt-in makes one job, and that
  root, an unwritable install directory, an unmigrated `/opt` host and an
  unverified metrics install each create nothing. The launcher's guard has
  its own cases with the clock, wake time and boot time stubbed: a firing
  30 s after wake or boot is discarded, a second firing inside the interval
  is discarded, a failed attempt is not retried, a clock that fails or
  prints garbage, an unreadable stamp and an unwritable stamp each hold
  with exit 6, and the metrics path reads no clock. The Linux guard (#411)
  has the mirror set — its two manager reads, the clock and the kernel's
  suspend counter stubbed — plus the manager-shaped cases the platform
  adds: a laptop whose manager forgot its resume runs three days running
  with exactly one `NOTE` line each and never holds, and so do an unreachable
  system manager, an unparseable resume and a resume dated after the
  activation; an unreachable user manager, a manager that does not show
  the unit activating, an unreadable kernel counter and every usage error
  each hold with exit 255, and so does a death the guard did not decide; a
  server that never sleeps runs three days running; and the
  installer's guard actions (only with the flag, before the unit, on both
  unit lines, preserved, not re-created, refused on systemd 242). "No
  update check" is asserted on the fixture agent's recorded argv, not on
  curl's. The opt-in launchd smoke does the same against real launchd,
  read-only. What no test observes is a day of sleep on a real Mac or a
  real systemd timer through a reload and a suspend (§4).
- **Uninstall — SHIPPED (#439), fixed up against human review at 8f30567.**
  `lib_test.sh` installs with `--enable-timer` then `--uninstall`s on both
  platforms (the same stubbed managers) and asserts every installer file is
  gone — seeding `.prev`, `.new` and `.rollback-displaced` by hand first, so
  each removal is asserted for real rather than by vacuous absence — that
  `stop` (and, where the unit file still exists, the separate `disable`
  call) / `bootout` reached both jobs (an exact-line match against the
  unit's FULL name — "solador-agent" was a literal prefix of
  "solador-agent-update.timer" under the OLD bare-name calls, so a plain
  substring check on those would have passed even if only the timer, never
  the metrics service, had been stopped; the full names this fix now uses,
  "solador-agent.service" and "solador-agent-update.timer", share no such
  prefix relationship, and the test still checks the exact line rather than
  leaning on that), the env file survives without `--purge` and
  is gone (along with the legacy `devcanopy-agent.env`) with it, a second
  `--uninstall` (with or without `--purge`) is a no-op that exits 0 and asks
  the service manager for nothing, `--purge` alone and `--uninstall` beside
  `--migrate-from-opt` or `--enable-timer` are each refused as usage errors,
  and `loginctl disable-linger` is never called. A unit/plist naming a
  binary outside `~/.local/bin` (a hand-edited `ExecStart=`/`ProgramArguments`
  standing in for an unmigrated `/opt` host) is asserted to be named
  `left behind: <path>` with the actual remedy on the following lines (never
  the old, unchecked "see --migrate-from-opt" wording — asserted absent by
  name) and never removed. The pre-rename unit is asserted disabled, stopped
  and removed too, and named in the same `reset-failed` sweep. The
  label-traversal case (`../x`) is proven with a seeded sentinel at the path
  it would resolve to (one directory above `~/Library/LaunchAgents`),
  asserted to survive byte-for-byte — not merely that the run refused, which
  a broken check could still satisfy vacuously if nothing happened to exist
  at that path yet.

  **The lock hold itself is proven two ways per tool tier, fixed up against
  a follow-up review that found the earlier revision's hold conditional on a
  pre-existing file and `flock(1)` being on PATH — neither of which a fresh
  host or stock macOS necessarily has.** First, the tier with neither
  `flock(1)` nor `perl` on `PATH`: `--uninstall` is asserted to refuse
  (exit 1, "cannot check the update lock") *before* any service-manager call
  is made, on an install that has never run `update`/`rollback` (no
  pre-existing lock file) — proving there is no window left in this tier to
  race, rather than a window that closes after a stop call the previous
  exit-5 test needed to reach. Second, the flock(1) and perl tiers, each
  proven with a genuinely competing process, from NO pre-existing lock
  file: a synthetic `flock(1)` (`TOOLBIN_FAKEFLOCK`, `perl`-backed, standing
  in for the real one no host running this suite has) or `TOOLBIN_NOFLOCK`
  (the real `perl`, with `flock(1)` hidden) is put on `--uninstall`'s own
  `PATH`, a stubbed `systemctl disable`/`launchctl bootout` is made to
  independently attempt a real, non-blocking `flock()` on the SAME lock
  file mid-call, and the run is asserted to both succeed (the hold does not
  itself break the happy path) and to have made that independent attempt
  see the lock as busy — proving the hold starts before the first
  service-manager call and survives through it, in both tiers, rather than
  assuming `exec 9>>` correctly creates a file that was never there. An
  install with genuinely nothing on disk is asserted to still report
  "Nothing installed" rather than "Done": the lock file THIS run had to
  create just to check contention is not itself installed state, so
  removing it again at the end must not flip that report.
  `hold_fake_lock`/`release_fake_lock` (used to prove the pre-existing-lock
  refusal, unchanged by this fix) back a real lock with a holder that blocks
  on a FIFO's `open()` rather than `sleep` (a `sleep` child would inherit the
  locked fd across `fork()` — no `CLOEXEC` on a plain `exec N>file` or on
  perl's own `open()` — and keep the lock held even after the parent holding
  it is killed, exactly the gotcha `agent/src/update.rs`'s `TransactionLock`
  documents), preferring real `flock(1)` and falling back to the identical
  `perl` mechanism `--uninstall` itself uses when `flock(1)` is absent —
  which it is on every macOS host this suite runs on, `./dev test`'s
  `rust-workspace` CI leg included, so this is the branch that dev machine
  and that CI leg actually exercise, not merely the one Linux CI's real
  `flock(1)` happens to cover. There is no longer a standalone one-shot
  `update_lock_busy` helper or its own dedicated test: install.sh's own
  acquisition (above) replaced its only caller, so an elaborate three-tier
  implementation with nothing left to call it was removed rather than kept
  around unused.

  `uninstall_remove`'s own failure path is proven by making `rm -f` itself
  fail: `chmod 555` on a target's parent directory (the binary's
  `~/.local/bin`, and — after an ordinary `--uninstall` first clears
  everything else from `~/.config` — the env file's, under `--purge`, the
  reviewer's own two repro cases) is asserted to leave no `removed:` line for
  that file, no `==> Done`, a `FAILED to remove: …` line naming it, and exit
  **6**; permissions are restored immediately after each case so neither a
  later step in the same test nor the suite's own cleanup is affected. The
  unremovable-binary case seeds `$bin.update.lock` *before* the `chmod 555`,
  since opening an EXISTING file for writing needs no directory write
  permission (only creating or unlinking one does) — without that seed the
  read-only directory would also block the lock's own acquisition, refusing
  before the binary-removal path under test is ever reached.

  `bootstrap.sh` needed no code change for `--uninstall`: its argument loop
  passes through anything it does not itself recognise, and `lib_test.sh`
  proves that generically (an existing `--enable-timer --migrate-from-opt`
  case) and once more by name for `--uninstall --purge`, plus a
  real-`install.sh` run through `bootstrap.sh` asserting the kept-env-file
  hint reads `bash bootstrap.sh ... --uninstall --purge` rather than a path
  under bootstrap's own (already-removed) staging directory. Root, an
  unreachable service manager (`STUB_SYSTEMCTL_USER_EXIT` / a
  `STUB_LAUNCHCTL_DOMAIN_EXIT` no gui domain) and an unsupported platform are
  each asserted to refuse before anything changes — including, since a
  round-4 review fix (#454), the lock file itself, split by precondition
  since the two cases below start from different hosts. On a genuinely
  clean host — no `~/.local/bin` at all, nothing solador-related or
  otherwise ever installed there — both an unsupported-OS refusal and a
  genuine "nothing installed" success run are asserted (`assert_untouched`)
  to create neither `<bin>.update.lock` nor `~/.local/bin` itself. Separately,
  the refusal where neither `flock(1)` nor `perl` is on PATH is asserted on
  a host where `~/.local/bin` already exists (the test creates it first,
  standing in for an operator's own directory, or one left over from an
  earlier full uninstall install.sh never `rmdir`s) but holds nothing
  solador-related: that case creates no lock file inside it, and a
  follow-up run made WITH a lock tool available still reports "Nothing
  installed" rather than "Done" (proving the earlier refusal left no
  stray artifact for the second run to find and report as removed); a hostile
  `SOLADOR_AGENT_LAUNCHD_LABEL` (`../x`, `x@BINARY@y`, `.hidden`, `a b`) is
  asserted to refuse `--uninstall` the same way it refuses a normal install.
  A manager that answers reachable but still refuses one specific stop or
  disable request (`STUB_SYSTEMCTL_STOP_EXIT` / `STUB_SYSTEMCTL_DISABLE_EXIT`
  on Linux, `STUB_LAUNCHCTL_BOOTOUT_EXIT` on macOS) is asserted to still
  remove every file, exit 4 rather than 0, name the two claims separately —
  a failed `stop` is asserted to say the process may still be running; a
  failed `disable` with a successful `stop` is asserted to name only the
  unit's unconfirmed future auto-start and to NEVER claim the process may
  still be running — and never print the token. The `systemctl` stub's
  `disable` models one piece of real systemd faithfully: it fails "Unit file
  <u> does not exist" with no stop when the unit file is absent from the
  stub's own `$HOME`, the same shape a genuine `disable` fails with — but
  install.sh only ever calls `disable` once it has gated the unit on that
  same file existing, so this branch is not reachable through install.sh's
  own calls any more; it stays in the stub as a record of what real systemd
  does. `show -p LoadState -p ActiveState <unit>` answers from
  `STUB_SYSTEMCTL_UNIT_STATE` (`<full-name>=<LoadState>:<ActiveState>` records,
  exact full-name match; an unlisted unit is `not-found:inactive`, or
  `loaded:inactive` with a file), and `stop` on a `not-found:inactive` unit
  exits 5 "not loaded". The fileless cases (held and stopped, stop refused
  exits 4, fileless `failed` oneshot, a `not-found:inactive` reference left
  alone, all four held, an unreadable state) are pinned, and a clean host
  makes neither call and reports "Nothing installed".

Which halves of the rejection test exist is worth saying precisely rather
than letting a checked box imply all of them:

- `scripts/build-agent.sh --sign` **re-verifies every signature against the
  committed public key** before a release can attach it, so a signing key that
  is not the published keypair's private half fails the release. CI then
  verifies a second time with the reference C `minisign`, a different
  implementation from the `rsign2` that signed. (#390)
- **The installer-side check is built and tested the way this section
  demands** (#392). `agent/deploy/lib_test.sh` runs `install.sh` end to end
  against fixtures signed with a throwaway key, using the *real* `minisign`:
  a tampered binary, one signed by another key, and one with no `.minisig`
  are each rejected before the candidate is executed and before any installed
  state changes — and the tamper case is then re-run with an accept-everything
  `minisign` on `PATH`, where it must go through. That second run is the
  proof: the rejection is the verifier's, not another failure landing first.
  A further case runs the installer as it sits in the repository and requires
  a throwaway-key fixture to be rejected under the committed key. Without
  `minisign` the cases report as `SKIP`, never as passes; both CI legs that
  run the suite (`agent-tests` on Linux, `rust-workspace` under macOS stock
  bash 3.2) install it and make those skips failures.
- **The agent-side check is built and proven the same way** (#393).
  `agent/tests/update_flow.rs` mints keys from fixed seeds in-test and signs
  in the documented minisign format (a self-test first proves the shipped
  verifier accepts those signatures and rejects a moved byte), then drives
  `update` against a tampered feed, a tampered binary, a third key, a valid
  signature over the wrong binary, a feed naming another release, a missing
  target and a malformed document — each asserted to leave the live path,
  `.prev`, `.new` and the restart count untouched, with no binary request
  where the feed was the thing refused. The proven-to-fail step was done on
  the PR that shipped it: with `Trust::verify` short-circuited to accept
  everything, nine tests went red — four unit tests (the moved byte, the
  untrusted key, the mislabelled signature, the hash mismatch) and five
  end-to-end (the tampered feed, the tampered binary, the third key, the
  signer's own self-test, and — because the reported key id stopped being
  the signer's — the either-key test) — and green again with the guard
  restored. Any change to that function is held to the same experiment.

## Open items

Both were resolved on 2026-09-07, when #381 was split into children. Kept here
rather than deleted, because the reasoning is what a later reader needs:

- **Cross-compilation uses `cargo-zigbuild`, not `cross`** (recorded in #390,
  and implemented by it). Zig as the linker needs no Docker on the runner, is
  faster per target, and musl is precisely its strength. See §1.
- **`agent/deploy/redeploy.sh` is KEPT**, as the from-source operator path for
  our own hosts, rather than retired once `solador-agent update` exists
  (recorded in #392). `install.sh` and `redeploy.sh` share
  `agent/deploy/lib.sh`, and now that `install.sh` no longer builds (#392),
  `redeploy.sh` *is* `build_release_binary`'s only caller — retiring both would
  leave that helper, and `lib_test.sh`'s load-bearing "refuses to fall back"
  assertion from #269, with no caller at all. #392 left `redeploy.sh` untouched.
