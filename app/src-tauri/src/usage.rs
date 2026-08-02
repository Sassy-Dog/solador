//! The **Usage** panel: Claude Code token rollups, plus a Neon and a Sentry
//! section when those providers are configured.
//!
//! Port of `DevCanopy/Views/Cockpit/Panels/ClaudeUsagePanel.swift`. The data
//! layer beneath it is [`usage`]; this module is the view side, and it holds to
//! the same rule as [`crate::github`] and [`crate::containers`]: **every string
//! and colour the frontend paints is made here.**
//!
//! Three rules run through it, all inherited from `crates/usage`:
//!
//! **Unknown is not zero.** Neon's and Sentry's summaries are enums whose
//! unmeasured variant carries no figures at all, and this module reads them
//! through their `Option` accessors. `None` renders the muted em dash; a
//! measured `0` renders `0`. The Sentry quota bar is *suppressed* when the count
//! is unknown rather than drawn at a defaulted zero — a full-looking green bar
//! nobody measured is worse than no bar.
//!
//! **An unconfigured provider has no section at all.** No Neon key means the
//! panel is pixel-identical to its Claude-only self: no heading, no em dash, no
//! layout shift. The em dash is for "configured, and we could not find out".
//!
//! **Three clocks, three footers.** Claude reads local files on the store's
//! refresh interval (`staleAfter` 150s); Neon and Sentry are hourly API reads
//! whose staleness window sits above their own cadence (90m), so a warning means
//! a stuck poller rather than the normal gap between polls. Each section carries
//! its own footer because each can fail on its own.

use serde_json::{json, Value};
use usage::{NeonInvoiceSummary, NeonUsageSummary, SentryUsageSummary, UsageTotals};
use viewmodel::cockpit::PanelKind;
use viewmodel::color;

use crate::panel::{progress_bar, status_footer};

// Re-exported so `main.rs` — where this module's name shadows the data crate's
// — reaches the layer beneath through here rather than through a `::usage::`
// escape hatch that reads like a typo. Same arrangement as `crate::github`'s
// `GitHubClient`, and for the same reason.
pub use usage::claude::default_projects_dir;
pub use usage::neon::NO_CONSUMPTION_MESSAGE as NEON_NO_CONSUMPTION_MESSAGE;
pub use usage::sentry::NO_STATS_MESSAGE as SENTRY_NO_STATS_MESSAGE;
pub use usage::{summarize_logs, NeonClient, SentryClient, UsageSummary};

/// Claude's `PanelStatusFooter(..., staleAfter: 150)` — 2.5× the default 60s
/// refresh interval, so one missed pass is not yet a warning.
pub const CLAUDE_STALE_AFTER_SECS: u64 = 150;

/// Neon's and Sentry's `staleAfter: 90 * 60`. Both poll hourly, so the window
/// has to sit above that cadence or every panel would be permanently stale.
pub const PROVIDER_STALE_AFTER_SECS: u64 = 90 * 60;

/// How often the Neon and Sentry reads run — their own fixed hourly cadence,
/// not the store's shared refresh interval. Consumption and event stats move on
/// the order of hours; asking every 30 seconds spends rate-limit budget to
/// learn nothing.
pub const PROVIDER_POLL_INTERVAL_SECS: u64 = 60 * 60;

/// Claude, before the first walk of the log tree has finished.
pub const LOADING_MESSAGE: &str = "reading logs…";

/// Claude, when there is no summary and nothing is in flight — reachable when
/// the log root itself could not be located, which is a different thing from an
/// empty week.
pub const NO_DATA_MESSAGE: &str = "no usage data";

/// A summary that walked the logs successfully and found nothing in the window.
/// Distinct from [`NO_DATA_MESSAGE`]: this one is a measurement.
pub const EMPTY_MESSAGE: &str = "no Claude usage in the last 7 days";

/// What the footer says when `~/.claude/projects` is not there at all. Swift
/// reports this from its own existence check, which is why it is the shell's
/// string and not the crate's.
pub const NO_LOG_ROOT_MESSAGE: &str = "no ~/.claude/projects";

/// Amber threshold for the Sentry quota bar — the same 90% the Azure budget bar
/// uses, because they are the same widget answering the same question.
const QUOTA_AMBER_AT: f64 = 0.9;
/// Red threshold: at quota, not past it.
const QUOTA_RED_AT: f64 = 1.0;

// MARK: - Formatting
//
// Every one of these is `Option`-in / `Option`-out, so an unmeasured figure has
// no path to a formatted string at all — `provider_row` is what turns the
// `None` into the em dash, in exactly one place.

/// Abbreviated token count: `1_234` -> `1k`, `1_200_000` -> `1.2M`.
///
/// Verbatim `ClaudeUsagePanel.tokens` — including the `%.0f` on the thousands
/// branch, which deliberately drops the decimal a reader might expect. The
/// panel's columns are monospace and fixed-width; a `1.2k` beside a `12k` is
/// what makes them jump.
#[must_use]
pub fn tokens(count: u64) -> String {
    let n = count as f64;
    if n >= 1_000_000.0 {
        format!("{:.1}M", n / 1_000_000.0)
    } else if n >= 1000.0 {
        format!("{:.0}k", n / 1000.0)
    } else {
        count.to_string()
    }
}

