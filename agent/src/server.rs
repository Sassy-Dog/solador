//! axum HTTP server: routes, bearer-token auth middleware, and shared state.

use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde_json::json;

use crate::containers::list_containers;
use crate::metrics::MetricsState;

/// Application state shared across handlers.
#[derive(Clone)]
pub struct AppState {
    pub metrics: MetricsState,
    /// Required bearer token. Requests must present `Authorization: Bearer <token>`.
    pub token: Arc<String>,
    pub hostname: String,
    pub version: &'static str,
}

/// Build the router with all `/v1` routes guarded by the bearer-token middleware.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/v1/snapshot", get(snapshot_handler))
        .route("/v1/containers", get(containers_handler))
        .route("/v1/health", get(health_handler))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .with_state(state)
}

/// Reject any request lacking a valid `Authorization: Bearer <token>` header.
async fn auth_middleware(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> axum::response::Response {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);

    match presented {
        Some(tok) if constant_time_eq(tok.as_bytes(), state.token.as_bytes()) => {
            next.run(request).await
        }
        _ => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        )
            .into_response(),
    }
}

/// Length-independent byte comparison to avoid leaking the token via timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

async fn snapshot_handler(State(state): State<AppState>) -> impl IntoResponse {
    let snap = state.metrics.latest().await;
    Json(snap)
}

async fn containers_handler() -> impl IntoResponse {
    // `list_containers` shells out; run it on the blocking pool.
    let containers = tokio::task::spawn_blocking(list_containers)
        .await
        .unwrap_or_default();
    Json(containers)
}

async fn health_handler(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "hostname": state.hostname,
        "version": state.version,
    }))
}
