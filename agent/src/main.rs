//! Solador per-host metrics agent.
//!
//! An axum HTTP server exposing host metrics and a container list as JSON,
//! guarded by a bearer token. Solador polls it over whatever network path reaches
//! the host (Tailscale is optional).

mod containers;
mod gpu;
mod metrics;
mod server;

use std::sync::Arc;

use server::{build_router, AppState};
use solador_agent::{tls, update};

/// The version this build ships as: the repo's CalVer, derived once by
/// `scripts/get-version-info.sh` and compiled in by `build.rs` (#390).
///
/// `None` is a real state, not an oversight. A build made outside a full git
/// checkout — a shallow clone, a source tarball — cannot be asked how many
/// commits landed this month, and this repo does not answer that with a
/// stand-in: `--version` refuses and `/v1/health` omits the key, exactly as the
/// cockpit's About row renders `—`. Every *published* binary carries one, and
/// the release workflow asserts that by executing `--version` on a runner
/// matching each target before anything is uploaded.
///
/// Deliberately NOT `CARGO_PKG_VERSION`. `agent/Cargo.toml`'s number is the
/// wire-contract marker its comment block describes; it has never named a
/// release, and falling back to it here would be a defaulted value filling a
/// gap.
const VERSION: Option<&str> = option_env!("SOLADOR_MARKETING_VERSION");

/// What argv asked this process to do.
///
/// Resolved before anything else happens — before tracing, before the token
/// check — because `--version` has to work on a machine that has no token and
/// no tailnet. That is the whole point of the release gate: a binary that
/// cannot start is worse than no binary, and the check must not be answerable
/// only by a correctly configured host.
#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    /// No arguments: run the server.
    Serve,
    /// `--version` / `-V`.
    Version,
    /// `--help` / `-h`.
    Help,
    /// `update`: replace the installed agent with the latest published
    /// release, verified, atomically, with automatic rollback (#393).
    Update,
    /// `rollback`: put the previous binary back, offline (#393).
    Rollback,
    /// `tls-fingerprint`: print the SHA-256 fingerprint of the certificate
    /// `SOLADOR_AGENT_TLS=1` serves, colon-hex, and nothing else (#447).
    /// Read-only — it never generates one; see `tls::read_cert`.
    TlsFingerprint,
    /// Anything else, carried verbatim so the message can name it. Refused
    /// rather than ignored: the agent takes no arguments in normal operation,
    /// so an argument that reaches it is a mistake somewhere (a hand-edited
    /// `ExecStart`, a typo in a wrapper), and silently serving anyway is how
    /// that mistake survives. The two subcommands take no arguments of their
    /// own either, for the same reason: `update --force` is refused, not
    /// ignored into an unforced update.
    Unknown(String),
}

fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Invocation {
    let mut command = None;
    let mut unknown = None;
    for arg in args {
        match arg.as_str() {
            "--version" | "-V" => return Invocation::Version,
            "--help" | "-h" => return Invocation::Help,
            "update" | "rollback" | "tls-fingerprint" if command.is_none() && unknown.is_none() => {
                command = Some(arg);
            }
            other => {
                unknown.get_or_insert_with(|| other.to_string());
            }
        }
    }
    match (command.as_deref(), unknown) {
        (_, Some(a)) => Invocation::Unknown(a),
        (Some("update"), None) => Invocation::Update,
        (Some("rollback"), None) => Invocation::Rollback,
        (Some("tls-fingerprint"), None) => Invocation::TlsFingerprint,
        (_, None) => Invocation::Serve,
    }
}

/// `--version` prints the version and **nothing else** — no program name, no
/// prefix, one line. That is a contract, not terseness: `agent/deploy/lib.sh`'s
/// `binary_version` and the release workflow both read it directly, and every
/// decoration is something a future script would have to strip.
fn print_version() -> i32 {
    match VERSION {
        Some(v) => {
            println!("{v}");
            0
        }
        None => {
            eprintln!(
                "solador-agent: this build carries no version. It was compiled outside a full \
                 git checkout (a shallow clone, or an unpacked source archive), so \
                 scripts/get-version-info.sh could not count the commits CalVer is made of. \
                 Published binaries always carry one — see docs/VERSIONING.md."
            );
            1
        }
    }
}

/// Written at column zero on purpose: a `\n\` continuation would swallow the
/// leading spaces of every line after it, and the indentation here is the
/// output.
const USAGE: &str = "\
solador-agent — per-host metrics agent for Solador

Usage: solador-agent [update | rollback | tls-fingerprint | --version | --help]

