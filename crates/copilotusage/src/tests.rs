//! Fixtures are shaped like the documented responses (field names exactly as
//! GitHub sends them) with invented amounts. Nothing here calls the live API.

use chrono::{DateTime, Utc};
use fault::Fault;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::models::UsageReport;
use crate::*;

const ORG_CREDITS: &str = include_str!("../tests/fixtures/ai_credit_org.json");
const PREMIUM: &str = include_str!("../tests/fixtures/premium_request.json");
const PARTIAL: &str = include_str!("../tests/fixtures/ai_credit_partial.json");
const EMPTY: &str = include_str!("../tests/fixtures/empty.json");
const TOKEN: &str = "ghp_secret_copilot_token_value";
const NOW: &str = "2026-11-20T22:00:00Z";

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(NOW).expect("ts").into()
}

fn items(body: &str) -> Vec<UsageItem> {
    serde_json::from_str::<UsageReport>(body)
        .expect("fixture")
        .usage_items
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn json(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.to_owned(), "application/json")
}

async fn client(server: &MockServer) -> CopilotUsageClient {
    CopilotUsageClient::with_base_url(&server.uri(), TOKEN).expect("client")
}

// ---- the fold ----

#[test]
fn a_report_folds_credits_included_and_billed() {
    let usage = summarize(&items(ORG_CREDITS));
    assert!(close(usage.used.expect("used"), 1100.0));
    assert!(close(usage.included.expect("included"), 700.0));
    assert!(close(usage.billed_usd.expect("billed"), 4.0));
}

#[test]
fn the_top_three_models_are_the_largest_by_gross_quantity() {
    let usage = summarize(&items(ORG_CREDITS));
    let names: Vec<&str> = usage.top_models.iter().map(|m| m.model.as_str()).collect();
    // model-alpha appears twice and is summed (600), then beta 300, gamma 120;
    // delta (80) is cut.
    assert_eq!(names, ["model-alpha", "model-beta", "model-gamma"]);
    assert!(close(usage.top_models[0].quantity, 600.0));
}

#[test]
fn a_missing_number_is_unmeasured_not_zero() {
    let usage = summarize(&items(PARTIAL));
    // The second item has no discountQuantity: the total is unknown, not 50.
    assert_eq!(usage.included, None);
    // Every item carries grossQuantity and netAmount.
    assert!(close(usage.used.expect("used"), 70.0));
    assert!(close(usage.billed_usd.expect("billed"), 0.2));
}

#[test]
fn an_empty_month_is_a_measured_zero() {
    let usage = summarize(&items(EMPTY));
    assert_eq!(usage.used, Some(0.0));
    assert_eq!(usage.included, Some(0.0));
    assert_eq!(usage.billed_usd, Some(0.0));
    assert!(usage.top_models.is_empty());
}

#[test]
fn an_item_with_no_model_is_summed_but_not_attributed() {
    let body = r#"{"usageItems":[{"grossQuantity":5,"discountQuantity":0,"netAmount":0.05}]}"#;
    let usage = summarize(&items(body));
    assert_eq!(usage.used, Some(5.0));
    assert!(usage.top_models.is_empty());
}

// ---- the reads ----

#[tokio::test]
async fn the_org_read_hits_the_documented_paths_with_the_documented_headers() {
    let server = MockServer::start().await;
    for report in ["ai_credit", "premium_request"] {
        Mock::given(method("GET"))
            .and(path(format!(
                "/organizations/example-org/settings/billing/{report}/usage"
            )))
            .and(query_param("year", "2026"))
            .and(query_param("month", "11"))
            .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
            .and(header("x-github-api-version", "2026-03-10"))
            .respond_with(json(if report == "ai_credit" {
                ORG_CREDITS
            } else {
                PREMIUM
            }))
            .expect(1)
            .mount(&server)
            .await;
    }
    let read = client(&server)
        .await
        .month_to_date(Billed::Org("example-org"), now())
        .await;
    assert_eq!(read.period, (2026, 11));
    assert!(read.ai_credit.is_ok());
    let premium = read.premium_request.expect("ok").expect("a line");
    assert!(close(premium.used.expect("used"), 40.0));
}

