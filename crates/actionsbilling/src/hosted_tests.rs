//! The spike and macOS rules. Every repo name and figure is invented.

use chrono::{DateTime, NaiveDate, Utc};
use wiremock::matchers::{path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::*;

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("date")
}

fn item(date: &str, sku: &str, unit: &str, quantity: f64, repo: &str) -> UsageItem {
    UsageItem {
        date: DateTime::parse_from_rfc3339(date).expect("ts").into(),
        product: "actions".into(),
        sku: sku.into(),
        unit_type: unit.into(),
        quantity,
        gross_amount: 0.0,
        discount_amount: 0.0,
        net_amount: 0.0,
        repository_name: repo.into(),
    }
}

fn linux(d: u32, quantity: f64, repo: &str) -> UsageItem {
    item(
        &format!("2026-11-{d:02}T00:00:00Z"),
        "Actions Linux",
        "Minutes",
        quantity,
        repo,
    )
}

const DEFAULTS: SpikeThresholds = SpikeThresholds {
    multiple: 3.0,
    floor_minutes: 1000.0,
};

/// `repo-a` runs `base` minutes on each of Nov 1..through-1 and `last` on
/// `through`.
fn steady_then(base: f64, last: f64, through: u32) -> HostedDays {
    let mut items: Vec<UsageItem> = (1..through).map(|d| linux(d, base, "repo-a")).collect();
    items.push(linux(through, last, "repo-a"));
    HostedDays::from_items(&items)
}

fn flags(hosted: &HostedDays, through: NaiveDate) -> SpikeReport {
    spike_flags(hosted, &PreviousMonth::NotNeeded, through, DEFAULTS)
}

fn minutes_on(hosted: &HostedDays, d: NaiveDate, repo: &str) -> f64 {
    hosted
        .repos_on(d)
        .find(|(name, _)| *name == repo)
        .map_or(0.0, |(_, m)| m.hosted)
}

#[test]
fn a_newest_day_over_the_floor_and_the_multiple_flags() {
    // Baseline 200/day; 1,000 is 5x and at the floor.
    let report = flags(&steady_then(200.0, 1000.0, 20), day(2026, 11, 20));
    assert_eq!(report.flags.len(), 1);
    let flag = &report.flags[0];
    assert_eq!(flag.repository, "repo-a");
    assert_eq!(flag.day, day(2026, 11, 20));
    assert!((flag.baseline_avg - 200.0).abs() < 1e-9);
    assert!((flag.multiple.unwrap() - 5.0).abs() < 1e-9);
    assert!(report.unknown_days.is_empty());
}

#[test]
fn the_day_before_the_newest_flags_too_and_older_days_do_not() {
    let mut items: Vec<UsageItem> = (1..=20).map(|d| linux(d, 200.0, "repo-a")).collect();
    // A spike on the 18th, then two calm days.
    items[17] = linux(18, 1500.0, "repo-a");
    let hosted = HostedDays::from_items(&items);
    // Through the 19th: the 18th is the day before the newest.
    let report = flags(&hosted, day(2026, 11, 19));
    assert_eq!(report.flags.len(), 1);
    assert_eq!(report.flags[0].day, day(2026, 11, 18));
    // Through the 20th: the 18th is two days back and no longer flags.
    assert!(flags(&hosted, day(2026, 11, 20)).flags.is_empty());
}

#[test]
fn just_under_the_floor_does_not_flag() {
    assert!(flags(&steady_then(100.0, 999.0, 20), day(2026, 11, 20))
        .flags
        .is_empty());
    assert_eq!(
        flags(&steady_then(100.0, 1000.0, 20), day(2026, 11, 20))
            .flags
            .len(),
        1
    );
}

#[test]
fn exactly_the_multiple_does_not_flag_and_a_hair_over_does() {
    // Baseline 1,000/day: 3x is exactly 3,000.
    assert!(flags(&steady_then(1000.0, 3000.0, 20), day(2026, 11, 20))
        .flags
        .is_empty());
    assert_eq!(
        flags(&steady_then(1000.0, 3001.0, 20), day(2026, 11, 20))
            .flags
            .len(),
        1
    );
}

