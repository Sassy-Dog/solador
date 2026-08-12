//! The **Azure Cost** panel: month-to-date spend, a month-over-month reference,
//! a projection against an optional budget, and the top resource groups and
//! types.
//!
//! Port of `AzureCostPanel`. The data layer
//! beneath it is [`azurecost`] — including the fingerprint cache that makes an
//! unchanged export cost one blob listing and no partition downloads — and this
//! module is the view side, holding to the same rule as every other panel here:
//! **every string and colour the frontend paints is made in Rust.**
//!
//! Unlike the Usage panel, USD *is* the point: this is a bill, not a
//! subscription, so the dollars are the headline rather than something computed
//! and hidden.
//!
//! Two states are easy to conflate and are deliberately kept apart. **No SAS
//! URL** is a setup instruction, muted — nothing is wrong. **A failed read** is
//! red and names the failure. Rendering the first as an error would send an
//! operator hunting a break that does not exist.

pub mod sas;

use chrono::{DateTime, Datelike, Utc};
use serde_json::{json, Value};
use viewmodel::cockpit::PanelKind;
use viewmodel::color;

use crate::panel::{progress_bar, status_footer, Configured};

/// `PanelStatusFooter(..., staleAfter: 5 * 60 * 60)`. The export publishes about
/// once a day and the poll runs every 4h, so the window sits an hour past a
/// missed cycle: a warning here means a stuck or failing poller, not the normal
/// gap between reads.
pub const STALE_AFTER_SECS: u64 = 5 * 60 * 60;

/// The panel's zero-setup state. An instruction, not a failure.
///
/// Names the storage account, not a SAS: the panel stopped storing a SAS when
/// it learned to mint its own per poll, and this message outlived that change —
/// telling operators to paste a credential the Settings surface no longer has a
/// field for.
pub const UNCONFIGURED_MESSAGE: &str = "Add an Azure storage account in Settings";

/// Configured, with a read in flight and nothing to show yet.
pub const LOADING_MESSAGE: &str = "reading export…";

/// Configured, not loading, no summary and no error — the state that should
/// never last, kept because the original keeps it and a blank card would be worse.
pub const NO_DATA_MESSAGE: &str = "no cost data";

/// Amber at 90% of budget, red at budget. Same thresholds as the Usage panel's
/// Sentry quota bar.
const BUDGET_AMBER_AT: f64 = 0.9;
const BUDGET_RED_AT: f64 = 1.0;

/// How many rows each breakdown column shows — `azurecost::TOP_N`, restated as
/// the *render* cap so a reader of this file can see it, and asserted equal to
/// the crate's in the tests below.
const TOP_ROWS: usize = azurecost::TOP_N;

/// Rendered wherever a figure is not a number. Same em dash as the host card's.
const UNKNOWN: &str = "—";

// MARK: - Formatting

/// USD with a thousands separator and cents, e.g. `$1,234.56`.
///
/// the original builds this with a `NumberFormatter` pinned to `en_US`, so the grouping
/// and the symbol are fixed rather than the viewer's locale — the export is
/// billed in USD and a card that said `1.234,56 €` would be wrong twice. This is
/// that formatter, hand-rolled: the shell has no ICU and does not want one for
/// three digits and a comma.
///
/// A non-finite amount is not a number and renders the em dash. It cannot arise
/// from a real export, but `$NaN` on the headline would be worse than admitting
/// it.
#[must_use]
pub fn usd(amount: f64) -> String {
    if !amount.is_finite() {
        return UNKNOWN.to_owned();
    }
    // `{:.2}` rather than `(x * 100.0).round()`: Rust's float formatting rounds
    // half to **even**, which is `NumberFormatter`'s default and therefore what
    // the original panel shows. `f64::round` is half-away-from-zero and would
    // disagree by a cent on every exact half-cent. Only the integer part is
    // regrouped below; the two decimals come out of the formatter already.
    let rendered = format!("{:.2}", amount.abs());
    let (whole, cents) = rendered
        .split_once('.')
        .unwrap_or((rendered.as_str(), "00"));
    let sign = if amount < 0.0 { "-" } else { "" };
    format!("{sign}${}.{cents}", group_thousands(whole))
}

