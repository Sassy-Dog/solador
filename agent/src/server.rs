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
    // Report the sampler's liveness, not a static "ok". A dead sampler serves
    // frozen numbers; surfacing `degraded` + the sample age lets the dashboard
    // distinguish a live host from one that's silently stuck.
    let stale = state.metrics.is_stale();
    let status = if stale { "degraded" } else { "ok" };
    Json(json!({
        "status": status,
        "hostname": state.hostname,
        "version": state.version,
        "sampleAgeSeconds": state.metrics.sample_age().as_secs(),
        "samplerStale": stale,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{header, Request};
    use tower::ServiceExt; // for `oneshot`

    const TOKEN: &str = "s3cr3t-token";

    fn test_router() -> Router {
        let state = AppState {
            metrics: MetricsState::fresh_for_test(crate::metrics::empty_snapshot()),
            token: Arc::new(TOKEN.to_string()),
            hostname: "test-host".to_string(),
            version: "0.0.0-test",
        };
        build_router(state)
    }

    /// Issue a GET /v1/health with an optional Authorization header.
    async fn health_status(auth: Option<&str>) -> StatusCode {
        let mut builder = Request::builder().uri("/v1/health");
        if let Some(value) = auth {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        let request = builder.body(Body::empty()).unwrap();
        test_router().oneshot(request).await.unwrap().status()
    }

    #[tokio::test]
    async fn missing_authorization_header_is_401() {
        assert_eq!(health_status(None).await, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn wrong_token_is_401() {
        let got = health_status(Some("Bearer not-the-token")).await;
        assert_eq!(got, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn token_without_bearer_scheme_is_401() {
        // A bare token (no `Bearer ` prefix) must not authenticate.
        let got = health_status(Some(TOKEN)).await;
        assert_eq!(got, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn correct_bearer_token_is_200() {
        let got = health_status(Some(&format!("Bearer {TOKEN}"))).await;
        assert_eq!(got, StatusCode::OK);
    }

    #[tokio::test]
    async fn health_reports_ok_for_fresh_sampler() {
        let request = Request::builder()
            .uri("/v1/health")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap();
        let response = test_router().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["status"], "ok");
        assert_eq!(v["samplerStale"], false);
        assert!(v["sampleAgeSeconds"].is_u64());
    }

    #[tokio::test]
    async fn health_reports_degraded_when_sampler_is_stale() {
        let state = AppState {
            metrics: MetricsState::fresh_for_test(crate::metrics::empty_snapshot()),
            token: Arc::new(TOKEN.to_string()),
            hostname: "test-host".to_string(),
            version: "0.0.0-test",
        };
        // Force the sampler stale, as if the background task had died.
        state
            .metrics
            .set_sample_age_for_test(std::time::Duration::from_secs(3600));
        let router = build_router(state);

        let request = Request::builder()
            .uri("/v1/health")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["status"], "degraded");
        assert_eq!(v["samplerStale"], true);
    }

    #[test]
    fn constant_time_eq_matches_equal_slices() {
        assert!(constant_time_eq(b"abc123", b"abc123"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn constant_time_eq_rejects_differing_content() {
        assert!(!constant_time_eq(b"abc123", b"abc124"));
        assert!(!constant_time_eq(b"abc123", b"xbc123"));
    }

    #[test]
    fn constant_time_eq_rejects_differing_length() {
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"abcd", b"abc"));
        assert!(!constant_time_eq(b"", b"x"));
    }
}