#[test]
fn days_with_no_items_inside_the_read_range_count_as_zero() {
    // Only the newest day has any item: the 7 days before it are a measured 0,
    // so the baseline is 0, a flag, and no ratio is invented for it.
    let hosted = HostedDays::from_items(&[linux(20, 1200.0, "repo-a")]);
    let report = flags(&hosted, day(2026, 11, 20));
    assert_eq!(report.flags.len(), 1);
    assert!(report.flags[0].baseline_avg.abs() < 1e-9);
    assert_eq!(report.flags[0].multiple, None);
}

#[test]
fn storage_and_other_products_never_count_as_minutes() {
    let hosted = HostedDays::from_items(&[
        item(
            "2026-11-20T00:00:00Z",
            "Actions storage",
            "GigabyteHours",
            50_000.0,
            "repo-a",
        ),
        UsageItem {
            product: "packages".into(),
            ..linux(20, 50_000.0, "repo-a")
        },
        linux(20, 10.0, "repo-a"),
    ]);
    assert!(flags(&hosted, day(2026, 11, 20)).flags.is_empty());
    // And the 10 real minutes are all that was counted.
    assert!((minutes_on(&hosted, day(2026, 11, 20), "repo-a") - 10.0).abs() < 1e-9);
}

#[test]
fn every_sku_counts_toward_a_repos_hosted_minutes() {
    let hosted = HostedDays::from_items(&[
        item(
            "2026-11-20T00:00:00Z",
            "Actions Linux",
            "Minutes",
            600.0,
            "repo-a",
        ),
        item(
            "2026-11-20T00:00:00Z",
            "Actions macOS 12-core",
            "Minutes",
            500.0,
            "repo-a",
        ),
    ]);
    assert_eq!(flags(&hosted, day(2026, 11, 20)).flags.len(), 1);
}

#[test]
fn repos_are_judged_on_their_own_baseline() {
    let mut items: Vec<UsageItem> = (1..20).map(|d| linux(d, 2000.0, "busy")).collect();
    items.extend((1..20).map(|d| linux(d, 100.0, "quiet")));
    items.push(linux(20, 2500.0, "busy")); // normal for it
    items.push(linux(20, 1500.0, "quiet")); // 15x its own baseline
    let report = flags(&HostedDays::from_items(&items), day(2026, 11, 20));
    let names: Vec<&str> = report.flags.iter().map(|f| f.repository.as_str()).collect();
    assert_eq!(names, ["quiet"]);
}

#[test]
fn a_window_across_the_month_start_uses_the_previous_month() {
    // Through Nov 3: both checked days' windows reach into October.
    let through = day(2026, 11, 3);
    assert!(needs_previous_month(through));
    assert_eq!(previous_period(through), (2026, 10));
    let october: Vec<UsageItem> = (20..=31)
        .map(|d| {
            item(
                &format!("2026-10-{d:02}T00:00:00Z"),
                "Actions Linux",
                "Minutes",
                300.0,
                "repo-a",
            )
        })
        .collect();
    let mut november: Vec<UsageItem> = (1..=2).map(|d| linux(d, 300.0, "repo-a")).collect();
    november.push(linux(3, 1500.0, "repo-a"));
    let report = spike_flags(
        &HostedDays::from_items(&november),
        &PreviousMonth::Read(HostedDays::from_items(&october)),
        through,
        DEFAULTS,
    );
    assert_eq!(report.flags.len(), 1);
    assert!((report.flags[0].baseline_avg - 300.0).abs() < 1e-9);
    assert!(report.unknown_days.is_empty());
}

#[test]
fn a_window_well_inside_the_month_needs_no_previous_read() {
    assert!(!needs_previous_month(day(2026, 11, 9)));
    assert!(needs_previous_month(day(2026, 11, 8)));
    assert_eq!(previous_period(day(2026, 1, 5)), (2025, 12));
}

#[test]
fn an_unreadable_previous_month_is_unknown_never_no_spike_and_never_a_short_window() {
    let through = day(2026, 11, 3);
    // Big November days and nothing else read: over a shortened window (the days
    // read) or one that counted October as 0 this would flag, and it must do
    // neither.
    let november = vec![
        linux(1, 5000.0, "repo-a"),
        linux(2, 5000.0, "repo-a"),
        linux(3, 6000.0, "repo-a"),
    ];
    let report = spike_flags(
        &HostedDays::from_items(&november),
        &PreviousMonth::Failed(ActionsBillingError::Unreachable),
        through,
        DEFAULTS,
    );
    assert!(report.flags.is_empty());
    assert_eq!(report.unknown_days, [day(2026, 11, 3), day(2026, 11, 2)]);
}