/// Compute-unit hours to one decimal, e.g. `12.4 CU-h`.
#[must_use]
pub fn cu_hours(hours: Option<f64>) -> Option<String> {
    hours.map(|h| format!("{h:.1} CU-h"))
}

/// Branch storage to one decimal, e.g. `3.2 GiB`. Real units only — the API
/// exposes no quota, so there is no percentage to show.
#[must_use]
pub fn gibibytes(gib: Option<f64>) -> Option<String> {
    gib.map(|g| format!("{g:.1} GiB"))
}

/// Accepted error events in the panel's abbreviated-count style.
#[must_use]
pub fn events(count: Option<u64>) -> Option<String> {
    count.map(tokens)
}

/// The Neon rate preferences, read at render time (the Sentry-quota pattern):
/// editing a rate repaints without waiting out the hourly cadence.
#[derive(Debug, Clone, Copy, Default)]
pub struct NeonRates {
    pub usd_per_cu_hour: f64,
    pub usd_per_gib_month: f64,
}

/// The last-invoice figure. `$` only for USD — a euro invoice wearing a
/// dollar sign would be a wrong number with correct digits.
#[must_use]
pub fn invoice_amount(summary: Option<&NeonInvoiceSummary>) -> Option<String> {
    match summary? {
        NeonInvoiceSummary::NoInvoices => None,
        NeonInvoiceSummary::Latest { total, currency } => Some(if currency == "USD" {
            format!("${total:.2}")
        } else {
            format!("{total:.2} {currency}")
        }),
    }
}

// MARK: - State

/// One provider's published state: what it last read, when, and why it last
/// failed.
///
/// `summary` is retained through a failure on purpose. Consumption and event
/// stats are hourly at best, so carrying the last good figure forward with the
/// reason in the footer beats blanking the section over one transient error —
/// and the footer says how old the figure is, so it cannot pass for current.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderState<S> {
    /// Whether the credential is present. `false` hides the section entirely.
    pub configured: bool,
    pub summary: Option<S>,
    pub last_updated: Option<u64>,
    pub last_error: Option<String>,
}

impl<S> Default for ProviderState<S> {
    fn default() -> Self {
        ProviderState {
            configured: false,
            summary: None,
            last_updated: None,
            last_error: None,
        }
    }
}

impl<S> ProviderState<S> {
    /// A credential that has gone away drops everything, so a stale figure can
    /// never sit behind a hidden section waiting to reappear when the key comes
    /// back.
    pub fn unconfigure(&mut self) {
        self.configured = false;
        self.summary = None;
        self.last_updated = None;
        self.last_error = None;
    }

    /// A successful read. `error` carries the "answered, but measured nothing"
    /// explanation, which is a successful read with an empty result — not a
    /// failure.
    pub fn succeeded(&mut self, summary: S, at: u64, error: Option<String>) {
        self.configured = true;
        self.summary = Some(summary);
        self.last_updated = Some(at);
        self.last_error = error;
    }

    /// A failed read: the reason is published, everything else is left exactly
    /// where it was.
    pub fn failed(&mut self, error: String) {
        self.configured = true;
        self.last_error = Some(error);
    }

    /// The credential store would not answer, so we do not know whether this
    /// provider is configured.
    ///
    /// Deliberately *not* [`unconfigure`](Self::unconfigure): a keychain that
    /// declines to answer would otherwise delete a live section and its retained
    /// figure for a full hour, with nothing on screen to say why. A provider
    /// that was never configured still stays silent — a hiccup must not conjure
    /// a section for a provider nobody set up.
    pub fn unreadable(&mut self, error: String) {
        if self.configured {
            self.last_error = Some(error);
        }
    }
}

/// Everything the Usage panel renders from.
#[derive(Debug, Default)]
pub struct UsageState {
    claude: Option<UsageSummary>,
    claude_updated: Option<u64>,
    claude_error: Option<String>,
    /// True from startup until the first walk finishes — what separates
    /// [`LOADING_MESSAGE`] from [`NO_DATA_MESSAGE`].
    claude_loading: bool,
    neon: ProviderState<NeonUsageSummary>,
    neon_invoice: ProviderState<NeonInvoiceSummary>,
    sentry: ProviderState<SentryUsageSummary>,
}

impl UsageState {
    #[must_use]
    pub fn new() -> Self {
        UsageState {
            claude_loading: true,
            ..UsageState::default()
        }
    }

    /// A completed log walk. `error` is the shell's own existence check on
    /// `~/.claude/projects`, not a failure from the walk (which skips what it
    /// cannot read rather than failing).
    pub fn apply_claude(&mut self, summary: Option<UsageSummary>, at: u64, error: Option<String>) {
        self.claude = summary;
        self.claude_updated = Some(at);
        self.claude_error = error;
        self.claude_loading = false;
    }

