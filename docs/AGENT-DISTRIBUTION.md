# Agent Distribution — design

**Status:** implemented except for unattended scheduling (#394). Agreed
2026-08-24; tracked by [#381](https://github.com/Sassy-Dog/solador/issues/381),
split into five children on 2026-09-07.

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
- **§4 Updating — SHIPPED, the manual command** ([#393](https://github.com/Sassy-Dog/solador/issues/393)):
  `solador-agent update` and `solador-agent rollback` are in the binary, with
  the automatic restore on a failed verification. **Unattended checking** —
  the timer / launchd job that runs `update` on a cadence — is
  [#394](https://github.com/Sassy-Dog/solador/issues/394) and is not built.
- **§5 Signing and trust — SHIPPED, less one operator step** (#393): the
  public keys are compiled into the agent, and the two-key rotation window
  exists as a provisioning procedure (`scripts/agent-standby-key.sh`) plus
  the second file it writes, `agent/release-signing-key-next.pub`. Until the
  operator has run that script and committed the file, a build carries the
  current key alone — a one-key trust set, which the tests cover as such,
  and which the first `v*` tag after this merges must not be cut on
  (`docs/SECRETS.md` says when).

How the Solador metrics agent reaches machines that are not ours, and how the
people running it stay up to date.

This was written as a **design for a change**. The sections marked SHIPPED above
now describe what exists; what is still design is the unattended job in §4's
last paragraph (#394).

## Why

The desktop app has a real public distribution story. Release `v2026.8.118`
ships `.dmg`, `_x64-setup.exe` and `.app.tar.gz`, each with a minisign `.sig`,
plus a `latest.json` update feed that Tauri's updater consumes.

The agent shipped **nothing** — zero binary assets on any release — until #390.
It now ships four minisigned binaries beside the app's, and since #392
`install.sh` installs from them. The three consequences below were the
*reason*; all three are addressed, the third by #393 (manually) with #394
still to schedule it.

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
   restart, and restores the previous binary itself if that fails. Nobody
   is told a version exists until #394 schedules the check.)*

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
   on the same install (a manual run racing #394's scheduled one) reports
   *busy*, exits **75**, and changes nothing; the lock dies with the
   process, so a crashed run cannot wedge the next. It serialises these two
   commands against each other only — `install.sh` and `redeploy.sh` write
   the same `.new`/`.prev` without it, so they are not to be run during an
   update. Then the **service manager must answer** (`systemctl --user
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

Exit codes are a contract for #394's scheduled job: `0` updated, or
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
(`--enable-timer`): a systemd timer on Linux, a launchd agent on macOS. People
running a monitoring agent on their own servers should not get surprise
restarts; those who want hands-off can ask. **That is #394 and is not built**;
`install.sh` refuses `--enable-timer` by name until it is.

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
The public key, meanwhile, must reach the host from a checkout of `main` —
the protected ref — not from a tag or an archive of one.

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
so an unattended job never needs a privilege it does not have.

**Both services render the actual paths.** The systemd unit is a template
whose `ExecStart` is rendered with the chosen absolute path (double-quoted
when it needs to be), and `EnvironmentFile=%h/.config/solador-agent.env` is
unchanged. On macOS the service is a **LaunchAgent** —
`~/Library/LaunchAgents/app.solador.agent.plist`, label `app.solador.agent`,
domain `gui/<uid>`, running as the invoking user, never a LaunchDaemon —
whose `ProgramArguments` is a launcher (`~/.local/bin/solador-agent-launchd`,
a copy of `deploy/run-agent.sh`) plus the binary and env-file paths. The
launcher exists because launchd has no `EnvironmentFile=` and its
`EnvironmentVariables` key would put the token into the plist; it reads the
same mode-0600 env file line by line at every start, exports the documented
keys only, never `source`s the file, and `exec`s the agent. So token rotation
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

The from-source path is deliberately still there: `redeploy.sh` builds with
`cargo` for our own Linux hosts and is `build_release_binary`'s only caller
now. See "Open items".

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