Takes no arguments in normal operation; it is configured entirely from the
environment (systemd EnvironmentFile on Linux, launchd on macOS):

  SOLADOR_AGENT_TOKEN  required bearer token; the agent refuses to start without it
  SOLADOR_AGENT_BIND   bind address (default: the detected Tailscale IP; with
                       none, all interfaces if SOLADOR_AGENT_TLS=1, else refuse)
  SOLADOR_AGENT_PORT   listen port (default: 7878)
  SOLADOR_AGENT_TLS    1 serves HTTPS with a self-signed certificate kept for
                       the host's lifetime, instead of plain HTTP (default: unset)

Commands (run from a shell, never as the service itself):
  update           replace the installed agent with the latest published release:
                   verify the signed feed and binary under the compiled-in keys,
                   skip when the installed bytes already match, stage beside the
                   live path, swap atomically (previous kept as .prev), restart the
                   service and require /v1/health to report the new version — or
                   restore .prev automatically and exit non-zero
  rollback         put .prev back and restart, offline; refuses when there is none
  tls-fingerprint  print the SHA-256 fingerprint of the certificate
                   SOLADOR_AGENT_TLS=1 serves, colon-hex, and nothing else;
                   refuses if the agent has never started with TLS on (it
                   never generates a certificate itself)

Exit codes: 0 updated, or already current and serving; 1 failed with nothing
changed (for rollback also: swapped but not back, or half done — both say so);
3 failed AND the previous binary could not be restored — inspect the service;
4 no applicable release (the feed is not newer than what is installed, or what
is installed is a source build, +dev, with nothing to compare); 5 failed, the
previous binary is back and serving; 75 another update/rollback holds the lock;
2 usage. tls-fingerprint: 0 printed, 1 no certificate yet.

Options:
  -V, --version  print the version and nothing else, then exit
  -h, --help     print this help, then exit
";

fn usage() -> String {
    format!(
        "{USAGE}\nVersion: {}\n",
        VERSION.unwrap_or("— (this build carries no version)")
    )
}

#[tokio::main]
async fn main() {
    match parse_args(std::env::args().skip(1)) {
        Invocation::Serve => {}
        Invocation::Version => std::process::exit(print_version()),
        Invocation::Help => {
            print!("{}", usage());
            return;
        }
        // Dispatched HERE — before tracing, before the token check, before a
        // sampler or a listener exists — because the updater is a separate
        // process from the service it restarts, and must never become one.
        Invocation::Update => std::process::exit(run_maintenance(Maintenance::Update).await),
        Invocation::Rollback => std::process::exit(run_maintenance(Maintenance::Rollback).await),
        Invocation::TlsFingerprint => std::process::exit(run_tls_fingerprint()),
        Invocation::Unknown(arg) => {
            eprintln!("solador-agent: unrecognized argument '{arg}'");
            eprint!("{}", usage());
            std::process::exit(2);
        }
    }

    init_tracing();

    // Required bearer token — refuse to start without it.
    let token = match std::env::var("SOLADOR_AGENT_TOKEN") {
        Ok(t) if !t.trim().is_empty() => t,
        _ => {
            eprintln!("FATAL: SOLADOR_AGENT_TOKEN must be set (non-empty). Refusing to start.");
            std::process::exit(1);
        }
    };

    // Port: env override, default 7878.
    let port: u16 = std::env::var("SOLADOR_AGENT_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(7878);

    let hostname = hostname();

    // Start the background metrics sampler.
    let metrics = metrics::spawn_sampler();

    let state = AppState {
        metrics,
        token: Arc::new(token),
        hostname: hostname.clone(),
        version: VERSION,
    };

    let app = build_router(state);

    // TLS is opt-in by config (#447): SOLADOR_AGENT_TLS=1 in the env file.
    let tls_on = tls::flag_enabled(std::env::var("SOLADOR_AGENT_TLS").ok().as_deref());

    // Resolve the bind host. Over plain HTTP the default is the host's
    // Tailscale (tailnet) IP, and no tailnet means a refusal to start — the
    // tailnet is the only thing protecting the token on the wire. With TLS on
    // (#449) the token no longer crosses the network in the clear, so a host
    // with no tailnet binds all interfaces instead. An explicit
    // SOLADOR_AGENT_BIND wins over both.
    let bind_host = match resolve_bind_host(
        std::env::var("SOLADOR_AGENT_BIND").ok(),
        tls_on,
        detect_tailscale_ip,
    ) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("FATAL: {e}");
            std::process::exit(1);
        }
    };

    let addr = format_bind_addr(&bind_host, port);

    if is_wildcard_host(&bind_host) {
        tracing::warn!("{}", wildcard_warning(&addr, tls_on));
    }

    // Without TLS, everything below this point is unchanged from before #447.
    if tls_on {
        serve_tls(app, &addr, &bind_host, &hostname).await;
        return;
    }

    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("FATAL: failed to bind {addr}: {e}");
            std::process::exit(1);
        }
    };

    tracing::info!(
        "solador-agent {} listening on {addr} (host={hostname})",
        VERSION.map_or_else(|| "(no version)".to_string(), |v| format!("v{v}"))
    );

    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("FATAL: server error: {e}");
        std::process::exit(1);
    }
}

