use super::*;
use actionsbilling::{ActionsBillingError, SkuUsage};
use serde_json::json;

const NOW: u64 = 1_700_000_000;
/// The store's default cadence for this panel.
const CADENCE: u64 = 3_600;

fn alerts() -> ActionsBillingAlerts {
    ActionsBillingAlerts::default()
}

fn payload(kind: Fixture) -> Value {
    view(
        &fixture_state(kind, NOW),
        &fixture_alerts(kind),
        CADENCE,
        NOW,
    )
}

fn totals(minutes: f64, net: f64) -> Totals {
    Totals {
        minutes,
        gross: net,
        discount: 0.0,
        net,
    }
}

/// A read whose budget is `spend / amount`, built the way the crate builds it.
fn read_at(spend: f64, amount: f64) -> MonthRead {
    MonthRead {
        period: FIXTURE_PERIOD,
        previous: PreviousMonth::NotNeeded,
        usage: Ok(UsageSummary::Measured {
            total: totals(100.0, spend),
            by_sku: vec![SkuUsage {
                sku: "Actions Linux".to_owned(),
                totals: totals(100.0, spend),
            }],
            by_repo: vec![],
            top_repos: vec![],
            data_through: DateTime::from_timestamp(1_699_988_400, 0).unwrap(),
            hosted: HostedDays::default(),
        }),
        budget: Ok(actionsbilling::budget_status(
            &[actionsbilling::Budget {
                id: Value::Null,
                budget_type: String::new(),
                budget_scope: "organization".to_owned(),
                budget_entity_name: String::new(),
                budget_product_sku: "actions".to_owned(),
                budget_amount: amount,
                prevent_further_usage: true,
            }],
            Some(spend),
        )),
    }
}

fn bar_color(spend: f64, amount: f64) -> String {
    let mut state = ActionsBillingState::new();
    state.record(read_at(spend, amount), NOW);
    let p = view(&state, &alerts(), CADENCE, NOW);
    p["budget"]["bar"]["color"].as_str().unwrap().to_owned()
}

#[test]
fn the_render_cap_is_the_crates_top_n() {
    assert_eq!(TOP_ROWS, actionsbilling::TOP_N);
}

#[test]
fn the_bar_is_green_below_amber_amber_at_ninety_percent_and_red_at_the_budget() {
    let green = color::hex(color::GREEN);
    let amber = color::hex(color::AMBER);
    let red = color::hex(color::RED);
    assert_eq!(bar_color(89.9, 100.0), green);
    assert_eq!(bar_color(90.0, 100.0), amber);
    assert_eq!(bar_color(99.9, 100.0), amber);
    assert_eq!(bar_color(100.0, 100.0), red);
    assert_eq!(bar_color(140.0, 100.0), red);
}

#[test]
fn an_over_budget_bar_pins_full_but_stays_red() {
    let p = payload(Fixture::OverBudget);
    assert_eq!(p["budget"]["bar"]["fraction"], json!(1.0));
    assert_eq!(p["budget"]["bar"]["color"], json!(color::hex(color::RED)));
}

#[test]
fn the_label_says_whether_the_budget_stops_jobs() {
    assert_eq!(
        payload(Fixture::Measured)["budget"]["label"],
        "BUDGET \u{b7} STOPS JOBS"
    );
    assert_eq!(
        payload(Fixture::OverBudget)["budget"]["label"],
        "BUDGET \u{b7} JOBS CONTINUE"
    );
}

#[test]
fn no_budget_says_so_and_draws_no_bar() {
    let p = payload(Fixture::NoBudget);
    assert!(p["budget"].is_null(), "no bar, and never a 0% one");
    assert_eq!(p["budgetNote"]["text"], NO_BUDGET_MESSAGE);
    assert!(!p["headline"].is_null(), "the usage still shows");
}

fn budget_of(read: MonthRead) -> Value {
    let mut state = ActionsBillingState::new();
    state.record(read, NOW);
    view(&state, &alerts(), CADENCE, NOW)["budget"].clone()
}

#[test]
fn spend_over_a_zero_budget_prints_the_spend_over_zero_with_a_full_red_bar() {
    let budget = budget_of(read_at(10.0, 0.0));
    assert_eq!(budget["value"], "$10.00 / $0.00");
    assert_eq!(budget["bar"]["fraction"], json!(1.0));
    assert_eq!(budget["bar"]["color"], json!(color::hex(color::RED)));
    // The same red a bar at 1.0 of a positive amount takes.
    assert_eq!(budget["bar"]["color"], json!(bar_color(100.0, 100.0)));
    let fixture = payload(Fixture::ZeroBudgetOver);
    assert_eq!(fixture["budget"]["value"], "$40.60 / $0.00");
    assert_eq!(fixture["budget"]["bar"]["color"], budget["bar"]["color"]);
    assert_eq!(fixture["budget"]["label"], "BUDGET \u{b7} STOPS JOBS");
}

