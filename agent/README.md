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
  "cpu": { "totalUsage": 37.5, "coreUsages": [40.0, 35.0], "model": "Apple M1 Max", "thermalState": 0 },
  "memory": { "usedGB": 12.3, "totalGB": 32.0, "swapUsedGB": 0.5, "pressure": 0.0 },
  "disk": { "readMBps": 1.2, "writeMBps": 0.3 },
  "network": { "downloadMBps": 0.2, "uploadMBps": 0.1 },
  "gpu": { "usage": 0.0, "vramUsedGB": 0.0, "vramTotalGB": 0.0 },
  "battery": null
}
```

Notes:
- `timestamp` is RFC3339 / ISO-8601 UTC (`...Z`).
- Disk and network values are **rates** (MiB/s), computed from a delta between two
  ~1s `sysinfo` refreshes by a background sampler. `/v1/snapshot` returns the
  latest sampled values.
- Percentages (`totalUsage`, `coreUsages`) are `0–100`.
- `thermalState` is `0` (nominal). `gpu` is all zeros. `battery` is `null` on
  servers. These are intentionally defaulted; the keys are never omitted (except
  `battery`, which is JSON `null`).
- Memory has **no** `usagePercentage` key — the Swift side computes it.
- `volumes` entries are `{ "mount": "/", "usedGB": 10.0, "totalGB": 100.0, "fstype": "ext4" }`.
  `fstype` is lowercased and omitted (not `null`) when unknown. Transient, remote,
  and pseudo filesystems are filtered out at the source (see `DEVCANOPY_AGENT_SKIP_FSTYPES`),
  and bind mounts of the same filesystem collapse to the shortest mount path.

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
| `DEVCANOPY_AGENT_PORT`   | no       | `7878`  | TCP port. Binds `0.0.0.0:<port>`. |
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

DEVCANOPY_AGENT_TOKEN=secret cargo run
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
3. Writes `~/.config/devcanopy-agent.env` with the token (prompts; press Enter to
   auto-generate) and port, mode `600`.
4. Installs the **user** unit `~/.config/systemd/user/devcanopy-agent.service`,
   then `systemctl --user enable --now devcanopy-agent` and enables lingering so
   it starts on boot and survives logout.

It runs as **your user** (not root) so rootless `podman ps` works.

Manage it:

```bash
systemctl --user status devcanopy-agent
systemctl --user restart devcanopy-agent
journalctl --user -u devcanopy-agent -f
```

To rotate the token: edit `~/.config/devcanopy-agent.env`, then
`systemctl --user restart devcanopy-agent`.

## How DevCanopy connects

- DevCanopy reaches the host over Tailscale at `http://<tailscale-host>:7878`.
- It sends `Authorization: Bearer <token>` (the same token from the env file) on
  every request, polling `/v1/snapshot` and `/v1/containers`.
- Keep the port off the public internet; expose it only on the tailnet.
