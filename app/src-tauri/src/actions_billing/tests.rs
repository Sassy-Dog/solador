use super::*;
use actionsbilling::{ActionsBillingError, SkuUsage};

const NOW: u64 = 1_700_000_000;
/// The store's default cadence for this panel.
const CADENCE: u64 = 3_600;

fn payload(kind: Fixture) -> Value {
    view(&fixture_state(kind, NOW), CADENCE, NOW)
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
        usage: Ok(UsageSummary::Measured {
            total: totals(100.0, spend),
            by_sku: vec![SkuUsage {
                sku: "Actions Linux".to_owned(),
                totals: totals(100.0, spend),
            }],
            by_repo: vec![],
            top_repos: vec![],
            data_through: DateTime::from_timestamp(1_699_988_400, 0).unwrap(),
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
    let p = view(&state, CADENCE, NOW);
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
    view(&state, CADENCE, NOW)["budget"].clone()
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
    let p = view(&ActionsBillingState::new(), CADENCE, NOW);
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
                usage: Err(error.clone()),
                budget: Err(error.clone()),
            },
            NOW,
        );
        let p = view(&state, CADENCE, NOW);
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
    let p = view(&state, CADENCE, NOW);
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
    let p = view(&state, CADENCE, NOW);
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
            usage: Err(ActionsBillingError::Forbidden),
            budget: Err(ActionsBillingError::Forbidden),
        },
        NOW,
    );
    let footer = view(&state, CADENCE, NOW)["footer"]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(footer.matches('\u{26a0}').count(), 1, "{footer}");
}

#[test]
fn a_retained_figure_after_a_month_rollover_names_its_own_month() {
    let mut state = ActionsBillingState::new();
    state.record(read_at(10.0, 100.0), NOW);
    let p = view(&state, CADENCE, NOW);
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
    let p = view(&state, CADENCE, NOW);
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
            usage: Err(ActionsBillingError::Unauthorized),
            budget: Err(ActionsBillingError::Undecodable),
        },
        NOW,
    );
    let rendered = format!("{:?} {}", state, view(&state, CADENCE, NOW));
    assert!(!rendered.contains(TOKEN));
}

#[test]
fn an_unreadable_credential_store_keeps_a_live_figure() {
    let mut state = ActionsBillingState::new();
    state.record(read_at(10.0, 100.0), NOW - 60);
    state.unreadable("the credential store would not answer");
    let p = view(&state, CADENCE, NOW);
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
    let p = view(&state, CADENCE, NOW);
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
    let before = view(&state, CADENCE, NOW);
    assert!(!before["headline"].is_null() && !before["budget"].is_null());

    state.begin("org-b");
    state.record(
        MonthRead {
            period: FIXTURE_PERIOD,
            usage: Err(ActionsBillingError::NotFound),
            budget: Err(ActionsBillingError::NotFound),
        },
        NOW,
    );
    let p = view(&state, CADENCE, NOW);
    assert!(p["headline"].is_null() && p["budget"].is_null());
    assert_eq!(
        p["message"]["text"],
        ActionsBillingError::NotFound.user_message()
    );
    assert!(p["breakdowns"].as_array().unwrap().is_empty());
    // No amber or red bar is left to raise Needs Attention.

    // The same org keeps its figures through a failure.
    let mut same = ActionsBillingState::new();
    same.begin("org-a");
    same.record(read_at(10.0, 100.0), NOW - 600);
    same.begin("org-a");
    assert!(!view(&same, CADENCE, NOW)["headline"].is_null());
}

#[test]
fn an_org_only_change_drops_the_old_orgs_figures_before_the_credential_read() {
    let mut state = ActionsBillingState::new();
    state.begin("org-a");
    state.record(read_at(95.0, 100.0), NOW - 600);
    assert!(!view(&state, CADENCE, NOW)["budget"].is_null());

    // The pass for org B names it, then the credential store will not answer.
    state.note_org("org-b");
    state.unreadable("the credential store would not answer");
    let p = view(&state, CADENCE, NOW);
    assert!(p["headline"].is_null() && p["budget"].is_null(), "{p}");
    assert!(p["breakdowns"].as_array().unwrap().is_empty());
    assert!(!p.to_string().contains("$95.00"), "{p}");
}

#[test]
fn an_unreadable_store_for_the_same_org_keeps_the_last_good_figures() {
    let mut state = ActionsBillingState::new();
    state.begin("org-a");
    state.record(read_at(95.0, 100.0), NOW - 600);
    state.note_org("org-a");
    state.unreadable("the credential store would not answer");
    let p = view(&state, CADENCE, NOW);
    assert_eq!(p["headline"]["value"], "$95.00");
    assert_eq!(p["budget"]["value"], "$95.00 / $100.00");
}