    pub fn neon_mut(&mut self) -> &mut ProviderState<NeonUsageSummary> {
        &mut self.neon
    }

    pub fn neon_invoice_mut(&mut self) -> &mut ProviderState<NeonInvoiceSummary> {
        &mut self.neon_invoice
    }

    pub fn sentry_mut(&mut self) -> &mut ProviderState<SentryUsageSummary> {
        &mut self.sentry
    }
}

// MARK: - View

/// One `LABEL … value` row. `None` renders the muted em dash — this is the one
/// place a missing figure becomes a string, so there is nowhere else for a
/// fabricated zero to sneak in.
fn provider_row(label: impl Into<String>, value: Option<String>) -> Value {
    match value {
        Some(text) => json!({
            "label": label.into(),
            "value": text,
            "valueColor": color::hex(color::INK),
        }),
        None => json!({
            "label": label.into(),
            "value": "—",
            "valueColor": color::hex(color::MUTED),
        }),
    }
}

/// One of the panel's three Claude state lines.
///
/// Carries its colour even though all three are muted, so the frontend never has
/// to know that they are — the Azure panel's equivalent line is muted for setup
/// and red for a failure, and one `{text, color}` shape across both keeps that
/// judgement on this side of the boundary in every case rather than most of them.
fn state_message(text: &str) -> Value {
    json!({ "text": text, "color": color::hex(color::MUTED) })
}

/// One Claude window row (`5H`, `WEEK`).
///
/// **No progress bar, deliberately.** Swift's `fiveHourTokenLimit` and
/// `weeklyTokenLimit` are both `nil`, and its `windowRow` draws a bar only
/// `if let limit, limit > 0`. A bar needs a ceiling, the subscription publishes
/// none, and a bar against an invented ceiling would be a percentage of a number
/// nobody set — so the row carries no `bar` field at all rather than a null one
/// that reads as a feature waiting to be wired.
fn window_row(label: &str, totals: &UsageTotals) -> Value {
    json!({
        "label": label,
        "value": tokens(totals.total_tokens()),
        "valueColor": color::hex(color::GREEN),
    })
}

/// The Neon section: month-to-date compute, storage, estimated charges, and
/// the last finalized invoice.
fn neon_section(
    state: &ProviderState<NeonUsageSummary>,
    invoice: &ProviderState<NeonInvoiceSummary>,
    rates: NeonRates,
    now: u64,
) -> Value {
    let summary = state.summary;
    let mut rows = vec![
        provider_row(
            "NEON COMPUTE (MTD)",
            cu_hours(summary.and_then(|s| s.compute_unit_hours())),
        ),
        provider_row(
            "NEON STORAGE",
            gibibytes(summary.and_then(|s| s.storage_gib())),
        ),
    ];
    // Absent, not "—", when unpriced or unmeasured: rates unset is setup, and
    // an estimate over unknown usage would be a fabricated number.
    if let Some(estimate) = summary
        .and_then(|s| usage::neon::estimate_usd(s, rates.usd_per_cu_hour, rates.usd_per_gib_month))
    {
        rows.push(provider_row(
            "NEON EST. CHARGES (MTD)",
            Some(format!("≈ ${estimate:.2}")),
        ));
    }
    rows.push(provider_row(
        "NEON LAST INVOICE",
        invoice_amount(invoice.summary.as_ref()),
    ));

    json!({
        "id": "neon",
        "rows": rows,
        // Consumption's error owns the footer — it is the section's primary
        // content; the invoice's reason shows only when consumption is healthy.
        "footer": status_footer(
            state.last_updated,
            state.last_error.as_deref().or(invoice.last_error.as_deref()),
            now,
            PROVIDER_STALE_AFTER_SECS,
        ),
    })
}

/// The Sentry section: accepted error events over the rolling window, with an
/// optional quota bar.
///
/// The bar needs **both** a configured quota and a known count. A quota with no
/// count would draw an empty green bar that reads "well under quota" when the
/// truth is "we have no idea", which is the fabricated-zero bug wearing a
/// different shape.
fn sentry_section(state: &ProviderState<SentryUsageSummary>, quota: u64, now: u64) -> Value {
    let accepted = state.summary.and_then(|s| s.accepted_error_events());
    let bar = match (quota, accepted) {
        (0, _) | (_, None) => Value::Null,
        (quota, Some(accepted)) => {
            progress_bar(accepted as f64 / quota as f64, QUOTA_AMBER_AT, QUOTA_RED_AT)
        }
    };
    json!({
        "id": "sentry",
        "rows": [provider_row(
            format!("SENTRY ERRORS ({}D)", usage::sentry::query::WINDOW_DAYS),
            events(accepted),
        )],
        "bar": bar,
        "footer": status_footer(
            state.last_updated,
            state.last_error.as_deref(),
            now,
            PROVIDER_STALE_AFTER_SECS,
        ),
    })
}

