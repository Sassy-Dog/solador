# Working on the Agent

> Moved verbatim from `AGENTS.md` (#638), which is held under the org's 32 KiB guidance budget. Edit it here; `AGENTS.md` points at this file.

- Rust source in `agent/src/` (`server.rs`, `metrics.rs`, `containers.rs`).
- Deploy via `agent/deploy`; see `agent/README.md` for endpoints and rollout.
- **`install.sh` downloads and verifies; `redeploy.sh` builds (#392).**
  `agent/deploy/install.sh` needs no Rust: it maps `uname` onto one of the
  four published triples and finds the release through the **signed feed**
  (#490): `agent-latest.json` + `.minisig` from the permanent `agent-latest`
  release, verified with the stock `minisign` under the checkout's
  `agent/release-signing-key.pub` **before** its `version` is read (a tampered
  feed is rejected unread, and a `version` that is not a strict CalVer is
  refused), then the binary from `agent-v<version>`. `/releases/latest` names the
  cockpit's train and is never consulted. `SOLADOR_AGENT_RELEASE` pins a tag
  instead — an `agent-v*` tag, or a legacy `v*` tag that carries agent assets
  (the last combined release is `LAST_COMBINED_AGENT_RELEASE`) — and skips the
  feed. Either way it downloads the raw binary plus `.minisig` into private
  staging under `~/.cache`, verifies it under that same key,
  and only then makes it executable, reads `--version`, and installs
  **user-owned at `~/.local/bin/solador-agent`** (staged `.new`, renamed over
  the live path, `.prev` kept — no `sudo` anywhere). A rejected signature runs
  nothing, installs nothing, stops nothing. A signed feed is not a freshness
  guarantee, so an unpinned re-run refuses to move an installed *release*
  backwards (an installed `+dev` source build is deliberately exempt: it is not
  a CalVer, and this script is how such a host gets onto a release). A release
  without agent assets (`v2026.9.3` and earlier, or an
  `agent-v` release that lacks this host's) is a **failure naming the tag**,
  never a source build, and so is a missing feed (nothing published through
  `agent-latest` yet: pin one). The Linux unit is a template rendered with the actual path; macOS is a
  **LaunchAgent** (`~/Library/LaunchAgents/app.solador.agent.plist`, label
  `app.solador.agent`, `gui/<uid>`) whose `ProgramArguments` is a launcher
  (`~/.local/bin/solador-agent-launchd`, from `deploy/run-agent.sh`) that reads
  the same 0600 env file line by line — never `source`d — so the token is in
  neither plist nor log; the plist sets `PATH` because launchd's compiled-in
  one has no `docker`/`tart`/`podman`, and an agent that cannot find a runtime
  serves `/v1/containers` as `[]` under a green `/v1/health`. An existing unit pointing at `/opt` stops the install
  with the explicit `--migrate-from-opt` step rather than migrating silently.
  `redeploy.sh` is untouched, Linux-only, and now `build_release_binary`'s
  only caller. Service identities here are the restart contract
  `solador-agent update` consumes.
- **`install.sh --system-daemon` is the split install for a non-login service
  user, macOS only (#506, part of #505).** The service user stages everything
  it owns exactly as a fresh per-user install does, renders
  `agent/deploy/app.solador.agent.daemon.plist` (the LaunchAgent template plus
  `UserName`/`GroupName` and `HOME`/`SOLADOR_AGENT_CONFIG_DIR`, no
  `SessionCreate`, same label, same `ProgramArguments` shape `update.rs`
  parses) into its own home, bootstraps **nothing**, and **prints** the one root
  step (`sudo install -o root -g wheel -m 644 <rendered>
  /Library/LaunchDaemons/app.solador.agent.plist && sudo launchctl bootstrap
  system ...`) — the script and its tests never run `sudo` or anything
  privileged, and `--system-daemon` refuses root (as `--uninstall` and
  `--enable-timer` do). Nothing is running until
  root loads it, so the `/v1/health` check and the TLS fingerprint belong to the
  follow-up `--system-daemon --verify` run. Refused: root, a non-macOS platform,
  and an existing per-user LaunchAgent install for the same user.
  `solador-agent update`/`rollback`
  support this mode (#507): run **as the service user** (`sudo -u <svc> -H
  ~<svc>/.local/bin/solador-agent update`), never as root. Adding
  `--enable-timer` stages `app.solador.agent.update.daemon.plist` as a second,
  daily LaunchDaemon under the same service user and prints its own root
  copy/load step. It uses the existing launcher guards, system-only PATH and
  `StartInterval=86400`, with no `RunAtLoad`, `KeepAlive` or catch-up. No-flag
  re-runs preserve the updater; uninstall removes its staged plist and prints
  the updater's root unload before the metrics daemon's. Scheduling is not
  inferred from staging. See the `update`/`rollback` bullet below for the
  mechanism. Exit 0 means
  *staged* on install, not serving.
  `--uninstall --system-daemon` removes the user files and prints the root
  `bootout`/remove step. `agent/README.md`'s "Running as a system daemon under a
  service user" is the procedure; `lib_test.sh` records `launchctl` and `sudo`
  argv so "bootstraps nothing" is asserted, each refusal with a negative
  control.
- **`agent/deploy/bootstrap.sh` is the checkout-free path onto `install.sh`
  (#434).** No `git clone` needed: it downloads the repository archive at a
  `main` commit (or a `--ref` confirmed, via GitHub's compare API, reachable
  from `main` — never an unmerged commit) from `codeload.github.com`,
  extracts only `agent/deploy/*` and the signing key(s), and runs the
  extracted `install.sh` unchanged. See `agent/README.md`'s Prerequisites
  and `docs/AGENT-DISTRIBUTION.md` §6 for why an archive of a commit
  `main`'s history contains keeps the same key-provenance property a
  checkout does.
- **`install.sh --uninstall` is the mirror of the install, for the invoking
  user only (#439).** Its refusals run in this order, each untouched: root
  first (same reason `--enable-timer` refuses it); then an unsupported
  platform (this script supports Linux/systemd and macOS/launchd, named as
  such); then the service manager being unreachable — the same `systemctl
  --user show-environment` / `gui/<uid>` domain check the install path
  makes — then the update transaction lock (`<bin>.update.lock`), whose
  whole section is skipped outright when `~/.local/bin` (`$DEST_BIN`'s own
  directory) does not exist at all, since nothing can possibly be installed
  under a directory that is not there. Where it does exist: this OPENS the
  lock file (`exec 9>>"$lock_file"`, creating it if missing and never
  truncating one that exists — the same `create(true).truncate(false)`
  `TransactionLock` opens it with) and takes a non-blocking exclusive flock
  on it — `flock -n 9` where `flock(1)` is on PATH, else the stock `perl`'s
  Fcntl flock on that SAME already-open fd (`open($fh,"<&=",9)`, confirmed
  empirically to persist after perl exits, not merely assumed) — and HOLDS
  it from before the first service-manager call until the binary and the
  lock file itself are removed, on both platforms, in every tier that can
  check it at all: a transaction starting in the window this spends
  stopping the service and removing its unit/plist meets that hold as busy
  on its own terms (exit 75). The hold is released (`exec 9>&-`) before the
  update stamp and, with `--purge`, the env-file, legacy-env and TLS
  removals, and the lock file is unlinked while still held — the hazard
  `agent/src/update.rs` documents for its own lock. Nothing runs between the unlink and the release, and on a clean run the unit/plist and the binary
  are already gone: a transaction
  starting afterwards has no install to resolve and nothing to swap. Where
  NEITHER tool exists there is no way left to check, and this refuses
  (busy) before anything changes rather than guessing free. **Whether THIS
  run created the lock file is decided atomically**, `( set -C; : >
  "$lock_file" )` before `exec 9>>` ever opens it: deleting a file that
  PRE-EXISTED on a "lock is busy" refusal is how a second process can end
  up "holding" a lock nobody actually contends for — `flock()` locks the
  open file description, not the path, so unlinking a file another process
  has locked and letting a third opener recreate the same name gives that
  third opener a lock on a *different* inode. So a busy result never
  deletes the lock file, whoever created it. Of the two
  narrower non-busy failures, only the open itself failing deletes a lock
  this run created; a perl reopen failure never deletes one, this run's or
  not — it proves nothing about whether the lock is free. Once held, a note
  (`pid=<pid> since=<epoch>`, the same shape `agent/src/update.rs` writes)
  is left in the file so a racing update/rollback names THIS uninstall, not
  a stale holder. **Stop and disable are two separate `systemctl` calls, never a
  combined `disable --now`**: real
  systemd's `disable` needs a unit's own FILE to know which enablement
  symlinks to remove, and fails "Unit file <u> does not exist" without one
  — *before* it ever reaches its own `--now` stop — so a combined call on
  a unit whose FILE is already gone never stops anything, it just fails
  outright. `disable` runs only while the unit's own FILE exists; `stop`
  runs when the FILE exists OR the user manager still holds the unit (#463),
  so a re-run after an exit 4 — whose first run already deleted the files —
  still stops what the manager is running instead of reporting "Nothing
  installed". Always against the unit's FULL name (`solador-agent.service`,
  never bare `solador-agent`: `list-units` does not append `.service`).
  "Holds" is `systemctl --user show -p LoadState -p ActiveState <unit>`
  reading `LoadState != not-found` OR `ActiveState != inactive`: a fileless
  unit something merely references (a dangling `*.wants` link, the update
  oneshot's own `After=solador-agent.service`) reads `not-found` +
  `inactive`, `stop` on it fails "not loaded" (exit 5), and counting it would
  be a false exit 4 forever — while a fileless oneshot left `failed` is
  held and stops fine. A state read that itself fails is exit 4, never a
  silent "Nothing installed" (#475: the summary says the state could not be
  read, that no stop or disable request was made, and claims removed files
  only when `UNINSTALL_REMOVED`). A failed `stop`, OR a failed `disable` while
  the file exists, both mean exit 4, reported as two DIFFERENT claims: a failed `stop` says the
  process may still be running; a failed `disable` with a successful
  `stop` says only that the unit's future auto-start is unconfirmed, and
  must never claim the process may still be running.

  On macOS, which asks `launchctl print` directly with no file gate, the
  stop is `launchctl bootout gui/<uid>/…`. Before either unit/plist is removed, the binary path it
  currently names is read (`unowned_service_binary`) — one outside
  `~/.local/bin` (an unmigrated `/opt` host, most likely) is reported as
  left behind with the actual remedy (`sudo rm -rf /opt/solador-agent` for
  that layout; "remove it as its owner" otherwise), never a
  `--migrate-from-opt` pointer, which does nothing useful post-uninstall.
  It then removes both unit/plist pairs (Linux `daemon-reload`s and
  `reset-failed`s all four unit names once something changed), the binary
  and its `.prev`/`.new`/`.update.lock`/`.rollback-displaced` siblings, the
  macOS launcher, the Linux guard, and the update stamp — a lock file THIS
  run had to create just to check for contention does not itself count as
  "something removed". The env file is **kept** and named in the output
  unless `--purge` also runs (it deletes the token, and the pre-rename
  `devcanopy-agent.env` beside it — install copied its token out of that
  file and never deleted it); `--purge` alone, or `--uninstall` beside
  `--migrate-from-opt` or `--enable-timer`, is a usage error. Never runs
  `loginctl disable-linger`, and is idempotent — a second run finds nothing
  left and says so. **Exit status:** `agent/deploy/install.sh`'s header is
  the one table of `--uninstall`'s exit codes (`--help` prints it); a run
  that changed something is not always 0 or 1, so read it there.
  `agent/README.md`'s "Moving the agent to another user" is the ordered
  procedure this exists for: install as the new user, re-pair the token,
  `--uninstall --purge` as the old one.
- **`SOLADOR_AGENT_TLS=1` serves HTTPS with a self-signed certificate kept
  for the host's lifetime (#447, part 1 of #445).** `agent/src/tls.rs`
  generates it ONCE — ECDSA P-256 via `rcgen`, `solador-agent.tls.key`
  (0600) and `solador-agent.tls.crt`, beside the env file and namespaced the
  same way `solador-agent.env` itself is (not bare `tls.key`/`tls.crt` in an
  XDG root every app shares) — and only the code that is
  about to *serve* it (the agent's own startup) may generate: everything
  else, `tls-fingerprint` included, is read-only, which is what keeps a
  `tls-fingerprint` run from racing the service over which bind host lands
  in the certificate's SAN list. The SAN list is the loopback baseline plus
  the resolved bind host, fixed when the certificate is generated — which is
  why the local probes do not depend on it (#449): `install.sh`'s
  post-install check verifies the certificate as the name `localhost` (in every
  certificate's baseline) while `curl --connect-to` dials the bind address, and
  `solador-agent update`'s post-restart probe trusts exactly this certificate
  as its only root. For an IP or a DNS-name bind it dials `localhost` with the
  connection pinned (`resolve_to_addrs`) to the bind: its IP, or the addresses
  the name resolves to (looked up before anything is swapped). A wildcard bind
  is **not** pinned: the probe dials loopback as written (`https://127.0.0.1:P`
  for `0.0.0.0` or an empty bind, `https://[::1]:P` for `::` / `[::]`) and
  verifies against the baseline `127.0.0.1` / `::1` IP SANs.
  Both verify the chain **and** the hostname — verification is never
  disabled, no `danger_accept_invalid_certs` or `danger_accept_invalid_hostnames`
  anywhere — and neither breaks when the bind changes after the certificate
  exists, an IP or a name alike. Both `update` and `rollback` refuse,
  before changing anything (`resolve_dial`), an IPv6 zone id (`fe80::1%en0`)
  **with TLS on or off (#476)** and, under TLS, a DNS name this host cannot
  resolve (5s bound) — each proven through `run_update` and `run_rollback` to
  change nothing (`update_flow.rs`: exit 1, live and `.prev` byte-identical, no
  restart, no release request). The zone id is refused because the probe cannot
  dial it: under TLS its `localhost` URL and pinned address have no place for an
  interface-local zone, and with TLS off the WHATWG URL parser `reqwest` uses has
  no zone-id syntax at all (RFC 6874's `%25` form included — pinned by a unit
  test), so without the refusal `update` would swap, fail the probe and report a
  recovery on a healthy host. That refusal is **the same everywhere**: the agent
  exits with a FATAL at start (`resolve_bind_host`), `install.sh` refuses before
  any download or token prompt, and `update`/`rollback` refuse before any change,
  all with `update::zone_id_refusal`'s core sentence. It is a decision, not a platform
  limit: measured on macOS, the agent *does* serve `fe80::1%lo0` /
  `%en0`, and curl dials the `%25` form, but the unencoded form `health_url`
  emits timed out against `%lo0` (answered on `%en0`), and no URL parser
  accepts it. `lib.sh` and `probe_target` agree on every other bind form, TLS
  off and on, including the bracketed ones (`[fd7a::1]`, `[100.64.0.9]`,
  `[host]`: a bracket pair is stripped and only an IPv6 literal is bracketed
  again); both tables hold the same rows. "Bind the address without the zone"
  does not fix a zone-id bind for a link-local address, which is reachable only
  through its zone: bind a non-link-local address. An operator who must roll back
  meanwhile does it by hand — stop the service, rename `<bin>.prev` over `<bin>`,
  start it — which bypasses the health verification (`agent/README.md`, **Roll
  back**). Axum's TLS comes from
  `axum-server`'s `tls-rustls-no-provider` feature rather than its
  `tls-rustls` one, specifically to avoid pulling in `aws-lc-rs` (a second
  crypto backend, and a `cmake`/C build the musl cross-compile does not
  need): the agent installs `rustls::crypto::ring::default_provider()`
  itself, the same `ring` the workspace already resolves for `reqwest`.
  `install.sh` writes `SOLADOR_AGENT_TLS=1` on a fresh install only, and
  only when the STAGED, verified release actually supports it — decided
  after staging, not before: `tls-fingerprint` exiting 2 (an unrecognized
  argument) on those exact bytes means a release published before #447,
  and a fresh install onto one stays plain HTTP rather than writing a
  setting the agent would silently ignore; `--enable-tls` against such a
  release is refused outright, naming the reason. A re-run otherwise
  respects an existing env file's choice unless given `--enable-tls`, and
  prints the fingerprint (read via `tls-fingerprint`, never parsed from the
  DER itself) in its Done block. `solador-agent.tls.key`/`solador-agent.tls.crt`
  are PEM on disk (DER
  stays the in-memory form `rcgen`/`rustls`/`reqwest` all want) —
  `agent/deploy/lib.sh`'s `verify_health` pins them into curl's `cacert`
  config option, which refuses raw DER outright (exit 77) on every TLS
  backend this repo's `curl`s use; that existence check runs on every
  retry attempt, not once up front, since a fresh install's own
  `systemctl --user restart` can return before the agent has generated the
  file. **The directory is resolved from the env file, never `$HOME`
  alone**: `run-agent.sh` and `solador-agent.service` both export
  `SOLADOR_AGENT_CONFIG_DIR` — the exact directory the env file was
  written into, keyed the same way the env file's own path is — and
  `main.rs`'s `tls_config_dir()` reads it before falling back to
  `$HOME/.config`. `install.sh` never exports the var itself: its own
  `tls-fingerprint` calls resolve through that same `$HOME/.config`
  fallback, which is exactly where `install.sh`'s own process writes the
  env file, so the two agree without it needing to export anything. The
  fallback otherwise matters only for a manual invocation with no launcher
  in front of it; launchd's `HOME` need not be the `HOME`
  `install.sh` ran under. `install.sh` re-runs, `update` and `rollback` never touch the key
  or certificate; `--uninstall` keeps them like the env file, `--uninstall
  --purge` removes them too, since purging the host's credentials means a
  re-pair. **The cockpit half is #448:** a cockpit that has it pairs with this
  certificate (Check certificate → compare the fingerprint → Trust) and pins
  it, and reports a regenerated one as *certificate changed* with a Re-pair
  action. **A cockpit older than #448 cannot dial an HTTPS agent** and reads
  it as unreachable — `install.sh`'s Done block and `agent/README.md`'s TLS
  section say so and name the `SOLADOR_AGENT_TLS=0` fallback. The
  Tailscale-only bind is relaxed for TLS hosts only (#449, below).
  **The bind default follows the transport (#449).** `resolve_bind_host` takes
  `tls_on`: an explicit `SOLADOR_AGENT_BIND` wins; else the detected Tailscale
  IP; else `0.0.0.0` with TLS on, or a refusal with it off (unchanged).
  `install.sh` mirrors it, and because TLS is not final until the staged
  binary has been asked about it, a host with no bind address is refused
  before any download only when TLS is already known to be off, and after
  staging otherwise. **A bind `install.sh` chose for TLS is provisional
  (#449):** it writes `SOLADOR_AGENT_BIND_AUTO=1` beside the `0.0.0.0` it
  picked (the agent ignores the key; the launcher recognises it silently), and
  every re-run sets that bind aside and resolves again — the tailnet IP if one
  is up now, `0.0.0.0` only while TLS is on, otherwise the plain-HTTP refusal —
  so it neither survives TLS being turned off nor pins a host to the wildcard
  after Tailscale arrives. A bind *without* the marker is the operator's
  explicit choice and is honoured, plain HTTP included, with a loud summary.
  Anyone turning TLS off by editing the env file by hand must remove **both**
  `SOLADOR_AGENT_BIND` and `SOLADOR_AGENT_BIND_AUTO` (or set one address), since
  the agent honours a bind it finds there. The wildcard-to-tailnet move is what
  the SAN-independent probes above exist for.
  **`SOLADOR_AGENT_REQUIRE_TAILNET=1` closes the wildcard for a host that must
  never have one (#497, the org's agent management spec §3.3).** `resolve_bind_host`
  takes it as a third input: under it the resolved bind must be a Tailscale IP
  literal — IPv4 `100.64.0.0/10`, or IPv6 `fd7a:115c:a1e0::/48` minus the 4via6
  prefix `fd7a:115c:a1e0:b1a::/64`, restated in `agent/src/main.rs`'s
  `is_tailnet_literal` as a twin of `crates/agentclient`'s plain-HTTP rule
  because `agent/` must not depend on that crate — or the agent exits FATAL.
  Refused under it: any wildcard, any other address, a bracketed form, any DNS
  name (its addresses can change after the check), an explicit
  `SOLADOR_AGENT_BIND` among them, and auto-detection finding nothing **even with
  TLS on** (the `0.0.0.0` fallback above is never taken). An empty bind is unset
  and falls to detection. The sentence is one constant,
  `update::TAILNET_REFUSAL_CORE`, which `install.sh` repeats byte for byte
  (`lib_test.sh` reads it out of the source) the way `zone_id_refusal` is
  shared; `install.sh` refuses before any download or prompt in every case,
  since nothing under it depends on TLS, persists the key as an installer-owned
  env-file key (and in `run-agent.sh`'s allow-list, which `lib_test.sh` holds
  to parity), keeps it on a re-run that does not mention it (any value other than
  `1` set in the installer's environment removes it), and says `Tailnet: only` in the Done block. Unset or `0` is
  exactly the behaviour above. On macOS a launcher installed before #497 drops the
  key and `update` never refreshes the launcher, so the setting is enforced there
  only after `install.sh` is re-run.
- **`solador-agent update` / `rollback` are in the binary (#393), and the
  order of operations is the security design.** `agent/src/update.rs`:
  refuse root; resolve #392's install — reading only — from the unit's
  `ExecStart=` or the plist's `ProgramArguments` and read the env file with
  `EnvironmentFile=` semantics, never `source`d (an install directory this
  user cannot write to is refused with the installer's migration step); take
  the transaction lock beside the resolved binary (`<bin>.update.lock`,
  non-blocking — a competing manual or scheduled run reports *busy*, exit
  75, and changes nothing) and require the service manager to **answer**
  before any download (a manager found unreachable at the restart is the
  half-applied update); fetch the feed from its **fixed location** — `agent-latest.json`
  + `.minisig` from the permanent `agent-latest` release
  (`<base>/releases/download/agent-latest/`, #488), never by asking which
  release is latest, because `/releases/latest` names the desktop train;
  verify the **exact served bytes** under the compiled-in trust set *before*
  decoding; require **every** target's `url` to be exactly
  `<base>/releases/download/agent-v<version>/solador-agent-<version>-<triple>`
  (the fetch location is a fixed name now, so that signed URL path is what
  binds the feed's `version` to its artifacts; a wrong URL on any target is
  refused before any download); hash the installed
  executable — equal to the feed's entry means **no download, no restart,
  even when the version differs**, and exit 0 once `/v1/health` confirms the
  service is serving those bytes' own version (bytes on disk are not a
  running service: an earlier run interrupted between its swap and its
  restart is a distinct, non-zero "current but not serving"); otherwise the
  feed must be **newer** than the installed CalVer (a replayed older feed is
  refused; an installed binary that carries no version, or a non-CalVer one,
  is refused rather than assumed older; one whose version contains `+dev` is
  a source build with nothing to compare, which is exit **4**, asked before
  the CalVer parse that would refuse it with exit 1 — the marker is the one
  #490 gives every build that is not an `agent-v*` release, and the check
  shipped first (#488) because the bridge release's updater (#472's cut-over,
  #492) had to know it before any build carried it); a feed that is not
  there — a 404 on `agent-latest` — is exit 1 with nothing changed, never a
  quiet day, so a deleted `agent-latest` cannot hide; download into memory and verify **signature and
  SHA-256** before a byte reaches disk; stage `<bin>.new` (0755) and execute
  its `--version`, which must be the feed's; copy the live binary to
  `<bin>.prev` through a sibling+rename; `rename()` `.new` over the live
  path (never overwrite in place — Linux `ETXTBSY`, arm64 macOS kills a
  process whose signed pages change); restart the **metrics** service
  (`systemctl --user restart` / `launchctl kickstart -k gui/<uid>/…`, never
  the updater's own process, which refuses if the manager says it *is* the
  service); **the system-domain LaunchDaemon variant (#507, the org's agent
  management spec §5.3) restarts with no `launchctl` mutation at all**:
  `resolve_launchd_with` adopts `/Library/LaunchDaemons/<label>.plist` (the
  shape #506's `app.solador.agent.daemon.plist` renders, read through
  `plutil`) only when its `UserName` is the invoking user (from the password
  database, not `$USER`) and its `ProgramArguments` binary is under that user's `$HOME`
  (not `pw_dir`) with no `..`; a per-user LaunchAgent plus such a system plist is
  **ambiguous** and refused naming both paths, a system plist for another
  user is refused with that reason (unless the invoking user has their own
  LaunchAgent, which then stays the install), and nothing is adopted
  otherwise. `Service::LaunchDaemon` does the manager check with
  `launchctl print system/<label>` (readable without root) before any
  download, takes the main pid from that print's `pid =` line, and
  **restarts by `SIGTERM` to that process after checking, in the preflight and
  again at the signal, that its owner is the invoking uid and its executable is
  the resolved binary** (a pid that is not ours is refused before any download
  or swap, and a signal is never sent to it), then waits, bounded, for a
  *different* pid that `KeepAlive` relaunches (the plist's
  `ThrottleInterval` is 3 s; a daemon with no pid at all is waited for the
  same way). No relaunch in the bound is a restart failure, so the existing
  restore (exit 5, or 3) applies, and `rollback` restarts the same way. Its
  `inspect_hint` is `launchctl print system/<label>` plus
  `ProgramArguments[3]`. It runs as the service user and still refuses root; the
  host side is the `DaemonHost` trait, so `update_flow.rs` drives the real
  mechanism over a fake that mimics `KeepAlive` and nothing in a test touches
  the real system domain or signals a real process; poll the authenticated
  `/v1/health` until it reports the new
  CalVer — an HTTP 200 with a stale or absent `version` is **not** a success;
  on any failure after the swap, restore `.prev` the same way, restart, and
  require the previous version back. **Exit non-zero either way**: **5**
  when recovery worked, **3** when recovery also failed (naming what is at
  the live path now) — never a "rolled back" claim over a service that is
  not back; **4** is "no applicable release" (not newer, or an installed
  `+dev` source build), which a from-source host answers every day and the
  scheduled job (#394, next bullet) does not alert on;
  1 is every refusal with nothing changed. `rollback` is the offline form:
  refuses with no `.prev` and touches nothing; otherwise swaps live and
  `.prev` so it is itself reversible, and verifies the restored version where
  it can name one, liveness where it cannot (a source build). The trust set
  is `build.rs`'s `TRUSTED_PUBLIC_KEYS`: `agent/release-signing-key.pub`
  (missing = build failure) plus `agent/release-signing-key-next.pub` when
  present; one key listed twice is refused, and the feed's unknown JSON keys
  are tolerated so a consumer can update past the release that adds one.
  `tests/update_flow.rs` drives all of it against a loopback release in the
  `agent-latest` layout (the feed on `agent-latest`, the binary on
  `agent-v<version>`; a request to anything else, `/releases/latest` above
  all, is logged and the request list asserted exactly), a
  temporary install tree and a fake manager that mimics a real restart, with
  keys minted in-test; `SOLADOR_DEPLOY_TEST_LAUNCHD=1` opts into the same on
  a throwaway real LaunchAgent (`SOLADOR_AGENT_SMOKE_NEWER_BINARY` adds the
  success path, `SOLADOR_AGENT_SMOKE_REAL_FEED=1` a read-only run against
  github.com through the CLI — which is a 404, exit 1, and its assertions
  fail, until the first `agent-v` release has published `agent-latest`). A
  build of this checkout is a `+dev` source build, which `update` does not
  compare, so unpinned the smoke runs only that read-only real-feed part and
  says it skipped the rest; the whole transaction needs a release-like version
  baked in at build time (`AGENT_MARKETING_VERSION=2098.1.1`, above today's
  CalVer and below the 2099.1.1 it offers).
  `update.rs`'s fixture tests and `crates/updatefeed`'s read the SAME directory,
  `tests/fixtures/agent-v/`, which the **producer** wrote
  (`solador-agent-feed build --tag agent-v2026.9.9`; `tests/fixtures/README.md`
  has the procedure), so the producer's URL shape and the consumer's
  `check_feed_urls` are tied by one file rather than by two copies. The negative control — bypass `Trust::verify`
  and watch nine tests go red — is recorded on the PR that shipped it, and
  the same proven-to-fail rule binds any change here. Every failure that
  leaves a service to look at ends with the manager's status command and the
  log path (`ServiceControl::inspect_hint`, the plist's `ProgramArguments[3]`
  on macOS). The maintenance command reads `HOME`, the proxy variables for
  the github.com client (the health probe ignores proxies), and
  `SOLADOR_AGENT_LAUNCHD_LABEL` — the one `SOLADOR_AGENT_*` it reads that the
  metrics service does not, which `lib_test.sh`'s launcher allow-list test
  names as the exception. The lock serialises `update`/`rollback` (and the
  scheduled job, which is `update`) against each other — and against
  `install.sh --uninstall` too, which holds the same lock for its own run
  (released before its last removals). A
  normal, no-flag `install.sh` and `redeploy.sh` still write the same
  `.new`/`.prev` without taking it, so do not run those during an update.
  **`update`'s own children cannot pin the lock** (#458): it is opened
  `O_CLOEXEC`, so the `--version` probe and the service manager's restart lose
  it at exec, and `update_flow.rs` audits exactly that (descriptors 0-1023 the
  candidate is handed, after a positive control). **A test that needs the lock
  held must not hold it through a `File` in the test binary's own process**:
  tests run on parallel threads and each spawns children, so any descriptor on
  the open file description can be inherited across another test's fork and
  outlive the `drop`. `a_competing_transaction…` holds it from a real `perl`
  child; the stale-own-lock unit test hands one deliberate `try_clone` to one
  child as its stdin rather than a `CLOEXEC`-less `dup`.
- **Unattended updating is opt-in, off by default, daily, and never catches
  up (#394).** `install.sh --enable-timer` — and only that flag — installs a
  job **separate from the metrics service** that runs the installed
  `solador-agent update`: a systemd user timer + oneshot on Linux
  (`agent/deploy/solador-agent-update.{timer,service}`, the oneshot's
  `ExecStart` rendered like the metrics unit's plus `update`, no
  `EnvironmentFile=`, `SuccessExitStatus=4`), a second LaunchAgent on macOS
  (`agent/deploy/app.solador.agent.update.plist`, label `<metrics
  label>.update`, `StartInterval=86400`, no `RunAtLoad`, `ProgramArguments`
  = the same launcher plus the literal `update`), or with `--system-daemon`
  a second LaunchDaemon with the same cadence and service-user identity.
  Separate because `update`
  restarts the metrics service and then verifies and restores it; inside
  that service it would kill its own verifier. A default install creates
  **nothing** and makes no check; a re-run without the flag leaves an
  earlier opt-in exactly as it is (files, enablement, phase) and says so —
  revocation is the documented disable/remove commands
  (`agent/README.md`, **Unattended updates**), or `install.sh --uninstall`,
  which removes it along with everything else (#439); daemon mode additionally
  needs `--system-daemon` and root's printed unload steps. The job is created only
  *after* the metrics install verified for per-user installs; daemon mode
  stages both jobs for root to load and the operator to verify. Opt-in is refused before anything is
  created as root, on an unwritable install directory or binary, and on an
  unmigrated `/opt` host (`--migrate-from-opt --enable-timer` does both).
  **Cadence is the recorded decision, daily with no catch-up**, and each
  platform holds it differently: on Linux the timer is **monotonic**
  (`OnActiveSec=24h` + `OnUnitActiveSec=24h`; the clock pauses through
  suspend, so in ordinary operation a wake finds no elapsed deadline —
  never `OnCalendar`, `Persistent` or `WakeSystem`) **and the oneshot's
  `ExecCondition=` is the guard (#411)**: `agent/deploy/update-guard.sh`,
  installed as `~/.local/bin/solador-agent-update-guard` and run with `%n`
  before every `ExecStart`, because a source trace of `timer.c` found that
  a `daemon-reload` after the first day re-arms `OnActiveSec` and the next
  resume fires it once at wake. It applies the launcher's two rules — skip
  within 5 min of the last resume (this activation's
  `InactiveExitTimestampMonotonic` from the user manager against
  `sleep.target`'s `InactiveEnterTimestampMonotonic` from the system
  manager, both unprivileged) or within 23 h of a stamp of the
  launcher's format at the launcher's path (written 0600 here) — with
  `ExecCondition=`'s own mapping: **1 skips cleanly
  (`Result=exec-condition`), 255 fails the unit** (the hold; a permanent
  one is in `--failed`, never a quiet day), and never a
  `SuccessExitStatus=` value, which a condition exit would *run* on.
  **The resume reading is advisory, never a hold** — `sleep.target` is
  `StopWhenUnneeded=` with no other referrer on stock distributions, so the
  system manager collects it after every cycle and a later `show` reads
  `0` (measured on systemd 256: reached and stopped at monotonic
  28446.57 s by the journal, `0` from `show` in the same second). The
  kernel's `/sys/power/suspend_stats/success` tells that `0` apart from a
  boot that has not slept; when the kernel counted a suspend the manager
  no longer has, the guard logs one line and the 23 h rule alone decides.
  Holding on that reading would, on a laptop after its first suspend,
  leave a red unit every day until reboot — not fail-closed, a job that
  never runs. Holds are the launcher's own —
  the clock, this activation's time, the stamp — plus the kernel counter
  when present but unreadable, every usage error, and an `EXIT` trap that
  turns a `set -e` death into a HELD 255, because the status such a death
  carries is `1`, the discard, and would read as a quiet day forever. A
  missing guard binary is exec failure 203, *inside* the skip range, so
  the unit also carries `AssertFileIsExecutable=` on it (an error line and
  a failed `start`; assertions change no unit state), the installer writes
  the guard before the unit, and the opt-in is refused when the *running*
  manager is < 243, where the key would be ignored. On macOS
  **the launcher is the guard**: in update mode
  `run-agent.sh` exports nothing from the env file (the updater reads it
  itself) and refuses, exit 0 with a logged reason, a firing within 5 min
  of the last wake or boot (`kern.waketime` / `kern.boottime`) or within
  23 h of the last attempt (`~/.config/solador-agent-update.last-attempt`,
  stamped *before* the attempt so a failure is not retried until tomorrow),
  and **holds with exit 6** — a code the agent never uses, so a permanent
  hold cannot read as a good day — when a clock or the stamp is unreadable
  or the stamp unwritable. `launchd.plist(5)` says a `StartInterval` firing
  that falls during sleep is missed and `StartCalendarInterval` is the key
  that coalesces missed firings at wake; the guard is what makes the
  property true either way. The fourth launcher argument is a one-word
  allow-list; three arguments is the metrics path, which reads no clock and
  writes no stamp. The updater plist pins `HOME` (the updater resolves the
  metrics plist under *its* HOME, which launchd need not set to the
  installer's), names the metrics label in `SOLADOR_AGENT_LAUNCHD_LABEL`,
  and carries the four system directories as `PATH` and nothing else (no
  group-writable `/opt/homebrew/bin` in front of a process that reads the
  token and renames a binary over the service). The installer exits **3**,
  not 1, when the metrics install verified but the opt-in failed, and
  prints the Done block first; its summary line asks the manager
  (`is-enabled` / `launchctl print`) rather than checking file presence, so
  a paused job is never reported as scheduled. `lib_test.sh` observes all
  of it as installer *actions* on both platforms ("no update check" is
  asserted on the fixture agent's recorded argv), drives both guards with
  stubbed inputs (clock/wake/boot on macOS; the two manager reads, the
  clock and the kernel counter under an override root on Linux), and the
  opt-in launchd smoke fires a real throwaway updater by hand (read-only:
  exit 4, or 1 with no network or — until the first `agent-v` release has
  published `agent-latest` — on its 404; metrics pid unchanged) and watches the
  guard discard the second firing. **Not observed anywhere**: a day of
  sleep on a real Mac, or a real systemd timer taken through a
  `daemon-reload` and a suspend/resume — `docs/AGENT-DISTRIBUTION.md` §4
  records that as the observation still owed; the guard holds the cadence
  either way.
- **The standby key is provisioned by `scripts/agent-standby-key.sh`, never
  by hand (#393 §A).** Non-printing, idempotent, refuses every half-state; the
  private half goes to Doppler `solador/custody` (a config with **no** sync)
  on stdin, the public half to `agent/release-signing-key-next.pub`, and
  custody is proven with the value *retrieved back* from Doppler under the
  stock `minisign`. `docs/SECRETS.md` has the topology and the procedure;
  `lib_test.sh` runs the script against a file-backed `doppler` stub with the
  real `minisign` behind an `rsign` stub.
- **The version comes out of the artifact, not a manifest (#390).**
  `agent/deploy/lib.sh`'s `binary_version` runs `<bin> --version` and **fails
  closed**: a binary that cannot name itself exits non-zero and prints nothing,
  and both deploy scripts abort there. Returning an empty string instead would
  let `verify_health` fall through to its rollback form — "just come back
  online" — and report a successful deploy that never proved which binary is
  serving. `crate_version()` is gone; it would now assert the wire-contract
  number against `/v1/health`.
- **`agent/deploy/` is gated too, as of #269**: `agent/deploy/lib_test.sh`
  (dependency-free bash; stubs cargo/curl/sleep and, since #392, every host
  command `install.sh` drives — uname, sw_vers, systemctl, launchctl — and
  runs the installer end to end against a temporary HOME) plus
  `shellcheck`/`bash -n`, all three in the `agent-tests` job;
  helper checks are selected for deploy/helper changes (see docs/CI.md). Before
  that the deploy path was the one place where "all green" carried no information —
  #264 broke every deploy (fixed in #268) and nothing could have caught it. Two assertions are
  load-bearing. `build_release_binary` *fails* when the workspace target dir is
  empty: a lenient fallback finds the stale pre-#264 binary in
  `agent/target/release/` and deploys it, reporting success. And the
  signature gate is proven with the **real `minisign`** (#392): a tampered
  fixture is rejected before it is executed, then the same fixture is fed
  through an accept-everything `minisign` stub and must *go through* — which is
  what shows the rejection was the verifier's. A stubbed success is never the
  security proof; without `minisign` those cases `SKIP` loudly, and CI installs
  it and sets `SOLADOR_DEPLOY_TEST_REQUIRE_MINISIGN=1` so that there the skip
  is a failure — in `agent-tests` (Linux, bash 5) and again in
  `rust-workspace` under macOS stock `/bin/bash` 3.2, the interpreter the
  installer and launcher actually run under on a Mac; `./dev test` on a Mac
  uses `/bin/bash` for the same reason. `SOLADOR_DEPLOY_TEST_LAUNCHD=1` opts
  into a real launchd bootstrap under a throwaway label (macOS only) and is
  deliberately not run in CI.
- **Those two shell gates now cover `scripts/*.sh` and `dev`/`prd` as well**
  (#390). `build-agent.sh`'s full run — all four targets, signed — happens only
  in `release-agent.yml` on an `agent-v*` tag, so an ungated break there surfaces mid-release
  — the #269 shape again, on the path with no second chance. Its build half runs
  for one target on agent/build changes (#457): `agent-tests` ("Rust agent")
  runs `./dev agent --targets x86_64-unknown-linux-musl`, which asserts the ELF is static
  and reads `--version` back out of it. The aarch64 and darwin builds (including
  the macOS floor check), release-agent.yml's per-target `--version` run and the
  signing still run only at release. The whole directory is covered rather than
  that one file so the next
  release script is not equally unguarded. Two comments in `scripts/lint.sh`
  had to be reworded: a comment opening with the linter's own name is parsed as
  a directive, and the file refuses to lint.
- **`lib_test.sh` bounds itself (#554).** A built-in watchdog stops a run after
  15 minutes by default (`SOLADOR_DEPLOY_TEST_TIMEOUT_SECS` overrides it, in
  seconds; a value that is not a positive whole number is refused with exit 2).
  A full run takes about 90-110 s (longer under load), so the bound is roughly
  10x that. On expiry it prints the case that was running and the suite's
  descendant process tree, then makes up to 10 one-second passes, each killing a
  fresh snapshot of that tree and re-signalling the shell, until it exits 124
  with its temp directory removed. If the shell is still alive after those
  passes it removes the temp directory and SIGKILLs the shell (exit 137). It
  lives in the file, so it covers `./dev test` and both CI invocations alike.
  Self-test cases run the suite as a child, chosen by the argument
  `--watchdog-probe <mode> <sleep-secs>` rather than an environment variable (a
  leaked variable must never replace the suite with one probe), with a
  deliberately blocking case (last, followed by another case, and followed by a
  second blocking case) and a control that finishes in time and must not trip
  it. Probes run before deployment fixture setup; a slow-fixture regression
  verifies they skip setup while normal runs still time out during it. A test must never use a bare `wait`, which would wait for the watchdog
  too; wait on a pid.
