# Agent Distribution — design

**Status:** partly implemented. Agreed 2026-08-24; tracked by
[#381](https://github.com/Sassy-Dog/solador/issues/381), split into five
children on 2026-09-07.

- **§1 Build and publish — SHIPPED** ([#390](https://github.com/Sassy-Dog/solador/issues/390)).
- **§3 Versioning — SHIPPED** with it; `docs/VERSIONING.md` carries the
  reclassification.
- §2 (the feed), §4 (`solador-agent update`), §6 (`install.sh`) are **not built
  yet** — #391, #393/#394 and #392. Everything they describe is still design.

How the Solador metrics agent reaches machines that are not ours, and how the
people running it stay up to date.

This was written as a **design for a change**. The sections marked SHIPPED above
now describe what exists; the rest is still design.

## Why

The desktop app has a real public distribution story. Release `v2026.8.118`
ships `.dmg`, `_x64-setup.exe` and `.app.tar.gz`, each with a minisign `.sig`,
plus a `latest.json` update feed that Tauri's updater consumes.

The agent shipped **nothing** — zero binary assets on any release — until #390.
It now ships four minisigned binaries beside the app's, but the three
consequences below were the *reason*, and only the first of them is addressed so
far: publishing an artifact and installing from it are different jobs, and
`install.sh` still builds from source (#392).

The state this was written against, and how much of it still holds: the only way
to install or update the agent was to clone this repository and run
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

### 4. Updating

`solador-agent update`, a subcommand of the binary — not a shell script.
`redeploy.sh` is bash and systemd-shaped; an in-binary command behaves
identically on macOS and Linux, needs no shell, and is testable in Rust.

1. Fetch `agent-latest.json`; **verify its signature**.
2. Compare content hash against the installed binary. Equal → stop, report
   "already current", exit 0.
3. Download the binary for this platform/arch (§1 publishes the binary itself,
   not an archive); **verify signature and hash
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
committed at `agent/release-signing-key.pub` (key id `03D2D786998D5EE8`), its
private half is the `prd` environment secret
`SOLADOR_AGENT_SIGNING_PRIVATE_KEY`, and it is not the cockpit's
`TAURI_SIGNING_PRIVATE_KEY`. Signatures are **plain minisign** — the Tauri
signer's extra base64 wrapper is the app's convention and there is no Tauri here
— so anyone can check a download with the reference tool:

```sh
minisign -Vm solador-agent-<version>-<triple> -p agent/release-signing-key.pub
```

Three properties of the release path, each chosen so a failure is loud:

- The signer is the pinned `rsign2` (`RSIGN_VERSION` in `scripts/config.sh`),
  minisign's Rust implementation. `scripts/build-agent.sh --sign` is the one
  implementation, used locally and in CI alike.
- **Every signature is re-verified against the committed public key** before
  anything is uploaded. That is what turns a mis-provisioned private key into a
  failed release instead of a release full of signatures nobody can check.
- CI then verifies again with the reference **C** `minisign` from apt — a
  different implementation from the one that signed, so "it verifies" is not
  merely the signer agreeing with itself.

Signing is its own release job and the **only** agent job holding a credential,
so the key reaches one runner rather than six, and build/verify start without
waiting on `prd`'s reviewer.

The two remaining halves of this section — the compiled-in public key and the
**two**-key rotation window — belong to the `update` child and are **not built
yet**. Nothing in the agent verifies a signature today.

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

**The signature-rejection test is the load-bearing one.** A tampered binary
must fail to install, and that test must be proven to fail against an unsigned
or modified artifact — not merely observed to pass. A verification step that
silently accepts everything is indistinguishable from one that works, and a
green test suite is exactly how that survives review.

Also required:

- **Hash-skip:** same content, different version → no swap, no restart.
- **Rollback:** a failed post-restart health check restores `.prev` and exits
  non-zero.
- **Feed parsing** against a locally served fixture; no network in tests.
- **Platform matrix — SHIPPED.** Each published binary executes `--version` on a
  matching runner before the release is published: `release-agent-verify` is a
  four-way matrix over `ubuntu-latest`, `ubuntu-24.04-arm`, `macos-latest` and
  `macos-15-intel`, and it runs the *uploaded* file rather than a rebuild.

The signing half of the rejection test is shipped and the verifying half is not,
which is worth saying precisely rather than letting a checked box imply both:

- `scripts/build-agent.sh --sign` **re-verifies every signature against the
  committed public key** before a release can attach it, so a signing key that
  is not the published keypair's private half fails the release. CI then
  verifies a second time with the reference C `minisign`, a different
  implementation from the `rsign2` that signed.
- What is **not** built is the agent-side check. Nothing in the agent verifies a
  signature yet, so "a tampered binary must fail to install" has no code to test
  — it arrives with the `update` subcommand, and the proven-to-fail requirement
  above binds that change, not this one.

## Open items

Both were resolved on 2026-09-07, when #381 was split into children. Kept here
rather than deleted, because the reasoning is what a later reader needs:

- **Cross-compilation uses `cargo-zigbuild`, not `cross`** (recorded in #390,
  and implemented by it). Zig as the linker needs no Docker on the runner, is
  faster per target, and musl is precisely its strength. See §1.
- **`agent/deploy/redeploy.sh` is KEPT**, as the from-source operator path for
  our own hosts, rather than retired once `solador-agent update` exists
  (recorded in #392). `install.sh` and `redeploy.sh` share
  `agent/deploy/lib.sh`, and once `install.sh` stops building, `redeploy.sh`
  becomes `build_release_binary`'s only caller — retiring both would leave that
  helper, and `lib_test.sh`'s load-bearing "refuses to fall back" assertion from
  #269, with no caller at all.