/// The whole panel payload.
///
/// `quota` is the store's `sentry_monthly_event_quota` and `rates` its
/// `neon_usd_per_cu_hour` / `neon_usd_per_gib_month` — both read at render time
/// rather than captured by the poller, because changing either must repaint the
/// panel without waiting out an hourly cadence for a number no API call is
/// involved in.
#[must_use]
pub fn view(state: &UsageState, quota: u64, rates: NeonRates, now: u64) -> Value {
    let kind = PanelKind::ClaudeUsage;

    // Three states, in Swift's own order: no summary at all (loading, or a log
    // root we could not locate), a summary that measured nothing, and content.
    let (message, windows, projects) = match &state.claude {
        None => {
            let text = if state.claude_loading {
                LOADING_MESSAGE
            } else {
                NO_DATA_MESSAGE
            };
            (state_message(text), Value::Array(vec![]), Value::Null)
        }
        Some(summary) if summary.last_7d.total_tokens() == 0 => (
            state_message(EMPTY_MESSAGE),
            Value::Array(vec![]),
            Value::Null,
        ),
        Some(summary) => {
            let windows = json!([
                window_row("5H", &summary.last_5h),
                window_row("WEEK", &summary.last_7d),
            ]);
            // Absent, not empty: Swift renders the divider and the heading only
            // `if !summary.projectsLast7d.isEmpty`.
            let projects = if summary.projects_last_7d.is_empty() {
                Value::Null
            } else {
                json!({
                    "label": "TOP PROJECTS (7D)",
                    "rows": summary
                        .projects_last_7d
                        .iter()
                        .take(4)
                        .map(|item| json!({
                            "name": item.name,
                            "value": tokens(item.totals.total_tokens()),
                            "dotColor": color::hex(color::GREEN_DIM),
                        }))
                        .collect::<Vec<_>>(),
                })
            };
            (Value::Null, windows, projects)
        }
    };

    // "" rather than a fabricated 0: with no summary there is no claim to make
    // about today, and Swift's `trailingLabel` returns the empty string there.
    let trailing = state.claude.as_ref().map_or_else(String::new, |summary| {
        format!("{} today", tokens(summary.today.total_tokens()))
    });

    let mut providers: Vec<Value> = Vec::new();
    if state.neon.configured {
        providers.push(neon_section(&state.neon, &state.neon_invoice, rates, now));
    }
    if state.sentry.configured {
        providers.push(sentry_section(&state.sentry, quota, now));
    }

    json!({
        "id": kind.id(),
        "title": kind.title(),
        "trailing": trailing,
        "message": message,
        "windows": windows,
        "projects": projects,
        "footer": status_footer(
            state.claude_updated,
            state.claude_error.as_deref(),
            now,
            CLAUDE_STALE_AFTER_SECS,
        ),
        "providers": providers,
    })
}

// MARK: - Fixtures

/// Which rendering `--dump-usage` should produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fixture {
    /// Everything measured: Claude content, Neon figures + last invoice,
    /// Sentry count + bar.
    Measured,
    /// Both providers configured and answering, neither measuring anything —
    /// the em-dash path, with the quota set and the bar therefore suppressed.
    Unmeasured,
    /// No Claude summary and no provider configured.
    Empty,
}

/// A hand-made state for the offline fixtures.
///
/// Hand-made rather than a real poll for the same reason the Containers fixture
/// is: the states worth testing (a Neon plan without consumption history, a
/// Sentry org with no ingest, a quota bar past amber) cannot be produced on
/// demand by whichever machine runs the dump.
#[must_use]
pub fn fixture_state(kind: Fixture, at: u64) -> UsageState {
    let mut state = UsageState::new();
    match kind {
        Fixture::Empty => {
            state.apply_claude(None, at, Some(NO_LOG_ROOT_MESSAGE.to_owned()));
        }
        Fixture::Measured => {
            state.apply_claude(Some(fixture_summary()), at, None);
            state.neon.succeeded(
                NeonUsageSummary::Measured {
                    compute_unit_hours: 12.4,
                    storage_gib: 3.25,
                    project_count: std::num::NonZeroU32::new(2).expect("non-zero"),
                },
                at,
                None,
            );
            state.neon_invoice.succeeded(
                NeonInvoiceSummary::Latest {
                    total: 15.91,
                    currency: "USD".into(),
                },
                at,
                None,
            );
            state.sentry.succeeded(
                SentryUsageSummary::Measured {
                    accepted_error_events: 9_400,
                    outcome_group_count: std::num::NonZeroUsize::new(3).expect("non-zero"),
                },
                at,
                None,
            );
        }
        Fixture::Unmeasured => {
            state.apply_claude(Some(fixture_summary()), at, None);
            state.neon.succeeded(
                NeonUsageSummary::Unmeasured,
                at,
                Some(usage::neon::NO_CONSUMPTION_MESSAGE.to_owned()),
            );
            state
                .neon_invoice
                .succeeded(NeonInvoiceSummary::NoInvoices, at, None);
            state.sentry.succeeded(
                SentryUsageSummary::Unmeasured,
                at,
                Some(usage::sentry::NO_STATS_MESSAGE.to_owned()),
            );
        }
    }
    state
}

