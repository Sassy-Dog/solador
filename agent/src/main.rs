//! DevCanopy per-host metrics agent.
//!
//! An axum HTTP server exposing host metrics and a container list as JSON,
//! guarded by a bearer token. DevCanopy (a macOS app) polls it over Tailscale.

mod containers;
mod metrics;
mod server;

use std::sync::Arc;

use server::{build_router, AppState};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() {
    init_tracing();

    // Required bearer token — refuse to start without it.
    let token = match std::env::var("DEVCANOPY_AGENT_TOKEN") {
        Ok(t) if !t.trim().is_empty() => t,
        _ => {
            eprintln!("FATAL: DEVCANOPY_AGENT_TOKEN must be set (non-empty). Refusing to start.");
            std::process::exit(1);
        }
    };

    // Port: env override, default 7878.
    let port: u16 = std::env::var("DEVCANOPY_AGENT_PORT")
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

    let addr = format!("0.0.0.0:{port}");
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("FATAL: failed to bind {addr}: {e}");
            std::process::exit(1);
        }
    };

    tracing::info!("devcanopy-agent v{VERSION} listening on {addr} (host={hostname})");

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