#[test]
fn measured_zero_spend_over_a_zero_budget_prints_both_zeros_with_no_bar() {
    let budget = budget_of(read_at(0.0, 0.0));
    assert_eq!(budget["value"], "$0.00 / $0.00");
    assert!(budget["bar"].is_null());
}

#[test]
fn unmeasured_spend_prints_an_em_dash_with_no_bar_for_any_amount() {
    for amount in [45.0, 0.0] {
        let mut read = read_at(10.0, amount);
        read.usage = Err(ActionsBillingError::RateLimited);
        read.budget = Ok(actionsbilling::budget_status(
            &[actionsbilling::Budget {
                id: Value::Null,
                budget_type: String::new(),
                budget_scope: "organization".to_owned(),
                budget_entity_name: String::new(),
                budget_product_sku: "actions".to_owned(),
                budget_amount: amount,
                prevent_further_usage: true,
            }],
            None,
        ));
        let budget = budget_of(read);
        assert_eq!(budget["value"], format!("{UNKNOWN} / {}", usd(amount)));
        assert!(budget["bar"].is_null());
    }
}

#[test]
fn credit_against_a_zero_budget_prints_the_spend_with_no_bar() {
    let budget = budget_of(read_at(-3.5, 0.0));
    assert_eq!(budget["value"], "-$3.50 / $0.00");
    assert!(budget["bar"].is_null());
}

#[test]
fn a_negative_amount_prints_no_spend_and_no_bar() {
    let budget = budget_of(read_at(10.0, -5.0));
    assert_eq!(budget["value"], format!("{UNKNOWN} / {}", usd(-5.0)));
    assert!(budget["bar"].is_null());
}

#[test]
fn a_positive_amount_prints_its_spend_beside_its_bar_as_before() {
    let budget = budget_of(read_at(27.25, 100.0));
    assert_eq!(budget["value"], "$27.25 / $100.00");
    assert_eq!(budget["bar"]["fraction"], json!(0.2725));
}

#[test]
fn a_month_with_no_actions_usage_says_so_in_words_not_zero() {
    let p = payload(Fixture::Empty);
    assert_eq!(p["message"]["text"], NO_USAGE_MESSAGE);
    assert!(p["headline"].is_null());
    assert_eq!(p["stats"], json!([]));
    assert_eq!(p["trailing"], "");
    let text = p.to_string();
    assert!(!text.contains("$0.00") && !text.contains("0 min"), "{text}");
}

#[test]
fn not_configured_shows_the_setup_sentence_naming_settings_sources() {
    let p = payload(Fixture::Unconfigured);
    assert_eq!(p["message"]["text"], UNCONFIGURED_MESSAGE);
    assert!(UNCONFIGURED_MESSAGE.contains("Settings \u{2192} Sources"));
    assert_eq!(p["message"]["color"], json!(color::hex(color::MUTED)));
    assert_eq!(p["loading"], false);
    assert!(p["headline"].is_null() && p["budget"].is_null());
}

#[test]
fn before_any_pass_has_read_the_store_the_panel_is_loading_not_unconfigured() {
    let p = view(&ActionsBillingState::new(), &alerts(), CADENCE, NOW);
    assert_eq!(p["message"]["text"], LOADING_MESSAGE);
    assert_eq!(p["loading"], true);
}

#[test]
fn a_failed_read_is_red_and_says_what_the_token_needs() {
    let p = payload(Fixture::Failed);
    let text = p["message"]["text"].as_str().unwrap();
    assert!(text.contains("rejected the credential"), "{text}");
    assert!(text.contains("Administration: read"), "{text}");
    assert!(text.contains("org admin"), "{text}");
    assert_eq!(p["message"]["color"], json!(color::hex(color::RED)));
    // The failure is the card's sentence, once: not repeated in the footer.
    assert!(p["footer"].is_null(), "{}", p["footer"]);
}

#[test]
fn each_classified_failure_renders_its_own_sentence() {
    for error in [
        ActionsBillingError::Unauthorized,
        ActionsBillingError::Forbidden,
        ActionsBillingError::NotFound,
        ActionsBillingError::RateLimited,
        ActionsBillingError::Undecodable,
        ActionsBillingError::Unreachable,
    ] {
        let mut state = ActionsBillingState::new();
        state.record(
            MonthRead {
                period: FIXTURE_PERIOD,
                previous: PreviousMonth::NotNeeded,
                usage: Err(error.clone()),
                budget: Err(error.clone()),
            },
            NOW,
        );
        let p = view(&state, &alerts(), CADENCE, NOW);
        assert_eq!(p["message"]["text"], error.user_message(), "{error:?}");
        assert_eq!(p["message"]["color"], json!(color::hex(color::RED)));
    }
}