#[test]
fn the_older_checked_day_can_be_unknown_while_the_newer_is_known() {
    // Through Nov 8: the 8th's window is Nov 1..7 (read); the 7th's reaches
    // October 31, which only the previous month holds.
    assert!(needs_previous_month(day(2026, 11, 8)));
    let hosted = HostedDays::from_items(&[linux(8, 1500.0, "repo-a")]);
    let report = spike_flags(
        &hosted,
        &PreviousMonth::Failed(ActionsBillingError::Forbidden),
        day(2026, 11, 8),
        DEFAULTS,
    );
    assert_eq!(report.unknown_days, [day(2026, 11, 7)]);
    assert_eq!(report.flags.len(), 1);
    assert_eq!(report.flags[0].day, day(2026, 11, 8));
}

#[test]
fn a_changed_threshold_changes_the_verdict_over_the_same_fold() {
    let hosted = steady_then(400.0, 1500.0, 20); // 3.75x
    let verdict = |multiple, floor_minutes| {
        spike_flags(
            &hosted,
            &PreviousMonth::NotNeeded,
            day(2026, 11, 20),
            SpikeThresholds {
                multiple,
                floor_minutes,
            },
        )
        .flags
        .len()
    };
    assert_eq!(verdict(3.0, 1000.0), 1);
    assert_eq!(verdict(4.0, 1000.0), 0);
    assert_eq!(verdict(3.0, 2000.0), 0);
}

// ---- macOS on self-hosted ----

fn macos(d: u32, quantity: f64, repo: &str) -> UsageItem {
    item(
        &format!("2026-11-{d:02}T00:00:00Z"),
        "Actions macOS 3-core",
        "Minutes",
        quantity,
        repo,
    )
}

fn names(listed: &[&str]) -> Vec<String> {
    listed.iter().map(|n| (*n).to_owned()).collect()
}

fn macos_for(items: &[UsageItem], through: NaiveDate, listed: &[&str]) -> MacosReport {
    macos_flags(
        &HostedDays::from_items(items),
        &PreviousMonth::NotNeeded,
        through,
        &names(listed),
    )
}

#[test]
fn macos_minutes_on_a_listed_repo_flag_on_the_newest_day_and_the_day_before() {
    let items = [macos(20, 12.0, "repo-m"), macos(19, 7.0, "repo-m")];
    let report = macos_for(&items, day(2026, 11, 20), &["repo-m"]);
    assert_eq!(report.flags.len(), 2);
    assert_eq!(report.flags[0].day, day(2026, 11, 20));
    assert!((report.flags[0].minutes - 12.0).abs() < 1e-9);
    assert_eq!(report.flags[1].day, day(2026, 11, 19));
    // Two days back: not checked.
    assert!(
        macos_for(&[macos(18, 12.0, "repo-m")], day(2026, 11, 20), &["repo-m"])
            .flags
            .is_empty()
    );
}

#[test]
fn macos_matching_is_case_insensitive_after_trimming_and_exact_otherwise() {
    let items = [macos(20, 5.0, "Repo-M")];
    assert_eq!(
        macos_for(&items, day(2026, 11, 20), &["  repo-m "])
            .flags
            .len(),
        1
    );
    // The name is the report's, not the typed one.
    assert_eq!(
        macos_for(&items, day(2026, 11, 20), &["repo-m"]).flags[0].repository,
        "Repo-M"
    );
    // Not a substring match.
    assert!(macos_for(&items, day(2026, 11, 20), &["repo"])
        .flags
        .is_empty());
}

#[test]
fn an_empty_list_is_off_and_says_nothing() {
    let report = macos_for(&[macos(20, 99.0, "repo-m")], day(2026, 11, 20), &[]);
    assert_eq!(report, MacosReport::default());
}

#[test]
fn only_hosted_macos_minutes_count_and_only_for_listed_repos() {
    let items = [
        linux(20, 500.0, "repo-m"),
        macos(20, 9.0, "someone-else"),
        item(
            "2026-11-20T00:00:00Z",
            "Actions macOS storage",
            "GigabyteHours",
            9.0,
            "repo-m",
        ),
    ];
    assert!(macos_for(&items, day(2026, 11, 20), &["repo-m"])
        .flags
        .is_empty());
}

