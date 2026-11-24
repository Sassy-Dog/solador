//! The seat read. The fixture is shaped like the endpoint's body with invented
//! figures and names, extra fields included (the real body carries an
//! organization list, enterprise ids and tracking ids). Nothing here calls the
//! live API.

use fault::Fault;
use serde_json::{json, Value};
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use crate::seat::{interpret, SeatBody};
use crate::*;

const SEAT: &str = include_str!("../tests/fixtures/seat.json");
const TOKEN: &str = "github_pat_secret_seat_token_value";
const USER: &str = "example-user";

fn body(v: &Value) -> SeatBody {
    serde_json::from_value(v.clone()).expect("decodes")
}

fn fixture() -> Value {
    serde_json::from_str(SEAT).expect("fixture")
}

/// The fixture with `premium_interactions` replaced by `patch`'s keys.
fn with_quota(patch: &Value) -> Value {
    let mut v = fixture();
    let quota = v["quota_snapshots"]["premium_interactions"]
        .as_object_mut()
        .expect("quota");
    for (k, val) in patch.as_object().expect("patch") {
        if val.is_null() {
            quota.remove(k);
        } else {
            quota.insert(k.clone(), val.clone());
        }
    }
    v
}

fn seat_of(v: &Value) -> Result<Seat, SeatError> {
    interpret(&body(v), USER)
}

fn kind(v: &Value) -> SeatErrorKind {
    seat_of(v).expect_err("a refusal").kind
}

async fn client(server: &MockServer) -> CopilotUsageClient {
    CopilotUsageClient::with_base_url(&server.uri(), TOKEN).expect("client")
}

// ---- the request ----

#[tokio::test]
async fn the_seat_is_requested_with_a_bearer_token_and_no_version_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/copilot_internal/user"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_raw(SEAT, "application/json"))
        .expect(1)
        .mount(&server)
        .await;
    let seat = client(&server).await.seat(USER).await.expect("a seat");
    assert_eq!(seat.plan.as_deref(), Some("business"));
    let requests = server.received_requests().await.expect("recorded");
    assert_eq!(requests.len(), 1);
    let request: &Request = &requests[0];
    assert!(
        request.headers.get("x-github-api-version").is_none(),
        "no version header on an undocumented endpoint"
    );
    assert!(request.url.query().is_none(), "no query string");
}

#[tokio::test]
async fn the_token_goes_in_a_header_never_the_url() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(header_exists("authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(SEAT, "application/json"))
        .mount(&server)
        .await;
    let _ = client(&server).await.seat(USER).await;
    for request in server.received_requests().await.expect("recorded") {
        assert!(!request.url.as_str().contains("github_pat_"));
    }
}

// ---- the decode ----

#[test]
fn the_body_keeps_only_the_named_fields() {
    let parsed: SeatBody = serde_json::from_str(SEAT).expect("decodes");
    // The model has no field for the organization list, the enterprise ids or
    // the tracking id, so nothing of them survives the decode.
    let text = format!("{parsed:?}");
    for dropped in [
        "example-org",
        "Example Org",
        "example-enterprise",
        "424242",
        "00000000",
        "percent_remaining",
    ] {
        assert!(!text.contains(dropped), "{dropped} was kept");
    }
    let seat = interpret(&parsed, USER).expect("seat");
    assert_eq!(seat.plan.as_deref(), Some("business"));
}

// ---- the figures ----

#[test]
fn fractional_credits_convert_to_usd_exactly() {
    let seat = seat_of(&fixture()).expect("seat");
    // 1234.56 credits at $0.01 is $12.3456, held at exponent 5.
    assert_eq!(seat.used.usd_minor(), 1_234_560);
    assert_eq!(USD_EXPONENT, 5);
    assert_eq!(
        seat.allowance,
        Allowance::Capped(Credits::from_f64(3900.0).unwrap())
    );
    match seat.allowance {
        Allowance::Capped(limit) => assert_eq!(limit.usd_minor(), 3_900_000),
        Allowance::Unlimited => panic!("capped"),
    }
}

#[test]
fn equal_figures_stay_equal_whatever_the_float_says() {
    let a = Credits::from_f64(0.1 + 0.2).unwrap();
    let b = Credits::from_f64(0.3).unwrap();
    assert_eq!(a, b);
}

#[test]
fn unlimited_has_no_limit_and_needs_no_entitlement() {
    let v = with_quota(&json!({"unlimited": true, "entitlement": null}));
    assert_eq!(seat_of(&v).expect("seat").allowance, Allowance::Unlimited);
}

#[test]
fn has_quota_false_with_valid_figures_still_gives_the_figures() {
    let v = with_quota(&json!({"has_quota": false}));
    let seat = seat_of(&v).expect("figures");
    assert_eq!(seat.used.usd_minor(), 1_234_560);
}

#[test]
fn the_reset_comes_from_the_timestamp_then_the_date() {
    let seat = seat_of(&fixture()).expect("seat");
    assert_eq!(seat.resets_at, Some(1_796_083_200)); // 2026-12-01T00:00:00Z
    let mut v = fixture();
    v["quota_reset_date_utc"] = Value::Null;
    assert_eq!(seat_of(&v).expect("seat").resets_at, Some(1_796_083_200));
    v["quota_reset_date"] = json!("not a date");
    assert_eq!(seat_of(&v).expect("seat").resets_at, None);
}

#[test]
fn a_blank_or_oversized_plan_name_is_no_plan() {
    for plan in [
        json!(""),
        json!("   "),
        json!("x".repeat(41)),
        json!("a\nb"),
    ] {
        let mut v = fixture();
        v["copilot_plan"] = plan;
        assert_eq!(seat_of(&v).expect("seat").plan, None);
    }
    let mut v = fixture();
    v["copilot_plan"] = Value::Null;
    assert_eq!(seat_of(&v).expect("seat").plan, None);
}

// ---- never a fabricated zero ----

#[test]
fn a_plan_not_billed_in_credits_is_never_dollars() {
    for patch in [
        json!({"token_based_billing": false}),
        json!({"token_based_billing": null}),
    ] {
        assert_eq!(kind(&with_quota(&patch)), SeatErrorKind::NotCreditBilled);
    }
}

#[test]
fn a_missing_figure_is_not_a_zero() {
    assert_eq!(
        kind(&with_quota(&json!({"credits_used": null}))),
        SeatErrorKind::MissingFigure
    );
    // No limit stated and not unlimited: the cap is unknown, not absent.
    assert_eq!(
        kind(&with_quota(&json!({"entitlement": null}))),
        SeatErrorKind::MissingFigure
    );
    let mut v = fixture();
    v["quota_snapshots"] = Value::Null;
    assert_eq!(kind(&v), SeatErrorKind::MissingFigure);
}

#[test]
fn a_negative_or_non_finite_figure_is_refused() {
    assert_eq!(
        kind(&with_quota(&json!({"credits_used": -1}))),
        SeatErrorKind::InvalidFigure
    );
    assert_eq!(
        kind(&with_quota(&json!({"entitlement": -0.5}))),
        SeatErrorKind::InvalidFigure
    );
    assert_eq!(Credits::from_f64(f64::NAN), None);
    assert_eq!(Credits::from_f64(f64::INFINITY), None);
    assert_eq!(Credits::from_f64(f64::NEG_INFINITY), None);
    assert_eq!(Credits::from_f64(1.0e13), None);
    assert!(Credits::from_f64(0.0).is_some());
}

// ---- whose figures ----

#[test]
fn the_login_match_is_ascii_case_insensitive() {
    for expected in ["example-user", "EXAMPLE-USER", " Example-User "] {
        assert!(interpret(&body(&fixture()), expected).is_ok(), "{expected}");
    }
}

#[test]
fn another_accounts_figures_are_never_returned() {
    assert_eq!(
        interpret(&body(&fixture()), "someone-else")
            .expect_err("mismatch")
            .kind,
        SeatErrorKind::WrongAccount
    );
    let mut v = fixture();
    v["login"] = Value::Null;
    assert_eq!(kind(&v), SeatErrorKind::WrongAccount);
    let message = interpret(&body(&fixture()), "someone-else")
        .expect_err("mismatch")
        .user_message();
    assert!(message.contains("different account"));
}

// ---- failures ----

async fn one_status(status: u16) -> SeatError {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(status).set_body_string(format!("echo {TOKEN}")))
        .mount(&server)
        .await;
    client(&server).await.seat(USER).await.expect_err("failing")
}