#[test]
fn a_failed_budget_read_keeps_the_usage_and_names_itself_in_the_footer() {
    let mut state = ActionsBillingState::new();
    state.record(read_at(10.0, 100.0), NOW - 600);
    let mut read = read_at(12.0, 100.0);
    read.budget = Err(ActionsBillingError::Forbidden);
    state.record(read, NOW);
    let p = view(&state, &alerts(), CADENCE, NOW);
    assert_eq!(p["headline"]["value"], "$12.00", "the fresh usage shows");
    assert!(p["message"].is_null());
    let footer = p["footer"]["text"].as_str().unwrap();
    assert!(footer.starts_with("\u{26a0} budget: "), "{footer}");
    assert!(footer.contains("last ok 10m ago"), "{footer}");
    // The retained bar is the old one, dated by its own spend, not the new
    // headline's: the pair is never printed as one ratio.
    assert_eq!(p["budget"]["value"], "$10.00 / $100.00");
}

#[test]
fn a_failed_usage_read_keeps_the_retained_figures_and_names_itself_in_the_footer() {
    let mut state = ActionsBillingState::new();
    state.record(read_at(10.0, 100.0), NOW - 600);
    let mut read = read_at(12.0, 100.0);
    read.usage = Err(ActionsBillingError::RateLimited);
    // The crate has no fraction without a spend.
    read.budget = Ok(actionsbilling::budget_status(&[], None));
    state.record(read, NOW);
    let p = view(&state, &alerts(), CADENCE, NOW);
    assert_eq!(p["headline"]["value"], "$10.00");
    let footer = p["footer"]["text"].as_str().unwrap();
    assert!(footer.starts_with("\u{26a0} usage: "), "{footer}");
    assert!(footer.contains("last ok 10m ago"), "{footer}");
}

#[test]
fn one_credential_failing_both_reads_says_it_once() {
    let mut state = ActionsBillingState::new();
    state.record(read_at(10.0, 100.0), NOW - 600);
    state.record(
        MonthRead {
            period: FIXTURE_PERIOD,
            previous: PreviousMonth::NotNeeded,
            usage: Err(ActionsBillingError::Forbidden),
            budget: Err(ActionsBillingError::Forbidden),
        },
        NOW,
    );
    let footer = view(&state, &alerts(), CADENCE, NOW)["footer"]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(footer.matches('\u{26a0}').count(), 1, "{footer}");
}

#[test]
fn a_retained_figure_after_a_month_rollover_names_its_own_month() {
    let mut state = ActionsBillingState::new();
    state.record(read_at(10.0, 100.0), NOW);
    let p = view(&state, &alerts(), CADENCE, NOW);
    assert_eq!(
        p["headline"]["caption"],
        "net Actions spend \u{b7} November 2023 to date"
    );
}

#[test]
fn the_reports_lag_is_a_plain_line_and_never_in_the_footer_or_freshness() {
    let p = payload(Fixture::Measured);
    assert_eq!(p["dataThrough"]["text"], "usage through Nov 14 19:00 UTC");
    assert!(p["footer"].is_null(), "{}", p["footer"]);
    assert!(p["freshness"].is_null(), "{}", p["freshness"]);
    // Even when the poller is late, the lag is not what the header says.
    let stale = payload(Fixture::Measured);
    let late = view(
        &fixture_state(Fixture::Measured, NOW - 23 * 3_600),
        &alerts(),
        CADENCE,
        NOW,
    );
    assert!(!late["footer"].to_string().contains("usage through"));
    assert!(!late["freshness"].to_string().contains("usage through"));
    assert_eq!(late["dataThrough"], stale["dataThrough"]);
}

#[test]
fn a_healthy_current_tile_has_no_footer_and_no_freshness() {
    for kind in [
        Fixture::Measured,
        Fixture::NoBudget,
        Fixture::Empty,
        Fixture::OverBudget,
    ] {
        let p = payload(kind);
        assert!(
            p["footer"].is_null() && p["freshness"].is_null(),
            "{kind:?}"
        );
    }
}

