# Solador Agent

A small per-host metrics agent. It exposes host metrics and a container/VM list
as JSON, guarded by a bearer token. The [Solador](../) app polls it to render
a dashboard, over any network path that reaches the host: **Tailscale is
optional**, not required.

Plain HTTP by default; `SOLADOR_AGENT_TLS=1` serves HTTPS instead, with a
self-signed certificate the agent generates once and keeps for the host's
lifetime (#447 — see **TLS**, below), which the cockpit pins by fingerprint
(#448). The two transports differ in what protects the bearer token:

- **Over plain HTTP the tailnet is the protection**, so the agent binds only
  the host's Tailscale IP and refuses to start without one (unless
  `SOLADOR_AGENT_BIND` says otherwise).
- **Over HTTPS the token is encrypted and the certificate pinned**, so a host
  with no Tailscale binds **all interfaces** instead of refusing (#449). See
  **Network exposure**, below — that is a real change in who can reach the
  port.
- **The cockpit never sends the token over plain HTTP off loopback and the
  tailnet (#449).** A paired host is dialled over pinned TLS; a host that was
  never paired is dialled over plain HTTP only when its address is loopback or
  Tailscale (`100.64.0.0/10`, `fd7a:115c:a1e0::/48` minus its 4via6 prefix
  `fd7a:115c:a1e0:b1a::/64`; a range, not proof of tailnet membership — see
  `SECURITY.md`), and is otherwise not
  polled at all until it is paired. That rule — not the port being closed to
  the internet — is what keeps the token off an open network.

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
  - `gpu` is **measured on Macs** (agent ≥ 0.5.1) **and on hosts with an
    NVIDIA card** (agent ≥ 0.4.0) — `sysinfo` reports no GPU on any platform.
    - **macOS** reads IOKit's `IOAccelerator` registry through
      `crates/accelerator`, the same reader and mapping the cockpit uses for
      its own card, so a Mac host's remote card matches what that Mac shows
      locally. On Apple Silicon the GPU has no VRAM of its own: `vramUsedGB` is
      the GPU's resident share of system memory and `vramTotalGB` is physical
      memory, the pool it allocates from.
    - **NVIDIA** reads `nvidia-smi`. `usage` is the utilisation percentage;
      `vramUsedGB` / `vramTotalGB` are its MiB figures in the same 1024-base
      "GB" as every other size here (a 12288 MiB card reads `12.0`). A
      multi-GPU host reports its **first** card, since the contract carries
      one `gpu`.

    Still `{}` wherever nothing was measured: a Mac that registers no
    accelerator (a VM), no `nvidia-smi` on `PATH`, a failed or hung
    invocation, or output the agent doesn't recognise. AMD and Intel GPUs on
    Linux are not read yet, so they are part of that set.
    The probe runs on its own task every 5s — never on the 1s sample path, so
    a wedged `nvidia-smi` (2s hard timeout) or a slow IOKit read (on a blocking
    thread) costs a stale GPU reading, not a stalled snapshot.
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
| `SOLADOR_AGENT_BIND`   | no       | tailnet IP, else see below | Host/interface to bind. **An explicit value always wins**, with TLS on or off. Unset: the detected Tailscale IP (`100.x`) if there is one. With no Tailscale IP, it depends on TLS (#449): **`SOLADOR_AGENT_TLS=1` binds all interfaces (`0.0.0.0`)**; plain HTTP **refuses to start** rather than send the token in the clear on whatever network the host is on. Set to `0.0.0.0` (or `::`) explicitly to bind all interfaces over plain HTTP too — only behind a firewall. See **Network exposure**. |
| `SOLADOR_AGENT_PORT`   | no       | `7878`  | TCP port. Bound on `SOLADOR_AGENT_BIND`. |
| `SOLADOR_AGENT_TLS`    | no       | unset (HTTP) | `1` serves HTTPS on the same port, with a self-signed certificate kept for the host's lifetime. **Turn it on with `install.sh --enable-tls`, not by hand-editing this line**: an *older*, pre-#447 installed agent reacts differently per platform to a hand-set key. **On Linux**, `EnvironmentFile=` passes it straight through: `update` swaps in a new HTTPS-only binary but its own pre-#447 code still probes it with `http://` — the health check fails, `.prev` is restored, and `update` exits 5. **On macOS**, the launcher installed before #447 forwards only `TOKEN`, `BIND`, `PORT`, `SKIP_FSTYPES` and `RUST_LOG` to the agent — never this key, and `update` replaces only the binary, never the launcher — so the new binary never sees `SOLADOR_AGENT_TLS`, keeps serving plain HTTP, and `update` exits 0 with TLS silently off. See **TLS**, below. Any other value (or absent) is plain HTTP. |
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

Three **commands** exist as well, dispatched at the same point — before
tracing, the token check, the sampler or a listener — so they are always a
separate process from the service they act on:

```bash
solador-agent update           # move this install to the latest published release, verified; see Updating
solador-agent rollback         # put the previous binary back, offline; see Roll back
solador-agent tls-fingerprint  # print the served certificate's SHA-256 fingerprint, colon-hex, and nothing else; see TLS
```

`update` and `rollback` are [#393](https://github.com/Sassy-Dog/solador/issues/393);
`tls-fingerprint` is [#447](https://github.com/Sassy-Dog/solador/issues/447).
None takes arguments of its own: `update --force` is refused, not ignored
into an unforced update. `update`/`rollback`'s exit codes are a contract (the
opt-in scheduled job, **Unattended updates** below, reads them): `0` updated,
or already current *and serving*;
`1` failed with nothing changed (a refusal, a network error, a rejected
signature — and, for `rollback`, a swap that did not come back or was left
half done, both named); `3` failed **and** the previous binary could not be
restored — inspect the service; `4` no applicable release (the feed is not
newer than what is installed — the normal answer on a from-source host
running ahead of the last tag, and nothing to page anyone for); `5` failed,
and the previous binary is back and serving — nothing is broken, but a human
should look at why; `75` another `update`/`rollback` holds the lock (the
line names the holder's pid and start time); `2` usage. (On macOS the
scheduled job's exit status is the launcher's when the launcher did not run
`update` at all: `0` for a firing it discarded by design, `6` for a check it
**held** because it could not read a clock or its stamp — see **Unattended
updates**. The agent itself never exits 6.) `tls-fingerprint` is simpler: `0`
printed, `1` no certificate exists yet (it never generates one — see **TLS**).

An argument the agent does not recognise is **refused** (exit 2), never ignored:
it takes none in normal operation, so one arriving means something upstream is
wrong — a hand-edited `ExecStart`, or a wrapper passing flags meant for
something else.

## TLS

[#447](https://github.com/Sassy-Dog/solador/issues/447), part 1 of
[#445](https://github.com/Sassy-Dog/solador/issues/445). `SOLADOR_AGENT_TLS=1`
in the env file serves `/v1/snapshot`, `/v1/containers` and `/v1/health` over
HTTPS on the same `SOLADOR_AGENT_PORT` instead of plain HTTP — routes, auth
and JSON are unchanged either way.

**The cockpit pairs with a TLS agent, and pins its certificate
([#448](https://github.com/Sassy-Dog/solador/issues/448)).** `install.sh`
defaults a *fresh* install to `SOLADOR_AGENT_TLS=1` (see below), and a cockpit
that has the pairing half reaches it like this: Settings → Connections → add
(or edit) the host → **Check certificate** fetches the certificate the
address presents *without trusting it* and shows its SHA-256 fingerprint →
compare that with what `install.sh` printed (or `solador-agent
tls-fingerprint`) → press **Trust**. Nothing is stored until you do, and the
fingerprint is never typed in. From then on the cockpit dials that host over
HTTPS and accepts **exactly that one certificate**: no certificate authority
is consulted, the name is not checked (the certificate is the identity, not
its SANs), and the handshake signature is still verified with the
certificate's own key, so a copy of the certificate without its private key is
refused. All of that happens inside the handshake, before a byte of HTTP — the
bearer token is never sent to a peer that has not proved it is the pinned
agent. A pinned host is **never dialled over `http://`**, not even after a
failure.

Two failures get their own words and are never shown as *unreachable*:
**certificate changed** (the agent presents something other than the pinned
certificate — for example after its `solador-agent.tls.*` files were deleted
and regenerated; the host's row in Settings then offers **Re-pair**, which
runs the same Check → compare → Trust flow and replaces the pin) and
**doesn't speak TLS yet** (a pinned host answered plain HTTP — its agent has
`SOLADOR_AGENT_TLS` off). To take a host back to plain HTTP, delete it in
Settings and add it again; there is deliberately no one-click downgrade.
A host that was never paired keeps polling over `http://` exactly as before.

A **cockpit older than #448 cannot dial an HTTPS agent** at all — it reads
such a host as unreachable. Update the cockpit, or set `SOLADOR_AGENT_TLS=0`
in the agent's env file and restart the service (or pass
`SOLADOR_AGENT_TLS=0` to `install.sh` itself, which — like
`SOLADOR_AGENT_BIND`/`_PORT` — always wins over the fresh-install default).
`install.sh`'s Done block says so whenever TLS ends up on.

**The certificate is self-signed, ECDSA P-256, and generated exactly once.**
On its first TLS-enabled start the agent generates a keypair
([`rcgen`](https://crates.io/crates/rcgen)) and writes it beside the env
file — `solador-agent.tls.key` (mode `0600`) and `solador-agent.tls.crt`,
namespaced the same way `solador-agent.env` itself is, not bare
`tls.key`/`tls.crt` in an XDG root every app shares — then keeps it for the
host's lifetime: every later start loads the same files rather than
regenerating. **Never delete them unless you mean to re-pair** — a new
certificate has a new fingerprint, and every cockpit that pinned the old one
reports this host as *certificate changed* and stops polling it until it is
re-paired.
The key is never logged anywhere; the certificate is not secret (its whole
purpose is to be handed out, as a fingerprint, for pinning).

**The directory is resolved from the env file, never from `$HOME` alone.**
`run-agent.sh` and `solador-agent.service` both export
`SOLADOR_AGENT_CONFIG_DIR` pointing at the exact directory the env file was
written into; the agent uses that when set, and falls back to
`$HOME/.config` otherwise. `install.sh` never exports the var itself — its
own `tls-fingerprint` calls resolve through that same `$HOME/.config`
fallback, which is exactly where `install.sh`'s own process writes the env
file, so the two agree without it needing to export anything. The fallback
otherwise applies only to a manual invocation with no launcher in front of
it. This matters on macOS specifically: launchd's `HOME` (the target user
record's) need not be the `HOME` `install.sh` ran under, so deriving the
certificate's location from `$HOME` alone could point the running service
at a directory that holds no env file at all — which is why the launcher
exports the var explicitly rather than relying on the fallback. **Never set
`SOLADOR_AGENT_CONFIG_DIR` in the env file itself**: on Linux,
`EnvironmentFile=` would then override the unit's own line above it, so the
agent would serve out of a different directory from the one the health
checks pin. On macOS such a line has no effect: the launcher does not forward
that key (it logs and ignores it) and always exports the env file's own
directory itself.

**`solador-agent tls-fingerprint`** prints the certificate's SHA-256
fingerprint, colon-hex, and nothing else — compare it with the fingerprint the
cockpit shows next to **Trust** when you pair this host. It is
**read-only**: unlike the agent's own startup, it never generates a
certificate, so it cannot race the running service over which host ends up
in the certificate's SAN list (see below). Run it any time after the agent
has started at least once with TLS on; before that, it refuses, naming the
reason.

**The certificate's SAN list is `localhost`, `127.0.0.1`, `::1`, plus the
resolved bind host** (the detected Tailscale IP, in the common case) — added
at generation time, since only the agent's own startup knows what it is
about to bind to. It is fixed from then on, and **the agent's own health
probes do not depend on it (#449, #457)**: both trust exactly this one
certificate as their only root — never with verification disabled — but check
it by identity, not by the address they dial. `install.sh`'s check verifies it
as the name `localhost` (in every certificate's baseline) while `curl
--connect-to` dials the bind address; `solador-agent update`'s post-restart
probe trusts the certificate as its only root. For an IP or a DNS-name bind it
dials `localhost` with the connection pinned to the bind (`resolve_to_addrs`,
the counterpart of `--connect-to`): its IP, or the addresses the name resolves
to (looked up before anything is swapped). A wildcard bind is **not** pinned:
the probe dials loopback as written — `https://127.0.0.1:P` for `0.0.0.0` or an
empty bind, `https://[::1]:P` for `::` / `[::]` — and verifies against the
baseline `127.0.0.1` / `::1` IP SANs. Chain and hostname are both verified. A
bind that changes after the certificate exists therefore breaks neither,
whether it is an IP or a name (the MagicDNS name of a host first installed on
all interfaces, say).

Two bind forms are **refused under TLS by both `solador-agent update` and
`solador-agent rollback`**, before anything is fetched, swapped or restarted:
an IPv6 zone id (`fe80::1%en0`), which the probe has no portable place to carry
(a zone is interface-local, and the probe's `localhost` URL and pinned address
name none), and a DNS name this host cannot resolve (bounded at 5 s) — a
refusal with nothing changed, never a failed recovery. Advice to "bind the
address without the zone" does not work for a link-local address, which is
reachable only through its zone: bind a non-link-local address, or turn TLS off
for that host. Until then, an operator who needs to roll back does it by hand
(see **Roll back**, "When `rollback` refuses the bind").

**A wildcard bind (#449) adds nothing to the list, and needs nothing.**
`0.0.0.0` and `::` are dropped from the SANs (nothing dials them), and the
local probes dial loopback for a wildcard. The cockpit is unaffected: it pins
the certificate's fingerprint and checks no hostname, so it reaches the agent
on any address. A host that generated its certificate under a wildcard bind and
is later bound to a concrete address (a Tailscale IP that appeared; an edited
`SOLADOR_AGENT_BIND`) keeps working for the same reason — the certificate's
name list is not consulted by anything that matters, so nothing needs
regenerating and nobody needs re-pairing. (An *external* client that verifies
the certificate by the name it dials would still see a mismatch; the SAN list
itself is unchanged and tracked in #457.)

### Network exposure

Binding all interfaces makes the authenticated agent reachable on **every
network the host is on** — on a cloud VM, that can include a public
interface. What protects it then is the bearer token and the encrypted,
pinned transport, and nothing about the network. (A cockpit paired with the
host uses that transport; an *unpaired* one is only ever allowed to use plain
HTTP on loopback or Tailscale, so it cannot put the token on an open network
by mistake.) So:

- On a host with a public address, **firewall the agent's port** (default
  `7878`) to the networks that should reach it, or set `SOLADOR_AGENT_BIND`
  to the one interface the cockpit dials (`SOLADOR_AGENT_BIND=192.168.1.20`).
- `install.sh` says which interface it bound and why, twice: when it decides
  (`==> Binding to 0.0.0.0 (all interfaces: no Tailscale IP detected, and TLS
  is on)`) and in the Done block (`Bind: 0.0.0.0:7878 — ALL interfaces`).
  The agent logs a warning at start whenever it binds a wildcard; with TLS off
  it is a separate, louder one (plain HTTP on every interface, token in
  cleartext, and how to fix it).
- `install.sh` marks a bind it chose this way with `SOLADOR_AGENT_BIND_AUTO=1`
  in the env file (the agent ignores the key). That bind is **provisional**:
  every re-run sets it aside and resolves the bind again — the Tailscale IP if
  there is one now, `0.0.0.0` only while TLS is on, and with TLS off and no
  tailnet the same refusal a fresh host gets (nothing changed, nothing
  downloaded). Name one address with `SOLADOR_AGENT_BIND` to keep plain HTTP on
  a host with no tailnet. A bind **without** the marker is your explicit choice
  and is kept, plain HTTP included; the installer then says, loudly, that the
  token is crossing every network in the clear. The same holds by hand: if you
  edit `SOLADOR_AGENT_TLS=0` into the env file without re-running the
  installer, **remove both `SOLADOR_AGENT_BIND` and `SOLADOR_AGENT_BIND_AUTO`**
  (so the agent finds the tailnet address itself) or set `SOLADOR_AGENT_BIND` to
  one address, because the agent honours the `0.0.0.0` already there.
- A host with Tailscale keeps today's default: the tailnet IP only.
- Plain HTTP is tailnet-only **by default**, and no tailnet is a refusal — the
  case where the network is the only thing between the token and a listener. An
  explicit `SOLADOR_AGENT_BIND` (or a wildcard bind kept after a hand edit to
  `SOLADOR_AGENT_TLS=0`) can change that, which is why the cockpit only dials
  plain HTTP to loopback or Tailscale addresses (`agentclient`'s `plain`
  module), whatever the agent is listening on.

**`install.sh` writes `SOLADOR_AGENT_TLS=1` on a fresh install** (no env file
existed yet) and prints the fingerprint in its Done block — **but only when
the STAGED binary actually supports TLS.** The decision is made after the
release is downloaded and verified, by asking those exact bytes
(`solador-agent tls-fingerprint`, which a release published before #447
refuses as an unrecognized argument, exit 2); a fresh install that lands on
such a release — `SOLADOR_AGENT_RELEASE` pinned to one, or simply run before
the first tag that carries #447 — stays plain HTTP rather than writing a
setting the agent cannot honour. `--enable-tls` against such a release is
refused outright, naming the reason, rather than silently doing nothing. A
re-run otherwise leaves an existing env file's choice alone unless given
`--enable-tls`; there is no `--disable-tls` — turning TLS back off, like
rotating the token, is an env-file edit made by hand. Without the flag ever
having taken effect, behaviour is byte-for-byte the agent's original
plain-HTTP form.

**`install.sh` re-runs, `solador-agent update` and `rollback` never touch the
key or certificate.** `install.sh --uninstall` keeps them, exactly as it keeps
the env file, and names them in its "left" output; `--uninstall --purge`
removes them too, because purging the host's credentials means a re-pair.

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
a replayed older feed. An unattended daily check is opt-in at install
(`--enable-timer`, [#394](https://github.com/Sassy-Dog/solador/issues/394);
**Unattended updates** below) and off by default.

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
  host's Tailscale IP. A host without Tailscale gets **all interfaces
  (`0.0.0.0`)** on a TLS install — the default for a fresh install (#449; see
  **Network exposure**) — or can set `SOLADOR_AGENT_BIND=<that address>` to
  pin one interface. Only with TLS off (`SOLADOR_AGENT_TLS=0`, or a binary
  that predates it) and neither a Tailscale IP nor `SOLADOR_AGENT_BIND` does
  the installer refuse; when it can tell in preflight, that is before any
  download.
- **Linux:** systemd with a reachable user manager (`systemctl --user`, so a
  real login session — not `sudo -u` or `su`). A non-systemd Linux (Alpine's
  OpenRC, say) is not supported by the installer; the musl binaries still run
  there, by the manual verify-and-`chmod` steps under **Releases**.
  **macOS 11 or later:** a login session for the user running the installer —
  the agent is a LaunchAgent in that user's `gui/<uid>` domain, so it needs
  someone logged in at the console (or via Screen Sharing), and it does not run
  before anyone logs in.
- `agent/deploy/*` and `agent/release-signing-key.pub`, not to build anything —
  by either of two paths ([#434](https://github.com/Sassy-Dog/solador/issues/434)):
  - **A checkout of this repository's `main`** (`git clone`).
  - **`deploy/bootstrap.sh`**, needing `curl`, `tar`, `mktemp`, `id`,
    `basename` and `bash` — the same "usual coreutils" assumption
    `install.sh`'s own Prerequisites make below — for a host with no
    checkout at all (`gzip`/`grep`/`sed`, for the best-effort resolved-commit
    readout, and `awk`, for `--help`, are used too but never required):
    ```bash
    curl -fsSLo bootstrap.sh https://raw.githubusercontent.com/Sassy-Dog/solador/main/agent/deploy/bootstrap.sh
    bash bootstrap.sh [--ref <40-hex sha>] [install.sh flags...]
    ```
    **Fetch `bootstrap.sh` itself from `/main/`, always — pinning is what
    `--ref` is for, never the fetch URL.** A copy fetched from
    `raw.githubusercontent.com/.../<sha>/agent/deploy/bootstrap.sh` gets NO
    protection from the reachability check below: that check is code inside
    the script, so a copy from a commit not already known to be on `main` is
    free to run its own version of it (skip it, or always answer yes) and
    trust its own key. Only a `bootstrap.sh` already known to be on `main`
    enforces the guarantee at all — the pinned form above still fetches from
    `/main/` and passes the pin as `--ref` for exactly this reason.

    A `--ref` other than `main` is trusted only once GitHub's compare API
    (unauthenticated) confirms it is reachable from `main` — `codeload`
    will archive *any* commit this public repository holds, merged or not,
    including an open pull request's head, and only a commit `main`'s own
    history already contains keeps the property below. It then downloads
    the repository **archive at a `main` commit** (or the exact commit
    named by `--ref`) from `codeload.github.com` over HTTPS, extracts only
    `agent/deploy/*` and the signing key(s) — nothing else the archive
    carries reaches disk — and runs the extracted `install.sh` unchanged,
    passing every remaining argument through (`-h`/`--help` is the one
    exception: `bootstrap.sh` answers that itself; run `install.sh`
    directly, or after extraction, for its own). This keeps the property
    below (the key still arrives from `main`, the protected ref, never from
    beside the binary): an archive of a commit `main`'s history contains —
    the default, or a `--ref` the compare check has confirmed — travels
    from `codeload.github.com` over the same GitHub HTTPS a `git clone`
    would use, so it is not "a tag or an archive of one" in the sense the
    note above refuses. Piping it into a shell is deliberately not the
    documented form — download it to a file first — but the whole script
    lives in one function called on its last line either way, so a
    transfer cut short downloads and runs nothing.
    `install.sh` itself is unchanged by which path fetched it: its own
    release resolution (`/releases/latest`, unsigned by design) and the
    fresh-install downgrade window docs/AGENT-DISTRIBUTION.md §6 already
    records are neither closed nor widened by `bootstrap.sh` — that window
    was never about how `install.sh` arrived. One thing it does change:
    `install.sh`'s own "re-run this" hints (the `/opt` migration step, a
    bind-address example, a version pin for a refused downgrade, unattended
    updates left off, ...) would otherwise name `$0` — a path under
    bootstrap.sh's own staging
    directory (`~/.cache/solador-agent-bootstrap.*`), removed the moment
    bootstrap.sh's EXIT trap runs, so a hint built from it would name a file
    that is already gone. `bootstrap.sh` exports `SOLADOR_AGENT_BOOTSTRAP=1`
    immediately before running the extracted `install.sh`, and every hint
    prints `bash bootstrap.sh [--ref <sha>] ...` instead whenever that is
    set — the command that will actually still exist next time.

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
`agent/deploy/`: `scripts/build-agent.sh`'s full four-target, signed run happens
only in `release.yml` on a `v*` tag, and an ungated break there would be found
mid-release. PR CI does build one target
through it, `x86_64-unknown-linux-musl` (#457), so a static-link break shows
up before a tag; the other three targets and the signing still run only at
release.

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
`minisign`, no public key, no release, no asset). Since #394 it also covers
**the unattended update job**, on both platforms, as observed installer
actions: a default install writes no timer, oneshot or updater plist and
says nothing to the service manager about one; `--enable-timer` renders the
oneshot with the absolute path plus `update` (quoted for a HOME with a
space), installs the timer verbatim, `enable --now`s it without ever
starting the oneshot, and on macOS bootstraps the `.update` sibling — parsed
back with `plistlib` for its five `ProgramArguments`, `StartInterval`,
pinned `HOME`, metrics label, and the absence of `RunAtLoad`, `KeepAlive`
and `StartCalendarInterval` — without a `kickstart`; a no-flag re-run leaves
either byte for byte and says nothing to the manager; a repeated opt-in
makes one job, not two; a removed job stays removed; the timer failing to
enable, or the updater plist failing its lint, is a failed opt-in that
names the half that failed; and an unmigrated `/opt` host, an unwritable
install directory, root, and a metrics install that did not verify each
create nothing. The launcher's update mode has its own cases with a stubbed
clock, wake time and boot time, and the Linux guard has the mirror set with
its manager reads, clock and kernel counter stubbed (**Unattended updates**
below lists both).
`redeploy.sh` keeps its source-level invariants: taking `.prev` before the
swap, and aborting on a binary that carries no version.

**`bootstrap.sh`'s own coverage lives in the same suite** (#434). It
downloads an ARCHIVE rather than a directory, so its fixtures are tarballs
shaped the way `codeload.github.com` shapes one (`solador-<ref>/...`) — the
stub `curl`'s existing `-o <dest> <url>` fixture lookup needed no changes to
serve them, since a codeload URL's basename is just the ref. Covered:
extraction restricted to `agent/deploy/*` and the signing key(s) (a decoy
crate, a workflow file and `agent/Cargo.toml` packed into the same archive
must never reach disk), pass-through arguments reaching `install.sh`
unchanged with `--ref` stripped, the staging directory under `~/.cache`
removed on every exit — a bootstrap.sh refusal, and a non-zero exit from
`install.sh` itself, which bootstrap.sh passes straight through — the
best-effort resolved-commit readout against a real pax global header
(captured verbatim from a live archive, not synthesised), the root refusal,
and `--ref` validation (a control character, `--ref=<sha>` as one token, and
a bare `--ref` with no value at all). The `--ref`-reachable-from-`main`
check is driven through every status the compare API returns
(`identical`/`ahead` accepted, `diverged`/`behind`/unanswerable refused) —
with jq absent, the default here, exercising the no-jq sed fallback, and,
when this machine has a real `jq` (SKIPped loudly otherwise, the same as the
minisign cases), the jq branch too. A pretty-printed (multi-line) compare
response is covered on both sides: the ordinary case, and an
adversarially-ordered one — a per-file `status` spelling `identical` placed
textually *before* the real top-level `status` of `diverged` — that the
fallback must still refuse; it squashes the response onto one line before
truncating at `commits`/`files` for exactly this reason, so a later field
can never survive to be read as the answer regardless of how the response
was formatted. The load-bearing signature-mismatch case runs through
`bootstrap.sh` too: a wrong key delivered via the extracted archive is
rejected by `install.sh`'s real `minisign` gate, then re-run against the
accept-everything stub to prove the rejection was the verifier's. `--help`
is covered piped as well as run from a file (`cat bootstrap.sh | bash -s --
--help`, where `$0` is the word `bash`, not this script) — the canned
fallback text it prints, rather than the file it has no way to read, keeps
that path exiting 0 too. And `install.sh`'s own re-run hints, reached
through a real `bootstrap.sh` run (`SOLADOR_AGENT_BOOTSTRAP=1`), are
asserted to name `bash bootstrap.sh ...` rather than a path under the
already-removed staging directory.

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
booting it out again; since #394 it goes on to opt that install in, observe
the `.update` sibling loaded with zero runs, fire it by hand (a read-only
`update` against the real feed: exit 4, or 1 with no network — never a
swap, no restart; the suite waits out the guard's five-minute wake window
first), fire it again to watch
the guard discard it, and remove it with the documented commands while the
metrics service keeps its pid. It is opt-in because it does touch the host.

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
./deploy/install.sh --enable-timer       # ...and opt in to a daily unattended update check
./deploy/install.sh --uninstall          # remove THIS USER's install (env file kept)
./deploy/install.sh --uninstall --purge  # ...and delete the env file (it holds the token) too
./deploy/install.sh --help
```

A re-run is one update path — `solador-agent update` (**Updating**, below)
is the in-binary one, and the one the opt-in scheduled job runs
(**Unattended updates**, below) — and on the
unpinned form it **refuses to move backwards**: if the binary already installed reports a newer CalVer than the
release `/releases/latest` resolved to — the one unsigned link in the chain,
see `docs/AGENT-DISTRIBUTION.md` §6 — the run stops naming both versions.
Pinning `SOLADOR_AGENT_RELEASE` is how an operator says a downgrade is meant.

Both forms need a **published** release that carries the agent binaries.
`v2026.9.3` and everything before it carry none, and a draft is neither
resolvable (`/releases/latest` skips drafts) nor downloadable, so until the
first post-#390 release is published every install fails at the download step
— naming the tag and the asset, which is the honest outcome.

No arguments is the normal form, and it installs the metrics service and
nothing else — **no** unattended update job. `--enable-timer` is the one
opt-in (**Unattended updates**, below); an argument the script does not know
is refused (exit 2), never ignored, in any position and beside any known
flag. Everything runs as **your user** and nothing uses `sudo`.

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
   Tailscale IP, else all interfaces if TLS is on, else refuse), the port (`SOLADOR_AGENT_PORT`, else the
   existing file's, else `7878`), and `SOLADOR_AGENT_TLS` (see **TLS**, below,
   for how its value is decided), mode `600`, written beside the live file and
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
8. **Only with `--enable-timer`**, and only after step 7 succeeded: installs
   the separate daily update job (**Unattended updates**, below). Without
   the flag this step neither creates a job nor touches one an earlier run
   created; the summary line says which of the two it found.

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
thing only: extending `PATH` (below). (The same launcher with a fourth
argument, the word `update`, is the opt-in updater's entry point — see
**Unattended updates**; in that mode it exports nothing from the env file.)
At every start the
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
`--migrate-from-opt` step — which is why the migration is explicit rather
than something an update does one night. `--enable-timer` inherits the same
refusal one step earlier: on an unmigrated `/opt` host it stops before
creating any job and names the combined step, `./deploy/install.sh
--migrate-from-opt --enable-timer`.

### Uninstall

`--uninstall` ([#439](https://github.com/Sassy-Dog/solador/issues/439))
removes everything the installer put on disk, for **the invoking user
only** — it never touches another user's files, and it refuses to run as
root for the same reason `--enable-timer` does (the install, and everything
it created, is user-owned):

```bash
./deploy/install.sh --uninstall           # keeps ~/.config/solador-agent.env (it holds the token)
./deploy/install.sh --uninstall --purge   # ...and deletes it, and the pre-rename devcanopy-agent.env, too
```

Its refusals run in this order, each untouched: **root** first (same reason
`--enable-timer` refuses it), then an **unsupported platform** (this script
supports Linux/systemd and macOS/launchd, named as such), then the
**service manager being unreachable**
(`systemctl --user show-environment` on Linux, the `gui/<uid>` domain on
macOS — the same check the install path makes: a `sudo -u`/`su` session, or
one with no `XDG_RUNTIME_DIR`, cannot ask systemd to stop anything), then the
**update transaction lock** (`<bin>.update.lock`, see **Updating**, below) —
whose own section is skipped outright when `~/.local/bin` (the binary's own
directory) does not exist at all, since nothing can possibly be installed
under a directory that is not there. Where it does exist: this opens the
lock file — creating it if missing, and never truncating one
that exists, the same `create(true).truncate(false)` `agent/src/update.rs`'s
own `TransactionLock` opens it with — and takes a non-blocking exclusive
flock on it, `flock(1)` where it is on `PATH`, else the stock `perl`'s Fcntl
flock on that same already-open file descriptor, and **holds it for the
whole run, on both platforms**: uninstalling mid-swap would race that
transaction's own binary rename, so a transaction starting in the window
this spends stopping the service and removing its unit/plist meets that hold
as busy on its own terms (exit 75, its own code) rather than racing anything
here. Where *neither* tool is on `PATH` there is no way left to ask the
kernel whether a transaction is running, and this refuses — busy — before
anything changes, rather than guessing free (#439's follow-up review: this
is what makes the earlier "wherever `flock(1)` exists" caveat, and the exit
5 it produced, go away — see **Exit status** below). Once held, a note
(`pid=<pid> since=<epoch>`, the same shape `agent/src/update.rs` writes) is
left in the file so a racing `update`/`rollback`'s own busy message names
*this* uninstall rather than a stale previous holder.

Whether THIS run created the lock file is decided atomically — a `set -C`
(noclobber) write, before the file is ever opened for the flock itself — not
by a separate existence check that a second process racing the same instant
could invalidate. A later review found the previous revision could still
delete a file that *pre-existed* on a "lock is busy" refusal, which is
exactly how a second process can end up holding a lock on a *different*
inode than the one it believes it shares: `flock()` locks the open file
description, not the path, so unlinking a file another process has locked
and letting a third opener recreate the same name hands that third opener a
lock nobody is actually contending over (`agent/src/update.rs` documents the
same hazard for its own transaction lock). The lock file is now **never**
deleted on a busy result, whoever created it. Of the two narrower non-busy
failures, only the open itself failing deletes a lock this run created; a
`perl` reopen failure never deletes one, this run's or not — it proves
nothing about whether the lock is free.

On Linux it then runs `systemctl --user stop`, and then `systemctl --user
disable` (no `--now`; the two are separate calls, never combined) on each of
the four units it knows about — the metrics service, the update timer, the
update oneshot and the pre-rename unit — wherever, and *only* wherever, the
unit's own file still exists on disk. That file is the ONLY signal: a
failure of either call means exit **4** (below), and both calls are made
against the unit's FULL name (e.g. `solador-agent.service`, never a bare
one) — not because `disable` needs the exact name to find its own file
(`systemctl` expands a bare name to `<name>.service` for `disable` too), but
because the update *timer* does: a bare `solador-agent-update` resolves to
the oneshot `.service`, never the `.timer`, so the timer must be named in
full, and full names are used everywhere for consistency.

**Known limit ([#455](https://github.com/Sassy-Dog/solador/issues/455)):** an
earlier revision also asked the running manager's own state
(`systemctl --user is-active`, falling back to `list-units --all` for a unit
left `failed` rather than genuinely active) so that a re-run after exit 4
could still find and stop a unit whose file that *earlier* run had already
removed regardless of whether the stop itself succeeded. Every review round
on that logic found a new Blocking problem in it, so it was backed out
rather than shipped — the finding lives on at #455, not lost. Until it
lands: once a unit's file is gone, a re-run has nothing left to gate that
unit on, so it reports "Nothing installed" even if the manager is still
holding it. **Confirm by hand** with `systemctl --user status <unit>` after
an exit 4 — the macOS path is unaffected, since `launchctl print` is always
asked directly with no file-existence gate in between. It then removes
`solador-agent.service`, `solador-agent.service.prev`,
`solador-agent-update.service`, `solador-agent-update.timer` and
`devcanopy-agent.service`, then `daemon-reload`s and `reset-failed`s all
four unit names (clearing any "failed" state a disable/stop that reported an
error left behind, even on a unit this run never touched). On macOS it runs
`launchctl bootout gui/<uid>/app.solador.agent` and
`gui/<uid>/app.solador.agent.update` (whichever are loaded) and removes both
plists. Before either unit/plist is removed, the binary path it currently
names is read: one outside `~/.local/bin` — an unmigrated `/opt` host, most
likely — is reported with the *actual* remedy rather than pointed at a flag
that does not apply post-uninstall:

```
left behind: /opt/solador-agent/solador-agent
             this user cannot delete it; its owner can, e.g.:
               sudo rm -rf /opt/solador-agent
```

(a path outside `/opt/solador-agent` gets the same two lines without the
`sudo rm -rf` suggestion, since there is no repo-known remedy to name).
Both platforms then remove the binary and its
`.prev`/`.new`/`.update.lock`/`.rollback-displaced` siblings, the macOS
launcher (`solador-agent-launchd`), the Linux update guard
(`solador-agent-update-guard`), and the update stamp
(`~/.config/solador-agent-update.last-attempt`). Logs are left alone.

It never runs `loginctl disable-linger` — another service on this login may
depend on it. A second run finds nothing left to remove and says so;
`--purge` alone (with no `--uninstall`) is refused, as is `--uninstall`
beside `--migrate-from-opt` or `--enable-timer`. The env file is named in the
output whether it is kept or removed, since it is the one file here that
carries the bearer token; `--purge` also removes the pre-rename
`devcanopy-agent.env`, since install copied its token out of that file and
never deleted it.

**Exit status**: 0 uninstalled (or already clean); 1 refused before anything
changed (root, an unsupported platform, an unreachable manager, the update
lock held or uncheckable, a hostile `SOLADOR_AGENT_LAUNCHD_LABEL`); 2 usage
(`--purge` without `--uninstall`, or the combinations above); 4 every file
was still removed, but a *reachable* manager refused a specific `stop` or
`disable` request anyway — the summary names *which*, since the two are not
the same claim: a failed `stop` means the process itself may still be
running and says so; a failed `disable` with a successful `stop` means only
its future auto-start (at next login/boot) is unconfirmed, and does not
claim the process is still running; 6 at least one file
that should have been removable — the service and other files may already be
gone — could not actually be deleted (a read-only parent directory, an
immutable file, or similar); fix that and re-run — 6 wins over 4 when both
apply. There is no exit 5: an earlier revision held the lock continuously
only when it already existed *and* `flock(1)` was on `PATH`, so a fresh host
(no lock file yet) or stock macOS (no `flock(1)`) fell back to a one-shot
check with a real window between it and the removals, and a transaction
starting in that window exited 5. #439's follow-up review closed the window
instead of narrowing it further, so that exit code has nothing left to name.

## Moving the agent to another user

Moving the agent from one Unix user to another on the same host — a
different login taking over the monitored workload — is **install as the
new user, then `--uninstall` ([#439](https://github.com/Sassy-Dog/solador/issues/439))
as the old one**, never an in-place move: `~/.local/bin` and
`~/.config/systemd/user` (or `~/Library/LaunchAgents`) belong to the account
that owns them, and neither service format has a "re-home this unit to
another user" operation.

**Both agents will be live on the same host at once, briefly — they cannot
share a bind address and port.** `SOLADOR_AGENT_BIND` defaults to the
*host's* Tailscale IP, or all interfaces on a TLS host without one (one per
machine, not one per Unix user), and
`SOLADOR_AGENT_PORT` defaults to `7878`; a fresh install has no env file of
its own to carry a different choice forward, so a plain `install.sh` as the
new user binds the exact socket the old user's agent already holds — the
same `EADDRINUSE` crash-loop **Upgrading from the pre-rename agent** (below)
describes for a single-user host, here between two different users on one
host instead. Give the new user's install a distinct port for the overlap:

1. **As the new user**, install exactly as any fresh host would — a
   checkout, or `bootstrap.sh` on a host with no checkout at all
   (**Prerequisites**, above) — on a port the old agent is not already using:

   ```bash
   SOLADOR_AGENT_PORT=7879 ./deploy/install.sh   # from a checkout; any port but the old agent's
   # or, checkout-free:
   curl -fsSLo bootstrap.sh https://raw.githubusercontent.com/Sassy-Dog/solador/main/agent/deploy/bootstrap.sh
   SOLADOR_AGENT_PORT=7879 bash bootstrap.sh
   ```

   Verify it is healthy — the installer's own final health check, or
   `systemctl --user status solador-agent` / `launchctl print
   gui/$(id -u)/app.solador.agent` — before touching the old user at all;
   that is what keeps a working agent serving throughout the move. A new
   user installing fresh **generates a new bearer token**; it does not, and
   cannot, inherit the old user's. If TLS is on ([#447](https://github.com/Sassy-Dog/solador/issues/447)),
   it generates a new **certificate** too, with a different fingerprint — a
   cockpit that had paired this host reports *certificate changed*
   ([#448](https://github.com/Sassy-Dog/solador/issues/448)), so re-homing this
   way means a re-pair (Settings → the host → **Re-pair**), the same as the
   token.
2. **In the cockpit**, replace that host's stored token *and port* with the
   new user's (Settings → Hosts). Until this step the cockpit is still
   polling the old user's agent — the new one is up but not yet the one
   being read.
3. **As the old user**, remove its install:

   ```bash
   ./deploy/install.sh --uninstall --purge          # from a checkout
   # or, checkout-free:
   bash bootstrap.sh --uninstall --purge
   ```

   `--purge` matters here specifically: leaving `~/.config/solador-agent.env`
   behind under an account nothing runs any more is a bearer token sitting on
   disk for no reason. (Without `--purge` the file survives — see
   **Uninstall**, above — which is the right default for every *other*
   reason to uninstall, just not this one.) Once the old agent's port is
   free, a subsequent `SOLADOR_AGENT_PORT=7878 ./deploy/install.sh` as the
   new user (a re-run, so its token is reused unchanged) can reclaim the
   default port; update the cockpit's stored port to match if you do.

   If the *old* user's install was itself never migrated off the pre-#392
   `/opt` layout, this step's output will name it as left behind with the
   remedy above (`sudo rm -rf /opt/solador-agent`) — that binary is
   root-owned, so this uninstall (which runs with no `sudo` anywhere)
   cannot remove it and must not pretend to. It is inert once nothing
   starts it any more; remove it by hand whenever you like.

Each step is independently re-runnable: re-installing as the new user, or
re-uninstalling as the old one, is a no-op or a safe refresh in the
ordinary case — step 3 exiting **0** ("Done" or "Nothing installed").

Step 3 exiting **4** is different, and worth knowing about before you rely
on a re-run to finish the move: every file was removed, but the manager
refused a specific `stop` or `disable` request for one of the four units it
knows about (the metrics service, the update timer and oneshot, and the
pre-rename `devcanopy-agent.service`). **Known limit
([#455](https://github.com/Sassy-Dog/solador/issues/455)):** whether a unit
counts as present is decided ONLY by whether its own unit file still exists
(see **Uninstall**, above) — so once that first run's best-effort removal
has taken the file with it, a RE-RUN has nothing left to gate the unit on
and reports "Nothing installed" rather than retrying the stop or disable the
manager refused. **Confirm by hand** — `systemctl --user status <unit>` —
that the old user's service is actually gone before treating the move as
finished; a clean re-run does not prove it on its own. (The macOS path is
unaffected: `launchctl print` is asked directly every time, with no
file-existence gate in between.)

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
   service listens on a detected tailnet address (or all interfaces, with TLS
   on) this command could not dial, and a
   probe of loopback would swap, fail, restore and exit 3 on a healthy host,
   so its absence is refused instead. Running as root is refused; an install
   this user cannot replace — the pre-#392 `/opt` layout — is refused with
   the migration step above.
2. Takes the transaction lock (`solador-agent.update.lock` beside the
   binary). A second `update` or `rollback` on the same install — yours
   racing a scheduled one, say — reports *busy* (exit 75) and changes
   nothing; the lock dies with the process, so a crashed run cannot wedge
   the next. (The lock covers these two commands, the scheduled job, which
   is this command, and — since #439's follow-up review — `install.sh
   --uninstall`, which now holds the same lock for its own run rather than
   merely checking it once. A normal, no-flag `install.sh` and
   `redeploy.sh` still do not take it, so do not run those during an
   update.)
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
`journalctl` lines the service sections above give). That output is the
process's own stdout (progress) and stderr (the one `ERROR:` line); **the
command writes no log file of its own** — the only log path it ever names
is the *service's* (the plist's `ProgramArguments[3]`, or the installer's
default `~/Library/Logs/solador-agent.log` when the plist names none), and
only inside that `tail` hint. Two values in that output look
like secrets to a scanner and are not: the numeric uid in `gui/<uid>` (the
launchd domain the operator types into `launchctl`, the same number `id -u`
prints) and a rejected signature's *trusted comment* (the asset name the
release signer put there — public, signed metadata, the string `minisign
-V` prints for anyone). From its own
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

## Unattended updates (`--enable-timer`)

**Off by default.** A default install creates the metrics service and nothing
else: no timer, no second LaunchAgent, no update check, no network request
beyond the install's own. Running an agent on your own servers should not
mean surprise restarts. Opting in is one flag, at install or on any re-run
([#394](https://github.com/Sassy-Dog/solador/issues/394)):

```bash
./deploy/install.sh --enable-timer
```

It installs a **separate** scheduled job — never a property of the metrics
service, because `solador-agent update` restarts that service and then
verifies (and, on failure, restores) it, and a job running *inside* the
service would be killed by its own restart — that runs the installed
`~/.local/bin/solador-agent update` as your user, with no `sudo`, no
checkout and no prompt. It is exactly the manual command of **Updating**
above with its exit codes: `4` (nothing newer) is the normal daily answer
and not a failure, `0` means an update happened or the bytes were already
current, anything else is a failed run that stays visible as one — there is
no retry loop, and a failed attempt is next tried the following day.

**Cadence: daily, no catch-up** (decision recorded on #394). One check per
24 hours while your session is up. The first check is a day after enabling,
never at enable time. A check whose moment falls while the machine is asleep,
or while nobody is logged in, is **discarded**: it is not run at wake, login
or boot to make up for the miss, and several missed intervals are not
coalesced into one late run. A session restart begins a fresh day. On macOS
this is a login-session job like the metrics agent — no boot-without-login
promise — and on Linux it lives in your user manager, which `loginctl
enable-linger` keeps up across logouts (the installer enables that, best
effort).

**Consent is preserved, and revoked only by you.** A re-run of the installer
*without* the flag leaves an enabled job exactly as it is (its files and its
enablement) and reports what the service manager says about it — enabled /
loaded, present but paused, or off — never re-enabling a job you paused; a
re-run *with* the flag regenerates the job's files from the checkout
(exactly like the metrics unit or plist) and never creates a second copy.
The disable/remove commands below take it away on its own, and
`install.sh --uninstall` takes it away along with everything else. A fresh
install without the flag never has one. One timing effect of any re-run on
Linux: the installer's `daemon-reload` re-bases a timer that has **not yet
fired** to the reload time, so a first check due in an hour becomes one due
in a day — later, never sooner, and never one on enable. A host that opted
in **before #411** keeps its unguarded oneshot across no-flag re-runs; the
summary line then reads `enabled but UNGUARDED` rather than `enabled`, and
one re-run *with* the flag is how the guard arrives.

**Exit status.** The installer exits `0` when the agent is installed and
serving (and, with the flag, scheduled); `1` when the install failed or was
refused; `2` on a bad argument; and **`3`** when the metrics service *is*
installed and serving but the opt-in failed — the `==> Done` block above the
error is true, and a script calling this can read it as such.

**Refused before anything is created**: as root (the updater refuses to run
as root, so the job would fail every day); on an install directory or binary
your user cannot write to; on an unmigrated `/opt` host, which needs the
explicit `--migrate-from-opt` first — both flags together do it in one run;
and, on Linux, when the *running* user manager is older than systemd 243
(RHEL 8, Ubuntu 18.04) or will not say its version, because `ExecCondition=`
would be ignored there and the updater would run unguarded.
The job is also only ever created *after* the metrics install verified, so a
failed install never records consent.

### Linux: `solador-agent-update.timer` + `.service` + the guard

Two user units beside the metrics one in `~/.config/systemd/user/`, plus
one script beside the binary. The units: a `oneshot` whose `ExecStart` is
the rendered binary path plus `update` (no `EnvironmentFile=` — the updater
reads `~/.config/solador-agent.env` itself, under the same rules, so the
token is in its process for exactly one header), with `SuccessExitStatus=4`
so a day with nothing newer does not paint the unit red; and a **monotonic**
timer, `OnActiveSec=24h` + `OnUnitActiveSec=24h`. Monotonic is what carries
the no-catch-up property on Linux in ordinary operation: the first firing
is a day after the timer starts, each later one a day after the last run
*started*, and the monotonic clock pauses through suspend
(`systemd.timer(5)`), so a wake never finds an elapsed deadline. An
`OnCalendar=` timer would fire on resume when its time passed during sleep,
and `Persistent=` would replay a firing missed across a stopped manager —
neither is used, nor `WakeSystem=`.

**The guard is what holds the property where the clock does not**
([#411](https://github.com/Sassy-Dog/solador/issues/411)). A source trace
of systemd's `timer.c` found that a `daemon-reload` after the timer's first
day (every installer re-run does one) re-arms the one-shot `OnActiveSec=`
such that the *next* suspend/resume fires it once, seconds after wake — the
coalesced catch-up the cadence forbids, and on a laptop whose network is
not back yet a failed attempt that costs the day. So the oneshot carries
`ExecCondition=/home/<you>/.local/bin/solador-agent-update-guard %n` (the
absolute path, rendered; installed by `--enable-timer` from
`agent/deploy/update-guard.sh`), which the manager runs before `ExecStart`
on **every** activation — the timer's, or a `systemctl --user start` by
hand — and which applies the same two rules as the macOS launcher: a firing
within **five minutes of the last resume** (or of the boot, on a boot that
has not slept) is skipped, and so is one within **23 hours of the last
attempt**, recorded in a one-line stamp of the launcher's format at the
same path, `~/.config/solador-agent-update.last-attempt` (this guard writes
it mode 0600; written *before* the attempt, so a failed run is not retried
until tomorrow). Its exit status is `ExecCondition=`'s own contract: **0**
runs `update`; **1** skips it cleanly — the unit ends `inactive` with
`Result=exec-condition`, not in `--failed`, and one line in the journal
says why; **255** *fails* the unit — `Result=exit-code`, listed by
`systemctl --user --failed` until its next activation or a `reset-failed`
— because an input the guard needs
could not be read or the stamp could not be written, and a hold that read
as a quiet day would be a fabricated state. Every usage error (wrong
argument shape, not under a unit, no `HOME`) is a hold for the same reason.

Where "seconds since the last resume" comes from, unprivileged: the guard
reads this activation's `InactiveExitTimestampMonotonic` from your user
manager (`systemctl --user show … %n`) and the last resume's
`InactiveEnterTimestampMonotonic` from the **system** manager's
`sleep.target` (`systemctl show … sleep.target`, a read-only property fetch
every user may make — every systemd sleep path pulls that target in and
stops it after the resume), both on `CLOCK_MONOTONIC`. **That second
reading is advisory.** `sleep.target` is `StopWhenUnneeded=` and nothing
else references it on a stock distribution, so the system manager
garbage-collects it once the cycle ends and a later `show` loads it fresh
and prints `0` — measured on systemd 256, in the same second the journal
recorded the target stopping. So `0` means "this boot has not slept" *or*
"it slept and the manager has forgotten when", and
`/sys/power/suspend_stats/success`, the kernel's own count, tells the two
apart: no suspends counted and the settle rule counts from the boot; one or
more counted and the guard logs one line — *the last resume cannot be
placed in time; the 23 h interval rule alone governs this firing* — and
falls through. It never holds on that reading (nor on a system manager it
cannot reach, nor on an answer it cannot parse), because "forgotten" is the
normal state of every laptop after its first suspend and a guard that holds
on the normal state is a job that never runs, with a red unit every day
teaching the operator to stop reading `--failed`. The interval rule needs
no resume and carries the cadence on its own; what the settle rule adds —
not while the network is still coming back after a wake — costs, when it
is unavailable, one attempt on a bad day, which the stamp then charges to
that day. Absent (no `CONFIG_PM_SLEEP`, or a kernel before 5.4) the counter
is not consulted; present and unreadable it is a hold, since that is a file
the kernel keeps and the line names it. Nothing here needs the journal
(which would survive the collection, but is readable only in `adm`/`wheel`,
which a dedicated service user is not in), logind's D-Bus, or root; the
guard's header records each reading as observed on a real user manager.
A `set -e` death anywhere in the guard is turned into a hold by an `EXIT`
trap, because the status such a death carries is `1` — the discard — and
would otherwise read as a quiet day, forever.
`ExecCondition=` itself needs systemd ≥ 243 (2019); the installer reads the
**running** user manager's version (`systemctl --user show -p Version`,
not the client's `--version`) and on an older one **refuses the opt-in**
rather than let the key be ignored and the updater run unguarded. Two
more facts of that contract shape the unit. `ExecCondition=` honours
`SuccessExitStatus=` too — a condition exit of `4` would *run* the updater
— so the guard's `1` and `255` are asserted to stay off that line. And a
condition binary that is *not there* is exec failure 203, inside the skip
range, so a unit whose guard was deleted would skip every day with
`Result=exec-condition`; the oneshot's `AssertFileIsExecutable=` on the
guard's path turns that into an error line in the journal and a failed
`start` (no more — an assertion changes no unit state, so it is not in
`--failed` either), the installer writes the guard before the unit that
names it, and the remove recipe below takes the units out first. The
unit also pins `PATH` to the system directories (the macOS updater
plist's decision — not `~/.local/bin`, the directory the installer writes
to, in front of a process that renames a binary over the service) and
unsets the guard's test seam.

```bash
systemctl --user list-timers solador-agent-update.timer      # next and last firing
systemctl --user status solador-agent-update.service         # exit 4 = nothing newer; "Skipped due to 'exec-condition'" = the guard discarded it
                                                             # failed + a "solador-agent-update-guard: HELD" line = held, nothing changed; failed + an updater exit 3 = check solador-agent.service
journalctl --user -u solador-agent-update -n 50 --no-pager   # the updater's own output, or the guard's one line
journalctl --user -u solador-agent-update -g 'solador-agent-update-guard:'   # just the guard's lines: a month of discards and a month of exit-4 days look alike in status
date -d @"$(cat ~/.config/solador-agent-update.last-attempt)" # the last attempt; older than two days while list-timers shows daily triggers = the guard is discarding every firing, holding every firing (is-failed says which), or the unit's assertion is failing (the guard file is gone)
systemctl --user reset-failed solador-agent-update.service   # after fixing what a hold named
systemctl --user start solador-agent-update.service          # check NOW, by hand (the guard still applies; `solador-agent update` does not)

systemctl --user disable --now solador-agent-update.timer    # pause: stop scheduling; files stay, metrics keeps running
systemctl --user disable --now solador-agent-update.timer \
  && rm ~/.config/systemd/user/solador-agent-update.{timer,service} \
  && systemctl --user daemon-reload \
  && rm -f ~/.local/bin/solador-agent-update-guard \
           ~/.config/solador-agent-update.last-attempt      # remove entirely: units first (disable first, or the running manager keeps a dangling timer), then the guard and the stamp
```

Neither of the last two touches `solador-agent.service`. Remove the units
before the guard, not after: a unit left behind without its guard is the
silent-skip shape above.

### macOS: `app.solador.agent.update`

A second LaunchAgent, `~/Library/LaunchAgents/app.solador.agent.update.plist`,
in the same `gui/<uid>` domain as the metrics one — always `<metrics
label>.update`, so `launchctl kickstart -k` of one never touches the other.
Its `ProgramArguments` are the metrics launcher's, plus the word `update`;
in that mode `solador-agent-launchd` exports nothing from the env file and
`exec`s `solador-agent update`, so the exit status launchd records is the
updater's own. `StartInterval` is 86400 with no `RunAtLoad`, so loading it
runs nothing. The plist pins `HOME` to the directory every other path in it
was rendered under (the updater resolves the metrics plist under *its* HOME,
and launchd's need not be the installer's) and names the metrics label in
`SOLADOR_AGENT_LAUNCHD_LABEL`; the token is in neither the plist nor the log,
`~/Library/Logs/solador-agent-update.log` (rotated at 10 MB like the agent's).

**The launcher is the no-catch-up guard on macOS**, not the plist.
`launchd.plist(5)` says a `StartInterval` firing that falls during sleep is
missed, and `StartCalendarInterval` — the key that *does* coalesce missed
firings into one run at wake — is not used; but the property is enforced
rather than trusted. Before every `update` the launcher refuses, exit 0 with
a log line saying why, a firing within **five minutes of the last wake or
boot** (`sysctl kern.waketime` / `kern.boottime` — a firing launchd delivers
because an interval elapsed with the lid closed arrives seconds after the
wake) or within **23 hours of the last attempt** (recorded in
`~/.config/solador-agent-update.last-attempt`, written *before* the attempt,
so a failed run is not retried until tomorrow). A discarded firing exits
`0`: it is the no-op the cadence describes, and the next is a day away. A
guard that cannot read a clock, finds that stamp unreadable, or cannot
write it, **holds** the check with exit **`6`** — a code the agent never
uses, so `launchctl print`'s `last exit code` cannot show a permanently
held job as a good day — and names what to fix in the log. The guard covers
the LaunchAgent only — a `solador-agent update` you type runs now. The
plist's `PATH` is the four system directories and nothing else: this job
needs no container CLI, and a process that reads the token and renames a
binary over the service, unattended, must not resolve a command under a
group-writable `/opt/homebrew/bin`.

```bash
launchctl print gui/$(id -u)/app.solador.agent.update      # loaded? runs, last exit code (4 = nothing newer, 0 = updated/current/discarded, 6 = held)
tail -n 50 ~/Library/Logs/solador-agent-update.log         # the updater's lines, or the launcher's reason for not running it
cat ~/.config/solador-agent-update.last-attempt            # epoch seconds of the last attempt
launchctl kickstart gui/$(id -u)/app.solador.agent.update  # fire the job by hand (the guard still applies; `solador-agent update` does not)

launchctl bootout gui/$(id -u)/app.solador.agent.update    # pause: stop scheduling until the next login (launchd reloads every plist here at login)
launchctl bootout gui/$(id -u)/app.solador.agent.update; \
  rm -f ~/Library/LaunchAgents/app.solador.agent.update.plist \
        ~/.config/solador-agent-update.last-attempt \
        ~/Library/Logs/solador-agent-update.log{,.1}       # remove entirely: job, stamp and log (a stale stamp would hold a later re-enable's first day)
```

Neither touches `gui/<uid>/app.solador.agent`, which keeps serving.

**What was observed and what was not.** `agent/deploy/lib_test.sh` drives
every rule of both guards with stubbed inputs. macOS: a stubbed clock, wake
time and boot time — a firing 30 s after wake, one 30 s after boot, a
second one inside the interval, the 23 h and 5 min boundaries, a clock that
fails or prints garbage, an unreadable stamp, an unwritable stamp, a clock
that moved backwards — and, opted in with `SOLADOR_DEPLOY_TEST_LAUNCHD=1`,
a real throwaway updater bootstrapped beside a real throwaway metrics
agent, seen loaded and not run, fired by hand into the real `update`
read-only (exit 4 against the published feed, or 1 with no route to
github.com — never a swap) without restarting the metrics service, fired
again to watch the guard discard it, and removed with the commands above
while the metrics service keeps its pid. Linux: the guard run directly with
its two manager reads, the clock and the kernel's counter stubbed (the
counter under an override root, never this machine's `/sys`) — the same
boundaries, a firing 30 s after resume and 30 s after boot, a laptop that
slept last night and runs anyway, a server that never sleeps running on
three consecutive days without a hold, a laptop whose manager forgot its
resume (the kernel counts a suspend, the manager reads 0) running on three
consecutive days with exactly one `NOTE` line each and never a hold, an
unreachable system manager, an unparseable resume and a resume dated after
the activation each running on the interval rule, an unreachable user
manager, a manager that does not show the unit activating, an unreadable
kernel counter and every usage error, each asserted to hold with exit 255
and to leave the stamp alone; a death the guard did not decide (a script
variable made readonly through `BASH_ENV` before it starts) held with 255
rather than exiting with `set -e`'s 1; a hand-edited stamp with a leading
zero, padding or a CRLF read as decimal, an empty one held, a stamp up to
one interval (23 h) ahead of the clock discarded and a second more held —
plus the
installer's actions: guard installed only with the flag, staged as `.new`
and renamed over, before the unit, mode 0755, named on both the
`ExecCondition=` and the `AssertFileIsExecutable=` lines, preserved by a
no-flag re-run, a pre-#411 unit reported `UNGUARDED` and retrofitted only
by the flag, not re-created after removal, and the opt-in refused on a
running systemd 242 or a bare `219` and accepted on 243. What no test
observes, and what was observed by hand instead, is recorded once, in
`docs/AGENT-DISTRIBUTION.md` §4.

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

**When `rollback` refuses the bind** (a TLS host bound to an IPv6 zone id or to
a name that does not resolve — see the certificate section above), the
command changes nothing, and on macOS it is the only documented path. The
escape is a manual swap that **bypasses the health verification entirely** —
nothing checks that the restored binary came back — and, unlike `rollback`, is
not reversible (the displaced binary is overwritten, not kept as `.prev`):

```bash
# Linux
systemctl --user stop solador-agent
mv -f ~/.local/bin/solador-agent.prev ~/.local/bin/solador-agent
systemctl --user start solador-agent

# macOS
launchctl bootout gui/$(id -u)/app.solador.agent
mv -f ~/.local/bin/solador-agent.prev ~/.local/bin/solador-agent
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/app.solador.agent.plist
```

Then fix the bind (a non-link-local address, or TLS off) before the next
`update`.

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

- Solador reaches the host at `<the configured bind address>:7878` —
  the Tailscale IP by default, all interfaces on a TLS host without Tailscale,
  or whatever `SOLADOR_AGENT_BIND` was set to — over `http://` when `SOLADOR_AGENT_TLS` is unset, or
  `https://` when it is `1`. The cockpit dials `https://` only for a host the
  operator **paired** — it fetched the certificate, showed the fingerprint and
  the operator pressed Trust — and then accepts exactly that certificate; see
  **TLS** above and
  [#448](https://github.com/Sassy-Dog/solador/issues/448). A host that was
  never paired is dialled over `http://` — **only when its address is loopback
  or Tailscale (#449)**; anywhere else the cockpit sends nothing (no request,
  no token) and the host reads "unpaired, off-tailnet — pair it in Settings"
  until it is paired. An agent with TLS on but no pairing, on the tailnet, still
  reads as unreachable until it is paired.
- It sends `Authorization: Bearer <token>` (the same token from the env file) on
  every request, polling `/v1/snapshot` and `/v1/containers`.
- The agent binds only that address (`SOLADOR_AGENT_BIND`). With Tailscale that
  is the tailnet IP, so the port is not served on the public NIC. **On a TLS
  host with no Tailscale the default is all interfaces (`0.0.0.0`) (#449)** —
  the port *is* reachable on every network the host is on, so firewall it or
  pin `SOLADOR_AGENT_BIND` (**Network exposure**). Verify what is bound with
  `ss -tlnp | grep 7878` (Linux) or `lsof -nP -iTCP:7878 -sTCP:LISTEN` (macOS):
  it shows the configured address, or `0.0.0.0` / `*` for all interfaces.
