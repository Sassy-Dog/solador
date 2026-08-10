# Secrets and credentials

This repository stores **no credentials**. Nothing here is a secret, and there
is no `.env` to fill in.

Two different things get called "secrets" below, and keeping them apart is the
whole point of this document:

- **Runtime credentials** — the tokens *you* give the app so it can read your
  GitHub, Neon, Sentry, Vercel and Azure accounts. These live in your OS
  credential store. The app never writes them to disk.
- **Build-time configuration** — values the maintainer's release build needs.
  These come from Doppler, and every one of them has a documented way to build
  without it.

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

Org convention: **Doppler is the source of truth**, and consumer repos must not
keep their own copy of a shared value.

| Value | Source | Building without it |
|---|---|---|
| `SENTRY_DSN` | Doppler `devcanopy/dev` | `./dev publish --skip-sentry`, or set `SENTRY_DSN` in the environment. A build with no DSN makes Sentry no-op, which is the default for every non-release build. |
| `TEAM_ID` (Apple) | Doppler `_stores/apple`, referenced from `devcanopy/dev` | Set `DEVELOPMENT_TEAM` in the environment or in the gitignored `Scripts/config.local.sh`. Unset, Xcode picks a team and `./dev build` still works. |

Neither is required to build, test or run this project. Both fail with a message
naming the escape hatch rather than a wall.

### Why the Apple team id is here at all

It is **not** confidential — a team id ships in the signature of every binary
Apple distributes, and you can read it out of any signed app. It is in Doppler
for single-source-of-truth reasons: it was previously copied into `project.yml`,
which is the kind of duplication that goes stale silently and that the org
convention exists to prevent.

That distinction is why its resolution ladder is deliberately soft — environment
override first, Doppler second, a warning and Xcode's own choice third. A hard
requirement would make the repo unbuildable for anyone without Doppler access,
which is a real cost to protect a value that is not protected anyway.

## Contributors

You need none of the above. `./dev`, `./dev test` and `./dev lint` work on a
clean clone with no credentials, no Doppler, and no Azure CLI. Panels you have
not configured say so rather than failing.