#[test]
fn a_stale_read_dims_the_headline_and_dates_it_in_the_header() {
    let fresh = payload(Fixture::Measured);
    let p = view(
        &fixture_state(Fixture::Measured, NOW - 23 * 3_600),
        &alerts(),
        CADENCE,
        NOW,
    );
    assert_ne!(p["headline"]["valueColor"], fresh["headline"]["valueColor"]);
    assert_eq!(
        p["headline"]["value"], fresh["headline"]["value"],
        "still shown"
    );
    assert!(p["footer"]["text"].as_str().unwrap().contains("stale"));
    let clock = p["freshness"]["text"].as_str().unwrap();
    assert!(clock.contains("as of 23h ago"), "{clock}");
}

#[test]
fn the_breakdowns_are_per_sku_and_the_top_repos_with_minutes_and_net() {
    let p = payload(Fixture::Measured);
    assert_eq!(p["headline"]["value"], "$40.60");
    assert_eq!(p["stats"][0]["value"], "7,545 min");
    assert_eq!(p["trailing"], "7,545 min \u{b7} $40.60");
    assert_eq!(p["breakdowns"][0]["title"], "BY SKU");
    assert_eq!(
        p["breakdowns"][0]["rows"][0]["value"],
        "5,900 min \u{b7} $12.30"
    );
    // Storage is billed in gigabyte-hours: dollars only, never `0 min`.
    assert_eq!(p["breakdowns"][0]["rows"][2]["name"], "Actions storage");
    assert_eq!(p["breakdowns"][0]["rows"][2]["value"], "$0.80");
    assert_eq!(p["breakdowns"][1]["title"], "TOP REPOS");
    assert_eq!(
        p["breakdowns"][1]["rows"].as_array().unwrap().len(),
        TOP_ROWS
    );
}

#[test]
fn an_org_level_item_is_a_row_not_a_dropped_one() {
    let mut state = ActionsBillingState::new();
    let mut read = read_at(5.0, 100.0);
    if let Ok(UsageSummary::Measured { top_repos, .. }) = &mut read.usage {
        top_repos.push(actionsbilling::RepoUsage {
            repository: None,
            totals: totals(40.0, 1.0),
        });
    }
    state.record(read, NOW);
    let p = view(&state, &alerts(), CADENCE, NOW);
    assert_eq!(p["breakdowns"][1]["rows"][0]["name"], NO_REPOSITORY);
}

#[test]
fn unmeasured_figures_render_an_em_dash() {
    assert_eq!(minutes(f64::NAN), UNKNOWN);
    assert_eq!(minutes(f64::INFINITY), UNKNOWN);
    assert_eq!(minutes(1_234.4), "1,234 min");
    assert_eq!(minutes(0.0), "0 min");
}

#[test]
fn the_token_is_in_no_payload_state_or_debug_output() {
    // The view never sees a credential; the error classes carry no string.
    const TOKEN: &str = "github_pat_never_in_a_payload";
    let mut state = ActionsBillingState::new();
    state.record(
        MonthRead {
            period: FIXTURE_PERIOD,
            previous: PreviousMonth::NotNeeded,
            usage: Err(ActionsBillingError::Unauthorized),
            budget: Err(ActionsBillingError::Undecodable),
        },
        NOW,
    );
    let rendered = format!("{:?} {}", state, view(&state, &alerts(), CADENCE, NOW));
    assert!(!rendered.contains(TOKEN));
}

#[test]
fn an_unreadable_credential_store_keeps_a_live_figure() {
    let mut state = ActionsBillingState::new();
    state.record(read_at(10.0, 100.0), NOW - 60);
    state.unreadable("the credential store would not answer");
    let p = view(&state, &alerts(), CADENCE, NOW);
    assert_eq!(
        p["headline"]["value"], "$10.00",
        "a locked keychain is not an unconfigure"
    );
}

#[test]
fn only_a_failure_a_retry_can_fix_asks_again_before_the_cadence() {
    let failing = |error: ActionsBillingError| {
        let mut state = ActionsBillingState::new();
        state.record(
            MonthRead {
                period: FIXTURE_PERIOD,
                previous: PreviousMonth::NotNeeded,
                usage: Err(error.clone()),
                budget: Err(error),
            },
            NOW,
        );
        state.wants_quick_retry()
    };
    assert!(failing(ActionsBillingError::Unreachable));
    assert!(failing(ActionsBillingError::VendorFailure { status: 502 }));
    for error in [
        ActionsBillingError::Unauthorized,
        ActionsBillingError::Forbidden,
        ActionsBillingError::NotFound,
        ActionsBillingError::RateLimited,
        ActionsBillingError::Undecodable,
    ] {
        assert!(
            !failing(error.clone()),
            "{error:?} is not asked again every minute"
        );
    }
    // Once something has been read, the operator's cadence rules.
    let mut state = ActionsBillingState::new();
    state.record(read_at(10.0, 100.0), NOW);
    state.record(
        MonthRead {
            period: FIXTURE_PERIOD,
            previous: PreviousMonth::NotNeeded,
            usage: Err(ActionsBillingError::Unreachable),
            budget: Err(ActionsBillingError::Unreachable),
        },
        NOW,
    );
    assert!(!state.wants_quick_retry());
    assert!(!ActionsBillingState::new().wants_quick_retry());
}