#[tokio::test]
async fn the_personal_read_hits_the_users_paths() {
    let server = MockServer::start().await;
    for report in ["ai_credit", "premium_request"] {
        Mock::given(method("GET"))
            .and(path(format!(
                "/users/example-user/settings/billing/{report}/usage"
            )))
            .respond_with(json(EMPTY))
            .expect(1)
            .mount(&server)
            .await;
    }
    let read = client(&server)
        .await
        .month_to_date(Billed::User("example-user"), now())
        .await;
    assert_eq!(read.ai_credit.expect("ok").used, Some(0.0));
    assert_eq!(read.premium_request, Ok(None));
}

#[tokio::test]
async fn a_retired_premium_endpoint_gives_no_line_and_no_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/organizations/example-org/settings/billing/ai_credit/usage",
        ))
        .respond_with(json(ORG_CREDITS))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/organizations/example-org/settings/billing/premium_request/usage",
        ))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let read = client(&server)
        .await
        .month_to_date(Billed::Org("example-org"), now())
        .await;
    assert!(read.ai_credit.is_ok());
    assert_eq!(read.premium_request, Ok(None));
}

#[tokio::test]
async fn a_failed_premium_read_does_not_hide_the_credits() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/organizations/example-org/settings/billing/ai_credit/usage",
        ))
        .respond_with(json(ORG_CREDITS))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/organizations/example-org/settings/billing/premium_request/usage",
        ))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let read = client(&server)
        .await
        .month_to_date(Billed::Org("example-org"), now())
        .await;
    assert!(read.ai_credit.is_ok());
    let error = read.premium_request.expect_err("a 500");
    assert_eq!(error.kind, ErrorKind::VendorFailure { status: 500 });
}

#[tokio::test]
async fn the_login_is_percent_encoded_into_one_segment() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(json(EMPTY))
        .mount(&server)
        .await;
    let c = client(&server).await;
    let _ = c.month_to_date(Billed::Org("a/b?c"), now()).await;
    let requests = server.received_requests().await.expect("recorded");
    assert!(!requests.is_empty());
    for request in requests {
        assert_eq!(
            request.url.path(),
            if request.url.path().contains("premium") {
                "/organizations/a%2Fb%3Fc/settings/billing/premium_request/usage"
            } else {
                "/organizations/a%2Fb%3Fc/settings/billing/ai_credit/usage"
            }
        );
    }
}

#[tokio::test]
async fn a_dot_segment_login_is_refused_without_a_request() {
    let server = MockServer::start().await;
    let c = client(&server).await;
    for login in ["", " ", ".", ".."] {
        let read = c.month_to_date(Billed::User(login), now()).await;
        assert_eq!(
            read.ai_credit.expect_err("refused").kind,
            ErrorKind::NotFound
        );
    }
    assert!(server
        .received_requests()
        .await
        .expect("recorded")
        .is_empty());
}

#[test]
fn a_blank_token_makes_no_client() {
    assert!(CopilotUsageClient::new("").is_none());
    assert!(CopilotUsageClient::new("   ").is_none());
    assert!(CopilotUsageClient::new(TOKEN).is_some());
}

// ---- errors ----

async fn one_status(status: u16, billed: Billed<'_>, rate_zero: bool) -> CopilotUsageError {
    let server = MockServer::start().await;
    let mut response = ResponseTemplate::new(status).set_body_string(format!("echo {TOKEN}"));
    if rate_zero {
        response = response.insert_header("x-ratelimit-remaining", "0");
    }
    Mock::given(method("GET"))
        .respond_with(response)
        .mount(&server)
        .await;
    client(&server)
        .await
        .month_to_date(billed, now())
        .await
        .ai_credit
        .expect_err("a failing status")
}