/// `1234567` -> `1,234,567`.
fn group_thousands(digits: &str) -> String {
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Month names in UTC and English, matching the original formatter's
/// `TimeZone(identifier: "UTC")` + `Locale(identifier: "en_US")`.
///
/// A table rather than a `strftime` directive because the export's month math is
/// UTC arithmetic and the panel must read the same on every machine — a
/// locale-sensitive formatter is one build flag away from stamping "juin" on an
/// Azure bill.
const MONTHS_LONG: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// The `MMM` forms, for the trailing label where space is tight.
const MONTHS_SHORT: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

#[must_use]
pub fn month_long(at: DateTime<Utc>) -> &'static str {
    MONTHS_LONG[(at.month0() as usize).min(11)]
}

#[must_use]
pub fn month_short(at: DateTime<Utc>) -> &'static str {
    MONTHS_SHORT[(at.month0() as usize).min(11)]
}

// MARK: - State

/// Everything the panel renders from, plus the cache the reader needs.
#[derive(Debug, Default)]
pub struct AzureState {
    /// Whether the export's address is configured — a storage account and a
    /// container. [`Absent`](Configured::Absent) is the setup state, not a
    /// failure. (Named `sas` for the credential this panel used to store; it
    /// mints a short-lived one per poll now and keeps nothing.)
    ///
    /// Three states rather than a `bool`: this defaulted to `false`, and the
    /// ladder in [`view`] reads "not configured" before it reads `loading`, so
    /// the panel opened on *"Add an Azure Cost SAS URL in Settings"* at a
    /// machine whose SAS was fine — and `loading: true`, set right below, was
    /// unreachable until the first poll had already begun.
    sas: Configured,
    /// The last successful read. Retained through a failure — a daily export
    /// carried forward with its age in the footer beats a blank card.
    summary: Option<azurecost::CostSummary>,
    last_updated: Option<u64>,
    last_error: Option<String>,
    /// True from startup (or from a newly-saved SAS) until the first read
    /// settles — what separates [`LOADING_MESSAGE`] from [`NO_DATA_MESSAGE`].
    loading: bool,
    /// The last successful fetch *with* its fingerprint. Handed back to
    /// [`azurecost::fetch_summary`] so an unchanged export downloads not one
    /// partition body.
    cache: Option<azurecost::FetchResult>,
}

impl AzureState {
    #[must_use]
    pub fn new() -> Self {
        AzureState {
            loading: true,
            ..AzureState::default()
        }
    }

    /// A pass read the credential store and found no SAS URL. Everything is
    /// dropped so a stale figure cannot sit behind the setup message and
    /// reappear the moment a *different* SAS is pasted in.
    pub fn unconfigure(&mut self) {
        *self = AzureState {
            sas: Configured::Absent,
            ..AzureState::default()
        };
    }

    /// A SAS is configured and a read is in flight.
    pub fn begin(&mut self) {
        self.sas = Configured::Present;
        if self.summary.is_none() {
            self.loading = true;
        }
    }

    pub fn succeeded(&mut self, result: azurecost::FetchResult, at: u64) {
        self.sas = Configured::Present;
        self.summary = Some(result.summary.clone());
        self.cache = Some(result);
        self.last_updated = Some(at);
        self.last_error = None;
        self.loading = false;
    }

    pub fn failed(&mut self, error: String) {
        self.sas = Configured::Present;
        self.last_error = Some(error);
        self.loading = false;
    }

    /// Whether the panel has ever completed a read. Drives the poll loop's
    /// cadence: until the first success it retries far sooner than the export's
    /// own 4h rhythm, so a launch with the network still coming up does not
    /// leave a red card on screen until teatime.
    #[must_use]
    pub fn has_succeeded(&self) -> bool {
        self.last_updated.is_some()
    }

    /// The fingerprinted last success, for the next read's cache check.
    #[must_use]
    pub fn cached(&self) -> Option<azurecost::FetchResult> {
        self.cache.clone()
    }
}

// MARK: - View

/// A `LABEL … $value` row (PRIOR MONTH, PROJECTED).
///
/// `None` is the em dash, not `$0.00`. the original's panel renders a missing
/// prior-month export as a real-looking `$0.00` (`AzureCostPanel` over
/// a non-optional `spendPriorMonth`); the port diverges deliberately because the
/// no-fake-numbers rule outranks bug-for-bug parity — a month that genuinely
/// cost nothing and a month nobody could read must not print the same string.
fn stat_row(label: &str, amount: Option<f64>) -> Value {
    json!({
        "label": label,
        "value": amount.map_or_else(|| UNKNOWN.to_owned(), usd),
        "valueColor": color::hex(color::INK),
    })
}

