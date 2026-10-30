# Solador releasing

> Moved verbatim from `AGENTS.md` (#638), which is held under the org's 32 KiB guidance budget. Edit it here; `AGENTS.md` points at this file.

### Releasing

There are **two release trains, and neither triggers the other** (#472, #491).
**The cockpit's:** pushing a `v*` tag runs `release.yml` (#307), which builds
the signed, notarized, stapled artifacts (#306) on the `prd` environment and
attaches them to a **draft** GitHub release; publishing it runs
`publish-feed.yml`. #15 tracked the whole path and is closed. **The agent's:**
pushing an `agent-v*` tag (minted by `./dev publish --agent`) runs
`release-agent.yml`, which drafts an `agent-v*` release; publishing that runs
`publish-agent-feed.yml`. See **The agent's release train** below. GitHub's tag
filter keeps them apart (`v*` does not match `agent-v…`, nor the converse), so a
`v*` tag produces no agent asset and an `agent-v*` tag no cockpit one: #390's
"one tag, one release, both products" no longer holds. **Neither train can be
exercised by a PR** — they trigger on a tag and on a publication — so what can
be tested lives in scripts a PR does run (`secrets-guard.sh`,
`agent-feed-guard.sh`), and a change to a release workflow reaches a release
only through a new tag (a `push` or `release` event runs the workflow file at
the tagged commit).

`./dev build --release` produces
`target/universal-apple-darwin/release/bundle/macos/Solador.app`
through `cargo tauri build` (#303). The bundler is not in `tauri-build` — that
build.rs helper reads `bundle.*` and assembles nothing — so it comes from
`tauri-cli`, pinned by `TAURI_CLI_VERSION` in `scripts/config.sh` and installed
on demand by `build.sh`. Both version numbers are **derived from git**:
`tauri.conf.json` authors no `version` at all, the CalVer arrives as a
`--config` overlay and lands as `CFBundleShortVersionString`, the build number
is stamped over `CFBundleVersion`, and the build asserts all of it back out of
the Info.plist rather than trusting that it worked.

**Every macOS bundle is universal** (`MACOS_UNIVERSAL_TARGET` in
`scripts/config.sh`, #335), which is why the output sits under the triple
rather than at `target/release/bundle`. `v2026.8.110` — the first release ever
cut — shipped **arm64-only** because no target was named, on a bundle whose own
floor is macOS 14.0, which Intel Macs run: "it built" and "our users can run
it" were two different claims. So the build now asserts `lipo -archs` reports
both slices, and checks the deployment floor **per architecture** — `vtool` on
a fat binary prints one block per slice, and reading only the first verifies
half the artifact while reporting on all of it. CI runs the same path at
the debug profile for app/backend/build changes (`./dev build --bundle`,
job **macOS bundle (unsigned)**), so bundling cannot break unnoticed the way the agent deploy did
in #269.

`./dev build --release --notarize` produces a **signed, notarized, stapled
`.dmg`** (#306). `./dev publish` is the same thing behind the CalVer mint, and
its blanket refusal is gone.

`release.yml` (#307) is the same build in CI, and **every job that holds a
credential declares `environment: prd`** — that is the security design, not a
detail. Solador is public, so the org's `APPLE_*` secrets (visibility `private`)
resolve to the empty string here; environment secrets are repo-scoped, so that
visibility never applies to them. `prd` also carries a required reviewer and a
tag-only deployment policy, which is what stops a push to `main` minting a
signed build: `v*` for the cockpit's train, plus `agent-v*` for the agent's — a
repository setting, not a file, added by hand (#492 step 6), so until it is
there neither agent credential job can enter `prd`. Three workflows declare it:
the two credential-holding jobs in `release.yml`; `release-agent.yml`'s
`publish` (its `build` and `verify` deliberately do not); and
`publish-agent-feed.yml`'s `agent-feed`, which signs `agent-latest.json` at
publish time (#391, see **The agent feed** below) with the same agent key
`release-agent.yml` signs the binaries with. **Publishing an agent release
therefore prompts `prd`'s reviewer once more**, after the prompt the tag push
already made for `publish`; a publish that looks stalled is waiting on it.
`ci.yml` declares no environment and references zero secrets, and the
`secrets-guard` job (`scripts/secrets-guard.sh`) asserts all of that rather
than trusting it: it allows `release.yml` by file and exactly two other jobs by
**`file:job` pair** — `release-agent.yml:publish` and
`publish-agent-feed.yml:agent-feed` — requires each one's `environment: prd`
line to be present, and fails closed **per pair** (a tree missing either file
is refused, not read as clean). So a secret in a sibling job, a new job, a
scoped job with its environment removed, a secret above `jobs:`, or either
scoped file deleted all fail CI, and `publish-feed.yml` has no allowance at
all — `scripts/secrets-guard-test.sh` runs those mutations against each pair on
every PR, because a guard that has only ever seen the valid tree is
indistinguishable from one that passes everything. A release is attached as a
**draft**, which is what makes the `release: published` triggers of
`publish-feed.yml` and `publish-agent-feed.yml` meaningful.

**Windows (#341, #342).** `release-windows` is the second leg of `release.yml`, on the
same `prd` environment behind the same `v*` gate, and it produces an
**Authenticode-signed NSIS installer**. `./dev build --release` is the same command
on both platforms; on Windows it bundles via `cargo tauri build --bundles nsis` to
`target/x86_64-pc-windows-msvc/release/bundle/nsis/*-setup.exe`.

**NSIS rather than MSI is arithmetic, not preference.** MSI's `ProductVersion` caps
its major field at 255, and CalVer's major is the year — 2026 does not fit, and no
later year will either.

**Both version numbers arrive during bundling, not after it** — the one place the
Windows path genuinely inverts the macOS one. There is no Info.plist; the analogue is
the PE VERSIONINFO resource compiled into the installer, and Tauri's NSIS template
takes both numbers as build *inputs* (`VIProductVersion`'s fourth word carries the
build number; the `ProductVersion` / `FileVersion` string keys read
`<version>+<build>`). Nothing is stamped over a finished file, so **the macOS "sign
last, because the signature seals the plist" constraint has no Windows counterpart**:
Authenticode appends to the PE and does not touch VERSIONINFO, which is exactly why
the validate step can re-read both numbers *after* signing.

What does carry over is the division of labour. **Tauri's in-bundle NSIS `signCommand`
hook is deliberately left off**, for the same reason `APPLE_SIGNING_IDENTITY` is:
finish the artifact, sign the exact file that ships, then assert the signature back
out of it. A `signCommand` would additionally sign the app binary *inside* the
installer, but the file SmartScreen evaluates is the MotW-carrying download, and
threading a CI-only `signCommand` through `build.sh`'s `--config` overlay would make
CI build differently from every other machine — the drift the "same command a
maintainer runs" rule exists to prevent.

**The signing credential is not a secret at all.** Azure Trusted Signing is reached
through a federated `azure/login` (`id-token: write`, subject scoped to
`environment:prd`), so there is no `.pfx` and nothing to leak — the same principle as
resolving the macOS identity from the keychain by prefix instead of configuring a
hash, reached by a different mechanism. The exchange succeeds only for a job running
in `prd`, which makes "a push to `main` cannot mint a signed build" a property of
Entra's trust configuration rather than of this file. The login is minted **after**
the long build and immediately before its only consumer, because the federated
session is an access token with an expiry rather than a refreshable login. The RFC
3161 timestamp is load-bearing: Trusted Signing certificates are short-lived by
design, and an untimestamped signature would die with its certificate.

**The artifact is validated, never inferred from the signing step's exit code** —
exactly one `*-setup.exe`, both numbers re-read out of the version resource,
`Get-AuthenticodeSignature` reporting `Valid`, a signer subject containing `Sassy Dog
Enterprises LLC` (which is what catches signing with the wrong certificate profile),
and a timestamper present. The same claim-splitting as the macOS `codesign` / `spctl`
re-read.

**One architecture, and it is not a universal analogue.** `WINDOWS_TARGET` is
`x86_64-pc-windows-msvc` and only that — a separate `--target`, never a second slice
of #335's universal story. There is no ARM64 Windows slice, so
`MACOS_UNIVERSAL_TARGET`'s "assert both slices, and check the floor per architecture"
discipline has no counterpart here; the single-file assertion above is what replaces
it. **Local Windows builds stay unsigned**, SmartScreen warning and all: the signing
capability lives in a federated identity no workstation holds.

One Windows-only checkout detail, easy to lose and expensive to rediscover:
`core.autocrlf input` is set **before** `actions/checkout`, because the runner image
defaults it to `true` and `dev` plus `scripts/*.sh` would then arrive CRLF for Git
Bash to read with a literal `\r` on every line. `.gitattributes` deliberately protects
only files whose bytes are load-bearing — the signature-covered fixtures and, since #599,
`crates/hostsetup/remote/host.sh` (embedded with `include_str!` and run by a *remote*
`/bin/sh`, so no checkout step could protect it) — so `dev` and `scripts/*.sh` are
protected here instead, at the only place a Windows checkout executes them.

**Windows updates in place too (#705), and the installer is the payload.**
`latest.json` carries `darwin-aarch64`, `darwin-x86_64` and `windows-x86_64`, and
all three are required. `tauri-plugin-updater` downloads the raw
Authenticode-signed `*-setup.exe`, verifies its minisign signature against
`plugins.updater.pubkey`, runs it with `/UPDATE`, and calls `std::process::exit(0)`
right after launching it. `release-windows` therefore does two more things after
Authenticode signing:

- **It minisigns the exact signed installer** with `cargo tauri signer sign`, the
  signer `build.sh`'s `make_updater_archive` uses and the same
  `TAURI_SIGNING_PRIVATE_KEY` pair (the job's preflight now checks six secrets,
  the password required non-empty). It is **after** Authenticode because
  Authenticode appends to the PE: a `.sig` made earlier covers bytes that never
  ship. The signature is then verified against `tauri.conf.json`'s pubkey with
  `solador-verify-signature` (the plugin's own algorithm; the Tauri CLI signs but
  cannot verify) before anything is uploaded, never on the signer's exit code, and
  the validate step fails if the `.sig` is missing. `<installer>.sig` is uploaded
  beside the installer.
- **`bundle.createUpdaterArtifacts` and the NSIS `signCommand` stay off.** The
  bundler would minisign a zip made *before* Authenticode, and a `signCommand`
  would put a CI-only branch in the build; the file the signer is handed has to be
  the signed one.

`publish-feed.yml` harvests exactly one `*-setup.exe` and its `.sig`, fails on zero
or more than one, and `solador-update-feed` verifies both signatures and **requires**
the Windows entry: a feed without `windows-x86_64` would make every updater-enabled
Windows install report a failed check on every launch (#368's symptom), so a
release missing the installer or its `.sig` has no feed. The corollary is that the
replay door cannot rebuild the feed for a release cut before this landed.

**Install closes and reopens Solador on Windows.** `plugins.updater.windows.
installMode` is `"passive"` in `tauri.conf.json`, stated rather than inherited: the
installer shows only a progress bar, and with the plugin's default
`restart_after_install` it passes `/R`, so Solador reopens on the new version. That
differs from macOS, where an installed update takes effect the next time the app is
opened, so `viewmodel::update` says it **before the click** (the offer line, the
help text and the button label); the shell only supplies `InstallEffect` by `cfg!`.
Nothing installs without the operator's click. `process::exit` runs no destructors
and no `RunEvent::Exit`; the audit found one thing a normal quit persists that it
would lose, the window geometry `tauri-plugin-window-state` saves on `Exit`, so
`run_update_install` passes the plugin an `on_before_exit` hook that saves it (and
nothing more: the plugin's default `cleanup_before_exit` hides every window, and a
failed installer launch returns an error without exiting, which must leave the
window visible to show it). Everything else,
the store included, is written when it changes.

**Existing Windows installs never update to this release.** They were built
`Channel::Absent`, show *"Updates are not published for this platform"* and never
check, so a user installs the first updater-enabled release by hand, once. Only
builds from that release on check `latest.json`. **No PR can exercise any of this**
(neither release train runs in a PR): install that release on Windows by hand,
publish the next one, and confirm the in-app update closes Solador and reopens it
on the new version.

A platform the feed has no key for (Linux, or Windows on another architecture)
still reaches `NotUpdatable::NoChannel` *before any request is made* (#368), rather
than asking, getting `TargetNotFound` and rendering *"the update check failed"*.
The shell's `channel_for(os, arch)` mirrors `crates/updatefeed`'s platform keys
rather than reading them, because the app deliberately does not depend on the
release tooling; when a platform gains a payload, those two change together.

**The agent's release train (#472, #491; #390 first built it as `release.yml`'s
third leg).** `release-agent.yml` runs on an `agent-v*` tag push, and its
`build`, `verify` and `publish` jobs produce four **raw binaries** —
`x86_64`/`aarch64-unknown-linux-musl`, `aarch64`/`x86_64-apple-darwin` — each
with a detached minisign `.minisig`, attached to a **draft** `agent-v*` release
created with `make_latest: false` (`--latest=false`). One tag per product, one
release per tag: the agent no longer rides the cockpit's `v*` release (see
`docs/VERSIONING.md` for its number and mint). `build` first runs
`scripts/assert-release-tag.sh --agent <tag>` (the agent mint re-run read-only;
it must answer `action=reuse` for exactly that tag) and pins
`AGENT_MARKETING_VERSION` from it, which `agent/build.rs` and `./dev agent` —
the same command locally — both honour. `publish` uploads only to a **draft**:
a re-run after the release was published is refused, because replacing
binaries whose hashes hosts have already been told would strand a host on that
version at exit 4.

**The agent's macOS floor is its own, and it is 11.0** — not the workspace's
14.0. `.cargo/config.toml` declares that floor for the cockpit's frontend
(`adoptedStyleSheets`); a daemon with no webview inheriting it would publish a
binary advertised for Intel Macs that an Intel Mac on macOS 12 or 13 cannot
launch — a floor arrived at by accident, which is the mistake #335 caught in the
bundle's architecture. `[env]` yields to an inherited value, so
`AGENT_MACOS_MIN_VERSION` is exported at the cargo invocation and read back out
of each Mach-O with `vtool`.

**musl, not gnu, and asserted out of the ELF.** A dynamically linked gnu build
resolves the *builder's* glibc and dies on any older host with `GLIBC_2.xx not
found` — invisible to us, fatal to a stranger's first install. So the build
checks `file` for `statically linked` (or `static-pie linked`; rustc's musl
targets emit either) *and*, where readelf exists, for the absence of a
`PT_INTERP` segment. Cross-compilation is **`cargo-zigbuild`**, not `cross`: no
Docker on the runner, faster per target, and musl is its strength. Both Linux
targets go through it, so the ARM artifact is not built by a different path than
the one the x86 runner exercised.

**Three jobs, not one, and the split is the acceptance criterion.** Build →
verify → publish. `--version` is executed for **every** target on a runner
matching it (`ubuntu-latest`, `ubuntu-24.04-arm`, `macos-latest`,
`macos-15-intel`) against the files the build uploaded, because an artifact that
does not start is worse than no artifact and a cross-compiled binary links
perfectly well on a machine that cannot run one instruction of it. Only the
publish job declares `environment: prd`, so the signing key touches one runner
rather than six and the other two start without waiting on the reviewer.

**The agent keypair is separate from the app's**, and that is the security
argument, not tidiness: the app updates on a laptop, the agent runs unattended as
a service on servers, which is the higher-value target. Its public half is
committed at `agent/release-signing-key.pub` and every signature is re-verified
against **that file** before upload — a mis-provisioned
`SOLADOR_AGENT_SIGNING_PRIVATE_KEY` fails the release rather than shipping
signatures nobody can check — then verified again by the reference C `minisign`,
a different implementation from the `rsign2` that signed. Signatures are plain
minisign, not the Tauri signer's double-base64 form, so any user can check a
download with the stock tool — `agent/deploy/install.sh` does exactly that
(#392), and so does **the agent binary itself** (#393): `solador-agent
update` verifies the feed and the downloaded binary under the public keys its
`build.rs` compiles in from `agent/release-signing-key.pub` and, once
committed, `agent/release-signing-key-next.pub` — the standby that makes a
rotation a release rather than a recall. See `docs/AGENT-DEPLOYMENT.md`.

**The agent feed (#391).** `agent-latest.json` is the agent's counterpart to
`latest.json` and deliberately **not** an extension of it: Tauri owns that
schema. It carries `version` (the agent release's CalVer, no `agent-v`) and
`targets` keyed by the full triple — exactly the four — each with `url` (on the
`agent-v<version>` release: the only shape the producer writes since #490, and
the only one `update.rs` accepts), the binary's plain-minisign
`signature` verbatim, and `sha256` over the raw bytes. **The hash is what lets
an installed agent answer "is the binary I am running already this release's?"
without downloading** (the version is compiled in, so a new version is always new
bytes); it is
computed by `crates/updatefeed::agent` from bytes whose signature *already
verified* under `agent/release-signing-key.pub`, never taken from a caller, and
a missing, duplicate or unknown target, a foreign signature or a moved byte
means nothing is written. The document is signed as a whole
(`agent-latest.json.minisig`, over the **exact served bytes**, trailing newline
included) by `scripts/agent-signing.sh` — the one signer, sourced by
`build-agent.sh` for the binaries and run as `agent-signing.sh sign` by the
workflow for the feed, so the two cannot be signed differently (not a `./dev`
command: locally the binaries sign through `./dev agent --sign`) — then
re-verified as a consumer would (bytes first, JSON second, every binary against
its entry) and by the reference C `minisign` before either half is uploaded.
Unlike the desktop feed this **needs a credential at publish time**, because the
feed is assembled from a public release's URLs and no such document exists
during the draft build; that is why `publish-agent-feed.yml`'s `agent-feed` job
is the one protected job outside `release.yml` and `release-agent.yml`, with a
credential-free `agent-eligibility` job in front of it that refuses drafts,
prereleases, releases without the eight agent assets, a manual replay that is
not running *at* its tag, and two more (#491, both in
`scripts/agent-feed-guard.sh` so `./dev test` can run them against a stub `gh`):
the **freeze guard** — the candidate must be the **newest published,
non-prerelease `agent-v*` release** by CalVer, read from the *paginated*
release list, unless the dispatch passes `force`, because a replay at an older
tag would `--clobber` a newer feed with an older one and every host would answer
exit 4, which the timer counts as success — and the **latest guard** —
`releases/latest` must name a `v*` tag, failing with `gh release edit <tag>
--latest=false` if an agent release took the slot from the cockpit's updater.
The freeze guard reads the list, never the served feed (which is briefly wrong
mid-`--clobber`), so re-running at the *current* tag after a failed upload
passes and a pulled bad release lets the previous one be re-served with no
`force`; it runs again inside `agent-feed` after `prd`'s approval, because that
wait can be hours. The feed goes onto the permanent **`agent-latest`
prerelease** — created once, its tag at a commit that never moves, only the two
feed assets, a prerelease so it can never be `releases/latest` — and the job
then **reads the served bytes back** from the URL hosts read and requires them
to equal the upload and to name this tag's version. **A `release` event runs
`publish-agent-feed.yml` at the tagged commit, not from `main`** — measured
against this repo's own runs — so a fix to that workflow reaches a release only
through a new agent tag, and the manual replay must be dispatched *from* the
tag. `agent/` does not depend on `crates/updatefeed` (nor on the cockpit's
reader of this feed, `crates/agentrelease`, #489) — `scripts/agent-deps-guard.sh`
asserts both absences with `cargo tree`, in CI's `secrets-guard` job and in
`./dev lint`; the consumer (`agent/src/update.rs`, #393) compiles the public
keys in and verifies with `minisign-verify` on its own.

**Updates (#308).** `--sign` also produces the updater payload: a minisigned
`Solador-<version>.app.tar.gz` beside the `.dmg`. The key is
`TAURI_SIGNING_PRIVATE_KEY` plus `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (must be
set; empty only for an unencrypted key), from Doppler `solador/prd`;
`release.yml` has both from the `prd` environment, and a local `./dev publish`
requires both **in pre-flight, before the mint** (#402), the same way it
requires `SENTRY_DSN`. A `.dmg` is not an update —
macOS applies a tarball — so both ship as release assets. (Windows is covered
above: its payload is the signed installer itself.) `bundle.
createUpdaterArtifacts` is deliberately **off**: the CLI builds that tarball
*during* bundling, which is before the `CFBundleVersion` stamp, before the
signature and before the staple, so it would carry a different artifact from
the one in the `.dmg`. `build.sh`'s `make_updater_archive` builds it last
instead and reads the version back out of the archive to prove the ordering —
the identical argument that keeps `hdiutil` rather than `--bundles dmg`.

The feed (`latest.json`) is generated by **`.github/workflows/publish-feed.yml`
on `release: published`** of a `v*` release, never at build time: `release.yml`
attaches to a *draft*, and a draft's assets are not publicly downloadable, so a
build-time manifest would point every client at a 404. The trigger is what makes
"a draft can never serve updates" a property rather than a rule. The workflow is
desktop-only since #491: its `feed` job does not run for an `agent-v*` release,
whose feed is the agent's own — and the condition is not a bare `startsWith` on
the tag, which is false on a `workflow_dispatch` (no release payload) and would
have shut the replay door, so each event's clause carries its own event name.
`crates/updatefeed` builds and **verifies** that manifest — it refuses to write
one unless both the macOS and the Windows signature cover their artifacts under the
public key in `tauri.conf.json`.

In the app: the check runs **once, in `setup`**, never on a cadence, and nothing
auto-installs — a cockpit on a second monitor does not restart itself. (On Windows
the operator's click on Install does close it, which the words say first.)
`tauri-plugin-updater` is granted **nothing** in `capabilities/default.json`
(the `tauri-plugin-notification` precedent), and "up to date", "checking", "the
check failed", "an update is waiting" and "this build has nothing to replace"
are five separate states in `viewmodel::update` that must never render alike.
The last of those carries three distinct *reasons* — a `./dev` build, a build that
cannot name itself, and a platform with no channel — which must not render alike
either; only the first two are facts about the build.

**The version compared against the feed is `settings::VERSION`, never the
plugin's own (#353).** Left alone, `tauri-plugin-updater` evaluates
`release.version > package_info().version`, and `package_info().version` reads
`tauri.conf.json`'s `version` — a key this repo does not author — so Tauri falls
back to `app/src-tauri/Cargo.toml`'s unpublished `0.1.0`. Every CalVer ever
released beats that, so every locally built cockpit announced an update on every
launch. `UpdaterBuilder` has **no `current_version` setter** (private field, set
from `package_info()`); the supported seam is `default_version_comparator`, and
`viewmodel::update::is_newer` is what it calls. The other half is that a
development build is not compared *at all*: `update::not_updatable` reads the
cargo profile and lands it in `Check::NotUpdatable` before any request is made,
because a release `.app.tar.gz` unpacked over `target/debug/Solador.app` is not
an update, and an offer that cannot be honoured is a fabricated state. Bumping
`app/src-tauri/Cargo.toml`'s `0.1.0` is **not** a fix — it would create a third
place a version lives.

**Signing runs last, after the `CFBundleVersion` stamp, and the ordering is not
a preference.** Info.plist is sealed by the signature, so stamping it afterwards
yields `invalid Info.plist (plist or signature have been modified)` — measured,
not assumed. That is also why Tauri's own `APPLE_SIGNING_IDENTITY` path is left
off: the CLI signs *during* bundling, which is before this repo has stamped
anything. Same reason the `.dmg` is built with `hdiutil` here rather than by
`--bundles dmg`. Keep both after the stamp.

The identity is resolved from the keychain by the prefix `Developer ID
Application` rather than configured, so no per-machine certificate hash lands in
the repo; `APPLE_SIGNING_IDENTITY` overrides for CI. **Two matches is refused,
never resolved by picking the first.** Gatekeeper shows `Sassy Dog Enterprises
LLC` — a distribution certificate's name is derived by Apple from the team's
legal entity, so no certificate under this team can read anything else.

Notarization uses the **App Store Connect API key** (`APPLE_ASC_KEY_ID`,
`APPLE_ASC_ISSUER_ID`, `APPLE_ASC_KEY_BASE64`, from Doppler `_stores/apple`),
never an Apple ID plus app-specific password. The key is written to a 0600 file
in a private temp dir because `notarytool` takes a path, and removed on every
exit path.

`./dev publish --skip-mint` runs the whole signed-and-notarized build **without
minting or pushing a tag** — the seam that lets the release path be exercised
without cutting a release.

Under `cargo tauri build`, `.cargo/config.toml`'s `MACOSX_DEPLOYMENT_TARGET` is
**shadowed**: the CLI exports the floor from `bundle.macOS.minimumSystemVersion`
into cargo's environment, and `[env]` yields to an inherited value. Both
declare 14.0; keep them equal, and do not assume editing the cargo config moves
the bundle's floor.