#[test]
fn unconfiguring_drops_every_retained_figure() {
    let mut state = ActionsBillingState::new();
    state.record(read_at(10.0, 100.0), NOW);
    state.unconfigure();
    let p = view(&state, &alerts(), CADENCE, NOW);
    assert_eq!(p["message"]["text"], UNCONFIGURED_MESSAGE);
    assert!(p["headline"].is_null() && p["budget"].is_null());
    assert!(!state.wants_quick_retry());
}

#[test]
fn the_payload_and_the_kind_agree_on_id_and_title() {
    let p = payload(Fixture::Measured);
    assert_eq!(p["id"], "actionsBilling");
    assert_eq!(p["title"], "Actions billing");
}

#[test]
fn a_failed_first_read_for_another_org_shows_no_figures_from_the_old_one() {
    let mut state = ActionsBillingState::new();
    state.begin("org-a");
    state.record(read_at(95.0, 100.0), NOW - 600);
    let before = view(&state, &alerts(), CADENCE, NOW);
    assert!(!before["headline"].is_null() && !before["budget"].is_null());

    state.begin("org-b");
    state.record(
        MonthRead {
            period: FIXTURE_PERIOD,
            previous: PreviousMonth::NotNeeded,
            usage: Err(ActionsBillingError::NotFound),
            budget: Err(ActionsBillingError::NotFound),
        },
        NOW,
    );
    let p = view(&state, &alerts(), CADENCE, NOW);
    assert!(p["headline"].is_null() && p["budget"].is_null());
    assert_eq!(
        p["message"]["text"],
        ActionsBillingError::NotFound.user_message()
    );
    assert!(p["breakdowns"].as_array().unwrap().is_empty());

    // The same org keeps its figures through a failure.
    let mut same = ActionsBillingState::new();
    same.begin("org-a");
    same.record(read_at(10.0, 100.0), NOW - 600);
    same.begin("org-a");
    assert!(!view(&same, &alerts(), CADENCE, NOW)["headline"].is_null());
}

#[test]
fn an_org_only_change_drops_the_old_orgs_figures_before_the_credential_read() {
    let mut state = ActionsBillingState::new();
    state.begin("org-a");
    state.record(read_at(95.0, 100.0), NOW - 600);
    assert!(!view(&state, &alerts(), CADENCE, NOW)["budget"].is_null());

    // The pass for org B names it, then the credential store will not answer.
    state.note_org("org-b");
    state.unreadable("the credential store would not answer");
    let p = view(&state, &alerts(), CADENCE, NOW);
    assert!(p["headline"].is_null() && p["budget"].is_null());
    assert!(p["breakdowns"].as_array().unwrap().is_empty());
    assert!(!p.to_string().contains("$95.00"));
    // Nothing was observed about the credential, so the keychain's refusal is
    // what the tile says, never the setup instruction.
    assert_eq!(
        p["message"]["text"],
        "the credential store would not answer"
    );
    assert_ne!(p["message"]["text"], UNCONFIGURED_MESSAGE);
}

#[test]
fn an_unreadable_store_for_the_same_org_keeps_the_last_good_figures() {
    let mut state = ActionsBillingState::new();
    state.begin("org-a");
    state.record(read_at(95.0, 100.0), NOW - 600);
    state.note_org("org-a");
    state.unreadable("the credential store would not answer");
    let p = view(&state, &alerts(), CADENCE, NOW);
    assert_eq!(p["headline"]["value"], "$95.00");
    assert_eq!(p["budget"]["value"], "$95.00 / $100.00");
}

// MARK: - Billing alerts (#770)

fn rows(p: &Value) -> Vec<&Value> {
    p["alerts"].as_array().unwrap().iter().collect()
}

fn labels(p: &Value) -> Vec<&str> {
    rows(p)
        .iter()
        .map(|r| r["label"].as_str().unwrap())
        .collect()
}