/// One titled top-N column.
fn breakdown(title: &str, resources: &[azurecost::ResourceCost]) -> Value {
    json!({
        "title": title,
        "rows": resources
            .iter()
            .take(TOP_ROWS)
            .map(|resource| json!({
                "name": resource.name,
                "value": usd(resource.cost),
                "dotColor": color::hex(color::GREEN_DIM),
            }))
            .collect::<Vec<_>>(),
    })
}

/// The panel payload.
///
/// `budget` is the store's `azure_monthly_budget_usd`, read at render time
/// rather than captured by the poller: it is a local number no fetch is involved
/// in, so changing it must repaint the bar now, not on the next four-hour cycle.
#[must_use]
pub fn view(state: &AzureState, budget: f64, now: u64) -> Value {
    let kind = PanelKind::AzureCost;
    let mut payload = json!({
        "id": kind.id(),
        "title": kind.title(),
        "trailing": "",
        "message": Value::Null,
        "headline": Value::Null,
        "stats": Value::Array(vec![]),
        "budget": Value::Null,
        "breakdowns": Value::Array(vec![]),
        "footer": Value::Null,
        // Published for the frontend's refresh cadence: it polls faster while a
        // panel is still filling in. Nothing is known until a pass has read the
        // credential store, and a configured panel is loading until its first
        // read settles either way.
        "loading": state.sas.is_unknown() || (state.loading && state.last_error.is_none()),
    });

    // the original's ladder, in the original's order. The unconfigured branch comes first so a
    // machine with no SAS never reports an error it has no way to have had —
    // but only `Absent` may take it. `Unknown` is the frame before any pass has
    // read the store, and it used to fall in here and tell an operator whose
    // SAS was fine to go and paste one.
    let Some(summary) = state.summary.as_ref().filter(|_| state.sas.is_present()) else {
        payload["message"] = match (state.sas, state.last_error.as_deref()) {
            (Configured::Unknown, _) => json!({
                "text": LOADING_MESSAGE,
                "color": color::hex(color::MUTED),
            }),
            (Configured::Absent, _) => json!({
                "text": UNCONFIGURED_MESSAGE,
                "color": color::hex(color::MUTED),
            }),
            // Red, and the failure named: this one *is* broken.
            (Configured::Present, Some(error)) => json!({
                "text": error,
                "color": color::hex(color::RED),
            }),
            (Configured::Present, None) => json!({
                "text": if state.loading { LOADING_MESSAGE } else { NO_DATA_MESSAGE },
                "color": color::hex(color::MUTED),
            }),
        };
        return payload;
    };

    // "month-to-date" in the normal case; when the read fell back to the last
    // completed month (the 1st-of-month rollover gap) the caption stamps which
    // month the figures cover, in amber, because the headline is then NOT what a
    // reader would assume it is.
    let (caption, caption_color) = match summary.as_of_month {
        None => ("month-to-date".to_owned(), color::MUTED),
        Some(month) => (
            format!("{} · this month pending", month_long(month)),
            color::AMBER,
        ),
    };

    payload["trailing"] = json!(match summary.as_of_month {
        None => format!("{} MTD", usd(summary.spend_mtd)),
        Some(month) => format!("{} · {}", usd(summary.spend_mtd), month_short(month)),
    });
    payload["headline"] = json!({
        "value": usd(summary.spend_mtd),
        "valueColor": color::hex(color::GREEN),
        "caption": caption,
        "captionColor": color::hex(caption_color),
    });
    payload["stats"] = json!([
        stat_row("PRIOR MONTH", summary.spend_prior_month),
        stat_row("PROJECTED", Some(summary.spend_projected)),
    ]);

    // No budget set means no bar — not a bar against a defaulted zero, which
    // would divide by it and read permanently red.
    if budget > 0.0 {
        payload["budget"] = json!({
            "label": "PROJECTED VS BUDGET",
            "value": format!("{} / {}", usd(summary.spend_projected), usd(budget)),
            "valueColor": color::hex(color::MUTED),
            "bar": progress_bar(
                summary.spend_projected / budget,
                BUDGET_AMBER_AT,
                BUDGET_RED_AT,
            ),
        });
    }

    let mut breakdowns: Vec<Value> = Vec::new();
    if !summary.by_resource.is_empty() {
        breakdowns.push(breakdown("TOP RESOURCE GROUPS", &summary.by_resource));
    }
    if !summary.by_type.is_empty() {
        breakdowns.push(breakdown("TOP RESOURCE TYPES", &summary.by_type));
    }
    payload["breakdowns"] = json!(breakdowns);

    payload["footer"] = status_footer(
        state.last_updated,
        state.last_error.as_deref(),
        now,
        STALE_AFTER_SECS,
    );
    payload
}

