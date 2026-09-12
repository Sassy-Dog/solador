# Secrets and credentials

This repository stores **no credentials**. Nothing here is a secret, and there
is no `.env` to fill in.

Two different things get called "secrets" below, and keeping them apart is the
whole point of this document:

- **Runtime credentials** — the tokens *you* give the app so it can read your
  GitHub, Neon, Sentry, Vercel and Azure accounts. These live in your OS
  credential store. The app never writes them to disk.
- **Build-time configuration** — optional environment variables the
  maintainer's *release* build uses. Everything else builds without them.

## Runtime credentials

Entered in **Settings**, stored in the OS credential store (Keychain on macOS,
Credential Manager on Windows), never in `store.json`.

| Credential | Panel | Scope needed |
|---|---|---|
| GitHub fine-grained PAT | Repos, Runners | read: Actions, Contents, Issues, Pull requests |
| Per-host bearer token | Hosts, Containers | whatever the agent was installed with |
| Neon API key | Usage | organization-scoped |
| Sentry auth token | Usage, Sentry Crons | `org:read` only |
| Vercel API token | Usage | read |
| OpenClaw bearer + device key | OpenClaw | gateway-dependent |

There is a test asserting the settings file holds no secret material, and the
Azure blob client strips URLs out of transport errors on purpose — a SAS is a
query string, and error text gets pasted into issues.

**Azure Cost has no stored credential at all.** The panel mints a short-lived,
container-scoped, read-only SAS per poll by shelling out to the Azure CLI
(`az`, signed in as you), and stores nothing. It needs `az` installed and
`az login` done; the storage account and container are ordinary settings.

## Build-time configuration

All **optional**, all read from the **environment**. No build script knows where
they come from, and no contributor needs to. The table is the list — do not
count them in prose here, because the count is what went stale last time.

| Variable | Needed for | Without it |
|---|---|---|
| `SENTRY_DSN` | where opt-in crash reports go — see below | The crash reporter is a silent no-op: nothing is sent, nothing errors. This is the normal state of any build you compile yourself. |
| `DEVELOPMENT_TEAM` | picking a specific Apple signing team | `codesign` falls back to an ad-hoc signature. `./dev build` works either way. |
| `APPLE_ASC_KEY_ID`, `APPLE_ASC_ISSUER_ID`, `APPLE_ASC_KEY_BASE64` | notarizing a release (`./dev build --notarize`, `./dev publish`) | The build **fails before submitting** and names which of the three is unset. Everything short of notarization — including `--sign` — works without them. |
| `APPLE_SIGNING_IDENTITY` | overriding which certificate signs | The identity is resolved from the keychain by the prefix `Developer ID Application`. Only needed where that is ambiguous or absent (CI). |

Nothing in the day-to-day loop needs any of them: `./dev`, `./dev test`,
`./dev lint` and `./dev build` all work on a clean clone with none of them set.
`./dev publish` is the exception — see `.envrc` and `scripts/publish.sh`.

### Crash reporting is opt-in and off by default