/// Where `tls::KEY_FILE` / `tls::CERT_FILE` live: beside
/// `solador-agent.env`, the agent's config directory (#447).
///
/// Resolution order:
/// 1. `SOLADOR_AGENT_CONFIG_DIR`, when set — the directory the env file
///    that configured THIS process actually lives in. `run-agent.sh`
///    (macOS) and `solador-agent.service` (Linux, `%h/.config` — a systemd
///    specifier the manager resolves from the target user's own account,
///    not this process's `HOME`) both export it, derived the same way
///    `agent/deploy/lib.sh` and `update.rs` already do:
///    `dirname(<the env file's path>)`.
/// 2. `$HOME/.config`, when `SOLADOR_AGENT_CONFIG_DIR` is unset — a manual
///    `solador-agent` invocation with no launcher in front of it (a
///    from-source Linux host, a developer running it directly), where this
///    process's own `HOME` IS the installer's.
///
/// The two can disagree: launchd's `HOME` (the target user record's) need
/// not be the `HOME` `install.sh` ran under (see `run-agent.sh`'s own
/// header comment), so deriving this from `$HOME` alone — as an earlier
/// revision did — could point the running service at a directory that
/// holds no env file at all while `lib.sh`'s `verify_health` and
/// `update.rs`'s `health_pin`, which both derive it from the actual env
/// file's path, keep looking in the right place.
fn tls_config_dir() -> Result<std::path::PathBuf, String> {
    resolve_tls_config_dir(
        std::env::var_os("SOLADOR_AGENT_CONFIG_DIR"),
        std::env::var_os("HOME"),
    )
}

/// The rules of [`tls_config_dir`] with its two environment values passed in,
/// so a test never has to touch the process environment (the same seam
/// `resolve_bind_host` has).
fn resolve_tls_config_dir(
    config_dir: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Result<std::path::PathBuf, String> {
    if let Some(dir) = config_dir {
        let dir = std::path::PathBuf::from(dir);
        return if dir.is_absolute() {
            Ok(dir)
        } else {
            Err(format!(
                "SOLADOR_AGENT_CONFIG_DIR={} is not an absolute path; SOLADOR_AGENT_TLS=1 needs \
                 one to find or create the certificate beside the env file",
                dir.display()
            ))
        };
    }
    home.map(std::path::PathBuf::from)
        .filter(|h| h.is_absolute())
        .map(|h| h.join(".config"))
        .ok_or_else(|| {
            "neither SOLADOR_AGENT_CONFIG_DIR nor HOME (as an absolute path) is set; \
             SOLADOR_AGENT_TLS=1 needs one of them to find or create the certificate beside \
             the env file"
                .to_string()
        })
}

/// Serve HTTPS on `addr` with the self-signed certificate kept for the
/// host's lifetime (#447): loaded from, or generated once into, the agent's
/// config directory (`tls::load_or_generate` — the ONLY call site in this
/// binary that may generate one; `tls-fingerprint`, `update` and `rollback`
/// only ever read). `bind_host` goes into the certificate's SAN list beside
/// the loopback baseline, so a client that dials the bind address by name can
/// verify it. The list is fixed at first start, and the local health probes do
/// not depend on it (#449): they pin the certificate itself, name `localhost`
/// (always in the baseline) and connect to the bind, an IP or a DNS name
/// alike. A wildcard bind adds nothing.
/// Exits the process on any fatal error, the same way the plain-HTTP path
/// above does.
async fn serve_tls(app: axum::Router, addr: &str, bind_host: &str, hostname: &str) {
    let dir = match tls_config_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("FATAL: {e}");
            std::process::exit(1);
        }
    };
    let material = match tls::load_or_generate(&dir, &[bind_host.to_string()]) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("FATAL: {e}");
            std::process::exit(1);
        }
    };
    // Not secret (see tls::fingerprint_hex's doc comment) — logging it is
    // what lets an operator confirm which certificate is serving without a
    // separate `tls-fingerprint` invocation. The key itself is never logged
    // anywhere in this binary.
    let fingerprint = tls::fingerprint_hex(&material.cert_der);

    // rustls needs a process-wide default crypto provider installed once.
    // axum-server's "tls-rustls-no-provider" feature deliberately picks
    // none, so this is the one place that chooses — `ring`, the same
    // backend `reqwest`'s `rustls-tls` feature already resolves for the
    // whole workspace (see agent/Cargo.toml), so the binary never links a
    // second crypto implementation. Already-installed is not an error: some
    // other path (a `reqwest::Client` built first) may have gotten there first.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let rustls_config = match axum_server::tls_rustls::RustlsConfig::from_der(
        vec![material.cert_der.clone()],
        material.key_der.clone(),
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "FATAL: could not build a TLS server config from {} / {}: {e}",
                dir.join(tls::KEY_FILE).display(),
                dir.join(tls::CERT_FILE).display()
            );
            std::process::exit(1);
        }
    };

    // Bound the same way, with the same message, as the plain-HTTP path
    // (#447 review round 2): `axum_server::bind_rustls` takes a
    // `SocketAddr`, which `addr.parse()` cannot produce for a hostname bind
    // (`SOLADOR_AGENT_BIND=localhost`, say) that `tokio::net::TcpListener`
    // accepts directly via its own async resolution — a difference that did
    // not matter before TLS became the fresh-install default, and does now.
    // `into_std()` hands axum-server a socket already in non-blocking mode,
    // which is what `tokio::net::TcpListener::from_std` (what
    // `from_tcp_rustls` calls internally) requires.
    let tokio_listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("FATAL: failed to bind {addr}: {e}");
            std::process::exit(1);
        }
    };
    let std_listener = match tokio_listener.into_std() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("FATAL: could not prepare {addr} for TLS: {e}");
            std::process::exit(1);
        }
    };

    // Logged only now, after the bind actually succeeded — a port already
    // in use must not read as a success line followed by an unrelated
    // "server error".
    tracing::info!(
        "solador-agent {} listening on https://{addr} (host={hostname}, tls fingerprint {fingerprint})",
        VERSION.map_or_else(|| "(no version)".to_string(), |v| format!("v{v}"))
    );

    if let Err(e) = axum_server::from_tcp_rustls(std_listener, rustls_config)
        .serve(app.into_make_service())
        .await
    {
        eprintln!("FATAL: server error: {e}");
        std::process::exit(1);
    }
}

