# Secrets and credentials

This repository stores **no credentials**. Nothing here is a secret, and there
is no `.env` to fill in.

Two different things get called "secrets" below, and keeping them apart is the
whole point of this document:

- **Runtime credentials** — the tokens *you* give the app so it can read your
  GitHub, Neon, Sentry, Vercel and Azure accounts. These live in your OS
  credential store. The app never writes them to disk.
- **Build-time configuration** — two optional environment variables the
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

Two values, both **optional**, both read from the **environment**. No build
script knows where they come from, and no contributor needs to.

| Variable | Needed for | Without it |
|---|---|---|
| `SENTRY_DSN` | nothing today — see below | No effect. The app has no Sentry SDK, and `./dev publish` refuses before reaching this value. |
| `DEVELOPMENT_TEAM` | picking a specific Apple signing team | `codesign` falls back to an ad-hoc signature. `./dev build` works either way. |

Nothing in the day-to-day loop needs either: `./dev`, `./dev test`, `./dev lint`
and `./dev build` all work on a clean clone with neither set.

> **`SENTRY_DSN` is inert.** This app does not report its own errors to Sentry:
> there is no `sentry` crate in any manifest and no panic hook. The integration
> that consumed a DSN (#18, opt-in) lived in the macOS app that was deleted, and
> the Tauri port never re-added it — it read the value from an xcodebuild build
> setting and a `SentryDSN` Info.plist key, neither of which this repo has. On
> top of that, its only would-be reader is `scripts/publish.sh`, which refuses
> before it gets there because there is no release path (**#15**). Set it if you
> like; nothing reads it. Do not confuse this with `crates/usage/src/sentry.rs`,
> which *reads* Sentry's API for the Usage and Crons panels — that uses the
> `org:read` auth token in the table above, not a DSN.

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
Signing and releasing belong in a separate workflow that does not run on
`pull_request`.

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