#[tokio::test]
async fn statuses_classify() {
    let org = Billed::Org("example-org");
    assert_eq!(
        one_status(401, org, false).await.kind,
        ErrorKind::Unauthorized
    );
    assert_eq!(one_status(403, org, false).await.kind, ErrorKind::Forbidden);
    assert_eq!(
        one_status(403, org, true).await.kind,
        ErrorKind::RateLimited
    );
    assert_eq!(one_status(404, org, false).await.kind, ErrorKind::NotFound);
    assert_eq!(
        one_status(429, org, false).await.kind,
        ErrorKind::RateLimited
    );
    assert_eq!(
        one_status(502, org, false).await.kind,
        ErrorKind::VendorFailure { status: 502 }
    );
    assert_eq!(
        one_status(418, org, false).await.kind,
        ErrorKind::UnexpectedStatus { status: 418 }
    );
}

#[tokio::test]
async fn a_body_that_does_not_parse_is_undecodable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(json("{\"nope\": 1}"))
        .mount(&server)
        .await;
    let read = client(&server)
        .await
        .month_to_date(Billed::User("example-user"), now())
        .await;
    assert_eq!(
        read.ai_credit.expect_err("undecodable").kind,
        ErrorKind::Undecodable
    );
}

#[tokio::test]
async fn a_user_403_names_plan_read_and_an_org_403_names_administration_read() {
    let user = one_status(403, Billed::User("example-user"), false).await;
    assert!(user.user_message().contains("Plan: read"));
    assert!(!user.user_message().contains("Administration"));
    let org = one_status(403, Billed::Org("example-org"), false).await;
    assert!(org.user_message().contains("Administration: read"));
    assert!(org.user_message().contains("org admin"));
    assert!(!org.user_message().contains("Plan: read"));
}

#[tokio::test]
async fn a_404_names_the_org_or_the_username() {
    let user = one_status(404, Billed::User("example-user"), false).await;
    assert!(user.user_message().contains("username"));
    let org = one_status(404, Billed::Org("example-org"), false).await;
    assert!(org.user_message().contains("enhanced billing"));
}

#[tokio::test]
async fn errors_carry_no_url_token_or_body() {
    for (status, billed) in [
        (401, Billed::Org("example-org")),
        (403, Billed::User("example-user")),
        (500, Billed::Org("example-org")),
        (418, Billed::User("example-user")),
    ] {
        let error = one_status(status, billed, false).await;
        for text in [
            error.to_string(),
            format!("{error:?}"),
            error.user_message(),
        ] {
            assert!(!text.contains("ghp_"), "token leaked");
            assert!(!text.contains("echo"), "body leaked");
            assert!(!text.contains("http://"), "url leaked");
            assert!(!text.contains("example-"), "login leaked");
        }
    }
}

#[test]
fn the_client_debug_redacts_the_token() {
    let c = CopilotUsageClient::new(TOKEN).expect("client");
    let text = format!("{c:?}");
    assert!(!text.contains("ghp_"));
    assert!(text.contains("<redacted>"));
}

#[test]
fn faults_map_onto_the_vocabulary() {
    let e = |kind| CopilotUsageError::new(Level::Org, kind);
    assert_eq!(
        e(ErrorKind::Unauthorized).fault(),
        Fault::CredentialRejected
    );
    assert_eq!(e(ErrorKind::Forbidden).fault(), Fault::CredentialRejected);
    assert_eq!(e(ErrorKind::NotFound).fault(), Fault::NotFound);
    assert_eq!(e(ErrorKind::RateLimited).fault(), Fault::RateLimited);
    assert_eq!(
        e(ErrorKind::VendorFailure { status: 500 }).fault(),
        Fault::VendorFailure
    );
    assert_eq!(
        e(ErrorKind::UnexpectedStatus { status: 418 }).fault(),
        Fault::Unexpected
    );
    assert_eq!(e(ErrorKind::Undecodable).fault(), Fault::Undecodable);
    assert_eq!(e(ErrorKind::Unreachable).fault(), Fault::Unreachable);
    assert!(e(ErrorKind::Forbidden).is_auth_failure());
    assert!(!e(ErrorKind::NotFound).is_auth_failure());
}
