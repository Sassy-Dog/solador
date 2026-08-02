# DevCanopy Agent

A small per-host metrics agent. It exposes host metrics and a container/VM list
over HTTP (JSON), guarded by a bearer token. The [DevCanopy](../) macOS app polls
it over **Tailscale** to render a dashboard.

Runs on Linux (e.g. `ubu-3xdv`) and macOS. Metrics come from
[`sysinfo`](https://crates.io/crates/sysinfo); the server is
[`axum`](https://crates.io/crates/axum) on `tokio`.

## Endpoints

All endpoints require `Authorization: Bearer <token>`. Missing or wrong token → `401`.

| Method & path     | Returns                                                            |
|-------------------|-------------------------------------------------------------------|
| `GET /v1/snapshot`  | Host metrics snapshot (CPU / memory / disk / network / gpu / battery). |
| `GET /v1/containers`| Array of containers/VMs from podman, docker, and tart.            |
| `GET /v1/health`    | `{ "status": "ok", "hostname": "...", "version": "..." }`         |

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
- Memory has **no** `usagePercentage` key — the Swift side computes it.
- `volumes` entries are `{ "mount": "/", "usedGB": 10.0, "totalGB": 100.0, "fstype": "ext4" }`.
  `fstype` is lowercased and omitted (not `null`) when unknown. Transient, remote,
  and pseudo filesystems are filtered out at the source (see `DEVCANOPY_AGENT_SKIP_FSTYPES`),
  and bind mounts of the same filesystem collapse to the shortest mount path.
- `processes` is the union of the top 5 by CPU and the top 5 by memory, so it runs
  5–10 entries long. Every entry is a **process** (agent ≥ 0.3.1): on Linux
  `sysinfo` hands back a *task* table, and both threads and kernel threads are
  filtered out of it here.
  - `cpuPercent` is the whole process's — the kernel already reports
    thread-group-wide times in `/proc/<pid>/stat`, so nothing is summed on top —
    and `memoryMB` is its RSS, listed once.
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
| `DEVCANOPY_AGENT_TOKEN`  | yes      | —       | Bearer token. Server refuses to start if unset/empty. |
| `DEVCANOPY_AGENT_BIND`   | no       | tailnet IP | Host/interface to bind. Defaults to the detected Tailscale IP (`100.x`), so the agent only listens on the tailnet. Set to `0.0.0.0` (or `::`) to bind all interfaces — opt-in only, behind a firewall. If unset and no Tailscale IP can be detected, the server refuses to start rather than exposing the public NIC. |
| `DEVCANOPY_AGENT_PORT`   | no       | `7878`  | TCP port. Bound on `DEVCANOPY_AGENT_BIND`. |
| `DEVCANOPY_AGENT_SKIP_FSTYPES` | no | see below | Comma-separated fstypes excluded from `volumes`. Setting it **replaces** the default list; an empty value disables filtering. |
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

## Build & run (local)

```bash
cargo build              # debug
cargo test               # unit + contract tests
cargo build --release    # optimized binary at target/release/devcanopy-agent

# Locally there's usually no tailnet IP, so bind loopback explicitly:
DEVCANOPY_AGENT_TOKEN=secret DEVCANOPY_AGENT_BIND=127.0.0.1 cargo run
# in another shell:
curl -s -H "Authorization: Bearer secret" localhost:7878/v1/snapshot | jq
curl -s localhost:7878/v1/snapshot          # -> 401
```

## Install on a Linux host (systemd user service)

From the crate directory on the target host (e.g. `ubu-3xdv`):

```bash
./deploy/install.sh
```

The script:
1. Builds `--release`.
2. Installs the binary to `/opt/devcanopy-agent/` (falls back to `~/.local/bin`).
3. Writes `~/.config/devcanopy-agent.env` with the token (prompted **without
   echo**; press Enter to auto-generate), the detected Tailscale bind address,
   and the port, mode `600`. The full token is never printed to stdout — the
   script reports only the env-file path and the token's last 4 characters.
4. Installs the **user** unit `~/.config/systemd/user/devcanopy-agent.service`,
   then `systemctl --user enable --now devcanopy-agent` and enables lingering so
   it starts on boot and survives logout.
5. **Verifies** by polling `/v1/health` (at the bind/port it just wrote, so this
   works on a tailnet-only agent) until it reports the `[package]` version from
   `Cargo.toml`. A healthy unit only proves *a* binary is up — if the version
   being served isn't the one just built, the script fails loudly naming both
   numbers rather than reporting a successful install over stale code.

It runs as **your user** (not root) so rootless `podman ps` works.

Manage it:

```bash
systemctl --user status devcanopy-agent
systemctl --user restart devcanopy-agent
journalctl --user -u devcanopy-agent -f
```

To rotate the token: edit `~/.config/devcanopy-agent.env`, then
`systemctl --user restart devcanopy-agent`.

## Redeploy an existing host (upgrade in place)

Use this once a host is already installed and you want to ship a new agent build.
It is the unattended counterpart to `install.sh`: it **never prompts for a token**
and only swaps the binary.

From the crate directory on the target host (e.g. `ubu-3xdv`, on a fresh checkout
of the new commit):

```bash
./deploy/redeploy.sh
```

What it does:
1. Reads the target version from `Cargo.toml` and builds `--release`.
2. Preserves the currently-installed binary as `devcanopy-agent.prev` (the
   rollback anchor).
3. **Atomically swaps** the new binary into place. The running binary can't be
   overwritten in place — Linux returns `ETXTBSY` ("Text file busy") — so the
   script stages the build to `devcanopy-agent.new` and `mv`s it over the live
   path. A rename over a running executable is allowed even when an in-place
   write is not.
4. Restarts the user service.
5. **Verifies** by polling `/v1/health` (using the token/bind/port from the env
   file) until it reports the version from `Cargo.toml`. If the new binary never
   reports the expected version, the script fails loudly and tells you to roll
   back — the bad binary is live but you have a one-command escape hatch.

It requires an existing `~/.config/devcanopy-agent.env` and systemd user unit; if
the host has never been installed it errors and points you at `install.sh`. It
honors the same `/opt` → `~/.local/bin` install layout (resolved from the unit's
`ExecStart`) and uses `sudo` only if the install directory isn't user-writable.

## Roll back a bad redeploy

If a redeploy ships a broken build (or `redeploy.sh` reports the health check
failed), restore the previous binary with one command:

```bash
./deploy/redeploy.sh rollback
```

It atomically swaps `devcanopy-agent.prev` back into place, restarts the service,
and verifies the agent comes back online via `/v1/health`. The swap is
reversible: the binary you rolled back over becomes the new `.prev`, so re-running
`rollback` rolls forward again. (The agent binary has no `--version` flag, so
rollback verifies the service is reachable rather than asserting an exact
version.)

No cargo or rebuild is needed to roll back — that's the point of keeping the prior
binary on the host.

## How DevCanopy connects

- DevCanopy reaches the host over Tailscale at `http://<tailscale-host>:7878`.
- It sends `Authorization: Bearer <token>` (the same token from the env file) on
  every request, polling `/v1/snapshot` and `/v1/containers`.
- The agent binds the tailnet interface by default (`DEVCANOPY_AGENT_BIND`), so
  the port is not served on the public NIC. Verify with
  `ss -tlnp | grep 7878` — it should show only the `100.x` address. Binding all
  interfaces (`0.0.0.0`) is opt-in and should only be done behind a firewall.