`crates/crashreport` (#309) carries the Sentry SDK and installs a panic hook —
but **only** when both of these are true, checked in this order:

1. The operator has ticked **Settings → General → Crash Reporting**. It is off
   in a fresh store and off in every store written before the feature existed.
   The toggle is the *only* opt-in: a DSN being compiled in is not consent, a
   release build is not consent, and neither ever becomes consent.
2. This build was compiled with `SENTRY_DSN`.

Anything else — including "the setting could not be read" — starts no client,
installs no panic hook, and reaches no network code. Turning the toggle **off**
takes effect immediately; turning it **on** takes effect at the next launch, and
the Settings tab says which of those states it is in rather than implying the
switch did more than it did.

`SENTRY_DSN` is read at **compile** time, and a `SENTRY_DSN` in your shell at
*run* time is deliberately ignored — an environment variable that could redirect
someone's crash reports to a third party is not a thing to leave lying around.
`scripts/publish.sh` reads it in pre-flight, **before** it mints a tag, and
`release.yml` sets it from the `prd` environment's secrets — so a release either
carries a DSN or was told to go without one by an explicit `--skip-sentry`.

**What a report may contain** is an allow-list, not a blocklist — see
`crates/crashreport/src/scrub.rs`. The event is rebuilt from a fixed set of
fields rather than edited, so `server_name` (the machine's hostname), `user`,
`request`, breadcrumbs, tags, extra, contexts, loaded modules, absolute paths,
source lines and local variables are gone by construction; the free text that
does survive (the panic message, symbol and file names) is then held to a
positive word rule that redacts addresses, URLs, host names, paths and
token-shaped strings. **No credential is ever collected in the first place** —
tokens live only in the OS credential store, and nothing puts one on an event.

> Do not confuse any of this with `crates/usage/src/sentry.rs`, which shares
> only a vendor name: that one *reads* Sentry's REST API for the Usage and
> Sentry Crons panels using the `org:read` token in the table above. It reports
> nothing, and it has no DSN.

### Locally — direnv

`.envrc` is committed and holds no values; it sources `.envrc.local`, which is
gitignored. Put real values there:

```bash
# .envrc.local
export SENTRY_DSN="https://…@….ingest.sentry.io/…"
export DEVELOPMENT_TEAM="XXXXXXXXXX"
```

Then `direnv allow` once. If you pull these from a secret manager, do that in
`.envrc.local` too — the build scripts never see the difference.

### In CI — workflow secrets

A release workflow sets them from `secrets.*`:

```yaml
env:
  SENTRY_DSN: ${{ secrets.SENTRY_DSN }}
  DEVELOPMENT_TEAM: ${{ secrets.APPLE_TEAM_ID }}
```

**Deliberately not in `ci.yml`.** That workflow references **zero** secrets, and
that is a security property rather than an oversight: this repository is public,
so a fork's pull request runs CI, and a secret reachable from a fork PR is a
secret you have given away. `ci.yml` builds and tests — neither needs one.
Signing and releasing belong in workflows that do not run on `pull_request`.

Exactly two workflows hold a credential, and every job that does declares
`environment: prd` — a required reviewer plus a `v*`-tag-only deployment
policy, so nothing on `main` or on a fork can reach one:

- `release.yml`, on a `v*` tag push: the Apple and Tauri updater secrets for
  the macOS leg, `SENTRY_DSN` for both cockpit legs, the `AZURE_CLIENT_ID` /
  `AZURE_TENANT_ID` / `AZURE_SUBSCRIPTION_ID` identifiers the Windows leg's
  federated Trusted Signing login is minted from, and
  `SOLADOR_AGENT_SIGNING_PRIVATE_KEY` for the agent binaries. Three of its
  five jobs; the agent's build and verify jobs hold nothing.
- `publish-feed.yml`, on `release: published`: its `agent-feed` job reads
  `SOLADOR_AGENT_SIGNING_PRIVATE_KEY` — the same key, no new scope — to sign
  `agent-latest.json`, which cannot be signed at build time because it is
  assembled from a public release's download URLs (#391). The desktop-feed job
  beside it reads nothing. A manual replay of that leg has to run *from* the
  tag with `leg: agent`, not from `main` with a tag typed in; a desktop-only
  replay runs from `main` with `leg: desktop` and never reaches the protected
  job.

`ci.yml`'s `secrets-guard` job (`scripts/secrets-guard.sh`) asserts this rather
than trusting it: any secret reference outside `release.yml`, or in
`publish-feed.yml` outside the `agent-feed` job, or in that job without its
`environment: prd` line, fails CI — and `scripts/secrets-guard-test.sh` runs
those mutations against the same script on every PR. The same job asserts the
other deliberate absence in this area, that `agent/` does not resolve the feed
producer `crates/updatefeed` (`scripts/agent-deps-guard.sh`).

### The agent's two signing keys: active and standby (#393)

The agent binary's updater (`solador-agent update`) trusts **two** public keys
compiled in at build time, so that losing or burning the active key is a
release rather than a recall (`docs/AGENT-DISTRIBUTION.md` §5). The two halves
of each key live in different places on purpose, and this table is the
authority on where:

| Key | Public half (committed) | Private half (Doppler, the source of truth) | Synced to GitHub | Read by |
|---|---|---|---|---|
| **Active** | `agent/release-signing-key.pub` (id `B2E5C62B763FD2C4`) | project `solador`, config **`prd`**, secret `SOLADOR_AGENT_SIGNING_PRIVATE_KEY` | the repo's **`prd` environment** (environment-scoped sync, live since 2026-08-15) | `release.yml` → `release-agent-publish`; `publish-feed.yml` → `agent-feed` |
| **Standby** | `agent/release-signing-key-next.pub` (written by the custody script; committed by the operator — until then a build is a one-key trust set) | project `solador`, config **`custody`**, secret `SOLADOR_AGENT_SIGNING_STANDBY_PRIVATE_KEY` | **nowhere** — the config has no integration, by design | nothing, until a rotation is separately authorised |

**Why a config that syncs nowhere.** Every value in a synced config reaches a
GitHub secret, and a GitHub secret is reachable by any job that declares the
right `environment:`. The standby exists precisely so that a compromise of the
routine release path does not also yield the *next* key; a standby sitting
beside the active key in `prd` would be one credential with two names. So the
standby is never in a synced config, never in `vars.*`, never in a
GitHub-only copy, and never a workflow input. A `release.yml` step that
referenced `secrets.SOLADOR_AGENT_SIGNING_STANDBY_PRIVATE_KEY` would resolve
to the empty string (the `prd` environment does not carry it) and fail the
key-shape preflight — the guard is the sync's absence, not a naming rule.

**What was in place when the standby was designed (read on 2026-09-12; names
only, never values).** Doppler project `solador` had four configs — `dev` and
`dev_personal` (environment `dev`), `gh-actions-repo`, and `prd` (root,
locked). Only `prd` had a live GitHub Actions sync (its audit log: *Added
GitHub, Actions: Sassy-Dog / solador / prd integration*, 2026-08-15); `dev`
and `gh-actions-repo` had synced to the pre-rename repository and those
integrations were removed on 2026-07-14 and 2026-08-15; `dev_personal` had
never synced. On the GitHub side the repo had exactly one environment, `prd`,
whose 18 secret names were exactly Doppler `prd`'s, and three repository-level
secrets (`DOPPLER_CONFIG`, `DOPPLER_ENVIRONMENT`, `DOPPLER_PROJECT`). No
standby existed anywhere. `prd` is ruled out because it syncs; the `dev`
configs are ruled out because they are a different ownership axis (developer
`SENTRY_DSN`s), not custody. Hence a **new** environment, `custody`.

**What the operator creates, once** — nothing in this repository can, and the
custody script deliberately refuses to:

```sh
doppler environments create "Custody - never synced" custody --project solador   # Doppler rejects parentheses in environment names
```

That creates the environment and its root config, both named `custody`.
**Never attach an integration to it.** Confirm it syncs nowhere, before and
after provisioning:

```sh
doppler configs logs --project solador --config custody --json   # no "Added … integration" entry
gh api repos/Sassy-Dog/solador/environments/prd/secrets --jq '.secrets[].name'   # standby absent
gh api repos/Sassy-Dog/solador/actions/secrets --jq '.secrets[].name'            # standby absent
```

**Provisioning is `scripts/agent-standby-key.sh`, never a hand-run
`rsign generate`.** Run it from a checkout of the branch, signed in to Doppler
(`doppler login`) and GitHub (`gh auth status` — the org-secrets listing needs
`admin:org`), with the stock `minisign` and a Rust toolchain on `PATH` (the
pinned `rsign2` is installed with `cargo install` if missing, and the last
step is a `cargo test`):

```sh
scripts/agent-standby-key.sh            # defaults: --project solador --config custody --repo Sassy-Dog/solador
git add agent/release-signing-key-next.pub
```

What it does, and what it refuses, in order:

1. Preflight — `doppler`, `gh`, a real `minisign` (one that identifies itself
   as minisign; an `rsign` symlink would be an accept-everything verifier),
   the pinned `rsign2` (installed if missing, in a step that holds no key).
   The target config must exist, must not be `prd`, and its audit log's
   newest integration event — walked page by page to the end of the log,
   and a page that cannot be read is a refusal — must not be an *Added*.
   `set +x` is forced.
2. Looks for the standby **by name only** (`doppler secrets --only-names
   --json`). Never reads the value at this step.
3. State: secret present + public file present → **reuse**, no generation;
   secret present + file missing → **refuse** (restore the committed file
   from history, or delete the Doppler secret deliberately and re-run to mint
   a fresh pair — the script does not guess); file present + secret missing →
   **refuse** (a public key with no private half in custody is worthless;
   remove the file deliberately); neither → generate with `rsign generate -W`
   into a `mktemp -d`/`chmod 700` directory under `umask 077`, write the
   public half to `agent/release-signing-key-next.pub` with the key id (read
   out of the key **bytes**, not the comment) on line 1, and upload the
   private half with `doppler secrets set … --silent < file` — **on stdin**,
   never on argv, never printed, the CLI's own echo discarded.
4. Custody proof with the value **retrieved from Doppler** (`doppler secrets
   get --plain --raw` into the private temp dir): signs non-secret fixture
   bytes with it, verifies under the committed standby file with the stock
   `minisign` (must pass), verifies the same signature under the active key
   (must **fail** — distinct identities), and verifies an unrelated
   throwaway key's signature under the standby file (must **fail** — the
   verifier is verifying). The standby's key id must differ from the active
   key's, and the file's line-1 comment must name the id its bytes carry.
5. GitHub, by secret-name metadata (GitHub cannot return values): the standby
   must be absent from the `prd` environment, from repository secrets and
   from the organisation's secrets, and the active key must be present in
   `prd` (a warning if not — the release path looks broken). A listing that
   cannot be read is a refusal, never an "absent".
6. Every private-key copy is removed, **then** `cargo test -p solador-agent`
   runs the updater's trust-set test, so "two distinct real keys are
   embedded" is asserted by the binary that will verify under them — in a
   step that holds no secret, since a test build runs third-party build
   scripts.
7. `trap … EXIT` removes the temp directory — the generated key, the
   retrieved copy, the throwaway key, the fixture — on every exit path,
   success or failure.

A re-run is safe: it repeats 4–6 against what exists and changes nothing.
**Run it before the first `v*` tag after #393 merges**: a release cut from a
tree without `agent/release-signing-key-next.pub` ships agents with a
one-key trust set, and the rotation window §5 promises does not exist for
those hosts until they update again.
`agent/deploy/lib_test.sh` runs the whole script against a file-backed
`doppler` stub, a `gh` stub and a `cargo` recorder, with `rsign` stubbed onto
the real `minisign`, and asserts every clause above — including that the
private key appears in no output and on no argv, and that the temp directory
is empty afterwards.

**Custody and access.** The Doppler workplace owner holds both keys' private
halves; the CLI's own login is the only credential the script uses. No CI job
can read the standby, no GitHub copy exists, and adding one is a deliberate
act with a reviewer attached — which is what a rotation is. **Provisioning is
not rotation**: after this, releases still sign under the active key. The
staged rotation (ship trust in both keys → verify fleet uptake → separately
authorise the switch of `prd`'s active secret to the standby → distribute a
replacement standby) is described in `docs/AGENT-DISTRIBUTION.md` §5, and
each of its later steps is its own decision. The last of them retires the
old standby by deleting the custody secret **and** removing
`agent/release-signing-key-next.pub` before running the script again — it
refuses either half-state on its own, so a rotation is two removals, then
one run.

### Why the Apple team id is here at all

It is **not** confidential — a team id ships in the signature of every binary
Apple distributes, and you can read it out of any signed app. It lives outside
the repository because a copy of it in a build config goes stale silently.

That is also why its handling is soft: unset produces a warning and an ad-hoc
signature, never a failure. A hard requirement would make the repository
unbuildable for anyone who has not been handed a value — a real cost, to protect
something that is not protected anyway.

## Contributors

You need none of the above. `./dev`, `./dev test` and `./dev lint` work on a
clean clone with no credentials and no Azure CLI. Panels you have
not configured say so rather than failing.
