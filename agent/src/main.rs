//! Solador per-host metrics agent.
//!
//! An axum HTTP server exposing host metrics and a container list as JSON,
//! guarded by a bearer token. Solador (a macOS app) polls it over Tailscale.

mod containers;
mod gpu;
mod metrics;
mod server;

use std::sync::Arc;

use server::{build_router, AppState};
use solador_agent::update;

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
            "update" | "rollback" if command.is_none() && unknown.is_none() => {
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

Usage: solador-agent [update | rollback | --version | --help]

Takes no arguments in normal operation; it is configured entirely from the
environment (systemd EnvironmentFile on Linux, launchd on macOS):

  SOLADOR_AGENT_TOKEN  required bearer token; the agent refuses to start without it
  SOLADOR_AGENT_BIND   bind address (default: the detected Tailscale IP)
  SOLADOR_AGENT_PORT   listen port (default: 7878)

Commands (run from a shell, never as the service itself):
  update    replace the installed agent with the latest published release:
            verify the signed feed and binary under the compiled-in keys,
            skip when the installed bytes already match, stage beside the
            live path, swap atomically (previous kept as .prev), restart the
            service and require /v1/health to report the new version — or
            restore .prev automatically and exit non-zero
  rollback  put .prev back and restart, offline; refuses when there is none

Exit codes: 0 updated, or already current and serving; 1 failed with nothing
changed (for rollback also: swapped but not back, or half done — both say so);
3 failed AND the previous binary could not be restored — inspect the service;
4 no applicable release (the feed is not newer than what is installed);
5 failed, the previous binary is back and serving; 75 another update/rollback
holds the lock; 2 usage.

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

    // Resolve the bind host. Default is the host's Tailscale (tailnet) IP so the
    // agent is *not* reachable on the public NIC. Binding all interfaces
    // (0.0.0.0 / ::) requires explicit opt-in via SOLADOR_AGENT_BIND.
    let bind_host = match resolve_bind_host(
        std::env::var("SOLADOR_AGENT_BIND").ok(),
        detect_tailscale_ip,
    ) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("FATAL: {e}");
            std::process::exit(1);
        }
    };

    let addr = format_bind_addr(&bind_host, port);
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

/// Decide the host portion of the bind address.
///
/// - If `SOLADOR_AGENT_BIND` is set (non-empty), honor it verbatim. This is
///   the only way to bind a non-tailnet interface, including the explicit
///   opt-ins `0.0.0.0` / `::` for all-interfaces.
/// - Otherwise default to the detected Tailscale IP so the agent only listens
///   on the tailnet.
/// - If neither is available, refuse to start rather than silently falling back
///   to `0.0.0.0` and exposing the host on its public NIC.
///
/// `detect` is injected so the decision is unit-testable without touching the
/// real network.
fn resolve_bind_host<F>(env_bind: Option<String>, detect: F) -> Result<String, String>
where
    F: FnOnce() -> Option<String>,
{
    if let Some(v) = env_bind {
        let v = v.trim();
        if !v.is_empty() {
            return Ok(v.to_string());
        }
    }

    match detect() {
        Some(ip) => Ok(ip),
        None => Err(
            "could not detect a Tailscale IP to bind to. Set SOLADOR_AGENT_BIND \
             to the tailnet address (e.g. 100.x.y.z), or to 0.0.0.0 to bind all \
             interfaces (only do this behind a firewall)."
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

    #[test]
    fn resolve_bind_host_prefers_explicit_env() {
        // Explicit env wins; detection is never consulted.
        let got = resolve_bind_host(Some("0.0.0.0".to_string()), || {
            panic!("detect must not be called when env is set")
        });
        assert_eq!(got.unwrap(), "0.0.0.0");

        let got = resolve_bind_host(Some("  100.1.2.3  ".to_string()), || {
            panic!("detect must not be called when env is set")
        });
        assert_eq!(got.unwrap(), "100.1.2.3");
    }

    #[test]
    fn resolve_bind_host_falls_back_to_detection_when_env_blank() {
        let got = resolve_bind_host(None, || Some("100.5.6.7".to_string()));
        assert_eq!(got.unwrap(), "100.5.6.7");

        // Empty / whitespace env is treated as unset.
        let got = resolve_bind_host(Some("   ".to_string()), || Some("100.5.6.7".to_string()));
        assert_eq!(got.unwrap(), "100.5.6.7");
    }

    #[test]
    fn resolve_bind_host_errors_when_no_tailnet_and_no_opt_in() {
        let got = resolve_bind_host(None, || None);
        assert!(got.is_err(), "must refuse to start, not default to 0.0.0.0");
        let msg = got.unwrap_err();
        assert!(msg.contains("SOLADOR_AGENT_BIND"));
    }

    #[test]
    fn format_bind_addr_handles_ipv4_and_ipv6() {
        assert_eq!(format_bind_addr("100.1.2.3", 7878), "100.1.2.3:7878");
        assert_eq!(format_bind_addr("0.0.0.0", 7878), "0.0.0.0:7878");
        assert_eq!(format_bind_addr("::", 7878), "[::]:7878");
        assert_eq!(format_bind_addr("fd7a::1", 9000), "[fd7a::1]:9000");
    }
}