#[test]
fn macos_on_the_day_before_a_month_start_is_unknown_when_october_is_unreadable() {
    let november = [macos(1, 4.0, "repo-m")];
    let report = macos_flags(
        &HostedDays::from_items(&november),
        &PreviousMonth::Failed(ActionsBillingError::Unreachable),
        day(2026, 11, 1),
        &names(&["repo-m"]),
    );
    assert_eq!(report.flags.len(), 1);
    assert_eq!(report.unknown_days, [day(2026, 10, 31)]);
}

// ---- the client reads the previous month only when the window needs it ----

const ORG_USAGE: &str = "/organizations/example-org/settings/billing/usage";

fn page(items: &[(&str, f64)]) -> ResponseTemplate {
    let rows: Vec<String> = items
        .iter()
        .map(|(date, quantity)| {
            format!(
                r#"{{"date":"{date}","product":"actions","sku":"Actions Linux","unitType":"Minutes","quantity":{quantity},"grossAmount":0.0,"discountAmount":0.0,"netAmount":0.0,"repositoryName":"repo-a"}}"#
            )
        })
        .collect();
    ResponseTemplate::new(200).set_body_raw(
        format!(r#"{{"usageItems":[{}]}}"#, rows.join(",")),
        "application/json",
    )
}

fn at(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339).expect("ts").into()
}

async fn mount_budgets(server: &MockServer) {
    Mock::given(path("/organizations/example-org/settings/billing/budgets"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"budgets":[],"has_next_page":false}"#,
            "application/json",
        ))
        .mount(server)
        .await;
}

fn client(server: &MockServer) -> ActionsBillingClient {
    ActionsBillingClient::with_base_url(&server.uri(), "stub-token").expect("client")
}

#[tokio::test]
async fn an_early_month_reads_the_previous_month_as_its_own_result() {
    let server = MockServer::start().await;
    Mock::given(path(ORG_USAGE))
        .and(query_param("month", "11"))
        .respond_with(page(&[("2026-11-03T05:00:00Z", 100.0)]))
        .mount(&server)
        .await;
    Mock::given(path(ORG_USAGE))
        .and(query_param("month", "10"))
        .and(query_param("year", "2026"))
        .respond_with(page(&[("2026-10-30T00:00:00Z", 250.0)]))
        .mount(&server)
        .await;
    mount_budgets(&server).await;
    let read = client(&server)
        .month_to_date("example-org", at("2026-11-03T06:00:00Z"))
        .await;
    assert!(matches!(read.usage, Ok(UsageSummary::Measured { .. })));
    let PreviousMonth::Read(october) = &read.previous else {
        panic!("the previous month was not read");
    };
    assert!((minutes_on(october, day(2026, 10, 30), "repo-a") - 250.0).abs() < 1e-9);
}

