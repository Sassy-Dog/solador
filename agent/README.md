# Solador Agent

A small per-host metrics agent. It exposes host metrics and a container/VM list
over HTTP (JSON), guarded by a bearer token. The [Solador](../) macOS app polls
it over **Tailscale** to render a dashboard.

Runs on Linux (e.g. `ubu-01`) and macOS. Metrics come from
[`sysinfo`](https://crates.io/crates/sysinfo); the server is
[`axum`](https://crates.io/crates/axum) on `tokio`.

## Endpoints

All endpoints require `Authorization: Bearer <token>`. Missing or wrong token → `401`.

| Method & path     | Returns                                                            |
|-------------------|-------------------------------------------------------------------|
| `GET /v1/snapshot`  | Host metrics snapshot (CPU / memory / disk / network / gpu / battery). |
| `GET /v1/containers`| Array of containers/VMs from podman, docker, and tart.            |
| `GET /v1/health`    | `{ "status": "ok", "hostname": "...", "version": "..." }`         |

`version` is the repo's CalVer (`2026.9.3`) — the number the release this binary
came from carries, **not** `agent/Cargo.toml`'s semver, which since #390 is a
wire-contract marker naming no release. A binary built outside a full git
checkout cannot count the commits CalVer is made of, so it **omits the key
entirely** rather than serving a stand-in; consumers decode that as unknown and
render `—`.

### `/v1/snapshot` shape

```json
{
  "timestamp": "2026-06-04T22:00:00Z",
  "cpu": { "totalUsage": 37.5, "coreUsages": [40.0, 35.0], "model": "Intel(R) Core(TM) i7-8559U" },
  "memory": { "usedGB": 12.3, "totalGB": 32.0, "swapUsedGB": 0.5, "pressure": 1.25 },
  "disk": { "readMBps": 1.2, "writeMBps": 0.3 },
  "network": { "downloadMBps": 0.2, "uploadMBps": 0.1 },
  "gpu": {},
  "battery": null
}
```

Notes:
- `timestamp` is RFC3339 / ISO-8601 UTC (`...Z`).
- Disk and network values are **rates** (MiB/s), computed from a delta between two
  ~1s `sysinfo` refreshes by a background sampler. `/v1/snapshot` returns the
  latest sampled values.
- Percentages (`totalUsage`, `coreUsages`) are `0–100`.
- **Measured, or absent** (agent ≥ 0.3.0). A key is only present when this agent
  actually sampled it, so `0.0` always means a *reading* of zero and never "we
  had nothing to say". Consumers render an absent key as `—`.
  - `pressure` is memory PSI (`some avg10` from `/proc/pressure/memory`,
    already a 0–100 percentage). Omitted where that file doesn't exist —
    macOS, or a kernel built without `CONFIG_PSI`.
  - `thermalState` is **always omitted**. The contract's 0–3 ladder is macOS's
    `ProcessInfo.ThermalState`; Linux exposes thermal zones in millidegrees, and
    collapsing those into the ladder needs per-machine trip points this agent
    doesn't know.
  - `gpu` is **measured on hosts with an NVIDIA card** (agent ≥ 0.4.0), from
    `nvidia-smi` — `sysinfo` reports no GPU on any platform. `usage` is the
    utilisation percentage; `vramUsedGB` / `vramTotalGB` are its MiB figures in
    the same 1024-base "GB" as every other size here (a 12288 MiB card reads
    `12.0`). A multi-GPU host reports its **first** card, since the contract
    carries one `gpu`.
    Still `{}` wherever nothing was measured: no `nvidia-smi` on `PATH` (every
    host without an NVIDIA driver, including macOS), a failed or hung
    invocation, or output the agent doesn't recognise. AMD and Intel GPUs are
    not read yet, so they are part of that set.
    The probe runs on its own task every 5s with a 2s hard timeout — never on
    the 1s sample path, so a wedged `nvidia-smi` costs a stale GPU reading and
    not a stalled snapshot.
  - `battery` stays JSON `null` (not omitted) — the one optional the contract
    deliberately keeps emitting.
  - Before the first sample lands, `disk`, `network`, `gpu` are all `{}` and
    `pressure` is absent: a rate needs two readings to diff. `/v1/health`'s
    `samplerStale` is how you tell that placeholder from a live sample.
  - Agents **before 0.3.0** sent `"thermalState": 0`, `"pressure": 0.0` and an
    all-zero `gpu` on every host. Those literals are indistinguishable from
    readings once on the wire, which is why they had to stop at the source
    (#183); a consumer that still needs to decode them keeps working, since
    every one of these keys is optional in both directions.
- Memory has **no** `usagePercentage` key — the consumer computes it.
- `volumes` entries are `{ "mount": "/", "usedGB": 10.0, "totalGB": 100.0, "fstype": "ext4" }`.
  `fstype` is lowercased and omitted (not `null`) when unknown. Transient, remote,
  and pseudo filesystems are filtered out at the source (see `SOLADOR_AGENT_SKIP_FSTYPES`),
  and bind mounts of the same filesystem collapse to the shortest mount path.
- `processes` is the union of the top 5 by CPU and the top 5 by memory, so it runs
  5–10 entries long. Every entry is a **process** (agent ≥ 0.3.1): on Linux
  `sysinfo` hands back a *task* table, and both threads and kernel threads are
  filtered out of it here.
  - `cpuPercent` is the whole process's — the kernel already reports
    thread-group-wide times in `/proc/<pid>/stat`, so nothing is summed on top —
    and `memoryMB` is its RSS, listed once. The unit is `sysinfo`'s: **percent
    of one core**, so a program saturating four cores reads `400`.
  - `cpuCores` is that same reading as a **core count** (`cpuPercent / 100`),
    and it is the one a consumer should render (agent ≥ 0.5.0). `400%` beside a
    machine-wide `totalUsage` of `0%` is two numbers wearing one `%` sign
    against denominators three orders of magnitude apart; `4.0 cores` is not.
  - Both figures are averaged over the process sampler's own cadence (~1 min),
    not the 1s snapshot cadence. That is now true **by construction**: the
    process refresh has a `sysinfo::System` of its own, so the window its CPU
    numerator spans is the window its denominator spans (#378).
  - Agents **before 0.5.0** omit `cpuCores` entirely, and on Linux their
    `cpuPercent` is inflated by the ratio between those two cadences — 60×,
    measured on a real host at `210.8%` for a process genuinely using 3.55% of
    one core, which is why a host card could show `sqlservr` at 220% while its
    header and all 36 cores read 0%. **The absent key is the version signal**:
    a consumer renders `—` rather than deriving a core count from a number it
    knows is wrong. Only redeploying the agent fixes the reading.
  - Agents **before 0.3.1** listed each thread as its own process, so one
    multi-threaded program (a SQL Server engine, say) appeared as several rows
    repeating its full RSS and splitting its CPU, and kernel threads like
    `txg_sync` appeared at all (#211). Only redeploying the agent fixes that;
    the rows are indistinguishable from real processes by the time they are on
    the wire.

### `/v1/containers` shape

```json
[ { "name": "llm", "statusText": "Up 2 days", "isRunning": true, "runtime": "podman", "image": "llama-swap:latest" } ]
```

`runtime` is one of `"docker"`, `"podman"`, `"tart"`. `image` is `null` for tart
VMs. Runtimes whose CLI is not on `PATH` are skipped silently. podman is queried
rootless, so it works as a normal user.

## Configuration

| Env var                  | Required | Default | Meaning                          |
|--------------------------|----------|---------|----------------------------------|
| `SOLADOR_AGENT_TOKEN`  | yes      | —       | Bearer token. Server refuses to start if unset/empty. |
| `SOLADOR_AGENT_BIND`   | no       | tailnet IP | Host/interface to bind. Defaults to the detected Tailscale IP (`100.x`), so the agent only listens on the tailnet. Set to `0.0.0.0` (or `::`) to bind all interfaces — opt-in only, behind a firewall. If unset and no Tailscale IP can be detected, the server refuses to start rather than exposing the public NIC. |
| `SOLADOR_AGENT_PORT`   | no       | `7878`  | TCP port. Bound on `SOLADOR_AGENT_BIND`. |
| `SOLADOR_AGENT_SKIP_FSTYPES` | no | see below | Comma-separated fstypes excluded from `volumes`. Setting it **replaces** the default list; an empty value disables filtering. |
| `RUST_LOG`               | no       | `info`  | Log filter (tracing).            |

Default skipped fstypes (transient/remote/pseudo filesystems, so automounts like
an autofs `/shared` can't flap in and out of the dashboard):

```
autofs, nfs, nfs4, cifs, smb, smb2, smb3, smbfs, 9p, afs, afpfs, ceph,
glusterfs, lustre, davfs, davfs2, sshfs, curlftpfs, tmpfs, devtmpfs, ramfs,
squashfs, overlay, overlayfs, iso9660
```

Any `fuse.*` subtype (e.g. `fuse.sshfs`, `fuse.rclone`) is also skipped whenever
filtering is enabled; `fuseblk` (NTFS via FUSE — a real local disk) is kept.

## Command line

The agent takes no arguments in normal operation — everything is configured from
the environment above. Two flags exist, and both exit immediately, before the
token check, so they work on a machine with no token and no tailnet:

```bash
solador-agent --version   # the version, on one line, and nothing else
solador-agent --help      # usage plus the environment variables
```

`--version` printing the bare version is a **contract**, not terseness:
`agent/deploy/lib.sh` and the release workflow both read it directly, and it is
what proves a published binary starts at all before a release attaches it. It
exits non-zero when this build carries no version, rather than printing a
plausible one.

Two **commands** exist as well
([#393](https://github.com/Sassy-Dog/solador/issues/393)), dispatched at the
same point — before tracing, the token check, the sampler or a listener — so
they are always a separate process from the service they act on:

```bash
solador-agent update      # move this install to the latest published release, verified; see Updating
solador-agent rollback    # put the previous binary back, offline; see Roll back
```

Neither takes arguments of its own: `update --force` is refused, not ignored
into an unforced update. Their exit codes are a contract (the scheduled job
#394 adds will read them): `0` updated, or already current *and serving*;
`1` failed with nothing changed (a refusal, a network error, a rejected
signature — and, for `rollback`, a swap that did not come back or was left
half done, both named); `3` failed **and** the previous binary could not be
restored — inspect the service; `4` no applicable release (the feed is not
newer than what is installed — the normal answer on a from-source host
running ahead of the last tag, and nothing to page anyone for); `5` failed,
and the previous binary is back and serving — nothing is broken, but a human
should look at why; `75` another `update`/`rollback` holds the lock (the
line names the holder's pid and start time); `2` usage.

An argument the agent does not recognise is **refused** (exit 2), never ignored:
it takes none in normal operation, so one arriving means something upstream is
wrong — a hand-edited `ExecStart`, or a wrapper passing flags meant for
something else.

## Releases

Since [#390](https://github.com/Sassy-Dog/solador/issues/390) a `v*` tag
publishes four agent binaries on the **same GitHub Release as the desktop app**:

| Asset | Host |
|---|---|
| `solador-agent-<version>-x86_64-unknown-linux-musl` | any x86-64 Linux distro |
| `solador-agent-<version>-aarch64-unknown-linux-musl` | ARM servers, Pi-class hosts |
| `solador-agent-<version>-aarch64-apple-darwin` | Apple Silicon Macs |
| `solador-agent-<version>-x86_64-apple-darwin` | Intel Macs |

The Linux builds are **statically linked musl**, so they need no glibc of any
particular vintage and no runtime dependency at all — a gnu build would die on
any host older than the builder with `GLIBC_2.xx not found`. Every one of the
four has had `--version` executed on a runner matching its target before the
release attached it.

Each binary ships with a detached `<asset>.minisig`. `deploy/install.sh`
(below) does all of the following for you; by hand, check the signature, then
make the file executable — a GitHub release asset carries no unix mode, so a
fresh download is **not** executable and running it before `chmod` fails with
`permission denied`:

```bash
curl -fLO --proto '=https' https://github.com/Sassy-Dog/solador/releases/download/v<version>/solador-agent-<version>-<triple>
curl -fLO --proto '=https' https://github.com/Sassy-Dog/solador/releases/download/v<version>/solador-agent-<version>-<triple>.minisig

minisign -Vm solador-agent-<version>-<triple> \
         -x solador-agent-<version>-<triple>.minisig \
         -p agent/release-signing-key.pub

chmod +x solador-agent-<version>-<triple>
./solador-agent-<version>-<triple> --version
```

`agent/release-signing-key.pub` is in this repository (key id
`B2E5C62B763FD2C4` — the file is the authority; a test in `crates/updatefeed`
reads the id out of its bytes, and it is the id encoded in the base64 line,
which is what `minisign -V` reports; the `untrusted comment:` line merely
repeats it). Take the key from a checkout of **`main`** — the branch the
ruleset protects — rather than from a tag or an archive of one: a tag is the
least-protected ref in this repository (docs/AGENT-DISTRIBUTION.md §5 records
that acceptance), and a key that arrived with the release it is meant to
verify proves nothing.

It is a **different keypair from the desktop app's** updater key, on purpose:
the app updates on someone's laptop, the agent runs unattended as a service on
servers, and a compromise of one must not yield the other.

The macOS binaries require **macOS 11 (Big Sur) or later**, and that floor is
the agent's own rather than the cockpit's: `.cargo/config.toml` declares 14.0
for the workspace because the cockpit's frontend needs it, and the agent — which
has no frontend — would otherwise inherit a number that quietly excludes every
Intel Mac still on macOS 12 or 13. `scripts/build-agent.sh` sets 11.0 for these
two targets and reads it back out of each Mach-O with `vtool`.

They are **not** Developer ID signed or notarized — that is the desktop app's
path, not this one. Fetch them with `curl` and Gatekeeper's quarantine never
applies; a browser download needs `xattr -d com.apple.quarantine <file>` first.

`deploy/install.sh` installs these (see **Install** below): it downloads the
binary for the host it runs on, verifies the signature with the stock
`minisign` under the committed key, and only then installs and starts it
([#392](https://github.com/Sassy-Dog/solador/issues/392)). **The agent binary
verifies too** ([#393](https://github.com/Sassy-Dog/solador/issues/393)):
`solador-agent update` checks the release's `agent-latest.json` and the
binary it names under public keys compiled in at build time — this file and,
once it is committed, `agent/release-signing-key-next.pub`, a **standby**
whose private half is held in a Doppler config that syncs nowhere
(`docs/SECRETS.md`). Two trusted keys are what make a lost or compromised
signing key a release rather than a recall: the next release is signed under
the standby every deployed agent already trusts. They do not revoke a key
and they are not anti-replay; the updater's newer-than rule is what refuses
a replayed older feed. Unattended update jobs are
[#394](https://github.com/Sassy-Dog/solador/issues/394).

**The first release to carry these binaries is the first `v*` tag cut after
#390 landed.** `v2026.9.3` and everything before it publish none, and
`install.sh` says so — as a failure naming the tag and the asset — rather than
building from source instead.

To produce them locally, with the command the release itself runs:

```bash
./dev agent                                        # every target this host can build
./dev agent --targets "x86_64-unknown-linux-musl"  # just one
```

## Prerequisites

Two different sets, because there are two different ways onto a host.

### To install a release (`deploy/install.sh`)

No Rust toolchain. A supported clean host needs:

- **`curl`** and the stock **`minisign`** — `brew install minisign` (macOS),
  `apt install minisign` (Debian 12+ / Ubuntu 24.04+ — earlier releases carry
  no package), `dnf install minisign` (Fedora), or
  <https://jedisct1.github.io/minisign/>. The installer refuses, and tells you
  this, when either is missing; it never installs a verifier, a package
  manager or a toolchain on your behalf. It also checks that what answers to
  `minisign` *is* minisign (`minisign -v`), because `rsign` — the repo's own
  signer — treats `-V` as "print the version" and exits 0.
- **bash** and the usual coreutils (`install`, `mktemp`, `cmp`, `awk`, `sed`, …).
- **A bind address the cockpit can dial.** By default the installer binds the
  host's Tailscale IP; a LAN or VPN host without Tailscale must set
  `SOLADOR_AGENT_BIND=<that address>` (or `0.0.0.0`, only behind a firewall).
  With neither, the installer refuses in preflight — before any download.
- **Linux:** systemd with a reachable user manager (`systemctl --user`, so a
  real login session — not `sudo -u` or `su`). A non-systemd Linux (Alpine's
  OpenRC, say) is not supported by the installer; the musl binaries still run
  there, by the manual verify-and-`chmod` steps under **Releases**.
  **macOS 11 or later:** a login session for the user running the installer —
  the agent is a LaunchAgent in that user's `gui/<uid>` domain, so it needs
  someone logged in at the console (or via Screen Sharing), and it does not run
  before anyone logs in.
- A checkout of this repository's **`main`** (`git clone`) — for
  `agent/deploy/*` and `agent/release-signing-key.pub`, not to build anything.
  There is deliberately no `curl | sh` bootstrap: the public key the download
  is verified under has to arrive by a path other than the download, and
  `main` is the ref the repository's ruleset protects (see the note on the key
  above).

### To build from source (`cargo`, `deploy/redeploy.sh`)

- **Rust** via [rustup](https://rustup.rs). The repo-root `rust-toolchain.toml`
  pins the version (currently 1.96.0, with `rustfmt` and `clippy`); rustup
  installs it on the first `cargo` invocation.
- Linux or macOS. There is no Windows build of the agent.

`agent/` is a member of the root Cargo workspace: one `Cargo.lock`, one
toolchain pin. Run `cargo` from anywhere in the repo, and the repo's `./dev test`
and `./dev lint` cover the agent too.

It keeps its own CI job (`agent-tests`) because it is the only piece that builds
and deploys to Linux. That job is scoped `-p solador-agent` deliberately — a
bare `cargo build` on the Linux runner would resolve the whole workspace,
including `app/src-tauri` and its webkit2gtk system dependencies.

### Testing `deploy/`

`agent-tests` also gates the deploy scripts, which are the agent's only path
onto a host:

```bash
bash agent/deploy/lib_test.sh          # the helpers in deploy/lib.sh
shellcheck -S warning agent/deploy/*.sh
bash -n agent/deploy/*.sh
```

`./dev test` runs the first, `./dev lint` the other two (skipping shellcheck
with a warning if it isn't installed). All three run unconditionally in CI.

Since #390 both shell gates cover `scripts/*.sh` and `dev`/`prd` too, not just
`agent/deploy/`: `scripts/build-agent.sh` runs only on a `v*` tag, and an
ungated break there would be found mid-release.

`lib_test.sh` is dependency-free — bash plus the coreutils the deploy scripts
already need, no bats and no jq — and stubs every host command (`cargo`,
`curl`, `sleep`, `uname`, `sw_vers`, `systemctl`, `loginctl`, `launchctl`,
`tailscale`), so it touches no host and takes about ten seconds. It covers
`binary_version` (the artifact's own `--version`, including its three
fail-closed cases — no version compiled in, nothing printed, no such binary),
`health_url` (wildcard → loopback, IPv6 bracketing), `health_version`,
`target_dir`, `verify_health` against a stubbed endpoint, the #392 helpers
(platform → triple for all four targets and every refusal, release-tag
validation, the `/releases/latest` redirect, `ExecStart` quoting, XML escaping,
template rendering), and — since #392 — **`install.sh` itself, run end to end
against a temporary HOME**: fresh install, re-run (token reused, the running
binary displaced by rename rather than overwritten, `.prev` kept), the
pre-rename handover and its stop-before-restart ordering, the `/opt` migration
gate and `--migrate-from-opt`, a HOME with a space, the macOS flow against a
stubbed `launchctl`, the launcher on its own, argument refusal, and every
preflight refusal (unsupported platform, macOS below 11, no login session, no
`minisign`, no public key, no release, no asset). `redeploy.sh` keeps its
source-level invariants: taking `.prev` before the swap, and aborting on a
binary that carries no version.

**The signature gate is tested with the real `minisign`, and that is the
load-bearing case of #392.** Fixtures are signed with a throwaway keypair the
suite generates, laid into a copy of the checkout as
`agent/release-signing-key.pub`, so the installer's own key-resolution path is
the one exercised (there is no override to reach for). A tampered binary, a
binary signed by another key, and a binary with no `.minisig` are each
rejected before the candidate is executed and before any installed state
changes — and the tamper case is then **re-run with an accept-everything
`minisign` on PATH**, where it must go through, which is what proves the
rejection was the verifier's rather than some other failure that happened to
land first. A last case runs the installer *as it sits in this repository*
against a throwaway-key fixture and requires the rejection to name
`release-signing-key.pub`. Without a usable `minisign` on the machine — absent,
or older than 0.11, which lacks the `-W` the throwaway keys need — every one of
those reports itself as `SKIP` with the reason, never as a pass, and the cases
that never reach verification (argument refusal, platform preflight, release
resolution) run regardless. CI runs the suite twice with
`SOLADOR_DEPLOY_TEST_REQUIRE_MINISIGN=1`, under which those skips are
**failures** — so removing the minisign step cannot turn a job green with the
load-bearing cases silently skipped: `agent-tests` (Linux, bash 5, minisign
from apt) and `rust-workspace` (macOS, stock `/bin/bash` 3.2, minisign from
Homebrew — the interpreter the installer actually runs under on a Mac, and
the one leg where the rendered plist meets a real `plutil -lint`). `./dev
test` on a Mac runs it under `/bin/bash` too.

Two cases stay out of the default run. `build_release_binary`'s real-cargo
cases skip without a toolchain. And `SOLADOR_DEPLOY_TEST_LAUNCHD=1` (macOS
only) runs the whole installer against the real `launchctl`, `plutil`, the
real launcher and a real authenticated health probe — with the download curl
still stubbed to serve the locally built `solador-agent` under the throwaway
key — bootstrapping a throwaway label into the invoking user's session and
booting it out again. It is opt-in because it does touch the host.

Since #393 the same suite also runs **`scripts/agent-standby-key.sh`** end to
end — the custody script that mints the standby signing key — against a
file-backed `doppler` stub, a `gh` stub answering fixed secret-name lists, a
`cargo` recorder, and `rsign` stubbed onto the real `minisign`, so the key
generated, the value "uploaded" and "retrieved", and the custody proof are
real cryptography with only the network faked. It asserts what the script
promises: a fresh run writes a two-line public file whose line 1 carries the
id its bytes encode, uploads a minisign secret key on stdin with only the
name and config on argv, proves custody with the retrieved value, leaves the
private key in no output and on no argv, and removes every temp file; a
re-run reuses rather than rotates; and each half-state (secret without file,
file without secret), `--config prd`, a missing config, a config whose audit
log shows an active sync, a standby that turns up in GitHub's `prd`
environment, and a retrieved value that is not the committed file's pair are
each refused loudly. Without `minisign` those cases `SKIP` like the signature
cases do, and CI requires them the same way.

**`solador-agent update` and `rollback` are tested in Rust**, not here:
`agent/tests/update_flow.rs` runs the whole transaction against a loopback
release server, a temporary install tree and a fake service manager that
does what a real restart does (executes the live path's `--version` and
serves it on a real authenticated loopback `/v1/health`), with keys minted
in-test — every refusal asserted to change nothing, every recovery asserted
on the bytes at the live path and at `.prev`. `SOLADOR_DEPLOY_TEST_LAUNCHD=1
cargo test -p solador-agent --test update_flow launchd_smoke` is the opt-in
real thing on macOS: a throwaway LaunchAgent running the real built agent, a
failed update rolled back on it automatically, two explicit `rollback`s
through the CLI; `SOLADOR_AGENT_SMOKE_NEWER_BINARY=<path>` (a second build
with a newer pinned `MARKETING_VERSION`) adds the successful-update path, and
`SOLADOR_AGENT_SMOKE_REAL_FEED=1` adds a read-only `solador-agent update`
against the real github.com feed under the compiled-in production key. None
of that runs in CI.

The other load-bearing case is `build_release_binary` **failing** when the
workspace target dir holds no binary, rather than falling back to a search.
Until #268 both deploy scripts looked in `agent/target/release/`, which #264
had stopped writing to — while still holding a stale binary from the last
standalone build on every already-installed host. A lenient implementation
finds that one, installs it, and reports a successful deploy of code several
releases old. `redeploy.sh` is that helper's only caller now, and the case
stays. End-to-end deploy against a real remote host stays out of scope;
`verify_health` covers that at runtime by asserting the *served* version.

## Build & run (local)

```bash
cargo build              # debug
cargo test               # unit + contract tests
cargo build --release    # optimized binary at target/release/solador-agent

# Locally there's usually no tailnet IP, so bind loopback explicitly:
SOLADOR_AGENT_TOKEN=secret SOLADOR_AGENT_BIND=127.0.0.1 cargo run
# in another shell:
curl -s -H "Authorization: Bearer secret" localhost:7878/v1/snapshot | jq
curl -s localhost:7878/v1/snapshot          # -> 401
```

## Install (Linux or macOS, from a signed release)

From the crate directory on the target host:

```bash
./deploy/install.sh                      # latest published release
SOLADOR_AGENT_RELEASE=v<version> ./deploy/install.sh   # a specific one
./deploy/install.sh --help
```

A re-run is one update path — `solador-agent update` (**Updating**, below)
is the in-binary one, and the one #394's scheduled job will use — and on the
unpinned form it **refuses to move backwards**: if the binary already installed reports a newer CalVer than the
release `/releases/latest` resolved to — the one unsigned link in the chain,
see `docs/AGENT-DISTRIBUTION.md` §6 — the run stops naming both versions.
Pinning `SOLADOR_AGENT_RELEASE` is how an operator says a downgrade is meant.

Both forms need a **published** release that carries the agent binaries.
`v2026.9.3` and everything before it carry none, and a draft is neither
resolvable (`/releases/latest` skips drafts) nor downloadable, so until the
first post-#390 release is published every install fails at the download step
— naming the tag and the asset, which is the honest outcome.

No arguments is the normal form. An argument the script does not know is
refused (exit 2), never ignored; `--enable-timer` in particular is #394's and
is refused by name. Everything runs as **your user** and nothing uses `sudo`.

The script:
1. Detects the platform (`uname -s` / `uname -m`) and maps it onto one of the
   four published targets; anything else — another OS, another architecture,
   macOS below 11 — refuses before anything is fetched.
2. Resolves the latest **published** release from the `/releases/latest`
   redirect (a draft is invisible there by construction) and validates that
   the tag is a CalVer tag, or takes `SOLADOR_AGENT_RELEASE` verbatim after the
   same validation. Then downloads `solador-agent-<version>-<triple>` and its
   `.minisig` into a private staging directory under `~/.cache`. A release that
   carries no agent binary — every one up to and including `v2026.9.3` — is a
   **failure** naming the tag and the asset; there is no source build and no
   other version behind it.
3. **Verifies the signature** with the stock `minisign` under
   `agent/release-signing-key.pub` from this checkout. A download that does
   not verify is not made executable, not asked its version, not installed,
   and does not stop or reconfigure anything already running; the staging
   directory is removed either way.
4. Only then makes the binary executable and reads its `--version` — which
   must equal the release's own number — and installs it, user-owned, at
   **`~/.local/bin/solador-agent`**: staged as `solador-agent.new` beside the
   live path and renamed over it (a running binary is never overwritten in
   place), with the displaced binary kept as `solador-agent.prev`. `.prev` is
   the *last-good* anchor: a re-run that installs the same bytes (the
   fix-and-retry the failure text recommends) leaves it alone rather than
   copying the live binary over it.
5. Writes `~/.config/solador-agent.env` with the token (prompted **without
   echo**; press Enter to auto-generate; reused on a re-run), the bind address
   (`SOLADOR_AGENT_BIND`, else the existing file's, else the detected
   Tailscale IP, else refuse) and the port (`SOLADOR_AGENT_PORT`, else the
   existing file's, else `7878`), mode `600`, written beside the live file and
   renamed into place. Any other line already in the file
   (`SOLADOR_AGENT_SKIP_FSTYPES=`, `RUST_LOG=`) is carried through. The full
   token is never printed — the script reports only the env-file path and the
   token's last 4 characters.
6. Installs and starts the service for the platform (next two sections),
   rendered with the **actual** binary path — nobody edits an `ExecStart` or a
   plist by hand.
7. **Verifies** by polling `/v1/health`, authenticated, at the bind/port it
   just wrote, until it reports the version the verified binary answered
   `--version` with. A running service only proves *a* binary is up; if the
   version being served is not the one just installed, the script exits
   non-zero naming both numbers rather than reporting a successful install over
   stale code.

To rotate the token on either platform: edit `~/.config/solador-agent.env`,
then restart the service (commands below).

### Linux: the systemd user service

`~/.config/systemd/user/solador-agent.service`, with
`ExecStart=/home/<you>/.local/bin/solador-agent` (double-quoted when the path
has a space) and `EnvironmentFile=%h/.config/solador-agent.env`. **Every run
of the installer regenerates that file** from the template (the displaced one
is kept as `solador-agent.service.prev`), so edits to the file itself do not
survive an upgrade; put overrides in a drop-in (`systemctl --user edit
solador-agent`), which does. To rotate the token, edit
`~/.config/solador-agent.env` — the installer writes bare `KEY=value` lines,
and both the unit and the installer's own reads also accept a value in one
pair of quotes. The installer
runs `daemon-reload`, `enable`, `restart` (not `enable --now`, which would not
restart a running unit onto the new binary) and enables lingering, best
effort, so it starts on boot and survives logout. It runs as **your user**
(not root) so rootless `podman ps` works.

```bash
systemctl --user status solador-agent
systemctl --user restart solador-agent
journalctl --user -u solador-agent -f
```

### macOS: the LaunchAgent

`~/Library/LaunchAgents/app.solador.agent.plist`, label **`app.solador.agent`**,
bootstrapped into **`gui/<uid>`** — a LaunchAgent running as the user who
installed it, **not** a LaunchDaemon and not root. It starts at that user's
login and runs inside that session; a Mac nobody logs in to does not run it,
and the installer refuses (before changing anything) when there is no login
session to bootstrap into — an SSH session with nobody at the console is the
usual way to hit that.

launchd has no `EnvironmentFile=`, and its `EnvironmentVariables` key would put
the token into the plist. So `ProgramArguments` is a small launcher installed
at `~/.local/bin/solador-agent-launchd` (a copy of `deploy/run-agent.sh`; the
checkout can be deleted afterwards) followed by the binary path, the env file
path and the log path — every path it needs arrives as an argument, so it
never assumes launchd's HOME is the installer's, and it reads `$HOME` for one
thing only: extending `PATH` (below). At every start the
launcher reads the same mode-0600 env file line by line — it never `source`s
it, so the token is never evaluated as shell — with the same value rules as
systemd's `EnvironmentFile=` (a trailing CR, surrounding whitespace and one
pair of quotes stripped; the installer's own reads use the same rules),
exports exactly the documented keys, and `exec`s the agent. The token appears
in neither the plist nor the log. The agent's stdout and stderr both go to
`~/Library/Logs/solador-agent.log`, which nothing rotates for you (that would
need a `newsyslog.d` rule, i.e. `sudo`): the launcher moves it to `.1` at the
next start once it passes 10 MB, and reopens its own streams on the fresh
file — launchd opened the old one before the launcher ran, and a rename alone
would keep every line of the new run in `.1`. `KeepAlive` restarts it on exit,
like `Restart=always`.

The plist also sets `PATH` (`/opt/homebrew/bin:/usr/local/bin:/opt/podman/bin`
ahead of the system directories), and that is load-bearing: launchd starts a
job with its compiled-in `PATH`, which holds none of `docker`, `tart` or
`podman`, and an agent that cannot find a runtime reports it as not installed
— `/v1/containers` would be `[]` forever while `/v1/health` stayed green.
systemd's user session already inherits a `PATH` with `/usr/local/bin`. The
launcher then appends `~/.docker/bin`, `~/.orbstack/bin` and `~/.rd/bin`
(Docker Desktop's, OrbStack's and Rancher Desktop's no-admin installs), which
the plist cannot name because launchd does not expand `$HOME`.

```bash
launchctl print gui/$(id -u)/app.solador.agent          # status, pid
launchctl kickstart -k gui/$(id -u)/app.solador.agent   # restart (e.g. after rotating the token)
tail -F ~/Library/Logs/solador-agent.log                # -F: the launcher renames it at 10 MB
launchctl bootout gui/$(id -u)/app.solador.agent        # stop and unload
```

Re-running the installer boots the loaded service out and bootstraps the
re-rendered plist rather than `kickstart`ing it, so a changed path takes
effect immediately instead of at the next login.

`redeploy.sh` and its `rollback` are Linux-only. On macOS the in-binary
command is the rollback path — it reads this plist, swaps the binaries,
kickstarts the label and verifies (see **Roll back** below):

```bash
~/.local/bin/solador-agent rollback
```

### Hosts installed before #392: the `/opt` layout

Earlier installs put the binary at `/opt/solador-agent/solador-agent` (with
`sudo`) and the unit's `ExecStart` points there. New installs are user-owned
at `~/.local/bin/solador-agent`, and the installer will **not** move a host
between the two by itself: when it finds an existing `solador-agent` user unit
whose `ExecStart` is anything other than `~/.local/bin/solador-agent`, it
stops before changing anything and prints the explicit step, which is:

```bash
./deploy/install.sh --migrate-from-opt
```

That keeps `~/.config/solador-agent.env` (token, bind, port, and any other
key) exactly as it is, installs the verified binary at
`~/.local/bin/solador-agent`, **regenerates** the unit from the template with
that path (the displaced unit is kept as `solador-agent.service.prev`; edits
you made to the file itself do not survive, drop-ins via `systemctl --user
edit` do), restarts, and verifies `/v1/health` serves the new version — all as
your user, with no `sudo`. The `/opt` binary is left where it is, and a copy
of it becomes `~/.local/bin/solador-agent.prev` so `redeploy.sh rollback`
has an anchor; once you are satisfied:

```bash
sudo rm -rf /opt/solador-agent
```

Nothing here changes `/opt`'s ownership or configures passwordless `sudo`, and
neither does `solador-agent update`: on a host whose unit starts a binary in
a directory this user cannot write to — the root-owned `/opt` layout is the
usual case — it stops before changing anything and prints this same
`--migrate-from-opt` step (#394's scheduled job will inherit that refusal) —
which is why the migration is explicit rather than something an update does
one night.

## Updating (`solador-agent update`)

From any shell on the host, as the user the service runs as — never as the
service's own `ExecStart`, and never with `sudo`:

```bash
~/.local/bin/solador-agent update
```

It is the download-install's update path moved into the binary, with one
thing the installer does not do: **it undoes itself when the new binary does
not come up.** In order, each step refusing before the next changes anything:

1. Reads where the service is (nothing changes yet): the binary from the
   systemd unit's `ExecStart=` or the plist's `ProgramArguments`, the env
   file beside it, and the token/bind/port from that file with the same
   rules the service starts under (never `source`d). `SOLADOR_AGENT_BIND`
   must be in that file (the installer always writes it): with no bind the
   service listens on a tailnet address this command could not dial, and a
   probe of loopback would swap, fail, restore and exit 3 on a healthy host,
   so its absence is refused instead. Running as root is refused; an install
   this user cannot replace — the pre-#392 `/opt` layout — is refused with
   the migration step above.
2. Takes the transaction lock (`solador-agent.update.lock` beside the
   binary). A second `update` or `rollback` on the same install — yours
   racing a scheduled one, say — reports *busy* (exit 75) and changes
   nothing; the lock dies with the process, so a crashed run cannot wedge
   the next. (The lock covers these two commands and #394's job; `install.sh`
   and `redeploy.sh` do not take it, so do not run those during an update.)
   Then the service manager must answer — `systemctl --user` or the
   `gui/<uid>` domain — before anything is downloaded, so a session with no
   manager (an `ssh` with nobody logged in, a `sudo -u` shell) is refused
   here rather than discovered at the restart.
3. Resolves the latest **published** release from the `/releases/latest`
   redirect, fetches *that tag's* `agent-latest.json` and `.minisig`, verifies
   the feed's exact bytes under the compiled-in keys before decoding it, and
   requires its `version` to be the tag's.
4. Hashes the installed binary. **Equal to the feed's entry means already
   current: nothing downloaded, nothing restarted** — even when the feed's
   version differs. It then asks `/v1/health` for the version those bytes
   claim (their own `--version`): serving it is exit 0; serving something
   else, or not answering, is a distinct failure (exit 1) that tells you to
   restart the service — the state an earlier run leaves if it was
   interrupted between its swap and its restart.
5. Otherwise the feed must be **newer** than the installed CalVer (read from
   the installed binary's own `--version`). An older feed — a replay, or a
   downgrade — is refused; a downgrade you mean is
   `SOLADOR_AGENT_RELEASE=v<version> ./deploy/install.sh`. An installed binary
   that carries no version cannot be compared and is refused rather than
   assumed older; re-run `install.sh` to move it onto a published release.
6. Downloads the binary into memory and verifies its signature **and** its
   SHA-256 against the feed entry before anything touches disk.
7. Stages it as `solador-agent.new` (mode 0755), runs the staged candidate's
   `--version`, and requires the feed's CalVer back; a candidate that cannot
   name it is removed unrun.
8. Keeps the live binary as `solador-agent.prev` and **renames** `.new` over
   the live path — never an in-place overwrite, and the live path is never
   absent for an instant.
9. Restarts the service (`systemctl --user restart solador-agent` /
   `launchctl kickstart -k gui/<uid>/app.solador.agent`) and polls the
   authenticated `/v1/health` — at the bind and port the env file says,
   loopback for a wildcard bind — until it reports the new version. A running
   service reporting the old version, or none, is **not** a success.
10. If the restart or that check fails, **restores `.prev` the same way,
    restarts, and requires the previous version back.** The command then
    exits `5` — a failed update is a failure even when recovery worked — and
    `3` if recovery also failed, naming both failures *and what is at the
    live path now* (the candidate, when the restore's own rename failed; the
    previous binary, when its restart or health check did), so a service
    that is down is never reported as rolled back.

The output names each step, the release, the key id that verified, and the
health URL; the token appears nowhere, and every failure that leaves a
service to look at ends with the manager's status command and the log path
(the same `launchctl print` / `tail` and `systemctl --user status` /
`journalctl` lines the service sections above give). From its own
environment the command reads `HOME`, the standard proxy variables for the
`github.com` client only (the probe of this host's own service ignores
proxies), and `SOLADOR_AGENT_LAUNCHD_LABEL`, which picks a different
LaunchAgent label (the same test seam the installer honours).

It needs nothing installed beyond the agent itself — no `curl`, no
`minisign`, no checkout: the verifier and the keys are in the binary, which
is the point of doing this in Rust rather than in `install.sh`. What it does
need is a host that can reach `github.com` over HTTPS and a service installed
by `install.sh` (or migrated by it). `redeploy.sh` remains the from-source
path for our own Linux hosts and is unchanged.

## Upgrading from the pre-rename agent

Hosts running the old `devcanopy-agent` (Linux) are handed over automatically
by `install.sh` — run it exactly as for a fresh install. It will:

1. Stop and disable the `devcanopy-agent` user unit **before** the new one
   starts, because the old one holds the port the new one wants.
2. Carry the bearer token across from `~/.config/devcanopy-agent.env`.

That second step is the one that matters. The env file name *and* the variable
name both changed, so without it the installer finds no existing token and
mints a fresh one — which the cockpit's stored per-host credential no longer
matches, with nothing anywhere reporting the divergence.

The old binary, env file and unit file are **left in place** as the rollback
path. Once the new agent is healthy:

```bash
rm -f ~/.config/systemd/user/devcanopy-agent.service ~/.config/devcanopy-agent.env
sudo rm -rf /opt/devcanopy-agent      # or ~/.local/bin/devcanopy-agent
systemctl --user daemon-reload
```

If you skip the handover and install over a running old agent, the failure is
misleading: the new unit crash-loops on `EADDRINUSE` every three seconds while
the old one keeps serving `/v1/health`, and the installer reports a version
timeout that names neither the port conflict nor the other unit.

## Redeploy an existing host from source (Linux, our own hosts)

`redeploy.sh` is the **from-source** path, kept deliberately for our own Linux
hosts (decision recorded on #392): it builds with `cargo`, so it needs the
Rust prerequisites above, and it is Linux/systemd-only. To move a host onto a
published release instead, re-run `install.sh` — since #392 that is the path
that needs no toolchain. `redeploy.sh` **never prompts for a token** and only
swaps the binary.

From the crate directory on the target host (e.g. `ubu-01`, on a fresh checkout
of the new commit):

```bash
./deploy/redeploy.sh
```

What it does:
1. Builds `--release`, then asks the binary it produced for its version
   (`--version`) — the artifact is the only thing that can say which CalVer it
   compiled in.
2. Preserves the currently-installed binary as `solador-agent.prev` (the
   rollback anchor).
3. **Atomically swaps** the new binary into place. The running binary can't be
   overwritten in place — Linux returns `ETXTBSY` ("Text file busy") — so the
   script stages the build to `solador-agent.new` and `mv`s it over the live
   path. A rename over a running executable is allowed even when an in-place
   write is not.
4. Restarts the user service.
5. **Verifies** by polling `/v1/health` (using the token/bind/port from the env
   file) until it reports the version from step 1. If the new binary never
   reports the expected version, the script fails loudly and tells you to roll
   back — the bad binary is live but you have a one-command escape hatch.

It requires an existing `~/.config/solador-agent.env` and systemd user unit; if
the host has never been installed it errors and points you at `install.sh`. It
resolves the live binary from the unit's `ExecStart` — the user-owned
`~/.local/bin/solador-agent` on a host installed or migrated by #392's
installer, `/opt/solador-agent/solador-agent` on one that has not been
migrated — and uses `sudo` only if that directory isn't user-writable. One
known limit: it reads `ExecStart` with a plain `awk '{print $1}'`, so on a
host whose HOME contains a space (where `install.sh` renders a double-quoted
`ExecStart`) `redeploy.sh` and its `rollback` resolve a truncated path and
fail; re-run `install.sh` on such a host instead.

## Roll back

Every path that replaces the binary — `install.sh`, `redeploy.sh`,
`solador-agent update` — keeps the displaced one as `solador-agent.prev`, and
two commands put it back. Both swap `.prev` into place through a staged
sibling and an atomic rename, restart the service, verify it came back, and
are **reversible**: the binary you rolled back over becomes the new `.prev`,
so running the same command again rolls forward. Neither needs cargo, a
checkout, or the network — that is the point of keeping the prior binary on
the host. With no `.prev` both refuse without touching the live binary.

**In the binary, on either platform** (#393):

```bash
~/.local/bin/solador-agent rollback
```

It verifies the restored **version** on `/v1/health` when the previous binary
can name one (`--version`), and **liveness only** when it cannot — a
source-built `.prev` from a shallow checkout — and its output says which.
Under liveness, an answer that *does* carry a version is held as the
displaced process still holding the socket, not accepted as "back". It takes
the same transaction lock as `update`, so it cannot interleave with one. Two
failures are its own, both exit `1`: *rollback swapped the binaries but the
service did not come back* (the swap stands and is reversible — run
`rollback` again to swap back), and *rollback half done* — the previous
binary is live but the displaced one could not be moved into `.prev`, so it
sits at `solador-agent.rollback-displaced` and nothing was restarted; the
message names all three files and the `mv` that finishes it.

**The from-source script, Linux only**:

```bash
./deploy/redeploy.sh rollback
```

It verifies liveness only, by the decision recorded in `deploy/lib.sh`: the
one case that script's rollback exists for is a `.prev` the operator cannot
describe. `install.sh` writes the same `.prev` when it replaces a binary, so
either command works after a download-install too.

After `solador-agent update` restores `.prev` on its own (exit `5`), `.prev`
still holds the same last-good binary as the live path — the failed
candidate is not kept — so a `rollback` right after an automatic recovery is
a no-op swap. If `update` reported exit `3`, read the line that says what is
at the live path: *the candidate* means the restore's own rename failed, and
`rollback` is the fix; *the previous binary* means it is back but did not
come up, so `rollback` would only swap the same bytes — inspect the service
with the commands the message ends with.

Two edges worth knowing. A `.prev` from before #393 has no `rollback`
subcommand of its own (it exits 2 on the word), which does not matter — it
is the *live* binary that runs the command, and it is what you roll back
*to*. And if the live binary itself is the one that is broken beyond running
`rollback` — the case that command cannot cover — the swap is the same
moves by hand, staged so the live path is never absent: copy the broken
binary aside (a copy, not a move — the live path must stay populated for a
`KeepAlive`/`Restart=always` respawn), then rename the staged copy over it:

```bash
cp -p ~/.local/bin/solador-agent ~/.local/bin/solador-agent.bad          # keep it, for the bug report
cp -p ~/.local/bin/solador-agent.prev ~/.local/bin/solador-agent.new
mv -f ~/.local/bin/solador-agent.new ~/.local/bin/solador-agent          # the one atomic step
launchctl kickstart -k gui/$(id -u)/app.solador.agent     # macOS
systemctl --user restart solador-agent                    # Linux
```

## How Solador connects

- Solador reaches the host at `http://<the configured bind address>:7878` —
  the Tailscale IP by default, or whatever `SOLADOR_AGENT_BIND` was set to on
  a LAN/VPN host.
- It sends `Authorization: Bearer <token>` (the same token from the env file) on
  every request, polling `/v1/snapshot` and `/v1/containers`.
- The agent binds only that address (`SOLADOR_AGENT_BIND`), so by default the
  port is not served on the public NIC. Verify with `ss -tlnp | grep 7878`
  (Linux) or `lsof -nP -iTCP:7878 -sTCP:LISTEN` (macOS) — it should show only
  the configured address. Binding all interfaces (`0.0.0.0`) is opt-in and
  should only be done behind a firewall.
