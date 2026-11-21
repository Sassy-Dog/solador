//! Fixtures are shaped like the real responses (field names exactly as GitHub
//! sends them) with invented amounts. Nothing here calls the live API.

use chrono::{DateTime, Utc};
use fault::Fault;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::models::{BudgetsPage, UsageReport};
use crate::*;

const USAGE: &str = include_str!("../tests/fixtures/usage_measured.json");
const USAGE_NO_ACTIONS: &str = include_str!("../tests/fixtures/usage_no_actions.json");
const BUDGETS: &str = include_str!("../tests/fixtures/budgets.json");
const TOKEN: &str = "ghp_secret_billing_token_value";
const NOW: &str = "2026-11-20T22:00:00Z";

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(NOW).expect("ts").into()
}

fn items(body: &str) -> Vec<UsageItem> {
    serde_json::from_str::<UsageReport>(body)
        .expect("usage fixture")
        .usage_items
}

fn budgets(body: &str) -> Vec<Budget> {
    serde_json::from_str::<BudgetsPage>(body)
        .expect("budgets fixture")
        .budgets
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn measured(summary: &UsageSummary) -> (&Totals, &[SkuUsage], &[RepoUsage], DateTime<Utc>) {
    match summary {
        UsageSummary::Measured {
            total,
            by_sku,
            by_repo,
            data_through,
            ..
        } => (total, by_sku, by_repo, *data_through),
        UsageSummary::NoUsage => panic!("expected a measured month"),
    }
}

// ---- the fold ----

#[test]
fn net_is_gross_minus_discount_per_item_in_the_fixture() {
    for item in items(USAGE) {
        assert!(
            close(item.net_amount, item.gross_amount - item.discount_amount),
            "{} / {}",
            item.sku,
            item.repository_name
        );
    }
}

#[test]
fn a_measured_month_folds_minutes_and_spend() {
    let summary = summarize_usage(&items(USAGE));
    let (total, by_sku, by_repo, through) = measured(&summary);

    // Minutes exclude the two GigabyteHours storage items and the non-actions
    // product lines.
    assert!(close(total.minutes, 3650.0), "{total:?}");
    // Spend includes storage, which is Actions spend.
    assert!(close(total.gross, 37.25), "{total:?}");
    assert!(close(total.discount, 10.0), "{total:?}");
    assert!(close(total.net, 27.25), "{total:?}");
    assert!(close(total.net, total.gross - total.discount));

    let skus: Vec<&str> = by_sku.iter().map(|s| s.sku.as_str()).collect();
    assert_eq!(
        skus,
        [
            "Actions Linux",
            "Actions Windows",
            "Actions macOS 3-core",
            "Actions storage"
        ]
    );
    let storage = by_sku.iter().find(|s| s.sku == "Actions storage").unwrap();
    assert!(close(storage.totals.minutes, 0.0), "storage is not minutes");
    assert!(close(storage.totals.gross, 0.75));
    let linux = &by_sku[0].totals;
    assert!(close(linux.minutes, 3500.0) && close(linux.net, 19.0));

    // The newest *Actions* item: the later ghas line does not move it.
    assert_eq!(
        through,
        DateTime::parse_from_rfc3339("2026-11-20T21:14:38Z").unwrap()
    );

    let beta = by_repo
        .iter()
        .find(|r| r.repository.as_deref() == Some("beta"))
        .unwrap();
    assert!(close(beta.totals.minutes, 2100.0) && close(beta.totals.net, 24.0));
    // Per-repo spend sums back to the total.
    let net: f64 = by_repo.iter().map(|r| r.totals.net).sum();
    assert!(close(net, total.net));
}

#[test]
fn top_repos_rank_by_minutes_and_keep_the_unnamed_row() {
    let UsageSummary::Measured { top_repos, .. } = summarize_usage(&items(USAGE)) else {
        panic!("measured");
    };
    let names: Vec<Option<&str>> = top_repos.iter().map(|r| r.repository.as_deref()).collect();
    // The empty repositoryName is its own row, not dropped; the storage-only
    // repo has no minutes and is not a "top by minutes" row.
    assert_eq!(names, [Some("beta"), Some("alpha"), None]);
    assert!(top_repos.len() <= TOP_N);
}

#[test]
fn top_repos_are_capped() {
    let mut many = Vec::new();
    for i in 0..(TOP_N + 3) {
        let mut item = items(USAGE)[0].clone();
        item.repository_name = format!("repo-{i}");
        item.quantity = (i + 1) as f64;
        many.push(item);
    }
    let UsageSummary::Measured {
        top_repos, by_repo, ..
    } = summarize_usage(&many)
    else {
        panic!("measured");
    };
    assert_eq!(top_repos.len(), TOP_N);
    assert_eq!(by_repo.len(), TOP_N + 3);
    assert_eq!(top_repos[0].repository.as_deref(), Some("repo-7"));
}

#[test]
fn a_month_with_no_actions_items_is_no_usage_not_zero() {
    let summary = summarize_usage(&items(USAGE_NO_ACTIONS));
    assert_eq!(summary, UsageSummary::NoUsage);
    assert_eq!(summary.total(), None);
    // An empty report is the same statement.
    assert_eq!(summarize_usage(&[]), UsageSummary::NoUsage);
}

#[test]
fn the_org_actions_budget_is_picked_out_and_divided() {
    let status = budget_status(&budgets(BUDGETS), Some(27.25));
    let BudgetStatus::Present {
        amount,
        prevent_further_usage,
        spend: BudgetSpend::Fraction { spend, fraction },
    } = status
    else {
        panic!("present with a fraction");
    };
    // Not the repository-scoped actions budget, not the org copilot one.
    assert!(close(amount, 100.0));
    assert!(prevent_further_usage);
    assert!(close(spend, 27.25));
    assert!(close(fraction, 0.2725));
}

#[test]
fn no_org_actions_budget_is_absent() {
    let only_other: Vec<Budget> = budgets(BUDGETS).into_iter().take(2).collect();
    assert_eq!(budget_status(&only_other, Some(1.0)), BudgetStatus::Absent);
    assert_eq!(budget_status(&[], Some(1.0)), BudgetStatus::Absent);
}

fn with_amount(amount: f64, spend: Option<f64>) -> BudgetStatus {
    let mut list = budgets(BUDGETS);
    list[2].budget_amount = amount;
    budget_status(&list, spend)
}

fn spend_of(status: BudgetStatus) -> BudgetSpend {
    let BudgetStatus::Present { spend, .. } = status else {
        panic!("present");
    };
    spend
}

#[test]
fn spend_over_a_zero_amount_is_a_named_state_not_infinity() {
    let status = with_amount(0.0, Some(27.25));
    assert_eq!(
        spend_of(status.clone()),
        BudgetSpend::OverZeroBudget { spend: 27.25 }
    );
    assert_eq!(status.spend(), Some(27.25));
}

#[test]
fn measured_zero_spend_against_a_zero_amount_is_not_over() {
    let status = with_amount(0.0, Some(0.0));
    assert_eq!(spend_of(status.clone()), BudgetSpend::AtZeroBudget);
    assert_eq!(status.spend(), Some(0.0));
}

#[test]
fn unmeasured_spend_is_unmeasured_whatever_the_amount() {
    for amount in [100.0, 0.0, -5.0] {
        let status = with_amount(amount, None);
        assert_eq!(spend_of(status.clone()), BudgetSpend::Unmeasured);
        assert_eq!(status.spend(), None);
    }
}

#[test]
fn negative_spend_against_a_zero_amount_is_stated_and_never_over() {
    let status = with_amount(0.0, Some(-3.5));
    assert_eq!(
        spend_of(status.clone()),
        BudgetSpend::CreditAgainstZeroBudget { spend: -3.5 }
    );
    assert_eq!(status.spend(), Some(-3.5));
}

#[test]
fn an_amount_too_small_to_divide_into_lands_by_its_spends_sign() {
    // The quotient would overflow to infinity, so it is a zero budget; zero or
    // tiny spend still divides to a finite fraction.
    assert_eq!(
        spend_of(with_amount(f64::MIN_POSITIVE, Some(27.25))),
        BudgetSpend::OverZeroBudget { spend: 27.25 }
    );
    assert_eq!(
        spend_of(with_amount(f64::MIN_POSITIVE, Some(-27.25))),
        BudgetSpend::CreditAgainstZeroBudget { spend: -27.25 }
    );
    assert_eq!(
        spend_of(with_amount(f64::MIN_POSITIVE, Some(0.0))),
        BudgetSpend::Fraction {
            spend: 0.0,
            fraction: 0.0
        }
    );
}

#[test]
fn a_negative_amount_compares_nothing() {
    let status = with_amount(-5.0, Some(27.25));
    assert_eq!(spend_of(status.clone()), BudgetSpend::Incomparable);
    assert_eq!(status.spend(), None);
}

#[test]
fn no_fraction_is_ever_infinite_or_nan() {
    for amount in [100.0, 0.0, -5.0, f64::MIN_POSITIVE] {
        for spend in [
            Some(0.0),
            Some(27.25),
            Some(f64::NAN),
            Some(f64::INFINITY),
            None,
        ] {
            if let BudgetSpend::Fraction { fraction, .. } = spend_of(with_amount(amount, spend)) {
                assert!(fraction.is_finite(), "{amount} {spend:?}");
            }
        }
    }
}

#[test]
fn summarize_names_the_month_from_now() {
    let s = summarize(&items(USAGE), &budgets(BUDGETS), now());
    assert_eq!(s.period, (2026, 11));
    assert!(matches!(s.usage, UsageSummary::Measured { .. }));
    assert!(matches!(s.budget, BudgetStatus::Present { .. }));
    let none = summarize(&items(USAGE_NO_ACTIONS), &budgets(BUDGETS), now());
    assert_eq!(none.usage, UsageSummary::NoUsage);
    assert!(matches!(
        none.budget,
        BudgetStatus::Present {
            spend: BudgetSpend::Unmeasured,
            ..
        }
    ));
}

// ---- the client ----

fn client(server: &MockServer) -> ActionsBillingClient {
    ActionsBillingClient::with_base_url(&server.uri(), TOKEN).expect("client")
}

fn json(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body, "application/json")
}