#[tokio::test]
async fn statuses_map_to_the_vocabulary_like_the_billing_reads() {
    assert_eq!(one_status(401).await.fault(), Fault::CredentialRejected);
    assert_eq!(one_status(403).await.fault(), Fault::CredentialRejected);
    assert_eq!(one_status(404).await.fault(), Fault::NotFound);
    assert_eq!(one_status(429).await.fault(), Fault::RateLimited);
    assert_eq!(one_status(503).await.fault(), Fault::VendorFailure);
    assert_eq!(one_status(418).await.fault(), Fault::Unexpected);
}

#[tokio::test]
async fn a_403_names_user_permissions_plan_read() {
    let message = one_status(403).await.user_message();
    assert!(message.contains("User permissions"), "{message}");
    assert!(message.contains("Plan: Read"), "{message}");
    assert!(!message.contains("Administration"));
}

#[tokio::test]
async fn a_body_that_does_not_decode_is_undecodable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>"))
        .mount(&server)
        .await;
    let error = client(&server).await.seat(USER).await.expect_err("html");
    assert_eq!(error.kind, SeatErrorKind::Request(ErrorKind::Undecodable));
    assert_eq!(error.fault(), Fault::Undecodable);
    assert!(!error.keeps_last_figure());
}

#[test]
fn only_a_failure_that_says_nothing_about_the_figure_keeps_it() {
    let keeps = |k| SeatError::new(k).keeps_last_figure();
    assert!(keeps(SeatErrorKind::Request(ErrorKind::Unreachable)));
    assert!(keeps(SeatErrorKind::Request(ErrorKind::RateLimited)));
    assert!(keeps(SeatErrorKind::Request(ErrorKind::Forbidden)));
    assert!(!keeps(SeatErrorKind::NotCreditBilled));
    assert!(!keeps(SeatErrorKind::MissingFigure));
    assert!(!keeps(SeatErrorKind::InvalidFigure));
    assert!(!keeps(SeatErrorKind::WrongAccount));
}

#[tokio::test]
async fn errors_carry_no_url_token_or_body() {
    let mut errors = vec![
        SeatError::new(SeatErrorKind::NotCreditBilled),
        SeatError::new(SeatErrorKind::MissingFigure),
        SeatError::new(SeatErrorKind::InvalidFigure),
        SeatError::new(SeatErrorKind::WrongAccount),
    ];
    for status in [401, 403, 404, 500, 418] {
        errors.push(one_status(status).await);
    }
    for error in errors {
        for text in [
            error.to_string(),
            format!("{error:?}"),
            error.user_message(),
        ] {
            assert!(!text.contains("github_pat_"), "token leaked");
            assert!(!text.contains("echo"), "body leaked");
            assert!(!text.contains("http://"), "url leaked");
            assert!(!text.contains("example-"), "login leaked");
        }
    }
}