#[test]
fn a_spike_is_one_red_row_with_its_figures() {
    let p = payload(Fixture::Spike);
    assert_eq!(labels(&p), ["SPIKE \u{b7} gadget-api"]);
    let row = rows(&p)[0];
    assert_eq!(row["value"], "1,500 min \u{b7} 5.0\u{d7}");
    assert_eq!(row["color"], color::hex(color::RED));
    let detail = row["detail"].as_str().unwrap();
    assert!(detail.contains("11-14"));
    assert!(detail.contains("against a 7-day average of"));
}

#[test]
fn macos_on_a_listed_repo_is_a_red_row_matched_case_insensitively() {
    let p = payload(Fixture::MacosOnSelfHosted);
    // Listed as `Widget-Web`; the report spells it `widget-web`.
    assert_eq!(
        labels(&p),
        [
            "MACOS ON SELF-HOSTED \u{b7} widget-web",
            "MACOS ON SELF-HOSTED \u{b7} widget-web"
        ]
    );
    assert_eq!(rows(&p)[0]["value"], "12 min on 11-14");
    assert_eq!(rows(&p)[1]["value"], "4 min on 11-13");
    assert!(rows(&p)
        .iter()
        .all(|r| r["color"] == color::hex(color::RED)));
}

#[test]
fn the_macos_rule_with_an_empty_list_adds_no_row() {
    let state = fixture_state(Fixture::MacosOnSelfHosted, NOW);
    let p = view(&state, &alerts(), CADENCE, NOW);
    assert!(rows(&p).is_empty());
}

#[test]
fn the_other_fixtures_raise_no_rule_rows_and_the_section_is_always_there() {
    for kind in [
        Fixture::Measured,
        Fixture::Empty,
        Fixture::Failed,
        Fixture::Unconfigured,
        Fixture::NoBudget,
        Fixture::OverBudget,
        Fixture::ZeroBudgetOver,
    ] {
        let p = payload(kind);
        assert!(rows(&p).is_empty(), "{kind:?}");
        assert_eq!(p["billingAlerts"]["heading"], "Billing alerts", "{kind:?}");
    }
}

#[test]
fn the_rules_are_judged_at_view_time_from_the_retained_fold() {
    // The same state, three settings: a save re-flags without a new poll.
    let state = fixture_state(Fixture::Spike, NOW);
    let with = |multiple: f64, floor: u32| {
        let alerts = ActionsBillingAlerts {
            spike_multiple: multiple,
            spike_floor_minutes: floor,
            ..ActionsBillingAlerts::default()
        };
        view(&state, &alerts, CADENCE, NOW)
    };
    assert_eq!(rows(&with(3.0, 1000)).len(), 1);
    assert!(rows(&with(6.0, 1000)).is_empty());
    assert!(rows(&with(3.0, 2000)).is_empty());
    assert_eq!(rows(&with(1.5, 1500)).len(), 1);
}

fn early_month_state(previous: PreviousMonth) -> ActionsBillingState {
    let item = |date: &str, quantity: f64| actionsbilling::UsageItem {
        date: DateTime::parse_from_rfc3339(date).unwrap().into(),
        product: "actions".to_owned(),
        sku: "Actions Linux".to_owned(),
        unit_type: "Minutes".to_owned(),
        quantity,
        gross_amount: 0.0,
        discount_amount: 0.0,
        net_amount: 0.0,
        repository_name: "repo-a".to_owned(),
    };
    let mut state = ActionsBillingState::new();
    state.record(
        MonthRead {
            period: FIXTURE_PERIOD,
            previous,
            usage: Ok(actionsbilling::summarize_usage(&[
                item("2023-11-01T00:00:00Z", 5_000.0),
                item("2023-11-02T00:00:00Z", 5_000.0),
                item("2023-11-03T05:00:00Z", 6_000.0),
            ])),
            budget: Ok(BudgetStatus::Absent),
        },
        NOW,
    );
    state
}

#[test]
fn an_unreadable_baseline_is_a_muted_unknown_row_never_a_spike_and_never_nothing() {
    let state = early_month_state(PreviousMonth::Failed(ActionsBillingError::Unreachable));
    let p = view(&state, &alerts(), CADENCE, NOW);
    assert_eq!(labels(&p), ["SPIKE CHECK"]);
    let row = rows(&p)[0];
    assert_eq!(row["value"], "\u{2014}");
    assert_eq!(row["color"], color::hex(color::MUTED));
    let detail = row["detail"].as_str().unwrap();
    assert!(
        detail.contains("baseline unknown") && detail.contains("11-03"),
        "{detail}"
    );
}