/// `solador-agent tls-fingerprint`: print the SHA-256 fingerprint of the
/// certificate `SOLADOR_AGENT_TLS=1` serves — colon-hex, one line, nothing
/// else, the same "print exactly this and nothing else" contract
/// `print_version` keeps. Read-only: it never generates a certificate (see
/// `tls::read_cert`), so it cannot race the agent's own first start over
/// which SAN list wins.
fn run_tls_fingerprint() -> i32 {
    let outcome = tls_config_dir().and_then(|dir| tls::read_cert(&dir).map_err(|e| e.to_string()));
    match outcome {
        Ok(cert_der) => {
            println!("{}", tls::fingerprint_hex(&cert_der));
            0
        }
        Err(e) => {
            eprintln!("ERROR: {e}");
            1
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Maintenance {
    Update,
    Rollback,
}

/// `solador-agent update` / `rollback`, wired to the real host: this user's
/// HOME, the service #392 installed, the env file it reads, the production
/// release base and the compiled-in trust set. Everything that can refuse
/// refuses before anything changes, and the exit code says which outcome
/// this was (see `USAGE`).
///
/// The token read from the env file goes to exactly one place, the
/// `Authorization` header of the local health probe; it is never printed
/// and `update::Serving`'s `Debug` redacts it.
async fn run_maintenance(what: Maintenance) -> i32 {
    let mut report = |line: &str| println!("{line}");
    let outcome = async {
        // Never as root: the install and the service are one user's, and
        // `sudo solador-agent update` would leave root-owned files beside
        // that user's binary and restart root's (nonexistent) service.
        update::refuse_privileged(update::current_euid())?;
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .filter(|h| h.is_absolute())
            .ok_or_else(|| {
                update::UpdateError::Install(
                    "HOME is not set to an absolute path; the installed service is resolved \
                     under it"
                        .to_string(),
                )
            })?;
        // The same override install.sh honours, so the test harness can
        // drive a throwaway LaunchAgent; validated the same way.
        let label = std::env::var("SOLADOR_AGENT_LAUNCHD_LABEL")
            .ok()
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| update::LAUNCHD_LABEL.to_string());
        let target = update::host_target()?;
        let trust = update::Trust::compiled_in()?;
        let install = update::resolve_install(&home, &label)?;
        let serving = update::read_serving(&install.env_file)?;
        let service = install.service.clone();
        let mut ctx = update::Context {
            release_base: update::RELEASE_BASE.to_string(),
            trust,
            install,
            serving,
            service: &service,
            target,
            running_version: VERSION.map(str::to_string),
            health_attempts: update::HEALTH_ATTEMPTS,
            health_interval: update::HEALTH_INTERVAL,
            report: &mut report,
        };
        match what {
            Maintenance::Update => update::run_update(&mut ctx).await.map(|o| match o {
                update::UpdateOutcome::AlreadyCurrent { version, .. } => {
                    format!("==> Done: already current ({version}); nothing changed.")
                }
                update::UpdateOutcome::Updated { from, to, key } => format!(
                    "==> Done: updated {from} -> {to} (binary verified under key {key}) and serving."
                ),
            }),
            Maintenance::Rollback => update::run_rollback(&mut ctx).await.map(|o| {
                format!(
                    "==> Done: rolled back to {} and serving{}.",
                    o.restored_version.as_deref().unwrap_or("a binary carrying no version"),
                    o.served_version
                        .as_deref()
                        .map(|v| format!(" (reports {v})"))
                        .unwrap_or_default()
                )
            }),
        }
    }
    .await;
    match outcome {
        Ok(line) => {
            println!("{line}");
            0
        }
        Err(e) => {
            eprintln!("ERROR: {e}");
            e.exit_code()
        }
    }
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).with_target(false).init();
}

/// Best-effort hostname for the `/v1/health` response.
fn hostname() -> String {
    sysinfo::System::host_name().unwrap_or_else(|| "unknown".to_string())
}

/// The Tailscale CGNAT range (`100.64.0.0/10`, RFC 6598) that `tailscale0`
/// addresses live in. Used to recognize a tailnet IP.
fn is_tailscale_ipv4(ip: std::net::Ipv4Addr) -> bool {
    let o = ip.octets();
    // 100.64.0.0/10 => first octet 100, second octet in 64..=127.
    o[0] == 100 && (64..=127).contains(&o[1])
}

/// Is this bind host a wildcard (all interfaces)?
fn is_wildcard_host(host: &str) -> bool {
    matches!(host, "0.0.0.0" | "::" | "[::]")
}

/// The runtime warning for an all-interfaces bind. The two cases are
/// different claims and read differently on purpose (#449): with TLS on the
/// token is encrypted and the certificate pinned; with TLS off — most likely
/// a hand edit of `SOLADOR_AGENT_TLS=0` that left the install-chosen
/// `0.0.0.0` bind behind — it is plain HTTP on every interface.
fn wildcard_warning(addr: &str, tls_on: bool) -> String {
    if tls_on {
        format!(
            "binding all interfaces ({addr}): the agent is reachable on every network this \
             host is on; its only protection is the bearer token and pinned TLS certificate. \
             Firewall the port, or set SOLADOR_AGENT_BIND to one interface, on a host with \
             a public address."
        )
    } else {
        format!(
            "PLAIN HTTP ON EVERY INTERFACE ({addr}): TLS is off and the agent is listening on \
             all networks this host is on, so the bearer token crosses the network in \
             CLEARTEXT and anyone who can reach the port can read it. This is usually a hand \
             edit of SOLADOR_AGENT_TLS=0 that left the all-interfaces bind install.sh chose. \
             Fix it: remove both SOLADOR_AGENT_BIND and SOLADOR_AGENT_BIND_AUTO from the env \
             file and re-run install.sh (it will bind the tailnet address), or set \
             SOLADOR_AGENT_TLS=1."
        )
    }
}

/// Decide the host portion of the bind address.
///
/// - If `SOLADOR_AGENT_BIND` is set (non-empty), honor it verbatim (an IPv6
///   zone id is the one refusal, TLS on or off, #476), in every
///   case below. This is how to bind a specific non-tailnet interface, or
///   `0.0.0.0` / `::` for all-interfaces over plain HTTP.
/// - Otherwise default to the detected Tailscale IP.
/// - With no tailnet IP, `tls_on` decides (#449): over plain HTTP, refuse to
///   start rather than silently falling back to `0.0.0.0` and sending the
///   bearer token in the clear on whatever network the host is on; with TLS
///   on the token is encrypted and the certificate pinned, so bind all
///   interfaces (`0.0.0.0`) instead of forcing Tailscale on the host.
///
/// `detect` is injected so the decision is unit-testable without touching the
/// real network.
fn resolve_bind_host<F>(env_bind: Option<String>, tls_on: bool, detect: F) -> Result<String, String>
where
    F: FnOnce() -> Option<String>,
{
    if let Some(v) = env_bind {
        let v = v.trim();
        if update::has_zone_id(v) {
            // Refused with TLS on or off (#476): `update`'s health probe
            // cannot dial it, and a link-local bind is reachable only from
            // its own link.
            return Err(update::zone_id_refusal(v));
        }
        if !v.is_empty() {
            return Ok(v.to_string());
        }
    }

    match detect() {
        Some(ip) => Ok(ip),
        None if tls_on => Ok("0.0.0.0".to_string()),
        None => Err(
            "could not detect a Tailscale IP to bind to, and TLS is off (the token \
             would cross the network in the clear). Set SOLADOR_AGENT_BIND to the \
             tailnet address (e.g. 100.x.y.z), turn TLS on (SOLADOR_AGENT_TLS=1) to \
             bind all interfaces by default, or set SOLADOR_AGENT_BIND=0.0.0.0 to \
             bind all interfaces anyway (only do this behind a firewall)."
                .to_string(),
        ),
    }
}

/// Join a bind host and port into a socket address, bracketing IPv6 literals.
fn format_bind_addr(host: &str, port: u16) -> String {
    if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Best-effort detection of this host's Tailscale IPv4 address.
///
/// Prefers the `tailscale` CLI (`tailscale ip -4`); if that is unavailable,
/// scans local interfaces for an address in the `100.64.0.0/10` CGNAT range.
fn detect_tailscale_ip() -> Option<String> {
    if let Some(ip) = tailscale_cli_ip() {
        return Some(ip);
    }
    tailscale_iface_ip()
}

/// Ask the `tailscale` CLI for the node's IPv4 address.
fn tailscale_cli_ip() -> Option<String> {
    let out = std::process::Command::new("tailscale")
        .args(["ip", "-4"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .lines()
        .map(str::trim)
        .find_map(|line| line.parse::<std::net::Ipv4Addr>().ok())
        .filter(|ip| is_tailscale_ipv4(*ip))
        .map(|ip| ip.to_string())
}

/// Fallback: scan local interface addresses for a `100.64.0.0/10` IPv4.
///
/// Parses `ip -4 -o addr` (Linux) output, which avoids pulling in an extra
/// crate just to enumerate interfaces.
fn tailscale_iface_ip() -> Option<String> {
    let out = std::process::Command::new("ip")
        .args(["-4", "-o", "addr"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        // Tokens look like: `12: tailscale0 inet 100.x.y.z/32 ...`
        for tok in line.split_whitespace() {
            let addr = tok.split('/').next().unwrap_or(tok);
            if let Ok(ip) = addr.parse::<std::net::Ipv4Addr>() {
                if is_tailscale_ipv4(ip) {
                    return Some(ip.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn no_arguments_means_serve() {
        assert_eq!(parse_args(args(&[])), Invocation::Serve);
    }

    #[test]
    fn version_and_help_are_recognized_in_both_spellings() {
        for a in ["--version", "-V"] {
            assert_eq!(parse_args(args(&[a])), Invocation::Version, "{a}");
        }
        for a in ["--help", "-h"] {
            assert_eq!(parse_args(args(&[a])), Invocation::Help, "{a}");
        }
    }

    /// An argument the agent does not understand is REFUSED, never ignored.
    /// The agent takes none in normal operation, so one arriving means
    /// something upstream is wrong — a hand-edited `ExecStart`, a wrapper
    /// passing flags meant for something else — and serving anyway is how that
    /// survives unnoticed.
    #[test]
    fn an_unrecognized_argument_is_refused_and_named() {
        assert_eq!(
            parse_args(args(&["--serve-everything"])),
            Invocation::Unknown("--serve-everything".to_string())
        );
    }

    /// `--version` must win over the garbage beside it: the release gate runs
    /// it on four target runners, and a binary that answered "unrecognized
    /// argument" there would fail the gate for the wrong reason.
    #[test]
    fn version_wins_over_an_unrecognized_argument() {
        assert_eq!(
            parse_args(args(&["--nonsense", "--version"])),
            Invocation::Version
        );
        assert_eq!(
            parse_args(args(&["update", "--version"])),
            Invocation::Version
        );
        assert_eq!(parse_args(args(&["rollback", "-h"])), Invocation::Help);
    }

    /// The two maintenance commands (#393) dispatch before serving, and take
    /// no arguments of their own: an option nobody defined is refused rather
    /// than ignored into an unforced update, and a second command word is
    /// refused rather than the first one winning.
    #[test]
    fn update_and_rollback_are_commands_and_take_nothing_else() {
        assert_eq!(parse_args(args(&["update"])), Invocation::Update);
        assert_eq!(parse_args(args(&["rollback"])), Invocation::Rollback);
        assert_eq!(
            parse_args(args(&["update", "--force"])),
            Invocation::Unknown("--force".to_string())
        );
        assert_eq!(
            parse_args(args(&["update", "rollback"])),
            Invocation::Unknown("rollback".to_string())
        );
        assert_eq!(
            parse_args(args(&["--yes", "update"])),
            Invocation::Unknown("--yes".to_string())
        );
        assert_eq!(
            parse_args(args(&["Update"])),
            Invocation::Unknown("Update".to_string())
        );
    }

    /// `tls-fingerprint` (#447) is a third command beside `update` and
    /// `rollback`, parsed the same way: recognized bare, refused with
    /// anything else beside it.
    #[test]
    fn tls_fingerprint_is_a_command_and_takes_nothing_else() {
        assert_eq!(
            parse_args(args(&["tls-fingerprint"])),
            Invocation::TlsFingerprint
        );
        assert_eq!(
            parse_args(args(&["tls-fingerprint", "--force"])),
            Invocation::Unknown("--force".to_string())
        );
        assert_eq!(
            parse_args(args(&["tls-fingerprint", "update"])),
            Invocation::Unknown("update".to_string())
        );
        assert_eq!(
            parse_args(args(&["update", "tls-fingerprint"])),
            Invocation::Unknown("tls-fingerprint".to_string())
        );
    }

    /// The one line `--version` prints IS the machine contract — `lib.sh`'s
    /// `binary_version` and the release workflow both read it verbatim. A
    /// prefix here would break both silently, so the shape is pinned.
    #[test]
    fn a_missing_version_is_reported_as_a_failure_rather_than_a_stand_in() {
        // The value is compile-time, so this asserts the mapping rather than
        // the build: whatever `VERSION` is, exit 0 means a version was printed
        // and exit 1 means none was — never a substitute.
        let code = print_version();
        assert_eq!(code, i32::from(VERSION.is_none()));
    }

    #[test]
    fn usage_names_the_version_state_it_is_in() {
        let text = usage();
        assert!(text.contains("--version"), "{text}");
        assert!(text.contains("SOLADOR_AGENT_TOKEN"), "{text}");
        assert!(text.contains("update"), "{text}");
        assert!(text.contains("rollback"), "{text}");
        assert!(text.contains("tls-fingerprint"), "{text}");
        assert!(text.contains("SOLADOR_AGENT_TLS"), "{text}");
        assert!(text.contains("75"), "{text}");
        match VERSION {
            Some(v) => assert!(text.contains(v), "{text}"),
            None => assert!(text.contains('—'), "{text}"),
        }
    }

    #[test]
    fn is_tailscale_ipv4_recognizes_cgnat_range() {
        assert!(is_tailscale_ipv4("100.64.0.1".parse().unwrap()));
        assert!(is_tailscale_ipv4("100.100.50.1".parse().unwrap()));
        assert!(is_tailscale_ipv4("100.127.255.254".parse().unwrap()));
        // Outside 100.64.0.0/10:
        assert!(!is_tailscale_ipv4("100.63.0.1".parse().unwrap()));
        assert!(!is_tailscale_ipv4("100.128.0.1".parse().unwrap()));
        assert!(!is_tailscale_ipv4("10.0.0.1".parse().unwrap()));
        assert!(!is_tailscale_ipv4("192.168.1.1".parse().unwrap()));
    }

    const TAILNET: &str = "100.5.6.7";

    fn bind(env: Option<&str>, tls_on: bool, tailnet: bool) -> Result<String, String> {
        resolve_bind_host(env.map(str::to_string), tls_on, || {
            tailnet.then(|| TAILNET.to_string())
        })
    }

    #[test]
    fn the_tls_off_wildcard_warning_is_its_own_loud_message() {
        let on = wildcard_warning("0.0.0.0:7878", true);
        let off = wildcard_warning("0.0.0.0:7878", false);
        assert_ne!(on, off);
        // Plain about the three things an operator must learn from it.
        assert!(off.contains("PLAIN HTTP ON EVERY INTERFACE"), "{off}");
        assert!(off.contains("CLEARTEXT"), "{off}");
        assert!(off.contains("SOLADOR_AGENT_BIND_AUTO"), "{off}");
        assert!(off.contains("SOLADOR_AGENT_TLS=1"), "{off}");
        assert!(off.contains("0.0.0.0:7878"), "{off}");
        // The TLS-on message stays the calmer one.
        assert!(!on.contains("CLEARTEXT"), "{on}");
        assert!(!on.contains("PLAIN HTTP"), "{on}");
    }

    fn os(s: &str) -> Option<std::ffi::OsString> {
        Some(std::ffi::OsString::from(s))
    }

    /// An absolute path on whatever OS runs the tests. `/home/ops` is not
    /// absolute on Windows (no drive), and the workspace's Windows job runs
    /// these too; `temp_dir()` is absolute everywhere.
    fn abs(leaf: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(leaf)
    }

    fn os_path(p: &std::path::Path) -> Option<std::ffi::OsString> {
        Some(p.as_os_str().to_owned())
    }

    #[test]
    fn tls_config_dir_uses_home_dot_config_when_the_variable_is_unset() {
        let home = abs("home-ops");
        let got = resolve_tls_config_dir(None, os_path(&home)).unwrap();
        assert_eq!(got, home.join(".config"));
    }

    #[test]
    fn tls_config_dir_prefers_the_variable_over_home() {
        let dir = abs("srv-agent");
        let got = resolve_tls_config_dir(os_path(&dir), os_path(&abs("home-ops"))).unwrap();
        assert_eq!(got, dir);
    }

    #[test]
    fn tls_config_dir_refuses_a_relative_variable_even_with_a_good_home() {
        let err =
            resolve_tls_config_dir(os("relative/dir"), os_path(&abs("home-ops"))).unwrap_err();
        assert!(
            err.starts_with("SOLADOR_AGENT_CONFIG_DIR=relative/dir is not an absolute path;"),
            "{err}"
        );
    }

    #[test]
    fn tls_config_dir_refuses_a_missing_or_relative_home() {
        for home in [None, os("relative/home"), os("")] {
            let err = resolve_tls_config_dir(None, home).unwrap_err();
            assert!(
                err.starts_with(
                    "neither SOLADOR_AGENT_CONFIG_DIR nor HOME (as an absolute path) is set;"
                ),
                "{err}"
            );
        }
    }

    #[test]
    fn resolve_bind_host_prefers_explicit_env_in_every_case() {
        // Explicit env wins over TLS on/off and a detected tailnet; detection
        // is never consulted.
        for tls_on in [false, true] {
            let got = resolve_bind_host(Some("192.168.1.20".to_string()), tls_on, || {
                panic!("detect must not be called when env is set")
            });
            assert_eq!(got.unwrap(), "192.168.1.20");
            let got = resolve_bind_host(Some("  100.1.2.3  ".to_string()), tls_on, || {
                panic!("detect must not be called when env is set")
            });
            assert_eq!(got.unwrap(), "100.1.2.3");
            // An explicit wildcard is honoured verbatim, with or without TLS.
            assert_eq!(bind(Some("0.0.0.0"), tls_on, true).unwrap(), "0.0.0.0");
            assert_eq!(bind(Some("::"), tls_on, false).unwrap(), "::");
        }
    }

    #[test]
    fn resolve_bind_host_uses_the_tailnet_ip_when_there_is_one() {
        // TLS does not change the choice when a tailnet IP exists.
        assert_eq!(bind(None, false, true).unwrap(), TAILNET);
        assert_eq!(bind(None, true, true).unwrap(), TAILNET);
        // Empty / whitespace env is treated as unset.
        assert_eq!(bind(Some("   "), false, true).unwrap(), TAILNET);
        assert_eq!(bind(Some(""), true, true).unwrap(), TAILNET);
    }

    #[test]
    fn resolve_bind_host_binds_all_interfaces_with_tls_and_no_tailnet() {
        assert_eq!(bind(None, true, false).unwrap(), "0.0.0.0");
        assert_eq!(bind(Some("  "), true, false).unwrap(), "0.0.0.0");
    }

    #[test]
    fn resolve_bind_host_errors_when_no_tailnet_and_no_tls() {
        // Unchanged from before #449: plain HTTP stays tailnet-only.
        let got = bind(None, false, false);
        assert!(got.is_err(), "must refuse to start, not default to 0.0.0.0");
        let msg = got.unwrap_err();
        assert!(msg.contains("SOLADOR_AGENT_BIND"));
        assert!(msg.contains("SOLADOR_AGENT_TLS"));
        assert!(bind(Some("\t"), false, false).is_err());
    }

    #[test]
    fn wildcard_hosts_are_recognised() {
        for h in ["0.0.0.0", "::", "[::]"] {
            assert!(is_wildcard_host(h), "{h}");
        }
        for h in ["127.0.0.1", "100.5.6.7", "192.168.1.20", "::1", ""] {
            assert!(!is_wildcard_host(h), "{h}");
        }
    }

    #[test]
    fn resolve_bind_host_refuses_a_zone_id_with_tls_on_or_off() {
        for tls in [false, true] {
            let err = resolve_bind_host(Some("fe80::1%en0".to_string()), tls, || None)
                .expect_err("a zone-id bind must be refused");
            assert!(
                err.contains("zone id") && err.contains("fe80::1%en0"),
                "{err}"
            );
        }
        // Control: the same bind without a zone starts.
        assert_eq!(
            resolve_bind_host(Some("fe80::1".to_string()), false, || None).unwrap(),
            "fe80::1"
        );
    }

    #[test]
    fn format_bind_addr_handles_ipv4_and_ipv6() {
        assert_eq!(format_bind_addr("100.1.2.3", 7878), "100.1.2.3:7878");
        assert_eq!(format_bind_addr("0.0.0.0", 7878), "0.0.0.0:7878");
        assert_eq!(format_bind_addr("::", 7878), "[::]:7878");
        assert_eq!(format_bind_addr("fd7a::1", 9000), "[fd7a::1]:9000");
    }
}