#[tokio::test]
async fn usage_read_sends_the_documented_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/organizations/example-org/settings/billing/usage"))
        .and(query_param("year", "2026"))
        .and(query_param("month", "11"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .and(header("accept", "application/vnd.github+json"))
        .and(header("x-github-api-version", "2022-11-28"))
        .respond_with(json(USAGE))
        .expect(1)
        .mount(&server)
        .await;
    let got = client(&server)
        .usage_items("example-org", 2026, 11)
        .await
        .expect("items");
    assert_eq!(got.len(), 9);
}

#[tokio::test]
async fn the_org_is_percent_encoded_in_the_path() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/organizations/a%2Fb%20c/settings/billing/usage"))
        .respond_with(json(USAGE))
        .expect(1)
        .mount(&server)
        .await;
    client(&server)
        .usage_items("a/b c", 2026, 11)
        .await
        .expect("items");
    // `.` and `..` would be resolved away by URL normalisation even when
    // encoded, walking out of /organizations/, so they are refused unsent.
    let server = MockServer::start().await;
    for org in ["", ".", "..", "  "] {
        assert_eq!(
            client(&server).usage_items(org, 2026, 11).await,
            Err(ActionsBillingError::NotFound),
            "{org:?}"
        );
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn month_to_date_reads_both_and_folds() {
    let server = MockServer::start().await;
    Mock::given(path("/organizations/example-org/settings/billing/usage"))
        .respond_with(json(USAGE))
        .mount(&server)
        .await;
    Mock::given(path("/organizations/example-org/settings/billing/budgets"))
        .respond_with(json(BUDGETS))
        .mount(&server)
        .await;
    let read = client(&server).month_to_date("example-org", now()).await;
    assert_eq!(read.period, (2026, 11));
    assert!(matches!(read.usage, Ok(UsageSummary::Measured { .. })));
    let Ok(BudgetStatus::Present {
        spend: BudgetSpend::Fraction { fraction, .. },
        ..
    }) = read.budget
    else {
        panic!("budget");
    };
    assert!(close(fraction, 0.2725));
}

#[tokio::test]
async fn a_failed_budget_read_never_hides_the_usage_read() {
    let server = MockServer::start().await;
    Mock::given(path("/organizations/example-org/settings/billing/usage"))
        .respond_with(json(USAGE))
        .mount(&server)
        .await;
    Mock::given(path("/organizations/example-org/settings/billing/budgets"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    let read = client(&server).month_to_date("example-org", now()).await;
    assert!(matches!(read.usage, Ok(UsageSummary::Measured { .. })));
    assert_eq!(read.budget, Err(ActionsBillingError::Forbidden));
}

#[tokio::test]
async fn a_failed_usage_read_leaves_the_budget_spend_unmeasured() {
    let server = MockServer::start().await;
    Mock::given(path("/organizations/example-org/settings/billing/usage"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    Mock::given(path("/organizations/example-org/settings/billing/budgets"))
        .respond_with(json(BUDGETS))
        .mount(&server)
        .await;
    let read = client(&server).month_to_date("example-org", now()).await;
    assert_eq!(
        read.usage,
        Err(ActionsBillingError::VendorFailure { status: 500 })
    );
    assert!(matches!(
        read.budget,
        Ok(BudgetStatus::Present {
            spend: BudgetSpend::Unmeasured,
            ..
        })
    ));
}

fn budgets_page(next: bool) -> String {
    let mut v: serde_json::Value = serde_json::from_str(BUDGETS).unwrap();
    v["has_next_page"] = next.into();
    v.to_string()
}

#[tokio::test]
async fn budgets_follow_has_next_page() {
    let server = MockServer::start().await;
    let route = "/organizations/example-org/settings/billing/budgets";
    Mock::given(path(route))
        .and(query_param("page", "1"))
        .respond_with(json(&budgets_page(true)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path(route))
        .and(query_param("page", "2"))
        .respond_with(json(&budgets_page(false)))
        .expect(1)
        .mount(&server)
        .await;
    let all = client(&server).budgets("example-org").await.expect("all");
    assert_eq!(all.len(), 6, "both pages, not page 1 only");
}

#[tokio::test]
async fn a_listing_that_never_ends_is_refused_not_truncated() {
    let server = MockServer::start().await;
    Mock::given(path("/organizations/example-org/settings/billing/budgets"))
        .respond_with(json(&budgets_page(true)))
        .mount(&server)
        .await;
    let err = client(&server)
        .budgets("example-org")
        .await
        .expect_err("truncated");
    assert_eq!(err, ActionsBillingError::BudgetsTruncated);
    assert_eq!(err.fault(), Fault::Undecodable);
}

async fn status_error(status: u16, rate_header: Option<&str>, body: &str) -> ActionsBillingError {
    let server = MockServer::start().await;
    let mut response = ResponseTemplate::new(status).set_body_string(body);
    if let Some(v) = rate_header {
        response = response.insert_header("x-ratelimit-remaining", v);
    }
    Mock::given(method("GET"))
        .respond_with(response)
        .mount(&server)
        .await;
    client(&server)
        .usage_items("example-org", 2026, 11)
        .await
        .expect_err("an error status")
}

#[tokio::test]
async fn every_status_is_classified() {
    let cases: [(u16, Option<&str>, Fault); 8] = [
        (401, None, Fault::CredentialRejected),
        (403, None, Fault::CredentialRejected),
        (403, Some("5"), Fault::CredentialRejected),
        (403, Some("0"), Fault::RateLimited),
        (404, None, Fault::NotFound),
        (429, None, Fault::RateLimited),
        (503, None, Fault::VendorFailure),
        (418, None, Fault::Unexpected),
    ];
    for (status, header, want) in cases {
        let err = status_error(status, header, "{}").await;
        assert_eq!(err.fault(), want, "HTTP {status} / {header:?}");
    }
    assert_eq!(
        status_error(403, None, "{}").await,
        ActionsBillingError::Forbidden
    );
    assert_eq!(
        status_error(401, None, "{}").await,
        ActionsBillingError::Unauthorized
    );
    assert_eq!(
        status_error(403, Some("0"), "{}").await,
        ActionsBillingError::RateLimited
    );
}

#[tokio::test]
async fn an_undecodable_body_is_classified() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(json("<html>not json</html>"))
        .mount(&server)
        .await;
    let c = client(&server);
    assert_eq!(
        c.usage_items("example-org", 2026, 11).await,
        Err(ActionsBillingError::Undecodable)
    );
    assert_eq!(
        c.budgets("example-org").await,
        Err(ActionsBillingError::Undecodable)
    );
}

#[tokio::test]
async fn a_transport_failure_is_unreachable() {
    let c = ActionsBillingClient::with_base_url("http://127.0.0.1:1", TOKEN).expect("client");
    let err = c
        .usage_items("example-org", 2026, 11)
        .await
        .expect_err("refused");
    assert_eq!(err, ActionsBillingError::Unreachable);
    assert_eq!(err.fault(), Fault::Unreachable);
}

#[test]
fn a_blank_token_yields_no_client() {
    assert!(ActionsBillingClient::new("").is_none());
    assert!(ActionsBillingClient::new("  ").is_none());
    assert!(ActionsBillingClient::new(TOKEN).is_some());
}

// ---- words ----

#[test]
fn the_forbidden_sentence_names_the_permission_and_the_admin() {
    let msg = ActionsBillingError::Forbidden.user_message();
    assert!(
        msg.starts_with(&Fault::CredentialRejected.message("GitHub")),
        "{msg}"
    );
    assert!(msg.contains("Administration: read"), "{msg}");
    assert!(msg.contains("org admin"), "{msg}");
    let msg = ActionsBillingError::Unauthorized.user_message();
    assert!(msg.contains("Administration: read"), "{msg}");
    assert!(msg.contains("org admin"), "{msg}");
    assert!(ActionsBillingError::Forbidden.is_auth_failure());
    assert!(ActionsBillingError::Unauthorized.is_auth_failure());
    assert!(!ActionsBillingError::NotFound.is_auth_failure());
}

#[test]
fn messages_come_from_the_vocabulary() {
    assert_eq!(
        ActionsBillingError::RateLimited.user_message(),
        Fault::RateLimited.message("GitHub")
    );
    assert_eq!(
        ActionsBillingError::Unreachable.user_message(),
        Fault::Unreachable.message("GitHub")
    );
    assert_eq!(
        ActionsBillingError::Undecodable.user_message(),
        Fault::Undecodable.message("GitHub")
    );
    assert_eq!(
        ActionsBillingError::VendorFailure { status: 502 }.user_message(),
        Fault::VendorFailure.message("GitHub")
    );
    assert!(ActionsBillingError::NotFound
        .user_message()
        .contains("enhanced billing"));
}

// ---- the token ----

#[tokio::test]
async fn the_token_never_appears_in_debug_or_error_output() {
    let server = MockServer::start().await;
    let c = client(&server);
    assert!(
        !format!("{c:?}").contains(TOKEN),
        "client Debug leaked the token"
    );
    assert!(
        !format!("{c:#?}").contains(TOKEN),
        "client pretty Debug leaked the token"
    );

    // A server that echoes the token back must not reach any error output.
    for status in [401u16, 403, 404, 429, 500, 418] {
        let err = status_error(status, None, TOKEN).await;
        for (which, text) in [
            ("Display", format!("{err}")),
            ("Debug", format!("{err:?}")),
            ("user message", err.user_message()),
        ] {
            // The message names the output, never the output itself.
            assert!(
                !text.contains(TOKEN),
                "HTTP {status}: {which} leaked the token"
            );
        }
    }
    let dead = ActionsBillingClient::with_base_url("http://127.0.0.1:1", TOKEN).expect("c");
    let err = dead.usage_items("o", 2026, 11).await.expect_err("refused");
    assert!(
        !format!("{err} {err:?} {}", err.user_message()).contains(TOKEN),
        "an unreachable error leaked the token"
    );
    let truncated = ActionsBillingError::BudgetsTruncated;
    assert!(
        !format!("{truncated:?}").contains(TOKEN),
        "BudgetsTruncated Debug leaked the token"
    );
}

#[tokio::test]
async fn a_missing_key_the_summary_decides_on_is_undecodable() {
    let mut item: serde_json::Value = serde_json::from_str(USAGE).unwrap();
    item["usageItems"][0]
        .as_object_mut()
        .unwrap()
        .remove("unitType");
    let mut no_prevent: serde_json::Value = serde_json::from_str(BUDGETS).unwrap();
    no_prevent["budgets"][2]
        .as_object_mut()
        .unwrap()
        .remove("prevent_further_usage");
    let mut no_next: serde_json::Value = serde_json::from_str(BUDGETS).unwrap();
    no_next.as_object_mut().unwrap().remove("has_next_page");

    let server = MockServer::start().await;
    let base = "/organizations/example-org/settings/billing";
    Mock::given(path(format!("{base}/usage")))
        .respond_with(json(&item.to_string()))
        .mount(&server)
        .await;
    assert_eq!(
        client(&server).usage_items("example-org", 2026, 11).await,
        Err(ActionsBillingError::Undecodable)
    );
    for body in [no_prevent, no_next] {
        let server = MockServer::start().await;
        Mock::given(path(format!("{base}/budgets")))
            .respond_with(json(&body.to_string()))
            .mount(&server)
            .await;
        assert_eq!(
            client(&server).budgets("example-org").await,
            Err(ActionsBillingError::Undecodable)
        );
    }
}