/// The Claude half of the fixture: a busy 5h window, a busier week, and four
/// projects plus a fifth that must not render.
fn fixture_summary() -> UsageSummary {
    let totals = |input: u64| UsageTotals {
        input_tokens: input,
        ..UsageTotals::default()
    };
    UsageSummary {
        today: totals(1_240_000),
        last_5h: totals(820_400),
        last_7d: totals(4_310_000),
        projects_last_7d: vec![
            usage::claude::UsageBreakdown {
                name: "devcanopy".to_owned(),
                totals: totals(2_100_000),
            },
            usage::claude::UsageBreakdown {
                name: "velovate".to_owned(),
                totals: totals(1_050_000),
            },
            usage::claude::UsageBreakdown {
                name: "qr-ninja".to_owned(),
                totals: totals(840_000),
            },
            usage::claude::UsageBreakdown {
                name: "tailoredtip".to_owned(),
                totals: totals(300_000),
            },
            usage::claude::UsageBreakdown {
                name: "sassydog-web".to_owned(),
                totals: totals(20_000),
            },
        ],
        models_last_7d: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_700_000_000;
    const QUOTA: u64 = 10_000;
    /// The rates the measured-fixture tests price against, so the estimate
    /// row (and the dumped `Fixture::Measured` payload) actually exercises
    /// the arithmetic rather than staying permanently absent.
    const RATES: NeonRates = NeonRates {
        usd_per_cu_hour: 0.106,
        usd_per_gib_month: 0.35,
    };

    fn measured() -> UsageState {
        fixture_state(Fixture::Measured, NOW)
    }

    fn section<'a>(payload: &'a Value, id: &str) -> Option<&'a Value> {
        payload["providers"]
            .as_array()?
            .iter()
            .find(|s| s["id"] == id)
    }

    // MARK: formatting

    #[test]
    fn token_counts_abbreviate_the_way_the_swift_panel_does() {
        assert_eq!(tokens(0), "0");
        assert_eq!(tokens(999), "999");
        assert_eq!(tokens(1_000), "1k");
        assert_eq!(tokens(1_234), "1k");
        assert_eq!(tokens(12_400), "12k");
        assert_eq!(tokens(999_999), "1000k");
        assert_eq!(tokens(1_000_000), "1.0M");
        assert_eq!(tokens(1_240_000), "1.2M");
    }

    #[test]
    fn provider_figures_carry_their_units_and_one_decimal() {
        assert_eq!(cu_hours(Some(12.44)), Some("12.4 CU-h".to_owned()));
        assert_eq!(gibibytes(Some(3.25)), Some("3.2 GiB".to_owned()));
        assert_eq!(events(Some(9_400)), Some("9k".to_owned()));
    }

    /// `None` in, `None` out — an unmeasured figure never becomes a string
    /// here, so the em dash is decided in exactly one place.
    #[test]
    fn an_unmeasured_figure_never_formats_into_a_number() {
        assert_eq!(cu_hours(None), None);
        assert_eq!(gibibytes(None), None);
        assert_eq!(events(None), None);
    }

    /// Non-USD invoices name their currency instead of wearing a $.
    #[test]
    fn a_non_usd_invoice_names_its_currency() {
        assert_eq!(
            invoice_amount(Some(&NeonInvoiceSummary::Latest {
                total: 12.5,
                currency: "EUR".into(),
            })),
            Some("12.50 EUR".into())
        );
    }

    // MARK: Claude states

    #[test]
    fn before_the_first_walk_the_panel_says_it_is_reading() {
        let payload = view(&UsageState::new(), 0, NeonRates::default(), NOW);
        assert_eq!(payload["message"]["text"], LOADING_MESSAGE);
        assert_eq!(payload["trailing"], "");
        assert!(payload["windows"].as_array().unwrap().is_empty());
        assert!(payload["projects"].is_null());
    }

    /// The log root could not even be located, so nothing was read. Distinct
    /// from an empty week, which *is* a measurement.
    #[test]
    fn a_missing_log_root_says_no_usage_data_and_names_itself_in_the_footer() {
        let payload = view(
            &fixture_state(Fixture::Empty, NOW),
            0,
            NeonRates::default(),
            NOW,
        );
        assert_eq!(payload["message"]["text"], NO_DATA_MESSAGE);
        assert_eq!(
            payload["footer"]["text"],
            format!("⚠ {NO_LOG_ROOT_MESSAGE} · last ok 0s ago")
        );
    }

    #[test]
    fn a_measured_but_empty_week_says_so_rather_than_rendering_zero_rows() {
        let mut state = UsageState::new();
        state.apply_claude(Some(UsageSummary::default()), NOW, None);
        let payload = view(&state, 0, NeonRates::default(), NOW);
        assert_eq!(payload["message"]["text"], EMPTY_MESSAGE);
        assert!(payload["windows"].as_array().unwrap().is_empty());
        // The trailing label still reports today's measured zero: the walk
        // succeeded, so "0 today" is a fact, not a fabrication.
        assert_eq!(payload["trailing"], "0 today");
    }

    #[test]
    fn content_renders_both_windows_and_at_most_four_projects() {
        let payload = view(&measured(), 0, RATES, NOW);
        assert!(payload["message"].is_null());

        let windows = payload["windows"].as_array().unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0]["label"], "5H");
        assert_eq!(windows[0]["value"], "820k");
        assert_eq!(windows[1]["label"], "WEEK");
        assert_eq!(windows[1]["value"], "4.3M");
        assert_eq!(windows[0]["valueColor"], color::hex(color::GREEN));

        assert_eq!(payload["projects"]["label"], "TOP PROJECTS (7D)");
        let rows = payload["projects"]["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 4, "the fifth project is cut");
        assert_eq!(rows[0]["name"], "devcanopy");
        assert_eq!(rows[0]["value"], "2.1M");
        assert_eq!(rows[0]["dotColor"], color::hex(color::GREEN_DIM));

        assert_eq!(payload["trailing"], "1.2M today");
    }

    /// The subscription publishes no ceiling, so the window rows carry no bar
    /// at all — not a null one, which would read as a feature half-wired.
    #[test]
    fn window_rows_carry_no_progress_bar() {
        let payload = view(&measured(), 0, RATES, NOW);
        for row in payload["windows"].as_array().unwrap() {
            assert!(row.get("bar").is_none(), "got {row}");
        }
    }

    #[test]
    fn a_project_list_that_is_empty_renders_no_heading_at_all() {
        let mut summary = fixture_summary();
        summary.projects_last_7d.clear();
        let mut state = UsageState::new();
        state.apply_claude(Some(summary), NOW, None);
        assert!(view(&state, 0, NeonRates::default(), NOW)["projects"].is_null());
    }

    // MARK: provider sections

    /// An unconfigured provider contributes nothing — no heading, no em dash,
    /// no divider. The panel is pixel-identical to its Claude-only self.
    #[test]
    fn an_unconfigured_provider_renders_no_section() {
        let payload = view(
            &fixture_state(Fixture::Empty, NOW),
            QUOTA,
            NeonRates::default(),
            NOW,
        );
        assert!(payload["providers"].as_array().unwrap().is_empty());
    }

    #[test]
    fn a_measured_provider_renders_its_figures_in_ink() {
        let payload = view(&measured(), QUOTA, RATES, NOW);
        let neon = section(&payload, "neon").expect("neon section");
        assert_eq!(neon["rows"][0]["label"], "NEON COMPUTE (MTD)");
        assert_eq!(neon["rows"][0]["value"], "12.4 CU-h");
        assert_eq!(neon["rows"][0]["valueColor"], color::hex(color::INK));
        assert_eq!(neon["rows"][1]["label"], "NEON STORAGE");
        assert_eq!(neon["rows"][1]["value"], "3.2 GiB");

        let sentry = section(&payload, "sentry").expect("sentry section");
        assert_eq!(sentry["rows"][0]["label"], "SENTRY ERRORS (30D)");
        assert_eq!(sentry["rows"][0]["value"], "9k");
    }

    /// The whole point of the enums in `crates/usage`: an API that answered but
    /// measured nothing renders an em dash, never a 0.
    #[test]
    fn an_unmeasured_provider_renders_an_em_dash_and_says_why() {
        let payload = view(
            &fixture_state(Fixture::Unmeasured, NOW),
            QUOTA,
            NeonRates::default(),
            NOW,
        );

        let neon = section(&payload, "neon").expect("neon section");
        for row in neon["rows"].as_array().unwrap() {
            assert_eq!(row["value"], "—", "got {row}");
            assert_eq!(row["valueColor"], color::hex(color::MUTED));
        }
        assert_eq!(
            neon["footer"]["text"],
            format!("⚠ {NEON_NO_CONSUMPTION_MESSAGE} · last ok 0s ago")
        );

        let sentry = section(&payload, "sentry").expect("sentry section");
        assert_eq!(sentry["rows"][0]["value"], "—");
    }

    // MARK: Neon estimate + invoice rows

    /// The estimate row appears only when rates are set AND usage is measured.
    #[test]
    fn the_estimate_row_needs_rates_and_a_measurement() {
        let payload = view(&measured(), QUOTA, RATES, NOW);
        let neon = section(&payload, "neon").expect("neon section");
        let rows = neon["rows"].as_array().unwrap();
        let est = rows
            .iter()
            .find(|r| r["label"] == "NEON EST. CHARGES (MTD)")
            .expect("estimate row");
        // Measured fixture: 12.4 CU-h × 0.106 + 3.25 GiB × 0.35 = 2.4519
        assert_eq!(est["value"], "≈ $2.45");

        let unpriced = view(&measured(), QUOTA, NeonRates::default(), NOW);
        let neon = section(&unpriced, "neon").expect("neon section");
        assert!(
            neon["rows"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["label"] != "NEON EST. CHARGES (MTD)"),
            "no rates ⇒ the row is absent, not —"
        );
    }

    /// The invoice row: real dollars when known, — before the first read or
    /// for an org with no invoices yet.
    #[test]
    fn the_invoice_row_shows_the_latest_total_or_a_dash() {
        let mut state = measured();
        state.neon_invoice_mut().succeeded(
            NeonInvoiceSummary::Latest {
                total: 15.91,
                currency: "USD".into(),
            },
            NOW,
            None,
        );
        let payload = view(&state, QUOTA, NeonRates::default(), NOW);
        let neon = section(&payload, "neon").expect("neon section");
        let row = neon["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["label"] == "NEON LAST INVOICE")
            .expect("invoice row");
        assert_eq!(row["value"], "$15.91");

        let mut fresh = measured();
        fresh
            .neon_invoice_mut()
            .succeeded(NeonInvoiceSummary::NoInvoices, NOW, None);
        let payload = view(&fresh, QUOTA, NeonRates::default(), NOW);
        let neon = section(&payload, "neon").expect("neon section");
        let row = neon["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["label"] == "NEON LAST INVOICE")
            .expect("invoice row");
        assert_eq!(row["value"], "—");
    }

    /// Consumption's error owns the footer; the invoice's shows only when
    /// consumption is healthy.
    #[test]
    fn the_consumption_error_outranks_the_invoice_error_in_the_footer() {
        let mut state = measured();
        state
            .neon_invoice_mut()
            .failed("invoices: Neon API request failed (HTTP 404)".to_owned());
        state
            .neon_mut()
            .failed("Neon API request failed (HTTP 500)".to_owned());
        let payload = view(&state, QUOTA, NeonRates::default(), NOW);
        let footer = &section(&payload, "neon").expect("neon")["footer"]["text"];
        assert!(footer.as_str().unwrap().contains("HTTP 500"));
    }

    // MARK: the quota bar

    #[test]
    fn the_quota_bar_needs_both_a_quota_and_a_known_count() {
        // Quota set, count known -> a bar.
        let with_bar = view(&measured(), QUOTA, RATES, NOW);
        let bar = &section(&with_bar, "sentry").expect("sentry")["bar"];
        assert_eq!(bar["fraction"], 0.94);
        assert_eq!(bar["color"], color::hex(color::AMBER), "9400/10000 is 94%");

        // No quota -> no bar, however well measured the count is.
        let no_quota = view(&measured(), 0, RATES, NOW);
        assert!(section(&no_quota, "sentry").expect("sentry")["bar"].is_null());

        // Quota set, count unknown -> no bar. A bar at a defaulted 0 would
        // read "comfortably under quota" when the truth is "we don't know".
        let unknown = view(
            &fixture_state(Fixture::Unmeasured, NOW),
            QUOTA,
            NeonRates::default(),
            NOW,
        );
        assert!(section(&unknown, "sentry").expect("sentry")["bar"].is_null());
    }

    #[test]
    fn the_quota_bar_reds_at_quota_and_pins_full_past_it() {
        let mut state = measured();
        state.sentry.succeeded(
            SentryUsageSummary::Measured {
                accepted_error_events: 25_000,
                outcome_group_count: std::num::NonZeroUsize::new(1).expect("non-zero"),
            },
            NOW,
            None,
        );
        let payload = view(&state, QUOTA, RATES, NOW);
        let bar = &section(&payload, "sentry").expect("sentry")["bar"];
        assert_eq!(bar["fraction"], 1.0);
        assert_eq!(bar["color"], color::hex(color::RED));
    }

    /// A real zero is a measurement and does get a bar — an empty one.
    #[test]
    fn a_measured_zero_still_draws_its_bar() {
        let mut state = measured();
        state.sentry.succeeded(
            SentryUsageSummary::Measured {
                accepted_error_events: 0,
                outcome_group_count: std::num::NonZeroUsize::new(2).expect("non-zero"),
            },
            NOW,
            None,
        );
        let payload = view(&state, QUOTA, RATES, NOW);
        let sentry = section(&payload, "sentry").expect("sentry");
        assert_eq!(sentry["rows"][0]["value"], "0");
        assert_eq!(sentry["bar"]["fraction"], 0.0);
    }

    // MARK: footers

    #[test]
    fn each_section_carries_its_own_footer_on_its_own_window() {
        // 10 minutes: stale for Claude (150s), fine for the hourly providers.
        let payload = view(&measured(), QUOTA, RATES, NOW + 600);
        assert_eq!(payload["footer"]["text"], "⚠ stale · updated 10m ago");
        assert!(section(&payload, "neon").expect("neon")["footer"].is_null());
        assert!(section(&payload, "sentry").expect("sentry")["footer"].is_null());
    }

    #[test]
    fn a_provider_failure_keeps_the_last_good_figure_and_dates_it() {
        let mut state = measured();
        state
            .neon
            .failed("Neon API request failed (HTTP 500)".to_owned());
        let payload = view(&state, QUOTA, RATES, NOW + 300);
        let neon = section(&payload, "neon").expect("neon");
        assert_eq!(neon["rows"][0]["value"], "12.4 CU-h", "last good is kept");
        assert_eq!(
            neon["footer"]["text"],
            "⚠ Neon API request failed (HTTP 500) · last ok 5m ago"
        );
    }

    /// Clearing a credential must take the figures with it, or the next time
    /// the key is saved the section reappears showing an hour-old number as if
    /// it had just been read.
    #[test]
    fn unconfiguring_a_provider_drops_its_retained_figure() {
        let mut state = measured();
        state.neon.unconfigure();
        let payload = view(&state, QUOTA, RATES, NOW);
        assert!(section(&payload, "neon").is_none());

        state.neon.configured = true;
        let payload = view(&state, QUOTA, RATES, NOW);
        assert_eq!(
            section(&payload, "neon").expect("neon")["rows"][0]["value"],
            "—"
        );
    }

    /// A keychain that will not answer is not a user without a Neon key.
    /// Reading it as one deletes a live section, and its retained figure, for a
    /// full hour with nothing on screen to say why.
    #[test]
    fn an_unreadable_credential_store_keeps_the_section_and_says_so() {
        let mut state = measured();
        state
            .neon_mut()
            .unreadable("couldn't read the credential store".to_owned());

        let payload = view(&state, QUOTA, RATES, NOW + 120);
        let neon = section(&payload, "neon").expect("the section stays");
        assert_eq!(neon["rows"][0]["value"], "12.4 CU-h", "last good is kept");
        assert_eq!(
            neon["footer"]["text"],
            "⚠ couldn't read the credential store · last ok 2m ago"
        );
    }

    /// …and must not conjure a section for a provider nobody configured.
    #[test]
    fn an_unreadable_credential_store_stays_silent_when_nothing_was_configured() {
        let mut state = fixture_state(Fixture::Empty, NOW);
        state
            .sentry_mut()
            .unreadable("couldn't read the credential store".to_owned());
        assert!(view(&state, QUOTA, NeonRates::default(), NOW)["providers"]
            .as_array()
            .expect("providers")
            .is_empty());
    }

    // MARK: the fixtures

    /// The Playwright suite renders these payloads, so a fixture that quietly
    /// lost the case it claims to exercise would leave that suite green while
    /// covering nothing. Asserted here, where a Rust test can see it.
    #[test]
    fn the_fixtures_cover_every_rendering_the_panel_has() {
        let measured = view(&fixture_state(Fixture::Measured, NOW), QUOTA, RATES, NOW);
        assert!(measured["message"].is_null(), "content, not a state line");
        assert_eq!(measured["windows"].as_array().unwrap().len(), 2);
        assert!(!measured["projects"]["rows"].as_array().unwrap().is_empty());
        assert_eq!(measured["providers"].as_array().unwrap().len(), 2);
        assert!(
            !section(&measured, "sentry").expect("sentry")["bar"].is_null(),
            "the quota bar is only exercised by this fixture"
        );
        let neon_rows = section(&measured, "neon").expect("neon")["rows"]
            .as_array()
            .unwrap()
            .clone();
        assert!(
            neon_rows
                .iter()
                .any(|r| r["label"] == "NEON EST. CHARGES (MTD)"),
            "priced rates must produce an estimate row in the fixture"
        );
        assert_eq!(
            neon_rows
                .iter()
                .find(|r| r["label"] == "NEON LAST INVOICE")
                .expect("invoice row")["value"],
            "$15.91"
        );

        let unmeasured = view(
            &fixture_state(Fixture::Unmeasured, NOW),
            QUOTA,
            NeonRates::default(),
            NOW,
        );
        assert_eq!(unmeasured["providers"].as_array().unwrap().len(), 2);
        assert!(
            section(&unmeasured, "sentry").expect("sentry")["bar"].is_null(),
            "a quota with no count must suppress the bar"
        );
        for id in ["neon", "sentry"] {
            let s = section(&unmeasured, id).expect(id);
            assert!(!s["footer"].is_null(), "{id} explains why it is blank");
            assert_eq!(s["rows"][0]["value"], "—");
        }
        assert!(
            section(&unmeasured, "neon").expect("neon")["rows"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["label"] != "NEON EST. CHARGES (MTD)"),
            "unpriced/unmeasured fixture must not show an estimate row"
        );

        let empty = view(
            &fixture_state(Fixture::Empty, NOW),
            QUOTA,
            NeonRates::default(),
            NOW,
        );
        assert_eq!(empty["message"]["text"], NO_DATA_MESSAGE);
        assert!(empty["providers"].as_array().unwrap().is_empty());
        assert_eq!(empty["trailing"], "");
    }
}