#[tokio::test]
async fn a_failed_previous_read_is_its_own_state_and_hides_nothing() {
    let server = MockServer::start().await;
    Mock::given(path(ORG_USAGE))
        .and(query_param("month", "11"))
        .respond_with(page(&[("2026-11-03T05:00:00Z", 100.0)]))
        .mount(&server)
        .await;
    Mock::given(path(ORG_USAGE))
        .and(query_param("month", "10"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    mount_budgets(&server).await;
    let read = client(&server)
        .month_to_date("example-org", at("2026-11-03T06:00:00Z"))
        .await;
    assert!(matches!(read.usage, Ok(UsageSummary::Measured { .. })));
    assert!(read.budget.is_ok());
    assert_eq!(
        read.previous,
        PreviousMonth::Failed(ActionsBillingError::VendorFailure { status: 500 })
    );
}

#[tokio::test]
async fn a_late_month_never_asks_for_the_previous_month() {
    let server = MockServer::start().await;
    Mock::given(path(ORG_USAGE))
        .and(query_param("month", "11"))
        .respond_with(page(&[("2026-11-20T05:00:00Z", 100.0)]))
        .mount(&server)
        .await;
    mount_budgets(&server).await;
    let read = client(&server)
        .month_to_date("example-org", at("2026-11-20T06:00:00Z"))
        .await;
    assert_eq!(read.previous, PreviousMonth::NotNeeded);
    let asked: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == ORG_USAGE)
        .map(|r| r.url.query().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(asked.len(), 1);
    assert!(asked[0].contains("month=11"));
}
// MARK: - Daily trend (#771)

fn trend(hosted: &HostedDays, through: NaiveDate) -> (NaiveDate, NaiveDate, Vec<f64>) {
    match hosted.daily_total(through) {
        DailyTrend::Series {
            first,
            last,
            minutes,
        } => (first, last, minutes),
        DailyTrend::StartsOnSecond => panic!("expected a series"),
    }
}

#[test]
fn the_trend_sums_every_repo_per_day_from_the_first_through_the_newest_day() {
    let hosted = HostedDays::from_items(&[
        linux(1, 100.0, "repo-a"),
        linux(1, 50.0, "repo-b"),
        linux(2, 30.0, "repo-a"),
        item(
            "2026-11-02T00:00:00Z",
            "Actions macOS 3-core",
            "Minutes",
            5.0,
            "repo-b",
        ),
        linux(3, 7.0, "repo-b"),
    ]);
    let (first, last, minutes) = trend(&hosted, day(2026, 11, 3));
    assert_eq!(first, day(2026, 11, 1));
    assert_eq!(last, day(2026, 11, 3));
    assert_eq!(minutes, [150.0, 35.0, 7.0]);
}

#[test]
fn a_day_with_no_items_is_a_measured_zero() {
    let hosted = HostedDays::from_items(&[linux(1, 10.0, "repo-a"), linux(4, 20.0, "repo-a")]);
    let (_, _, minutes) = trend(&hosted, day(2026, 11, 4));
    assert_eq!(minutes, [10.0, 0.0, 0.0, 20.0]);
}

#[test]
fn days_after_the_newest_day_are_not_in_the_series() {
    // Items past `through` cannot come from the report, but the series stops at
    // `through` regardless: no point and no zero beyond it.
    let hosted = HostedDays::from_items(&[linux(1, 10.0, "repo-a"), linux(9, 99.0, "repo-a")]);
    let (_, last, minutes) = trend(&hosted, day(2026, 11, 3));
    assert_eq!(last, day(2026, 11, 3));
    assert_eq!(minutes, [10.0, 0.0, 0.0]);
}

#[test]
fn storage_and_other_products_never_enter_the_trend() {
    let hosted = HostedDays::from_items(&[
        item(
            "2026-11-02T00:00:00Z",
            "Actions storage",
            "GigabyteHours",
            50_000.0,
            "repo-a",
        ),
        UsageItem {
            product: "packages".into(),
            ..linux(2, 50_000.0, "repo-a")
        },
        linux(1, 1.0, "repo-a"),
    ]);
    let (_, _, minutes) = trend(&hosted, day(2026, 11, 2));
    assert_eq!(minutes, [1.0, 0.0]);
}

#[test]
fn the_second_is_the_first_day_with_a_line() {
    let hosted = HostedDays::from_items(&[linux(1, 10.0, "repo-a")]);
    assert_eq!(
        hosted.daily_total(day(2026, 11, 1)),
        DailyTrend::StartsOnSecond
    );
    let (_, _, minutes) = trend(&hosted, day(2026, 11, 2));
    assert_eq!(minutes.len(), 2);
}

#[test]
fn an_empty_fold_through_a_later_day_is_all_measured_zeros() {
    let (_, _, minutes) = trend(&HostedDays::default(), day(2026, 11, 3));
    assert_eq!(minutes, [0.0, 0.0, 0.0]);
}

#[test]
fn the_series_length_is_the_day_of_the_month() {
    let (first, _, minutes) = trend(&HostedDays::default(), day(2026, 2, 28));
    assert_eq!(first, day(2026, 2, 1));
    assert_eq!(minutes.len(), 28);
}

#[test]
fn org_level_minutes_count_toward_the_day_but_feed_no_repo_rule() {
    // An empty repositoryName is an org-level item: real minutes in the day's
    // total, and no repo's.
    let hosted = HostedDays::from_items(&[
        linux(1, 100.0, "repo-a"),
        linux(1, 40.0, ""),
        linux(2, 25.0, ""),
    ]);
    let (_, _, minutes) = trend(&hosted, day(2026, 11, 2));
    assert_eq!(minutes, [140.0, 25.0]);
    assert_eq!(minutes_on(&hosted, day(2026, 11, 1), "repo-a"), 100.0);
    assert_eq!(hosted.repos_on(day(2026, 11, 2)).count(), 0);
    // Storage with no repo still never counts.
    let hosted = HostedDays::from_items(&[item(
        "2026-11-01T00:00:00Z",
        "Actions storage",
        "GigabyteHours",
        9.0,
        "",
    )]);
    assert_eq!(trend(&hosted, day(2026, 11, 2)).2, [0.0, 0.0]);
}
