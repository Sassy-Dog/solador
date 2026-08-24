# Agent Distribution — design

**Status:** design, not yet implemented. Agreed 2026-08-24.

How the Solador metrics agent reaches machines that are not ours, and how the
people running it stay up to date.

This is a **design for a change**, not a description of what exists. Everything
in "Today" is current; everything under "Design" is not built yet.

## Why

The desktop app has a real public distribution story. Release `v2026.8.118`
ships `.dmg`, `_x64-setup.exe` and `.app.tar.gz`, each with a minisign `.sig`,
plus a `latest.json` update feed that Tauri's updater consumes.

The agent ships **nothing**. Zero binary assets on any release.

The only way to install or update it is to clone this repository and run
`cargo build --release` on the target host (`agent/deploy/install.sh`). Three
consequences, in order of how much they hurt:

1. **Every monitored machine needs a Rust toolchain.** For an Apache-2.0
   project asking strangers to run an agent on their servers, this is the
   barrier that matters. Nobody installs `cargo` on a NAS to try a dashboard.
2. **macOS has no install path at all.** `install.sh` hard-fails without
   `systemctl` (`"Linux + systemd required"`), while `agent/README.md` states
   the agent "Runs on Linux … and macOS". Half the stated platform support is
   undeliverable today.
3. **Updating is a manual `git pull` + rebuild** on each host. There is no
   mechanism by which a user learns a new version exists.

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

`aarch64-unknown-linux-musl` requires a cross toolchain in CI
(`cargo-zigbuild` or `cross`). This is a real cost and is stated here so it is
not discovered during implementation.

**Every built binary must have `--version` executed on a matching runner before
publish.** An artifact that does not start is worse than no artifact.

### 2. The feed

A separate `agent-latest.json`, signed, **not** the app's `latest.json`.

Tauri's updater owns `latest.json` on a schema it controls. Adding agent
entries risks breaking desktop updates for a change that has nothing to do with
the app.

Each entry carries the download URL, the signature, and the **content hash** of
the binary. The hash is load-bearing — see versioning below.

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

**This trips `docs/VERSIONING.md`'s own revisit clause.** That document
currently classifies the agent as N/A — "an internal artifact hand-deployed to
our own hosts … never published to a registry or distributed externally" — and
says to "Revisit at the first artifact that leaves our machines." This design is
that artifact. Per that document's own rule ("When this doc and the scripts
disagree, that is drift — fix one of them in the same PR"), the implementing
change must reclassify the agent as a shipping tier in the same PR. Landing this
without that edit leaves a spec asserting the opposite of what ships.

### 4. Updating

`solador-agent update`, a subcommand of the binary — not a shell script.
`redeploy.sh` is bash and systemd-shaped; an in-binary command behaves
identically on macOS and Linux, needs no shell, and is testable in Rust.

1. Fetch `agent-latest.json`; **verify its signature**.
2. Compare content hash against the installed binary. Equal → stop, report
   "already current", exit 0.
3. Download the tarball for this platform/arch; **verify signature and hash
   before anything touches disk**.
4. Write `<bin>.new`; `rename()` over the live path.
5. Retain the displaced binary as `<bin>.prev`.
6. Restart the service; poll `/v1/health`; assert the reported version matches
   what was installed.
7. **On failed verification, restore `.prev` automatically and exit non-zero.**

Step 7 is the only behavioural addition. Today's `redeploy.sh` verifies and
reports, leaving a bad binary in place for a human to roll back. On a stranger's
unattended host there is no human, and "verified, failed, left it broken" is how
a monitoring outage becomes a silent one.

`solador-agent rollback` remains available as an explicit command.

**Unattended checking ships off by default**, opt-in at install
(`--enable-timer`): a systemd timer on Linux, a launchd agent on macOS. People
running a monitoring agent on their own servers should not get surprise
restarts; those who want hands-off can ask.

### 5. Signing and trust

A self-updating daemon is a remote-code-execution channel into every host that
runs it. If an attacker can convince the agent a binary is legitimate, they own
the machine. This section is the security boundary of the whole design.

**HTTPS is not sufficient.** It authenticates the transport, not the artifact.
It does not survive a compromised release asset, a mirror, or an intercepting
proxy. The signature is what binds the bytes to us.

- The agent verifies a signature over **both the feed and the tarball**, before
  writing anything.
- The **public key is compiled into the binary**. Not fetched, not
  trust-on-first-use — a TOFU daemon is defeated by anyone present at install
  time.
- The private key lives in CI secrets and never leaves them.

**A separate keypair from the app's.** Compromise of the agent key must not
yield a signed desktop app, and vice versa. The audiences and threat models
differ: the app updates on a person's laptop, the agent runs unattended as a
service on servers, which is the higher-value target.

**Rotation must ship on day one.** A key compiled into a binary cannot be
rotated by the update path it protects: if it is lost or compromised, every
deployed agent is stranded and every user must reinstall by hand. The agent
therefore accepts **two** valid public keys from the first release, so rotation
is a release rather than a recall.

**Accepted risk, stated explicitly:** a tag push publishes a release, and
GitHub branch rulesets do not cover tag refs. The release trigger is the least
protected link in this chain. Acceptable for a solo maintainer; it should be a
known acceptance rather than a later discovery.

### 6. Installing

`install.sh` downloads the signed binary for the detected platform and arch
instead of running `cargo build --release`. This removes the Rust toolchain
from the requirements — the single largest barrier to a stranger running this.

It must also gain a **launchd path for macOS**, closing the gap where the
README promises macOS support the installer refuses to deliver.

The bearer-token prompt and env-file handling are unchanged.

## Testing

**The signature-rejection test is the load-bearing one.** A tampered tarball
must fail to install, and that test must be proven to fail against an unsigned
or modified artifact — not merely observed to pass. A verification step that
silently accepts everything is indistinguishable from one that works, and a
green test suite is exactly how that survives review.

Also required:

- **Hash-skip:** same content, different version → no swap, no restart.
- **Rollback:** a failed post-restart health check restores `.prev` and exits
  non-zero.
- **Feed parsing** against a locally served fixture; no network in tests.
- **Platform matrix:** each published binary executes `--version` on a matching
  runner before the release is published.

## Open items

- Cross-compilation tooling choice for `aarch64-unknown-linux-musl`
  (`cargo-zigbuild` vs `cross`) — decide during implementation.
- Whether `agent/deploy/redeploy.sh` is retired once `solador-agent update`
  exists, or kept as the operator-side path for our own hosts.