#[test]
fn a_spike_with_no_minutes_in_the_7_days_before_says_from_0() {
    // October holds nothing and the newest day is the only one with minutes, so
    // there is no baseline to take a multiple of.
    let item = actionsbilling::UsageItem {
        date: DateTime::parse_from_rfc3339("2023-11-03T05:00:00Z")
            .unwrap()
            .into(),
        product: "actions".to_owned(),
        sku: "Actions Linux".to_owned(),
        unit_type: "Minutes".to_owned(),
        quantity: 6_000.0,
        gross_amount: 0.0,
        discount_amount: 0.0,
        net_amount: 0.0,
        repository_name: "repo-a".to_owned(),
    };
    let mut state = ActionsBillingState::new();
    state.record(
        MonthRead {
            period: FIXTURE_PERIOD,
            previous: PreviousMonth::Read(HostedDays::default()),
            usage: Ok(actionsbilling::summarize_usage(&[item])),
            budget: Ok(BudgetStatus::Absent),
        },
        NOW,
    );
    let p = view(&state, &alerts(), CADENCE, NOW);
    assert_eq!(labels(&p), ["SPIKE \u{b7} repo-a"]);
    let row = rows(&p)[0];
    assert_eq!(row["value"], "6,000 min \u{b7} from 0");
    let detail = row["detail"].as_str().unwrap();
    assert!(detail.contains("11-03"));
    assert!(detail.contains("against a 7-day average of 0 min"));
}

#[test]
fn a_readable_previous_month_is_the_baseline() {
    // October holds nothing for the repo: a measured 0, so the 6,000-minute
    // newest day against a baseline of its own early days is judged, not unknown.
    let state = early_month_state(PreviousMonth::Read(HostedDays::default()));
    let p = view(&state, &alerts(), CADENCE, NOW);
    assert!(
        labels(&p).iter().all(|l| l.starts_with("SPIKE \u{b7}")),
        "{:?}",
        labels(&p)
    );
    assert!(!labels(&p).is_empty());
}

#[test]
fn a_retained_fold_is_dropped_with_its_org() {
    let mut state = early_month_state(PreviousMonth::Failed(ActionsBillingError::Unreachable));
    state.begin("org-a");
    state.note_org("org-b");
    assert!(rows(&view(&state, &alerts(), CADENCE, NOW)).is_empty());
}

#[test]
fn the_section_carries_the_three_controls_with_their_saved_values() {
    let alerts = ActionsBillingAlerts {
        spike_multiple: 2.5,
        spike_floor_minutes: 300,
        self_hosted_repos: vec!["alpha".to_owned(), "beta".to_owned()],
    };
    let section = alerts_section(&alerts);
    assert!(section["shared"]
        .as_str()
        .unwrap()
        .contains("every Actions billing tile"));
    let fields = section["fields"].as_array().unwrap();
    let ids: Vec<&str> = fields.iter().map(|f| f["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        ["spikeMultiple", "spikeFloorMinutes", "selfHostedRepos"]
    );
    assert_eq!(fields[0]["value"], 2.5);
    assert_eq!(fields[1]["value"], 300);
    assert_eq!(fields[2]["value"], "alpha, beta");
}

fn input(multiple: Value, floor: Value, repos: &str) -> Value {
    json!({"spikeMultiple": multiple, "spikeFloorMinutes": floor, "selfHostedRepos": repos})
}

#[test]
fn a_save_persists_and_survives_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store::Store::open_in(dir.path(), false).unwrap();
    save_alerts(&mut store, &input(json!(4), json!(250), "Alpha, beta")).unwrap();
    let reopened = store::Store::open_in(dir.path(), false).unwrap();
    let saved = &reopened.settings().actions_billing_alerts;
    assert_eq!(saved.spike_multiple, 4.0);
    assert_eq!(saved.spike_floor_minutes, 250);
    assert_eq!(saved.self_hosted_repos, ["Alpha", "beta"]);
}

#[test]
fn a_refused_save_says_why_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store::Store::open_in(dir.path(), false).unwrap();
    save_alerts(&mut store, &input(json!(4), json!(250), "alpha")).unwrap();
    let before = store.settings().actions_billing_alerts.clone();
    for (bad, sentence) in [
        (
            input(json!(1), json!(250), "alpha"),
            store::actions_billing_alerts::REFUSE_MULTIPLE,
        ),
        (
            input(json!(4), json!(0), "alpha"),
            store::actions_billing_alerts::REFUSE_FLOOR,
        ),
        (
            input(json!(4), json!(2.5), "alpha"),
            store::actions_billing_alerts::REFUSE_FLOOR,
        ),
        (
            input(json!(4), json!(250), "a,,b"),
            store::actions_billing_alerts::REFUSE_EMPTY_REPO,
        ),
    ] {
        assert_eq!(save_alerts(&mut store, &bad).unwrap_err(), sentence);
        assert_eq!(store.settings().actions_billing_alerts, before);
    }
    let reopened = store::Store::open_in(dir.path(), false).unwrap();
    assert_eq!(reopened.settings().actions_billing_alerts, before);
}
// MARK: - Daily trend (#771)