// MARK: - Fixtures

/// Which rendering `--dump-azure` should produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fixture {
    /// A normal month: headline, both breakdowns, a budget bar past amber.
    Measured,
    /// The 1st-of-month rollover gap — figures cover the last completed month,
    /// and the caption says so in amber.
    Fallback,
    /// A failed read, red, with the failure named.
    Failed,
    /// No SAS URL at all — the setup instruction.
    Unconfigured,
}

/// A hand-made state for the offline fixtures — hand-made for the same reason
/// the other panels' are: a rollover gap and an expired SAS cannot be produced
/// on demand by whichever machine runs the dump.
#[must_use]
pub fn fixture_state(kind: Fixture, at: u64) -> AzureState {
    let mut state = AzureState::new();
    let costs = |pairs: &[(&str, f64)]| {
        pairs
            .iter()
            .map(|(name, cost)| azurecost::ResourceCost {
                name: (*name).to_owned(),
                cost: *cost,
            })
            .collect::<Vec<_>>()
    };
    let summary = azurecost::CostSummary {
        spend_mtd: 1_284.55,
        spend_prior_month: Some(2_011.40),
        spend_projected: 1_942.18,
        by_resource: costs(&[
            ("rg-platform", 612.10),
            ("rg-gadget", 388.44),
            ("rg-pipe-fitting", 141.02),
            ("rg-shared", 92.80),
            ("rg-scratch", 40.19),
            ("rg-sixth-never-rendered", 9.99),
        ]),
        by_type: costs(&[
            ("Virtual Machines", 701.25),
            ("Storage", 244.60),
            ("Azure Database for PostgreSQL", 180.00),
            ("Bandwidth", 96.70),
            ("Container Apps", 62.00),
        ]),
        as_of_month: None,
        error: None,
    };

    match kind {
        Fixture::Unconfigured => state.unconfigure(),
        Fixture::Measured => state.succeeded(
            azurecost::FetchResult {
                summary,
                fingerprint: azurecost::ExportFingerprint::default(),
            },
            at,
        ),
        Fixture::Fallback => {
            let month = DateTime::from_timestamp(1_780_272_000, 0).expect("2026-06-01T00:00:00Z");
            state.succeeded(
                azurecost::FetchResult {
                    summary: azurecost::CostSummary {
                        as_of_month: Some(month),
                        ..summary
                    },
                    fingerprint: azurecost::ExportFingerprint::default(),
                },
                at,
            );
        }
        Fixture::Failed => state.failed(
            azurecost::AzureCostError::Http {
                status: 403,
                body: None,
            }
            .user_message(),
        ),
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_700_000_000;
    const BUDGET: f64 = 2_000.0;

    fn measured() -> AzureState {
        fixture_state(Fixture::Measured, NOW)
    }

    // MARK: formatting

    #[test]
    fn money_carries_a_thousands_separator_and_always_two_decimals() {
        assert_eq!(usd(0.0), "$0.00");
        assert_eq!(usd(9.5), "$9.50");
        assert_eq!(usd(999.999), "$1,000.00");
        assert_eq!(usd(1_234.56), "$1,234.56");
        assert_eq!(usd(1_284.554), "$1,284.55");
        assert_eq!(usd(1_000_000.0), "$1,000,000.00");
    }

    /// A credit note is a real thing on an Azure bill and must not render as a
    /// charge.
    #[test]
    fn a_negative_amount_keeps_its_sign_outside_the_symbol() {
        assert_eq!(usd(-1_234.56), "-$1,234.56");
    }

    #[test]
    fn a_non_finite_amount_renders_the_em_dash_rather_than_nan() {
        assert_eq!(usd(f64::NAN), UNKNOWN);
        assert_eq!(usd(f64::INFINITY), UNKNOWN);
    }

    /// `NumberFormatter` rounds half to **even** by default, so `$0.125` is
    /// `$0.12` and `$0.135` is `$0.14`. `f64::round` would give `$0.13` for the
    /// first — a cent's disagreement with the original panel on the same export.
    #[test]
    fn cents_round_half_to_even_like_the_original_formatter() {
        assert_eq!(usd(0.125), "$0.12");
        assert_eq!(usd(0.135), "$0.14");
        assert_eq!(usd(2.675), "$2.67", "the classic binary-float half-cent");
    }

    #[test]
    fn month_names_are_utc_and_english_in_both_widths() {
        let june = DateTime::from_timestamp(1_780_272_000, 0).expect("2026-06-01T00:00:00Z");
        assert_eq!(month_long(june), "June");
        assert_eq!(month_short(june), "Jun");
    }

    // MARK: the state ladder

    #[test]
    fn with_no_sas_url_the_panel_asks_for_one_in_muted_text() {
        let payload = view(&fixture_state(Fixture::Unconfigured, NOW), BUDGET, NOW);
        assert_eq!(payload["message"]["text"], UNCONFIGURED_MESSAGE);
        assert_eq!(payload["message"]["color"], color::hex(color::MUTED));
        assert!(payload["headline"].is_null());
        assert_eq!(payload["trailing"], "", "no figure means no trailing claim");
    }

    #[test]
    fn a_configured_panel_with_no_read_yet_says_it_is_reading() {
        let mut state = AzureState::new();
        state.begin();
        assert_eq!(
            view(&state, BUDGET, NOW)["message"]["text"],
            LOADING_MESSAGE
        );

        state.failed("boom".to_owned());
        state.last_error = None;
        assert_eq!(
            view(&state, BUDGET, NOW)["message"]["text"],
            NO_DATA_MESSAGE
        );
    }

    /// A failure with nothing to fall back on is red and names itself — the one
    /// state that must not be confused with "not set up yet".
    #[test]
    fn a_failed_read_with_no_prior_summary_is_red_and_named() {
        let payload = view(&fixture_state(Fixture::Failed, NOW), BUDGET, NOW);
        assert_eq!(
            payload["message"]["text"],
            "SAS expired or invalid — paste a new one in Settings"
        );
        assert_eq!(payload["message"]["color"], color::hex(color::RED));
    }

    /// A failure *with* a prior summary keeps the card and moves the reason to
    /// the footer: a daily export carried forward is still worth looking at.
    #[test]
    fn a_failed_read_after_a_good_one_keeps_the_card_and_dates_it() {
        let mut state = measured();
        state.failed("Blob request failed (HTTP 500)".to_owned());
        let payload = view(&state, BUDGET, NOW + 3_600);
        assert!(payload["message"].is_null());
        assert_eq!(payload["headline"]["value"], "$1,284.55");
        assert_eq!(
            payload["footer"]["text"],
            "⚠ Blob request failed (HTTP 500) · last ok 1h ago"
        );
    }

    /// A keychain that will not answer is not a user who has not set up Azure.
    /// Reading it as one paints the setup instruction over a working
    /// configuration — and drops the fingerprint cache, so the next good read
    /// re-downloads every partition the cache exists to avoid.
    #[test]
    fn a_failed_mint_keeps_the_card_and_the_cache() {
        let mut state = measured();
        state.failed("Azure CLI could not mint a SAS: Please run 'az login'".to_owned());

        let payload = view(&state, BUDGET, NOW + 60);
        assert!(payload["message"].is_null(), "the card stays up");
        assert_eq!(payload["headline"]["value"], "$1,284.55");
        assert_eq!(
            payload["footer"]["text"],
            "⚠ Azure CLI could not mint a SAS: Please run 'az login' · last ok 1m ago"
        );
        assert!(state.cached().is_some(), "the fingerprint cache survives");
    }

    /// The reported bug. A fresh panel has not read the credential store, so it
    /// cannot know there is no SAS URL in it — and it used to say so anyway, on
    /// every launch, at a machine where the SAS was fine. `loading: true` was
    /// already set here; the `configured == false` arm above it won.
    #[test]
    fn the_panel_says_it_is_reading_before_it_has_looked_for_a_sas() {
        let payload = view(&AzureState::new(), BUDGET, NOW);
        assert_eq!(payload["message"]["text"], LOADING_MESSAGE);
        assert_eq!(payload["message"]["color"], color::hex(color::MUTED));
        assert_eq!(payload["loading"], true);
        assert!(payload["headline"].is_null());
        assert!(payload["footer"].is_null(), "nothing to be stale yet");
    }

    /// …and the setup instruction still belongs to the pass that earned it.
    #[test]
    fn the_panel_asks_for_a_sas_once_a_pass_finds_none() {
        let mut state = AzureState::new();
        state.unconfigure();
        let payload = view(&state, BUDGET, NOW);
        assert_eq!(payload["message"]["text"], UNCONFIGURED_MESSAGE);
        assert_eq!(payload["loading"], false, "we looked; this is not loading");
    }

    /// The 4h poll only earns its cadence once it has something to show. Until
    /// then `azure_loop` retries in a minute, so a launch with the network still
    /// coming up is not red until teatime.
    #[test]
    fn the_panel_reports_whether_it_has_ever_read_the_export() {
        let mut state = AzureState::new();
        assert!(!state.has_succeeded());
        state.failed("unreachable".to_owned());
        assert!(!state.has_succeeded(), "a failure is not a reading");
        assert!(measured().has_succeeded());
    }

    // MARK: content

    #[test]
    fn the_headline_is_month_to_date_spend_with_a_muted_caption() {
        let payload = view(&measured(), BUDGET, NOW);
        assert_eq!(payload["headline"]["value"], "$1,284.55");
        assert_eq!(payload["headline"]["valueColor"], color::hex(color::GREEN));
        assert_eq!(payload["headline"]["caption"], "month-to-date");
        assert_eq!(
            payload["headline"]["captionColor"],
            color::hex(color::MUTED)
        );
        assert_eq!(payload["trailing"], "$1,284.55 MTD");
    }

    /// The rollover gap: the figures are last month's, so the caption says
    /// which month and turns amber, and the trailing label stamps it too.
    #[test]
    fn a_fallback_month_is_stamped_in_amber_on_both_the_caption_and_the_trailing() {
        let payload = view(&fixture_state(Fixture::Fallback, NOW), BUDGET, NOW);
        assert_eq!(payload["headline"]["caption"], "June · this month pending");
        assert_eq!(
            payload["headline"]["captionColor"],
            color::hex(color::AMBER)
        );
        assert_eq!(payload["trailing"], "$1,284.55 · Jun");
    }

    #[test]
    fn the_two_stat_rows_are_prior_month_and_projected() {
        let payload = view(&measured(), BUDGET, NOW);
        let stats = payload["stats"].as_array().unwrap();
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0]["label"], "PRIOR MONTH");
        assert_eq!(stats[0]["value"], "$2,011.40");
        assert_eq!(stats[1]["label"], "PROJECTED");
        assert_eq!(stats[1]["value"], "$1,942.18");
    }

    /// The no-fake-numbers rule at its sharpest: a month nobody could read must
    /// not print the same string as a month that genuinely cost nothing. The
    /// crate keeps them apart as `None` vs `Some(0.0)` and the row must too.
    #[test]
    fn an_unreadable_prior_month_renders_the_em_dash_not_a_zero() {
        let mut state = measured();
        if let Some(summary) = state.summary.as_mut() {
            summary.spend_prior_month = None;
        }
        let payload = view(&state, BUDGET, NOW);
        let stats = payload["stats"].as_array().unwrap();
        assert_eq!(stats[0]["label"], "PRIOR MONTH");
        assert_eq!(stats[0]["value"], UNKNOWN);
        assert_eq!(
            stats[1]["value"], "$1,942.18",
            "an unknown prior month must not disturb the projection"
        );
    }

    /// The other half of the same distinction: a real $0.00 month still prints
    /// a real $0.00, so the em dash above is carrying information rather than
    /// swallowing the low end of the range.
    #[test]
    fn a_genuinely_zero_prior_month_still_renders_as_money() {
        let mut state = measured();
        if let Some(summary) = state.summary.as_mut() {
            summary.spend_prior_month = Some(0.0);
        }
        let payload = view(&state, BUDGET, NOW);
        assert_eq!(payload["stats"][0]["value"], "$0.00");
    }

    #[test]
    fn each_breakdown_column_shows_at_most_five_rows() {
        let payload = view(&measured(), BUDGET, NOW);
        let columns = payload["breakdowns"].as_array().unwrap();
        assert_eq!(columns.len(), 2);
        assert_eq!(columns[0]["title"], "TOP RESOURCE GROUPS");
        assert_eq!(columns[0]["rows"].as_array().unwrap().len(), TOP_ROWS);
        assert_eq!(columns[0]["rows"][0]["name"], "rg-platform");
        assert_eq!(columns[0]["rows"][0]["value"], "$612.10");
        assert_eq!(columns[1]["title"], "TOP RESOURCE TYPES");
        assert_eq!(TOP_ROWS, 5, "the render cap is the reader's own TOP_N");
    }

    #[test]
    fn an_empty_breakdown_renders_no_column_rather_than_an_empty_one() {
        let mut state = measured();
        if let Some(summary) = state.summary.as_mut() {
            summary.by_type.clear();
        }
        let payload = view(&state, BUDGET, NOW);
        let columns = payload["breakdowns"].as_array().unwrap();
        assert_eq!(columns.len(), 1);
        assert_eq!(columns[0]["title"], "TOP RESOURCE GROUPS");
    }

    // MARK: the budget bar

    #[test]
    fn no_budget_means_no_bar() {
        assert!(view(&measured(), 0.0, NOW)["budget"].is_null());
        assert!(view(&measured(), -1.0, NOW)["budget"].is_null());
    }

    #[test]
    fn the_budget_bar_pairs_projected_with_budget_and_ambers_at_ninety_percent() {
        let payload = view(&measured(), BUDGET, NOW);
        assert_eq!(payload["budget"]["label"], "PROJECTED VS BUDGET");
        assert_eq!(payload["budget"]["value"], "$1,942.18 / $2,000.00");
        // 1942.18 / 2000 = 0.971
        assert_eq!(payload["budget"]["bar"]["color"], color::hex(color::AMBER));

        let payload = view(&measured(), 1_000.0, NOW);
        assert_eq!(payload["budget"]["bar"]["fraction"], 1.0);
        assert_eq!(payload["budget"]["bar"]["color"], color::hex(color::RED));
    }

    // MARK: staleness + the fixtures

    #[test]
    fn staleness_is_measured_against_the_four_hour_cadence_not_a_panel_default() {
        // 4h is one cycle: fine. 5h is a missed one plus an hour: not.
        assert!(view(&measured(), BUDGET, NOW + 4 * 3_600)["footer"].is_null());
        assert_eq!(
            view(&measured(), BUDGET, NOW + 5 * 3_600 + 60)["footer"]["text"],
            "⚠ stale · updated 5h ago"
        );
    }

    /// The Playwright suite renders these payloads, so a fixture that lost the
    /// case it claims to exercise would leave that suite green while covering
    /// nothing.
    #[test]
    fn the_fixtures_cover_every_rendering_the_panel_has() {
        let measured = view(&fixture_state(Fixture::Measured, NOW), BUDGET, NOW);
        assert!(measured["message"].is_null());
        assert!(!measured["budget"].is_null(), "only this one has a bar");
        assert_eq!(measured["breakdowns"].as_array().unwrap().len(), 2);
        assert_eq!(measured["headline"]["caption"], "month-to-date");

        let fallback = view(&fixture_state(Fixture::Fallback, NOW), BUDGET, NOW);
        assert_eq!(
            fallback["headline"]["captionColor"],
            color::hex(color::AMBER),
            "the fallback caption is the amber one"
        );

        let failed = view(&fixture_state(Fixture::Failed, NOW), BUDGET, NOW);
        assert_eq!(failed["message"]["color"], color::hex(color::RED));

        let unconfigured = view(&fixture_state(Fixture::Unconfigured, NOW), BUDGET, NOW);
        assert_eq!(unconfigured["message"]["text"], UNCONFIGURED_MESSAGE);
        assert_eq!(
            unconfigured["message"]["color"],
            color::hex(color::MUTED),
            "setup is not a failure"
        );
    }
}
