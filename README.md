# Solador

A cockpit for everything around your code — machines, CI, containers, spend,
vendor status, agents — read at a glance from a second monitor.

*Solador* is Spanish for **tiler**: the tradesperson who lays tiles. The cockpit
is a grid of tiles; this is what arranges them.

![The Solador cockpit](docs/assets/screenshots/cockpit.png)

Runs on **macOS and Windows**.

## Is this for you?

Probably not, and it's cheaper to say so here than after you've cloned it.

Solador is opinionated about a specific stack. It is most useful if you run
several repos on **GitHub Actions**, keep machines you reach over **Tailscale**,
and pay **Neon, Sentry, Vercel or Azure**. Every panel is independent and
degrades to a setup line when unconfigured, so you can use two of nine — but if
none of that describes you, there is not much here.

It is also **not a monitoring product**. There are no alerts, no history beyond
what fits on screen, no retention, no server. It is a window you leave open.

## What it shows

| Panel | Reads |
|---|---|
| **Hosts** | CPU, memory, disk, network, GPU, battery — this machine plus any host running the [agent](agent/) |
| **Containers / VMs** | docker, podman and tart, locally and on every host |
| **GitHub Repos** | running workflows, longest-running elapsed, branch and worktree counts |
| **GitHub Runners** | your org's self-hosted runners, with an absence roster |
| **Usage** | Claude Code token rollups, plus Neon, Sentry and Vercel consumption |
| **Azure Cost** | the daily cost export, month to date |
| **Sentry Crons** | every cron monitor that is not `ok`, and **how long it has been broken** |
| **Services** | availability for GitHub, Anthropic, Vercel, Neon and Azure |
| **OpenClaw** | an agent farm, over a live WebSocket |

<details>
<summary>Individual panels</summary>

| | |
|:--:|:--:|
| ![Repos](docs/assets/screenshots/panel-repos.png) | ![Runners](docs/assets/screenshots/panel-runners.png) |
| ![Containers](docs/assets/screenshots/panel-containers.png) | ![Usage](docs/assets/screenshots/panel-usage.png) |
| ![Sentry Crons](docs/assets/screenshots/panel-crons.png) | ![Services](docs/assets/screenshots/panel-services.png) |

At a narrower width the layout reflows rather than scrolling:

![Narrow](docs/assets/screenshots/cockpit-narrow.png)

</details>

## The one design rule

**Unknown is representable, and it is never rendered as zero.**

Every value a producer might fail to measure is optional the whole way down —
an absent key decodes to nothing, and `0` means *measured zero*. A dash is a
dash:

- `—` means nobody could find out.
- A dimmed `0` means there are genuinely none.
- `≈` with an amber tint means the figure is inferred, not observed.
- An empty green panel is treated as a **bug**, because a panel that has never
  successfully read anything looks identical to one where all is well.

Most of the fiddly code here exists to keep those apart. If you only take one
idea from this repository, take that one.

## No telemetry

The app carries **no crash-reporting or analytics SDK of any kind**. The only
thing it sends anywhere are the API calls you configure, with your own
credentials, to the vendors you chose. Credentials live in your OS credential
store, never in the settings file.

## Prerequisites

Solador targets **macOS and Windows**. There is no Linux build of the app (the
[agent](agent/) is a separate Linux-friendly service).

**Everywhere**

- **Rust** via [rustup](https://rustup.rs). You do not need to pick a version —
  `rust-toolchain.toml` pins one (currently 1.96.0) and rustup installs it,
  with `rustfmt` and `clippy`, the first time you run `cargo`.

**macOS**

- **Xcode Command Line Tools** — `xcode-select --install`.

  This is not optional and it is not about Swift: Rust links macOS binaries with
  Apple's linker against Apple's SDK. Without it you get a `cc`/`ld` failure
  that does not mention Xcode. It also supplies `codesign` (the dev launcher
  re-signs the app bundle so your Keychain does not re-prompt on every run) and
  `iconutil` (builds `icon.icns`).

  Full Xcode is **not** required to build or run. It is only implicated in
  releases: `notarytool` and `stapler` resolve through `Xcode.app` on a machine
  where `xcode-select -p` points there. Releases are not implemented yet — see
  [Status](#status).

**Windows**

- **Microsoft C++ Build Tools** (the MSVC toolchain Rust links with).
- **WebView2 runtime** — preinstalled on Windows 11; on Windows 10 install the
  Evergreen runtime.

**Only if you run the frontend tests or regenerate screenshots**

- **Node 22** — for the Playwright suite in `tests/frontend/`. Not needed to
  build or run the app.

**Optional, and only to make panels show something**

- `docker`, `podman` or `tart` — the Containers panel reads whichever it finds.
- The **Azure CLI** (`az`), signed in — the Azure Cost panel mints a short-lived
  SAS per poll rather than storing a credential.

Every one of these is absent-tolerant at runtime: a missing tool makes its panel
say so, not crash.

## Running it

There are **no binaries yet** — see [Status](#status). Build from source:

```bash
git clone https://github.com/cpmadrid/solador
cd solador
./dev            # build and run (debug)
./dev run --release
```

Nothing is configured on first launch: no repos, no hosts, no credentials. Each
panel tells you what it wants. Open **Settings** and add what you care about.

| To get | Give it |
|---|---|
| Repos | a fine-grained GitHub PAT with read access to Actions, Contents, Issues and Pull requests |
| Runners | the same token, plus your **GitHub organization** |
| Hosts | the [agent](agent/) on each machine, and its bearer token |
| Usage / Cost | a Neon org key, a Sentry `org:read` token, a Vercel token, an Azure Cost SAS URL |

## Layout

```
app/src-tauri/   Tauri shell: one poll task per panel, plus the settings surface
app/ui/          Frontend — plain HTML/CSS/JS, no bundler
crates/          The real work: viewmodel, store, github, usage, azurecost,
                 servicestatus, openclaw, localhost, wire, agentclient
agent/           The per-host metrics agent (workspace member, Linux CI job)
tests/frontend/  Playwright suite for app/ui
tests/fixtures/    Wire-contract fixtures both agent/ and crates/ assert against
```

Every string and colour a panel paints is decided in Rust and published to the
frontend. The frontend lays out; it does not invent labels.

## Development

```bash
./dev test        # cargo test --workspace + the Playwright suite
./dev lint        # fmt + clippy, exactly what CI runs
./dev format      # fix formatting
```

`./scripts/install-hooks.sh` wires lint to a pre-push hook.

Screenshots in this README are generated, not captured:

```bash
cd tests/frontend && npm run screenshots
```

They render the real frontend against the same fixtures the tests assert on, so
they cannot drift from the shipped palette.

## Status

Early. It runs every day on the author's desk, which is a different claim from
*finished*:

- **No signed builds.** macOS Gatekeeper will block an unsigned app, so today
  the practical audience is people willing to build it themselves.
- **No releases**, and therefore no upgrade path.
- The Tauri IPC boundary has no automated coverage; `app/README.md` carries a
  manual smoke checklist that is the only thing exercising it.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security reports: [SECURITY.md](SECURITY.md).

## License

[Apache-2.0](LICENSE).
