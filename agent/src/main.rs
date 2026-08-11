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

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() {
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

    tracing::info!("solador-agent v{VERSION} listening on {addr} (host={hostname})");

    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("FATAL: server error: {e}");
        std::process::exit(1);
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