fn trend_values(p: &Value) -> Vec<f64> {
    p["trend"]["values"]
        .as_array()
        .expect("a trend series")
        .iter()
        .map(|v| v.as_f64().expect("a number"))
        .collect()
}

#[test]
fn the_measured_fixture_carries_a_trend_from_the_first_through_the_newest_day_with_a_zero_day() {
    let p = payload(Fixture::Measured);
    let values = trend_values(&p);
    // Nov 1 through Nov 14, the newest day in the report; Nov 9 had no item.
    assert_eq!(values.len(), 14);
    assert!(values[8].abs() < 1e-9, "the day with no items is a 0");
    assert_eq!(values.iter().filter(|v| **v > 0.0).count(), 13);
    assert_eq!(p["trend"]["label"], "HOSTED MINUTES PER DAY");
    assert_eq!(p["trend"]["first"], "Nov 1");
    assert_eq!(p["trend"]["last"], "Nov 14");
    assert_eq!(p["trendNote"], Value::Null);
}

#[test]
fn the_trend_range_is_zero_to_the_series_maximum() {
    let p = payload(Fixture::Measured);
    let peak = trend_values(&p).into_iter().fold(0.0_f64, f64::max);
    assert!(peak > 0.0);
    assert!(p["trend"]["lo"].as_f64().unwrap().abs() < 1e-9);
    assert!((p["trend"]["hi"].as_f64().unwrap() - peak).abs() < 1e-9);
    assert_eq!(
        p["trend"]["caption"],
        format!("Nov 1 \u{2013} Nov 14 \u{b7} peak {}", minutes(peak))
    );
    assert!(p["trend"]["color"].as_str().unwrap().starts_with('#'));
}

#[test]
fn the_trend_is_the_sum_of_the_repo_days_and_leaves_storage_out() {
    let p = payload(Fixture::Spike);
    let values = trend_values(&p);
    // Nov 1..13 are 300 + 200 + 90 each; Nov 14 is 220 + 1,500 + 80.
    assert_eq!(values.len(), 14);
    assert!(values[..13].iter().all(|v| (v - 590.0).abs() < 1e-9));
    assert!((values[13] - 1_800.0).abs() < 1e-9);
}

#[test]
fn on_the_first_of_a_month_there_is_no_line_and_a_sentence_instead() {
    let p = payload(Fixture::FirstDay);
    assert_eq!(p["trend"], Value::Null);
    assert_eq!(p["trendNote"]["text"], "the daily trend starts on the 2nd");
    assert_eq!(p["trendNote"]["color"], color::hex(color::MUTED));
}

#[test]
fn a_failed_read_no_usage_or_no_setup_has_no_trend_and_no_sentence() {
    for kind in [Fixture::Failed, Fixture::Empty, Fixture::Unconfigured] {
        let p = payload(kind);
        assert_eq!(p["trend"], Value::Null, "{kind:?}");
        assert_eq!(p["trendNote"], Value::Null, "{kind:?}");
    }
}

#[test]
fn the_trend_adds_up_to_the_hosted_minutes_stat_org_level_items_included() {
    let p = payload(Fixture::Measured);
    let sum: f64 = trend_values(&p).iter().sum();
    assert_eq!(minutes(sum), p["stats"][0]["value"]);
}

#[test]
fn a_month_with_no_minutes_is_a_flat_measured_zero_line_not_a_sentence() {
    // A storage-only month is `Measured` with an empty hosted fold: every day
    // is a measured 0, so the line stays, at the floor, and says so.
    let through = DateTime::parse_from_rfc3339("2023-11-03T10:00:00Z")
        .unwrap()
        .into();
    let (trend, note) = trend_view(&HostedDays::default(), through);
    assert_eq!(note, Value::Null);
    assert_eq!(trend["values"], json!([0.0, 0.0, 0.0]));
    assert_eq!(trend["lo"], json!(0.0));
    assert_eq!(trend["hi"], json!(0.0));
    assert_eq!(trend["caption"], "Nov 1 \u{2013} Nov 3 \u{b7} peak 0 min");
}

#[test]
fn the_trend_adds_no_overview_row_and_the_fixtures_raise_no_rule() {
    // Rows are what the overview tile is made of; the trend is not one.
    assert!(rows(&payload(Fixture::Measured)).is_empty());
    assert!(rows(&payload(Fixture::FirstDay)).is_empty());
}
