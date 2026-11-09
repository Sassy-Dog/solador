//! Three panels share this state. **Usage** is the metered Neon, Sentry and
//! Vercel sections. **Anthropic** and **OpenAI** show whichever parts of their
//! source the operator added: a subscription's limit windows, API spend, and,
//! for Anthropic, this machine's Claude Code rollups.
//!
//! Those readings are never merged: the rollups are this machine's consumption
//! from local logs, the plan meters a subscription's utilisation of its own
//! limits ([`crate::plan_usage`]), and API spend is what an organization was
//! charged. The plans lead, because a percentage of a limit is the figure an
//! operator acts on; API spend follows; the rollups come last under their own
//! `machineHeading`.
//!
//! **A part that is not set up says nothing.** The operator chose to leave it
//! out, so it gets no row, no warning and no setup line. A panel's only setup
//! sentence is for the source having no part at all.
//!
//! Port of `ClaudeUsagePanel`. The data
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
//! **An unconfigured provider has no section at all.** No Neon key means no
//! Neon heading, no em dash and no layout shift: the panel shows only the
//! providers that are set up. The em dash is for "configured, and we could not
//! find out".
//!
//! **Three clocks, one warning line.** Claude reads local files on its own
//! rollup cadence (`staleAfter` 2.5 cycles, 150s at the default 60s); Neon,
//! Sentry and Vercel are API reads on the providers' cadence (hourly by
//! default) whose staleness window is 1.5 cycles of that cadence (90m at the
//! default), so a warning means a stuck poller rather than the normal gap
//! between polls. Each section decides
//! its own warning because each can fail on its own — and every one of them is
//! then folded into the panel's **single header line** by
//! [`merged_footer`](crate::panel::merged_footer), naming the section it came
//! from.
//!
//! **Nothing a section can say may live under the section** (#351). A warning in
//! the body makes the card a line taller the moment it fires. The header is
//! always rendered, so a warning there costs no height at all. That is also why
//! the attribution is load-bearing rather than decorative — the hoisted lines are
//! byte-identical without it, and `⚠ stale · updated 23h ago` twice over says
//! the same thing twice and identifies neither section.
//!
//! **And each metered section carries a second clock** (#338, the Azure Cost
//! panel's arrangement one panel over). `freshness` is [`Freshness`] over the
//! age of the last **success** and answers *"how old is the figure on screen"*;
//! the warning answers *"did the last attempt fail, or is the poller late"*. The
//! first speaks after one cycle and the second after one and a half, so half a
//! cycle apart (30 minutes at the hourly default), and a nearly-hour-old dollar
//! figure rendering identically to one measured a
//! second ago is the same class of bug as unknown-rendered-as-zero. They are two
//! fields for that reason and **must not be folded into one string**.
//!
//! **Those clocks are in the header too** (#355). They stayed in the body when
//! the warnings left it, on the reasoning that a clock dates a figure that is in
//! the body — but they carry the same appear/disappear height cost the warnings
//! did: a section is `Live` while it polls on cadence and paints nothing, so the
//! line shows up only when a poll is missed, and its arrival makes the card a
//! line taller. One line rather than six, and the invariant is not held until it
//! is gone. So each section's clock is
//! [`attributed_freshness_payload`](crate::panel::attributed_freshness_payload)
//! and they fold into the panel's own single header line by
//! [`merged_freshness`](crate::panel::merged_freshness) — a **second** header
//! element beside the warning, never the same string. Same header is not the
//! same question.

use serde_json::{json, Value};
use usage::{
    ApiSpendSummary, NeonInvoiceSummary, NeonUsageSummary, SentryUsageSummary, UsageTotals,
    VercelUsageSummary,
};
use viewmodel::cockpit::PanelKind;
use viewmodel::color;
use viewmodel::format::relative_age;
use viewmodel::freshness::Freshness;

use crate::panel::{
    attributed_freshness_payload, attributed_status_footer, merged_footer, merged_freshness,
    progress_bar, status_footer, Configured,
};
use crate::plan_usage::{PlanInputs, PlanSource};
use subscription::Provider;

// Re-exported so `main.rs` — where this module's name shadows the data crate's
// — reaches the layer beneath through here rather than through a `::usage::`
// escape hatch that reads like a typo. Same arrangement as `crate::github`'s
// `GitHubClient`, and for the same reason.
pub use usage::claude::default_projects_dir;
pub use usage::neon::NO_CONSUMPTION_MESSAGE as NEON_NO_CONSUMPTION_MESSAGE;
pub use usage::sentry::NO_STATS_MESSAGE as SENTRY_NO_STATS_MESSAGE;
pub use usage::vercel::NO_SPEND_MESSAGE as VERCEL_NO_SPEND_MESSAGE;
pub use usage::{
    summarize_logs, AnthropicCostClient, NeonClient, OpenAiCostClient, SentryClient, UsageSummary,
    VercelClient,
};

/// Claude's `PanelStatusFooter(..., staleAfter: 150)` — 2.5× the default 60s
/// rollup cadence, so one missed pass is not yet a warning. The window at the
/// default; the live one follows [`PanelInterval::ClaudeRollups`] through
/// `UsageState::set_claude_cadence_secs`.
pub const CLAUDE_STALE_AFTER_SECS: u64 = 150;

/// The Neon, Sentry and Vercel footers' staleness window for the operator's
/// [`PanelInterval::UsageProviders`] cadence: one and a half cycles
/// ([`crate::panel::stale_after_for_slow_cadence`]), which is the 90 minutes
/// these footers have always had at the default hour (#635). Read at render
/// time, so a cadence save moves it in the same response.
#[must_use]
pub fn provider_stale_after_secs(cadence_secs: u64) -> u64 {
    crate::panel::stale_after_for_slow_cadence(cadence_secs)
}

/// How often the Neon, Sentry and Vercel reads run by default — hourly, on a cadence of their own, not the
/// Claude rollups' cadence. Consumption and event stats move on the
/// order of hours; asking every 30 seconds spends rate-limit budget to learn
/// nothing.
///
/// **`usage_loop` no longer reads this.** Since #301 the cadence comes from the
/// store, and this hour is its *default*, declared in
/// `store::settings::PanelInterval::spec` — the only place that can hold it,
/// since `crates/store` cannot import this crate. What stays here is this
/// source's own recommendation, the role `azurecost::POLL_INTERVAL` plays for
/// the cost export, and `cfg(test)` because the mirror test is now its only
/// reader: editing this number moves no cadence, it makes the test say the two
/// have parted.
#[cfg(test)]
pub const PROVIDER_POLL_INTERVAL_SECS: u64 = 60 * 60;

/// Claude, before the first walk of the log tree has finished.
pub const LOADING_MESSAGE: &str = "reading logs…";

/// Claude, when there is no summary and nothing is in flight — reachable when
/// the log root itself could not be located, which is a different thing from an
/// empty week.
pub const NO_DATA_MESSAGE: &str = "no usage data";

/// The Usage panel before a pass has read every key.
pub const PROVIDERS_LOADING_MESSAGE: &str = "reading usage…";

/// A summary that walked the logs successfully and found nothing in the window.
/// Distinct from [`NO_DATA_MESSAGE`]: this one is a measurement.
pub const EMPTY_MESSAGE: &str = "no Claude usage in the last 7 days";

/// What the footer says when `~/.claude/projects` is not there at all. the original
/// reports this from its own existence check, which is why it is the shell's
/// string and not the crate's.
pub const NO_LOG_ROOT_MESSAGE: &str = "no ~/.claude/projects";

/// The heading over this machine's token rollups, below the plan meters.
pub const MACHINE_HEADING: &str = "THIS MACHINE";

/// Amber threshold for the Sentry quota bar — the same 90% the Azure budget bar
/// uses, because they are the same widget answering the same question.
const QUOTA_AMBER_AT: f64 = 0.9;
/// Red threshold: at quota, not past it.
const QUOTA_RED_AT: f64 = 1.0;

// The metered sections' ids — **and their names in the panel header**, on both
// hoisted lines.
//
// One constant serving all of them, deliberately. A hoisted line's whole job is
// to send a reader to a block on the card, so `⚠ neon: stale · updated 23h ago`
// — or `neon: as of 23h ago` — above a section the payload keys as something
// else would be an attribution pointing at nothing. Two strings could drift
// apart; one cannot.
const NEON_ID: &str = "neon";
const SENTRY_ID: &str = "sentry";
const VERCEL_ID: &str = "vercel";
/// API spend's ids, on the same terms: each is the section's name in the header
/// line and its row on the tile.
pub const ANTHROPIC_API_ID: &str = "anthropic-api";
pub const OPENAI_API_ID: &str = "openai-api";
/// Month to date, like every other cost figure in the app.
pub const API_SPEND_LABEL: &str = "API spend (MTD)";

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
    pub configured: Configured,
    pub summary: Option<S>,
    pub last_updated: Option<u64>,
    pub last_error: Option<String>,
    /// Whether `last_error` is the credential store's refusal
    /// ([`unreadable`](Self::unreadable)) rather than a vendor's failure. Only
    /// that reason is dropped when the next read begins.
    reason_is_unreadable: bool,
}

impl<S> Default for ProviderState<S> {
    fn default() -> Self {
        ProviderState {
            configured: Configured::Unknown,
            summary: None,
            last_updated: None,
            last_error: None,
            reason_is_unreadable: false,
        }
    }
}

impl<S> ProviderState<S> {
    /// A credential that has gone away drops everything, so a stale figure can
    /// never sit behind a hidden section waiting to reappear when the key comes
    /// back.
    pub fn unconfigure(&mut self) {
        self.configured = Configured::Absent;
        self.summary = None;
        self.last_updated = None;
        self.last_error = None;
        self.reason_is_unreadable = false;
    }

    /// A pass read this provider's key. Called before the request goes out, so
    /// the section can say it is loading rather than waiting on the round trip
    /// to learn it exists at all.
    ///
    /// A reason the credential store set is cleared too: a keychain that has
    /// recovered must not keep showing the reason it was locked until the next
    /// read lands. A vendor's failure keeps its reason while the next read is in
    /// flight, so a provider that keeps failing does not flip to loading.
    pub fn begin(&mut self) {
        self.configured = Configured::Present;
        if self.reason_is_unreadable {
            self.last_error = None;
            self.reason_is_unreadable = false;
        }
    }

    /// A successful read. `error` carries the "answered, but measured nothing"
    /// explanation, which is a successful read with an empty result — not a
    /// failure.
    pub fn succeeded(&mut self, summary: S, at: u64, error: Option<String>) {
        self.configured = Configured::Present;
        self.summary = Some(summary);
        self.last_updated = Some(at);
        self.last_error = error;
        self.reason_is_unreadable = false;
    }

    /// A failed read: the reason is published, everything else is left exactly
    /// where it was.
    ///
    /// This still marks the provider configured, because only a pass holding a
    /// key gets far enough to fail — but [`begin`](Self::begin) should already
    /// have done so. Learning it here is what used to make a provider's very
    /// first appearance be a row of em dashes under an error.
    pub fn failed(&mut self, error: String) {
        self.configured = Configured::Present;
        self.last_error = Some(error);
        self.reason_is_unreadable = false;
    }

    /// Whether this provider is configured and has neither a figure nor a
    /// reason yet: no read has settled, or the credential store's refusal was
    /// dropped when a new read began.
    #[must_use]
    pub fn is_loading(&self) -> bool {
        self.configured.is_present() && self.summary.is_none() && self.last_error.is_none()
    }

    /// The credential store would not answer, so we do not know whether this
    /// provider is configured.
    ///
    /// Deliberately *not* [`unconfigure`](Self::unconfigure): a keychain that
    /// declines to answer would otherwise delete a live section and its retained
    /// figure for a full hour, with nothing on screen to say why. A provider
    /// that was never configured still gets no section — a hiccup must not
    /// conjure one for a provider nobody set up — but one no pass has settled
    /// keeps the reason, or the panel would wait on it for as long as the
    /// keychain stays locked. Its section needs `is_present`, so the reason
    /// shows only in the sentence a panel with no section paints.
    pub fn unreadable(&mut self, error: String) {
        if !self.configured.is_absent() {
            self.last_error = Some(error);
            self.reason_is_unreadable = true;
        }
    }

    /// No pass has read this provider's key yet, and none has failed to.
    #[must_use]
    pub fn is_undecided(&self) -> bool {
        self.configured.is_unknown() && self.last_error.is_none()
    }
}

/// Everything the Usage, Anthropic and OpenAI panels render from.
#[derive(Debug, Default)]
pub struct UsageState {
    claude: Option<UsageSummary>,
    claude_updated: Option<u64>,
    /// When the log walk last completed **without** an error.
    ///
    /// Separate from `claude_updated` because [`status_footer`] renders its
    /// argument as `last ok {age}`. A machine with no `~/.claude/projects` gets
    /// that error on every pass, and feeding it the attempt clock produced
    /// `⚠ no ~/.claude/projects · last ok 0s ago` — permanently, and about a
    /// reading that never happened.
    claude_succeeded: Option<u64>,
    claude_error: Option<String>,
    /// The Claude-rollup cadence the last pass ran on, which the footer's
    /// staleness window follows. `None` before the first pass: the default's.
    claude_cadence_secs: Option<u64>,
    /// True from startup until the first walk finishes — what separates
    /// [`LOADING_MESSAGE`] from [`NO_DATA_MESSAGE`].
    claude_loading: bool,
    neon: ProviderState<NeonUsageSummary>,
    neon_invoice: ProviderState<NeonInvoiceSummary>,
    sentry: ProviderState<SentryUsageSummary>,
    vercel: ProviderState<VercelUsageSummary>,
    /// The Claude plan's limit windows, behind their own opt-in. A separate
    /// reading from `claude` above, which is this machine's log rollup: the
    /// two must never be merged (`crate::plan_usage`).
    claude_plan: PlanSource,
    /// The Codex plan's limit windows, behind their own opt-in.
    codex_plan: PlanSource,
    /// This month's API spend, each from its vendor's Admin key.
    anthropic_api: ProviderState<ApiSpendSummary>,
    openai_api: ProviderState<ApiSpendSummary>,
}

impl UsageState {
    #[must_use]
    pub fn new() -> Self {
        UsageState {
            claude_loading: true,
            ..UsageState::default()
        }
    }

    /// Told by the loop at the top of each pass, so a cadence edit moves the
    /// footer's window with the very pass that runs on it.
    pub fn set_claude_cadence_secs(&mut self, cadence_secs: u64) {
        self.claude_cadence_secs = Some(cadence_secs);
    }

    /// How old the Claude rollups may be before the footer says so, following
    /// the cadence last told.
    pub(crate) fn claude_stale_after_secs(&self) -> u64 {
        self.claude_cadence_secs.map_or(
            CLAUDE_STALE_AFTER_SECS,
            crate::panel::stale_after_for_cadence,
        )
    }

    /// A completed log walk. `error` is the shell's own existence check on
    /// `~/.claude/projects`, not a failure from the walk (which skips what it
    /// cannot read rather than failing).
    pub fn apply_claude(&mut self, summary: Option<UsageSummary>, at: u64, error: Option<String>) {
        self.claude = summary;
        self.claude_updated = Some(at);
        if error.is_none() {
            self.claude_succeeded = Some(at);
        }
        self.claude_error = error;
        self.claude_loading = false;
    }

    pub fn neon_mut(&mut self) -> &mut ProviderState<NeonUsageSummary> {
        &mut self.neon
    }

    pub fn neon_invoice_mut(&mut self) -> &mut ProviderState<NeonInvoiceSummary> {
        &mut self.neon_invoice
    }

    /// Clears consumption *and* invoice state together. A vanished credential
    /// must take the invoice figure with it, not just the consumption one — an
    /// untouched `neon_invoice` would keep rendering last month's dollar total
    /// as current the moment the key is saved again, with no read behind it.
    /// One call for the pair means the shell cannot unconfigure one half and
    /// forget the other.
    pub fn neon_unconfigure(&mut self) {
        self.neon.unconfigure();
        self.neon_invoice.unconfigure();
    }

    pub fn sentry_mut(&mut self) -> &mut ProviderState<SentryUsageSummary> {
        &mut self.sentry
    }

    pub fn vercel_mut(&mut self) -> &mut ProviderState<VercelUsageSummary> {
        &mut self.vercel
    }

    pub fn anthropic_api_mut(&mut self) -> &mut ProviderState<ApiSpendSummary> {
        &mut self.anthropic_api
    }

    pub fn openai_api_mut(&mut self) -> &mut ProviderState<ApiSpendSummary> {
        &mut self.openai_api
    }

    /// Local usage was turned off, so nothing from the logs is kept. Back to the
    /// state a first walk starts from, which is what turning it on again needs.
    pub fn clear_claude(&mut self) {
        self.claude = None;
        self.claude_updated = None;
        self.claude_succeeded = None;
        self.claude_error = None;
        self.claude_loading = true;
    }

    #[must_use]
    pub fn plan(&self, provider: Provider) -> &PlanSource {
        match provider {
            Provider::Claude => &self.claude_plan,
            Provider::Codex => &self.codex_plan,
        }
    }

    pub fn plan_mut(&mut self, provider: Provider) -> &mut PlanSource {
        match provider {
            Provider::Claude => &mut self.claude_plan,
            Provider::Codex => &mut self.codex_plan,
        }
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
/// **No progress bar, deliberately.** the original's `fiveHourTokenLimit` and
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

/// One metered provider's contribution to the panel: the rows the body paints,
/// and the two header lines it raises — the warning about its poller and the
/// clock dating its figures.
///
/// Three values because the three render in three different places, and the two
/// lines are returned **together with** the section rather than being rebuilt in
/// [`view`]: the rule deciding *which* failure a section reports — Neon's
/// consumption error outranking its invoice error, against consumption's own
/// clock — belongs beside the section that owns it, not in the loop that
/// collects them.
///
/// `warning` is `Null` for a healthy section exactly as [`status_footer`] has
/// always been, and `freshness` carries a null `text` for a current one, so
/// neither reaches the header when there is nothing to say.
struct ProviderView {
    section: Value,
    warning: Value,
    freshness: Value,
}

/// How old one metered section's figures are, named, as the header receives it.
///
/// The clock is `last_updated`, which is this provider's last **success** — the
/// very field [`status_footer`] promises `last ok` about, asked the other
/// question. `None` (nothing has ever been read) classifies to
/// [`Freshness::Unknown`], which publishes a null age rather than a zero: a
/// section that has never answered must not paint as the freshest thing on the
/// card, and must not reserve a blank line's worth of header either.
///
/// Attributed with `source`, because the panel has one header and three of
/// these: read at the same moment, two sections emit the byte-identical
/// `as of 23h ago`, and a line carrying it twice names neither of them.
///
/// `cadence_secs` is the operator's `PanelInterval::UsageProviders`, not
/// [`provider_stale_after_secs`]. The two are deliberately different edges — a
/// reading stops being current after one whole cycle, and the *warning* only
/// fires once the poller looks stuck — which is the window neither field can
/// express alone.
fn provider_freshness<S>(
    source: &str,
    state: &ProviderState<S>,
    cadence_secs: u64,
    now: u64,
) -> Value {
    attributed_freshness_payload(
        source,
        Freshness::classify(
            state.last_updated.map(|at| now.saturating_sub(at)),
            cadence_secs,
        ),
    )
}

/// The Neon section: month-to-date compute, storage, estimated charges, and
/// the last finalized invoice.
fn neon_section(
    state: &ProviderState<NeonUsageSummary>,
    invoice: &ProviderState<NeonInvoiceSummary>,
    rates: NeonRates,
    cadence_secs: u64,
    now: u64,
) -> ProviderView {
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

    ProviderView {
        section: json!({
            "id": NEON_ID,
            "rows": rows,
        }),
        // Consumption's error owns the warning — it is the section's primary
        // content; the invoice's reason shows only when consumption is healthy.
        // The "last ok" age is always consumption's `last_updated`, even when
        // the displayed error text is the invoice's: one warning per section,
        // and consumption is the age that matters here since the invoice is a
        // slow-moving monthly figure anyway.
        warning: attributed_status_footer(
            NEON_ID,
            state.last_updated,
            state
                .last_error
                .as_deref()
                .or(invoice.last_error.as_deref()),
            now,
            provider_stale_after_secs(cadence_secs),
        ),
        // Consumption's clock too, because it is this section's primary
        // content. The invoice is a monthly figure whose own age says nothing a
        // reader can act on.
        freshness: provider_freshness(NEON_ID, state, cadence_secs, now),
    }
}

/// The Sentry section: accepted error events over the rolling window, with an
/// optional quota bar.
///
/// The bar needs **both** a configured quota and a known count. A quota with no
/// count would draw an empty green bar that reads "well under quota" when the
/// truth is "we have no idea", which is the fabricated-zero bug wearing a
/// different shape.
fn sentry_section(
    state: &ProviderState<SentryUsageSummary>,
    quota: u64,
    cadence_secs: u64,
    now: u64,
) -> ProviderView {
    let accepted = state.summary.and_then(|s| s.accepted_error_events());
    let bar = match (quota, accepted) {
        (0, _) | (_, None) => Value::Null,
        (quota, Some(accepted)) => {
            progress_bar(accepted as f64 / quota as f64, QUOTA_AMBER_AT, QUOTA_RED_AT)
        }
    };
    ProviderView {
        section: json!({
            "id": SENTRY_ID,
            "rows": [provider_row(
                format!("SENTRY ERRORS ({}D)", usage::sentry::query::WINDOW_DAYS),
                events(accepted),
            )],
            "bar": bar,
        }),
        warning: attributed_status_footer(
            SENTRY_ID,
            state.last_updated,
            state.last_error.as_deref(),
            now,
            provider_stale_after_secs(cadence_secs),
        ),
        freshness: provider_freshness(SENTRY_ID, state, cadence_secs, now),
    }
}

/// USD to the cent. Sub-cent figures render `$0.00`, which is honest: at
/// month-to-date scale the rows that matter are dollars, and a `$0.0004`
/// would be precision nobody can act on dressed as significance.
#[must_use]
pub fn usd(amount: Option<f64>) -> Option<String> {
    amount.map(|a| format!("${a:.2}"))
}

/// The Vercel section: what the month costs, what it costs *beyond the plan*,
/// and where the money goes.
///
/// Two totals rather than one because they answer different questions.
/// `EffectiveCost` amortizes the subscription and is what Vercel costs to run;
/// `BilledCost` is what an invoice would add on top, and on a plan with
/// included allowance it is usually near zero. Headlining the second would
/// report two cents for an account spending real money.
fn vercel_section(
    state: &ProviderState<VercelUsageSummary>,
    cadence_secs: u64,
    now: u64,
) -> ProviderView {
    let summary = state.summary.as_ref();
    let mut rows = vec![
        provider_row(
            "VERCEL SPEND (MTD)".to_owned(),
            usd(summary.and_then(VercelUsageSummary::effective_usd)),
        ),
        provider_row(
            "VERCEL BEYOND PLAN".to_owned(),
            usd(summary.and_then(VercelUsageSummary::billed_usd)),
        ),
    ];
    // The costliest services, named. The plan itself leads this list — it
    // arrives as a `Usage` row called "Pro" — which is the truth about where
    // the money goes even though it is not a lever.
    for svc in summary
        .map(VercelUsageSummary::top_services)
        .unwrap_or_default()
    {
        rows.push(provider_row(
            svc.name.to_uppercase(),
            usd(Some(svc.effective_usd)),
        ));
    }
    ProviderView {
        section: json!({
            "id": VERCEL_ID,
            "rows": rows,
            "bar": Value::Null,
        }),
        warning: attributed_status_footer(
            VERCEL_ID,
            state.last_updated,
            state.last_error.as_deref(),
            now,
            provider_stale_after_secs(cadence_secs),
        ),
        freshness: provider_freshness(VERCEL_ID, state, cadence_secs, now),
    }
}

/// The sentence for a Usage panel with no section to show, or `None` while one
/// is still to come.
///
/// Only a provider a pass has seen to be `Absent` may paint a setup instruction,
/// so this says so only when all three are. A provider that could not be read
/// says why instead.
fn no_section_message(state: &UsageState) -> Value {
    use crate::settings::{ADD_SOURCE_LABEL, OPEN_LABEL, SOURCES_LABEL};
    let undecided =
        state.neon.is_undecided() || state.sentry.is_undecided() || state.vercel.is_undecided();
    let reason = [
        state.neon.last_error.as_deref(),
        state.sentry.last_error.as_deref(),
        state.vercel.last_error.as_deref(),
    ]
    .into_iter()
    .flatten()
    .next();
    if undecided {
        state_message(PROVIDERS_LOADING_MESSAGE)
    } else if let Some(reason) = reason {
        state_message(reason)
    } else {
        state_message(&format!(
            "No cloud usage yet \u{2014} add Neon, Sentry or Vercel in \
             {OPEN_LABEL} \u{2192} {SOURCES_LABEL} \u{2192} {ADD_SOURCE_LABEL}"
        ))
    }
}

/// The Usage panel's payload: the metered providers and nothing else.
///
/// `quota` is the store's `sentry_monthly_event_quota` and `rates` its
/// `neon_usd_per_cu_hour` / `neon_usd_per_gib_month` — both read at render time
/// rather than captured by the poller, because changing either must repaint the
/// panel without waiting out an hourly cadence for a number no API call is
/// involved in.
///
/// `cadence_secs` is the operator's cadence for the **metered providers**
/// (`PanelInterval::UsageProviders`), and it is what each section's clock on the
/// panel's `freshness` line is classified against. Read at render time for the
/// same reason as the two above: a shortened interval must re-date the sections
/// now rather than an hour from now.
#[must_use]
pub fn view(
    state: &UsageState,
    quota: u64,
    rates: NeonRates,
    cadence_secs: u64,
    now: u64,
) -> Value {
    let kind = PanelKind::Usage;

    // `is_present`, not `!is_absent`: a provider nobody has looked for yet
    // contributes no markup, exactly as an unconfigured one does. The section
    // appears once a pass has read its key — which is `begin()`, before the
    // request, so its first frame is a loading line rather than the row of em
    // dashes a failure-first appearance used to produce.
    let configured = [
        state
            .neon
            .configured
            .is_present()
            .then(|| neon_section(&state.neon, &state.neon_invoice, rates, cadence_secs, now)),
        state
            .sentry
            .configured
            .is_present()
            .then(|| sentry_section(&state.sentry, quota, cadence_secs, now)),
        state
            .vercel
            .configured
            .is_present()
            .then(|| vercel_section(&state.vercel, cadence_secs, now)),
    ];
    let mut warnings: Vec<Value> = Vec::new();
    let mut clocks: Vec<Value> = Vec::new();
    let mut providers: Vec<Value> = Vec::new();
    for provider in configured.into_iter().flatten() {
        providers.push(provider.section);
        warnings.push(provider.warning);
        clocks.push(provider.freshness);
    }

    json!({
        "id": kind.id(),
        "title": kind.title(),
        // This panel has no one figure of its own to lead with.
        "trailing": "",
        // A panel with no section still says why, or its Full view is blank.
        "message": if providers.is_empty() { no_section_message(state) } else { Value::Null },
        // ONE line for the whole panel — every metered section's warning, each
        // naming itself. There is deliberately no `footer` on a section: a
        // warning under the body is what made the card grow and shove the rest
        // of the cockpit around (#351), so the payload gives the frontend
        // nowhere to put one.
        "footer": merged_footer(&warnings),
        // And ONE clock line, beside it in the same header and never folded
        // into it: `footer` says the poller is late, this says how old the
        // figures are, and between the two edges only this one speaks (#338).
        // A section carries no `freshness` key for the same reason it carries no
        // `footer` — the line appeared and disappeared with a missed poll, and
        // in the body that cost the card a line of height (#355).
        "freshness": merged_freshness(&clocks),
        "providers": providers,
        // Any provider still waiting on its first answer keeps the frontend on
        // its fast refresh, and so does one no pass has read yet. Published
        // rather than inferred from the message text, which this boundary never
        // asks the frontend to read.
        "loading": state.neon.is_loading()
            || state.sentry.is_loading()
            || state.vercel.is_loading()
            || state.neon.is_undecided()
            || state.sentry.is_undecided()
            || state.vercel.is_undecided(),
    })
}

/// What this machine's rollups contribute to the Anthropic panel.
struct Rollups {
    /// The state line when there is nothing to list, or `Null`.
    message: Value,
    heading: Value,
    windows: Value,
    projects: Value,
}

impl Rollups {
    /// Local usage is off: nothing to say, and nothing retained may show.
    fn off() -> Self {
        Rollups {
            message: Value::Null,
            heading: Value::Null,
            windows: Value::Array(vec![]),
            projects: Value::Null,
        }
    }

    /// Three states, in the original's own order: no summary at all (loading, or
    /// a log root we could not locate), a summary that measured nothing, and
    /// content. The heading is `Null` with no rows under it: the message above
    /// the body already names that state, and a heading over nothing would be a
    /// second, emptier way of saying it.
    fn of(state: &UsageState) -> Self {
        let none = |message: &str| Rollups {
            message: state_message(message),
            heading: Value::Null,
            windows: Value::Array(vec![]),
            projects: Value::Null,
        };
        match &state.claude {
            None if state.claude_loading => none(LOADING_MESSAGE),
            None => none(NO_DATA_MESSAGE),
            Some(summary) if summary.last_7d.total_tokens() == 0 => none(EMPTY_MESSAGE),
            Some(summary) => Rollups {
                message: Value::Null,
                heading: json!(MACHINE_HEADING),
                // `LAST 5H` / `LAST 7D`, never `5H` / `WEEK`: the plan meters
                // above are `5h` and `Week`, and a token count read as a share
                // of a limit is the confusion the two labels exist to prevent.
                windows: json!([
                    window_row("LAST 5H", &summary.last_5h),
                    window_row("LAST 7D", &summary.last_7d),
                ]),
                // Absent, not empty: the original renders the divider and the
                // heading only `if !summary.projectsLast7d.isEmpty`.
                projects: if summary.projects_last_7d.is_empty() {
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
                },
            },
        }
    }
}

/// One vendor's API spend for its panel, once a pass has found its Admin key.
fn api_section(
    id: &str,
    state: &ProviderState<ApiSpendSummary>,
    cadence_secs: u64,
    now: u64,
) -> Option<ProviderView> {
    if !state.configured.is_present() {
        return None;
    }
    let summary = state.summary.as_ref();
    let mut section = provider_row(API_SPEND_LABEL, usd(summary.and_then(ApiSpendSummary::usd)));
    section["id"] = json!(id);
    // How far the figure reaches, which is not when it was read: Anthropic's
    // lag by hours. The header's clock answers the other question.
    section["note"] = summary
        .and_then(ApiSpendSummary::refreshed_at)
        .map_or(Value::Null, |at| {
            let age = now.saturating_sub(u64::try_from(at.timestamp()).unwrap_or(now));
            json!(format!("counted {}", relative_age(age)))
        });
    Some(ProviderView {
        section,
        warning: attributed_status_footer(
            id,
            state.last_updated,
            state.last_error.as_deref(),
            now,
            provider_stale_after_secs(cadence_secs),
        ),
        freshness: provider_freshness(id, state, cadence_secs, now),
    })
}

/// The sentence for a source with no part to show. Only an `Absent` key may
/// paint the setup instruction: an unread one may well be stored, and an
/// unreadable one says why instead.
fn nothing_added_message(title: &str, api: &ProviderState<ApiSpendSummary>) -> Value {
    if api.is_undecided() {
        return state_message(PROVIDERS_LOADING_MESSAGE);
    }
    if let Some(reason) = api.last_error.as_deref() {
        return state_message(reason);
    }
    state_message(&crate::settings::not_set_up_message(title))
}

/// The Anthropic panel's payload: Claude's plan limits, API spend, and this
/// machine's Claude Code rollups, each only if the operator added it.
///
/// `inputs` is read at render time rather than captured by the poller: taking a
/// part off must hide it now, not after the next poll has dropped it.
///
/// Claude's plan clock is dated against `inputs.cadence_secs` and API spend's
/// against `inputs.spend_cadence_secs`. The rollups contribute no clock, only a
/// warning: they are on the much faster `PanelInterval::ClaudeRollups` cadence,
/// and the warning already measures it.
#[must_use]
pub fn anthropic_view(state: &UsageState, inputs: PlanInputs, now: u64) -> Value {
    let kind = PanelKind::Anthropic;
    let logs_on = inputs.claude_local_logs;
    let Rollups {
        message,
        heading,
        windows,
        projects,
    } = if logs_on {
        Rollups::of(state)
    } else {
        Rollups::off()
    };
    let plan = crate::plan_usage::view(Provider::Claude, state.plan(Provider::Claude), inputs, now);
    let api = api_section(
        ANTHROPIC_API_ID,
        &state.anthropic_api,
        inputs.spend_cadence_secs,
        now,
    );

    // "" rather than a fabricated 0: with no summary there is no claim to make
    // about today, and the original's `trailingLabel` returns the empty string
    // there. The tile's Claude Code line is built from this, so "" is also how
    // local usage being off removes it.
    let trailing = state
        .claude
        .as_ref()
        .filter(|_| logs_on)
        .map_or_else(String::new, |summary| {
            format!("{} today", tokens(summary.today.total_tokens()))
        });

    // The panel's one warning line starts with Claude's own, so the header reads
    // in the body's order. It is unattributed: Claude is the panel's own subject,
    // and `⚠ claude: no ~/.claude/projects` would name a section that is not one.
    let mut warnings = Vec::new();
    if logs_on {
        warnings.push(status_footer(
            state.claude_succeeded,
            state.claude_error.as_deref(),
            now,
            state.claude_stale_after_secs(),
        ));
    }
    warnings.extend(plan.warnings);
    let mut clocks = plan.clocks;
    let api = api.map(|view| {
        warnings.push(view.warning);
        clocks.push(view.freshness);
        view.section
    });
    let message = if logs_on || inputs.claude || api.is_some() {
        message
    } else {
        nothing_added_message(kind.title(), &state.anthropic_api)
    };

    json!({
        "id": kind.id(),
        "title": kind.title(),
        "trailing": trailing,
        "message": message,
        // The heading over this machine's own rollups, which sit under the
        // plan meters: the two answer different questions (what this machine
        // burned, and how much of the subscription is used), so the panel
        // names the line between them rather than leaving it to layout.
        "machineHeading": heading,
        "windows": windows,
        "projects": projects,
        "footer": merged_footer(&warnings),
        "freshness": merged_freshness(&clocks),
        "plans": plan.sections,
        "api": api,
        "loading": (logs_on && state.claude_loading)
            || plan.loading
            || state.anthropic_api.is_loading()
            || state.anthropic_api.is_undecided(),
    })
}

/// The OpenAI panel's payload: Codex's plan limits and API spend, each only if
/// the operator added it.
#[must_use]
pub fn openai_view(state: &UsageState, inputs: PlanInputs, now: u64) -> Value {
    let kind = PanelKind::OpenAi;
    let plan = crate::plan_usage::view(Provider::Codex, state.plan(Provider::Codex), inputs, now);
    let mut warnings = plan.warnings;
    let mut clocks = plan.clocks;
    let api = api_section(
        OPENAI_API_ID,
        &state.openai_api,
        inputs.spend_cadence_secs,
        now,
    )
    .map(|view| {
        warnings.push(view.warning);
        clocks.push(view.freshness);
        view.section
    });
    let message = if inputs.codex || api.is_some() {
        Value::Null
    } else {
        nothing_added_message(kind.title(), &state.openai_api)
    };
    json!({
        "id": kind.id(),
        "title": kind.title(),
        "trailing": "",
        "message": message,
        "footer": merged_footer(&warnings),
        "freshness": merged_freshness(&clocks),
        "plans": plan.sections,
        "api": api,
        "loading": plan.loading
            || state.openai_api.is_loading()
            || state.openai_api.is_undecided(),
    })
}

// MARK: - Fixtures

/// Which rendering `--dump-usage` should produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fixture {
    /// Everything measured: Claude content, both plans (Claude's an Ultra
    /// plan with no 5-hour window and a Fable week running out early, Codex's
    /// a session too young to project), both vendors' API spend, Neon figures
    /// + last invoice, Sentry count + bar.
    Measured,
    /// [`Fixture::Measured`] with every plan meter green and under pace, plus a
    /// Vercel reading, for the website's Usage card and the README's full image
    /// (#685). Kept apart from `Measured` so
    /// the e2e suite's fixture stays at two providers; the reading goes through
    /// the same `ProviderState` and section a poll fills.
    Showcase,
    /// Both providers configured and answering, neither measuring anything —
    /// the em-dash path, with the quota set and the bar therefore suppressed.
    Unmeasured,
    /// No Claude summary and no provider configured.
    Empty,
    /// The plan block's edge cases: an Enterprise seat's budget with no
    /// windows and no plan name, beside a Codex that failed before any
    /// reading. Claude content, no metered provider.
    PlanEdges,
}

/// A hand-made state for the offline fixtures.
///
/// Hand-made rather than a real poll for the same reason the Containers fixture
/// is: the states worth testing (a Neon plan without consumption history, a
/// Sentry org with no ingest, a quota bar past amber) cannot be produced on
/// demand by whichever machine runs the dump.
///
/// **Two clocks, because the panel has two.** Claude's log walk lands at
/// `claude_at`, the metered providers' last success at `providers_at`. Separate
/// arguments because they are separate cadences, and the `--stale` fixture is
/// about the slow one: aging both together would date the Claude rollups 23h
/// back as well — a second, unrelated staleness on the same card, in a fixture
/// whose whole job is to show what one hour past the *providers'* cycle looks
/// like.
#[must_use]
pub fn fixture_state(kind: Fixture, claude_at: u64, providers_at: u64) -> UsageState {
    let at = providers_at;
    let mut state = UsageState::new();
    match kind {
        Fixture::Empty => {
            state.apply_claude(None, claude_at, Some(NO_LOG_ROOT_MESSAGE.to_owned()));
        }
        Fixture::Measured | Fixture::Showcase => {
            state.apply_claude(Some(fixture_summary()), claude_at, None);
            // Read at the providers' clock, reset times from the render's: a
            // stale dump moves only when the plans were read, so its rows and
            // meters are the healthy dump's. Only the pace goes: a stale
            // reading projects nothing.
            let plans = if kind == Fixture::Showcase {
                showcase_plans(claude_at)
            } else {
                measured_plans(claude_at)
            };
            for (provider, reading) in plans {
                state.plan_mut(provider).state.succeeded(reading, at, None);
            }
            state.anthropic_api.succeeded(
                ApiSpendSummary::Measured {
                    usd: 412.80,
                    refreshed_at: chrono::DateTime::from_timestamp(
                        i64::try_from(claude_at.saturating_sub(3 * 3_600)).unwrap_or(0),
                        0,
                    ),
                },
                at,
                None,
            );
            state.openai_api.succeeded(
                ApiSpendSummary::Measured {
                    usd: 86.12,
                    refreshed_at: None,
                },
                at,
                None,
            );
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
            if kind == Fixture::Showcase {
                // The figures are the real capture's service names
                // (`crates/usage/tests/fixtures/vercel_charges.jsonl`); `Pro`
                // is the plan's ~$1.29/day over the 14 days the fixture clock
                // has reached.
                state.vercel.succeeded(
                    VercelUsageSummary::Measured {
                        effective_usd: 21.37,
                        billed_usd: 0.02,
                        top_services: vec![
                            usage::vercel::ServiceSpend {
                                name: "Pro".into(),
                                effective_usd: 18.06,
                            },
                            usage::vercel::ServiceSpend {
                                name: "Function Invocations".into(),
                                effective_usd: 1.84,
                            },
                            usage::vercel::ServiceSpend {
                                name: "Fluid Active CPU".into(),
                                effective_usd: 0.97,
                            },
                        ],
                    },
                    at,
                    None,
                );
            }
        }
        Fixture::PlanEdges => {
            state.apply_claude(Some(fixture_summary()), claude_at, None);
            state
                .plan_mut(Provider::Claude)
                .state
                .succeeded(budget_plan(), at, None);
            state.plan_mut(Provider::Codex).state.failed(
                subscription::PlanUsageError::new(
                    Provider::Codex,
                    subscription::PlanErrorKind::NotSignedIn,
                )
                .user_message(),
            );
        }
        Fixture::Unmeasured => {
            state.apply_claude(Some(fixture_summary()), claude_at, None);
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
    // A pass reads every key, so what a fixture left alone is a key it found
    // missing, not one nobody has looked for.
    if state.neon.configured.is_unknown() {
        state.neon_unconfigure();
    }
    if state.sentry.configured.is_unknown() {
        state.sentry.unconfigure();
    }
    if state.vercel.configured.is_unknown() {
        state.vercel.unconfigure();
    }
    if state.anthropic_api.configured.is_unknown() {
        state.anthropic_api.unconfigure();
    }
    if state.openai_api.configured.is_unknown() {
        state.openai_api.unconfigure();
    }
    state
}

fn plan_window(
    at: u64,
    minutes: u32,
    scope: Option<&str>,
    used: f64,
    resets_in: u64,
) -> subscription::PlanWindow {
    subscription::PlanWindow {
        span: subscription::WindowSpan::Minutes(minutes),
        scope: scope.map(str::to_owned),
        used_percent: used,
        resets_at: chrono::DateTime::from_timestamp(
            i64::try_from(at + resets_in).unwrap_or(i64::MAX),
            0,
        ),
    }
}

/// The measured fixture's plans, every reset counted from `at`. Shown only
/// when the dump turns the plans on ([`FIXTURE_PLANS`]).
///
/// Each one is an edge the panel has to get right, rather than a pretty
/// picture: Claude is an Ultra plan, which has no 5-hour window, so it has no
/// `5h` meter; its Fable week is 78% used with 91 of 168 hours gone, which
/// runs out in about a day, before the reset. Codex's session opened 20
/// minutes ago, too early to project.
fn measured_plans(at: u64) -> [(Provider, subscription::PlanUsage); 2] {
    use subscription::{PlanUsage, SESSION_MINUTES, WEEK_MINUTES};
    let week_reset = 3 * 86_400 + 5 * 3_600;
    [
        (
            Provider::Claude,
            PlanUsage {
                plan: Some("Ultra".into()),
                windows: vec![
                    plan_window(at, WEEK_MINUTES, None, 41.0, week_reset),
                    plan_window(at, WEEK_MINUTES, Some("Fable"), 78.0, week_reset),
                ],
                spend: None,
            },
        ),
        (
            Provider::Codex,
            PlanUsage {
                plan: Some("plus".into()),
                windows: vec![
                    plan_window(at, SESSION_MINUTES, None, 12.0, 4 * 3_600 + 40 * 60),
                    plan_window(at, WEEK_MINUTES, None, 22.0, 4 * 86_400 + 2 * 3_600),
                ],
                spend: None,
            },
        ),
    ]
}

/// The showcase's plans: every meter green and under pace, because the
/// showcase's one anomaly is a failing repo
/// (`the_showcase_dashboard_has_exactly_one_tile_out_of_true`).
fn showcase_plans(at: u64) -> [(Provider, subscription::PlanUsage); 2] {
    use subscription::{PlanUsage, SESSION_MINUTES, WEEK_MINUTES};
    let week_reset = 3 * 86_400 + 5 * 3_600;
    [
        (
            Provider::Claude,
            PlanUsage {
                plan: Some("Ultra".into()),
                windows: vec![
                    plan_window(at, WEEK_MINUTES, None, 41.0, week_reset),
                    plan_window(at, WEEK_MINUTES, Some("Fable"), 33.0, week_reset),
                ],
                spend: None,
            },
        ),
        (
            Provider::Codex,
            PlanUsage {
                plan: Some("plus".into()),
                windows: vec![
                    plan_window(at, SESSION_MINUTES, None, 34.0, 2 * 3_600 + 14 * 60),
                    plan_window(at, WEEK_MINUTES, None, 22.0, 4 * 86_400 + 2 * 3_600),
                ],
                spend: None,
            },
        ),
    ]
}

/// An Enterprise seat as Claude's reply shapes it: no window, a dollar budget,
/// and no plan name, as on a machine whose Claude Code profile has none.
fn budget_plan() -> subscription::PlanUsage {
    let usd = |minor| subscription::Money {
        minor,
        exponent: 2,
        currency: Some("USD".into()),
    };
    subscription::PlanUsage {
        plan: None,
        windows: vec![],
        spend: Some(subscription::PlanSpend {
            used: Some(usd(31_245)),
            limit: Some(usd(50_000)),
            stated_percent: Some(62.49),
            enabled: None,
        }),
    }
}

/// The plan opt-ins the measured dumps render with: both on, so they show the
/// plan rows. The other dumps render with both off, as a fresh store does,
/// which is what puts the setup line in front of the browser suite.
pub const FIXTURE_PLANS: PlanInputs = PlanInputs {
    claude: true,
    codex: true,
    cadence_secs: 15 * 60,
    claude_local_logs: true,
    spend_cadence_secs: 60 * 60,
};

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
                name: "solador".to_owned(),
                totals: totals(2_100_000),
            },
            usage::claude::UsageBreakdown {
                name: "gadget".to_owned(),
                totals: totals(1_050_000),
            },
            usage::claude::UsageBreakdown {
                name: "pipe-fitting".to_owned(),
                totals: totals(840_000),
            },
            usage::claude::UsageBreakdown {
                name: "flywheel".to_owned(),
                totals: totals(300_000),
            },
            usage::claude::UsageBreakdown {
                name: "acme-web".to_owned(),
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
    /// The cadence every test below classifies the metered sections' freshness
    /// against: their own default, which is what an unconfigured store polls
    /// at. Read from `PanelInterval` rather than written out, so a change to
    /// the default cannot leave these tests asserting against a cadence the app
    /// no longer uses.
    const CADENCE: u64 = store::settings::PanelInterval::UsageProviders
        .spec()
        .default_secs as u64;

    const LOGS_ONLY: PlanInputs = PlanInputs {
        claude_local_logs: true,
        ..PlanInputs::OFF
    };

    /// The operator this rework is for: the subscription and nothing else.
    const SUBSCRIPTION_ONLY: PlanInputs = PlanInputs {
        claude: true,
        ..PlanInputs::OFF
    };

    fn measured() -> UsageState {
        fixture_state(Fixture::Measured, NOW, NOW)
    }

    fn spend(usd: f64) -> ApiSpendSummary {
        ApiSpendSummary::Measured {
            usd,
            refreshed_at: None,
        }
    }

    /// A store with nothing added, after a pass has read both Admin keys.
    fn nothing_added() -> UsageState {
        let mut state = UsageState::new();
        state.anthropic_api.unconfigure();
        state.openai_api.unconfigure();
        state
    }

    /// The Anthropic panel with only local usage set up.
    fn anthropic(state: &UsageState) -> Value {
        anthropic_view(state, LOGS_ONLY, NOW)
    }

    /// The Anthropic panel with its plan on, so a test of the rollups' own
    /// empty states reads them where the panel shows them.
    fn anthropic_on(state: &UsageState) -> Value {
        anthropic_view(state, FIXTURE_PLANS, NOW)
    }

    fn section<'a>(payload: &'a Value, id: &str) -> Option<&'a Value> {
        payload["providers"]
            .as_array()?
            .iter()
            .find(|s| s["id"] == id)
    }

    /// The Neon/Sentry/Vercel footers' window follows the cadence the panel is
    /// rendered with (#635).
    fn providers_footer(providers_at: u64, cadence: u64) -> Value {
        let state = fixture_state(Fixture::Measured, NOW, providers_at);
        view(&state, QUOTA, RATES, cadence, NOW)["footer"].clone()
    }

    #[test]
    fn the_provider_window_is_one_and_a_half_cycles_of_the_cadence() {
        assert_eq!(
            provider_stale_after_secs(CADENCE),
            90 * 60,
            "no regression at 1h"
        );
        assert_eq!(provider_stale_after_secs(300), 450);
        assert_eq!(provider_stale_after_secs(2 * 3_600), 3 * 3_600);
        for cadence in [300, CADENCE, 2 * 3_600, 6 * 3_600] {
            let window = provider_stale_after_secs(cadence);
            assert!(
                providers_footer(NOW - window, cadence).is_null(),
                "{cadence}"
            );
            let text = providers_footer(NOW - window - 1, cadence)["text"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            assert!(text.contains("neon: stale"), "{cadence}: {text}");
        }
    }

    #[test]
    fn a_healthy_provider_loop_never_shows_a_stale_footer() {
        for cadence in [300, CADENCE, 2 * 3_600, 6 * 3_600] {
            assert!(
                providers_footer(NOW - cadence, cadence).is_null(),
                "{cadence}"
            );
        }
    }

    #[test]
    fn at_the_floor_a_stuck_provider_poller_is_flagged_within_two_cycles() {
        let text = providers_footer(NOW - 2 * 300, 300)["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert_eq!(
            text,
            "⚠ neon: stale · updated 10m ago ⚠ sentry: stale · updated 10m ago"
        );
    }

    // MARK: plan block

    /// Off is no plan section; on, the measured fixture's plan renders as its
    /// own section, and the token rollups are untouched either way.
    #[test]
    fn the_plan_block_is_its_own_reading_beside_the_rollups() {
        let off = anthropic(&measured());
        assert_eq!(off["plans"], json!([]));

        let on = anthropic_on(&measured());
        let ids: Vec<&str> = on["plans"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, [crate::plan_usage::CLAUDE_ID]);
        assert_eq!(on["plans"][0]["label"], "Claude · Ultra");
        assert_eq!(on["windows"], off["windows"], "the rollups do not move");
        assert_eq!(on["windows"][0]["label"], "LAST 5H");
        assert_eq!(on["machineHeading"], MACHINE_HEADING);
    }

    /// The measured fixture's plans are the edges the panels must get right: an
    /// Ultra plan has no 5-hour window and so no `5h` meter, its Fable week is
    /// burning fast enough to run out before the reset, and a Codex session
    /// opened 20 minutes ago is too young to project.
    #[test]
    fn the_measured_plans_are_an_ultra_week_running_out_and_a_young_session() {
        let anthropic = anthropic_on(&measured());
        let claude = &anthropic["plans"][0];
        let labels: Vec<&str> = claude["metrics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["label"].as_str().unwrap())
            .collect();
        assert_eq!(labels, ["Week", "Week · Fable"], "no 5h meter on Ultra");
        // 78% with 91 of 168 hours gone: the rest lasts 91 × 22/78 ≈ 25.7h.
        assert_eq!(claude["value"], "out in ~1d 1h");
        assert_eq!(claude["valueColor"], color::hex(color::AMBER));
        assert_eq!(claude["metrics"][1]["color"], color::hex(color::AMBER));

        let openai = openai_view(&measured(), FIXTURE_PLANS, NOW);
        let codex = &openai["plans"][0];
        assert_eq!(codex["label"], "Codex · Plus");
        assert_eq!(codex["metrics"][0]["label"], "5h");
        assert!(codex["metrics"][0]["pace"].is_null());
        assert_eq!(
            codex["metrics"][0]["note"],
            "resets in 4h 40m · too early to project"
        );
    }

    /// An Enterprise seat's budget with no plan name, and a Codex that failed
    /// before any reading: one budget meter, and a dash with its reason in
    /// the header.
    #[test]
    fn the_plan_edges_fixture_is_a_budget_and_a_dash() {
        let state = fixture_state(Fixture::PlanEdges, NOW, NOW);
        let v = anthropic_on(&state);
        let claude = &v["plans"][0];
        assert_eq!(claude["label"], "Claude");
        assert_eq!(claude["value"], "62% of budget");
        assert_eq!(claude["metrics"][0]["label"], "Budget");
        assert_eq!(claude["metrics"][0]["value"], "$312 / $500 · 62%");
        assert_eq!(claude["metrics"][0]["span"], "full");
        let openai = openai_view(&state, FIXTURE_PLANS, NOW);
        let codex = &openai["plans"][0];
        assert_eq!(codex["value"], viewmodel::plan_usage::NO_READING_VALUE);
        assert_eq!(codex["metrics"], json!([]));
        assert!(openai["footer"]["text"]
            .as_str()
            .unwrap()
            .contains("codex-plan: "));
    }

    /// A plan's failure reads in the header in body order: after Claude's own
    /// line, which is the panel's subject and leads.
    #[test]
    fn a_plan_warning_follows_the_rollups_warning() {
        let mut state = fixture_state(Fixture::Empty, NOW, NOW);
        state
            .plan_mut(Provider::Claude)
            .state
            .failed("Claude Code's sign-in has expired".into());
        let inputs = PlanInputs {
            claude: true,
            ..LOGS_ONLY
        };
        let payload = anthropic_view(&state, inputs, NOW);
        let text = payload["footer"]["text"].as_str().unwrap();
        assert!(
            text.starts_with(&format!("⚠ {NO_LOG_ROOT_MESSAGE}")),
            "{text}"
        );
        assert!(
            text.contains("claude-plan: Claude Code's sign-in"),
            "{text}"
        );
    }

    #[test]
    fn a_plan_that_is_on_and_unanswered_keeps_its_panel_loading() {
        let mut state = fixture_state(Fixture::Measured, NOW, NOW);
        state.plan_mut(Provider::Claude).reset();
        state.plan_mut(Provider::Codex).reset();
        let on = PlanInputs {
            claude: true,
            codex: true,
            ..PlanInputs::OFF
        };
        assert_eq!(anthropic_view(&state, on, NOW)["loading"], true);
        assert_eq!(openai_view(&state, on, NOW)["loading"], true);
        assert_eq!(anthropic(&state)["loading"], false);
        assert_eq!(openai_view(&state, PlanInputs::OFF, NOW)["loading"], false);
    }

    // MARK: each panel's own data

    /// The three views share one poller and nothing else: a failure in one must
    /// not reach another's chip or warning, and a figure must not cross over.
    #[test]
    fn each_view_warns_only_about_its_own_reading() {
        let mut state = fixture_state(Fixture::Empty, NOW, NOW);
        state.neon_mut().begin();
        state
            .neon_mut()
            .failed("Neon API request failed".to_owned());
        state
            .plan_mut(Provider::Codex)
            .state
            .failed("Codex's sign-in has expired".into());
        let on = PlanInputs {
            codex: true,
            ..LOGS_ONLY
        };

        let usage = view(&state, QUOTA, RATES, CADENCE, NOW);
        let line = usage["footer"]["text"].as_str().unwrap();
        assert!(line.contains("neon: Neon API request failed"), "{line}");
        assert!(!line.contains(NO_LOG_ROOT_MESSAGE), "{line}");
        assert!(!line.contains("codex-plan"), "{line}");

        let anthropic = anthropic_view(&state, on, NOW);
        let line = anthropic["footer"]["text"].as_str().unwrap();
        assert_eq!(line, format!("⚠ {NO_LOG_ROOT_MESSAGE}"));

        let openai = openai_view(&state, on, NOW);
        let line = openai["footer"]["text"].as_str().unwrap();
        assert!(line.contains("codex-plan: Codex's sign-in"), "{line}");
        assert!(!line.contains("neon"), "{line}");
    }

    /// A stale plan's clock stays on its own tile; Usage's clock is the metered
    /// providers' alone.
    #[test]
    fn each_view_dates_only_its_own_reading() {
        let state = fixture_state(Fixture::Measured, NOW, NOW - 7 * 3_600);
        let line = |v: Value| v["freshness"]["text"].as_str().unwrap_or("").to_owned();
        let usage = line(view(&state, QUOTA, RATES, CADENCE, NOW));
        assert!(usage.contains("neon"), "{usage}");
        assert!(!usage.contains("plan"), "{usage}");
        let anthropic = line(anthropic_on(&state));
        assert!(anthropic.contains("claude-plan"), "{anthropic}");
        assert!(!anthropic.contains("codex-plan"), "{anthropic}");
        assert!(!anthropic.contains("neon"), "{anthropic}");
        let openai = line(openai_view(&state, FIXTURE_PLANS, NOW));
        assert!(openai.contains("codex-plan"), "{openai}");
        assert!(!openai.contains("claude-plan"), "{openai}");
    }

    /// Claude's tokens are the Anthropic panel's trailing figure. On Usage they
    /// would be a figure about something else.
    #[test]
    fn only_the_anthropic_panel_says_todays_tokens() {
        let state = measured();
        assert_eq!(anthropic(&state)["trailing"], "1.2M today");
        assert_eq!(openai_view(&state, FIXTURE_PLANS, NOW)["trailing"], "");
        assert_eq!(view(&state, QUOTA, RATES, CADENCE, NOW)["trailing"], "");
    }

    /// Usage carries the metered providers and no plan or rollup key at all, so
    /// nothing downstream can paint Claude's data under the wrong name.
    #[test]
    fn usage_carries_no_plan_or_rollup_key() {
        let payload = view(&measured(), QUOTA, RATES, CADENCE, NOW);
        assert_eq!(payload["id"], "usage");
        for key in ["plans", "api", "windows", "projects", "machineHeading"] {
            assert!(payload.get(key).is_none(), "{key}");
        }
    }

    /// OpenAI's source has no provider, and Anthropic's none of Codex's.
    #[test]
    fn each_ai_view_carries_only_its_own_plan() {
        let anthropic = anthropic_on(&measured());
        assert_eq!(anthropic["id"], "anthropic");
        assert_eq!(anthropic["title"], "Anthropic");
        assert_eq!(anthropic["plans"].as_array().unwrap().len(), 1);
        assert!(anthropic.get("providers").is_none());
        let openai = openai_view(&measured(), FIXTURE_PLANS, NOW);
        assert_eq!(openai["id"], "openai");
        assert_eq!(openai["title"], "OpenAI");
        assert_eq!(openai["plans"].as_array().unwrap().len(), 1);
        assert_eq!(openai["plans"][0]["id"], crate::plan_usage::CODEX_ID);
        assert!(openai.get("providers").is_none());
        assert!(openai.get("windows").is_none());
    }

    // MARK: each panel's empty state

    /// A part that is not set up says nothing at all: the operator chose it.
    #[test]
    fn a_subscription_only_card_shows_the_plan_and_nothing_about_the_rest() {
        let mut state = measured();
        state.apply_claude(None, NOW, Some(NO_LOG_ROOT_MESSAGE.to_owned()));
        state.anthropic_api.unconfigure();
        let payload = anthropic_view(&state, SUBSCRIPTION_ONLY, NOW);
        assert_eq!(payload["plans"].as_array().unwrap().len(), 1);
        assert!(payload["message"].is_null());
        assert!(payload["footer"].is_null(), "the logs may not warn");
        assert!(payload["api"].is_null());
    }

    /// The Claude Code line is `trailing`: with local usage off there is no row
    /// for the tile to build, and nothing retained can reach the card.
    #[test]
    fn local_usage_off_shows_nothing_from_the_logs() {
        let payload = anthropic_view(&measured(), SUBSCRIPTION_ONLY, NOW);
        assert_eq!(payload["trailing"], "");
        assert!(payload["machineHeading"].is_null());
        assert_eq!(payload["windows"], json!([]));
        assert!(payload["projects"].is_null());
    }

    /// Waiting on a walk that will never run would pin the tile loading.
    #[test]
    fn local_usage_off_is_not_waiting_on_a_walk() {
        let mut state = UsageState::new();
        state.anthropic_api.unconfigure();
        let payload = anthropic_view(&state, SUBSCRIPTION_ONLY, NOW);
        assert_ne!(payload["message"]["text"], LOADING_MESSAGE);
        state.plan_mut(Provider::Claude).state.succeeded(
            subscription::PlanUsage::default(),
            NOW,
            None,
        );
        assert_eq!(
            anthropic_view(&state, SUBSCRIPTION_ONLY, NOW)["loading"],
            false
        );
    }

    /// Turning it back on must not repaint whatever the last walk left.
    #[test]
    fn clearing_the_rollups_starts_again_from_a_first_walk() {
        let mut state = measured();
        state.clear_claude();
        let payload = anthropic(&state);
        assert_eq!(payload["message"]["text"], LOADING_MESSAGE);
        assert_eq!(payload["trailing"], "");
    }

    #[test]
    fn a_source_with_nothing_added_says_where_to_add_it() {
        let state = nothing_added();
        assert_eq!(
            anthropic_view(&state, PlanInputs::OFF, NOW)["message"]["text"],
            "Anthropic isn\u{2019}t set up \u{2014} add it in Settings \u{2192} Sources \u{2192} Add source"
        );
        assert_eq!(
            openai_view(&state, PlanInputs::OFF, NOW)["message"]["text"],
            "OpenAI isn\u{2019}t set up \u{2014} add it in Settings \u{2192} Sources \u{2192} Add source"
        );
    }

    /// Only `Absent` may paint a setup instruction: before the first pass an
    /// Admin key may well be stored.
    #[test]
    fn no_setup_line_until_the_key_has_been_read() {
        let state = UsageState::new();
        for payload in [
            anthropic_view(&state, PlanInputs::OFF, NOW),
            openai_view(&state, PlanInputs::OFF, NOW),
        ] {
            assert_eq!(payload["message"]["text"], PROVIDERS_LOADING_MESSAGE);
            assert_eq!(payload["loading"], true);
        }
    }

    /// A locked keychain is not "not set up", so the card says why instead.
    #[test]
    fn an_unreadable_key_says_so_rather_than_offering_setup() {
        let mut state = UsageState::new();
        state.anthropic_api.unreadable("keychain locked".to_owned());
        let payload = anthropic_view(&state, PlanInputs::OFF, NOW);
        assert_eq!(payload["message"]["text"], "keychain locked");
        assert_eq!(payload["loading"], false);
    }

    /// A walk still running is not a verdict: the rollups may yet have rows.
    #[test]
    fn anthropic_keeps_saying_it_is_reading_until_the_walk_lands() {
        let payload = anthropic(&UsageState::new());
        assert_eq!(payload["message"]["text"], LOADING_MESSAGE);
    }

    // MARK: API spend

    #[test]
    fn api_spend_is_its_own_block_with_its_own_id_and_figure() {
        let mut state = nothing_added();
        state.anthropic_api.succeeded(spend(412.8), NOW, None);
        state.openai_api.succeeded(spend(1.5), NOW, None);
        let anthropic = anthropic_view(&state, PlanInputs::OFF, NOW);
        assert_eq!(anthropic["api"]["id"], ANTHROPIC_API_ID);
        assert_eq!(anthropic["api"]["label"], API_SPEND_LABEL);
        assert_eq!(anthropic["api"]["value"], "$412.80");
        assert!(anthropic["message"].is_null());
        let openai = openai_view(&state, PlanInputs::OFF, NOW);
        assert_eq!(openai["api"]["id"], OPENAI_API_ID);
        assert_eq!(openai["api"]["value"], "$1.50");
        assert!(openai["message"].is_null());
    }

    /// Unknown is not zero, and a real zero is not unknown.
    #[test]
    fn unmeasured_spend_is_a_dash_and_measured_zero_is_a_zero() {
        let mut state = nothing_added();
        state
            .anthropic_api
            .succeeded(ApiSpendSummary::Unmeasured, NOW, None);
        let unknown = anthropic_view(&state, PlanInputs::OFF, NOW);
        assert_eq!(unknown["api"]["value"], "\u{2014}");
        assert_eq!(unknown["api"]["valueColor"], color::hex(color::MUTED));
        state.anthropic_api.succeeded(spend(0.0), NOW, None);
        assert_eq!(
            anthropic_view(&state, PlanInputs::OFF, NOW)["api"]["value"],
            "$0.00"
        );
    }

    /// Anthropic's figures lag by hours, so the block says how far they reach.
    #[test]
    fn anthropic_spend_says_how_far_its_figures_reach() {
        let mut state = nothing_added();
        let reach = chrono::DateTime::from_timestamp(NOW as i64 - 3 * 3600, 0).unwrap();
        state.anthropic_api.succeeded(
            ApiSpendSummary::Measured {
                usd: 1.0,
                refreshed_at: Some(reach),
            },
            NOW,
            None,
        );
        assert_eq!(
            anthropic_view(&state, PlanInputs::OFF, NOW)["api"]["note"],
            "counted 3h ago"
        );
    }

    #[test]
    fn a_failed_spend_read_warns_under_its_own_name_and_keeps_its_figure() {
        let mut state = nothing_added();
        state.anthropic_api.succeeded(spend(9.0), NOW, None);
        state
            .anthropic_api
            .failed("Anthropic rejected it".to_owned());
        let payload = anthropic_view(&state, PlanInputs::OFF, NOW);
        assert_eq!(payload["api"]["value"], "$9.00");
        assert!(payload["footer"]["text"]
            .as_str()
            .unwrap()
            .contains("anthropic-api"));
    }

    #[test]
    fn a_spend_clock_is_dated_against_the_providers_cadence() {
        let mut state = nothing_added();
        state.openai_api.succeeded(spend(1.0), NOW - 7200, None);
        let inputs = PlanInputs::OFF;
        let payload = openai_view(&state, inputs, NOW);
        assert!(payload["freshness"]["text"]
            .as_str()
            .unwrap()
            .contains("openai-api"));
    }

    #[test]
    fn openai_with_only_spend_added_has_no_setup_line() {
        let payload = openai_view(&measured(), PlanInputs::OFF, NOW);
        assert!(payload["message"].is_null());
        assert_eq!(payload["plans"], json!([]));
        assert_eq!(payload["loading"], false);
    }

    /// Unknown stays unknown. Before a pass has read the keychain, the panel
    /// has nothing to say about how it is configured, so it must not tell a
    /// configured operator to set up.
    #[test]
    fn usage_paints_no_setup_sentence_before_a_pass_has_read_the_keys() {
        let payload = view(&UsageState::new(), QUOTA, RATES, CADENCE, NOW);
        assert_eq!(payload["loading"], true);
        assert_eq!(payload["message"]["text"], PROVIDERS_LOADING_MESSAGE);

        let mut state = UsageState::new();
        state.neon_unconfigure();
        state.sentry_mut().unconfigure();
        let payload = view(&state, QUOTA, RATES, CADENCE, NOW);
        assert_eq!(payload["loading"], true, "Vercel is still unknown");
        assert_eq!(payload["message"]["text"], PROVIDERS_LOADING_MESSAGE);
    }

    /// With every key read and none found, the Full view says what to do rather
    /// than rendering blank.
    #[test]
    fn usage_with_no_provider_says_what_to_add() {
        let payload = view(
            &fixture_state(Fixture::Empty, NOW, NOW),
            QUOTA,
            RATES,
            CADENCE,
            NOW,
        );
        assert_eq!(payload["loading"], false);
        assert_eq!(
            payload["message"]["text"],
            "No cloud usage yet — add Neon, Sentry or Vercel in Settings → Sources → Add source"
        );
        assert!(payload["providers"].as_array().unwrap().is_empty());
    }

    #[test]
    fn usage_with_a_provider_has_no_message() {
        let payload = view(&measured(), QUOTA, RATES, CADENCE, NOW);
        assert!(payload["message"].is_null());
    }

    /// A locked keychain never reaches `begin` or `unconfigure`, so a provider
    /// nobody has seen stays `Unknown`. It must settle, or Usage waits for a
    /// first reading forever and a healthy dashboard never leaves its fast
    /// refresh. It says what happened, and never that nothing is set up.
    #[test]
    fn a_locked_keychain_settles_usage_with_its_reason_and_no_setup_sentence() {
        let reason = "couldn't read the credential store";
        let mut state = UsageState::new();
        state.neon_mut().unreadable(reason.to_owned());
        state.neon_invoice_mut().unreadable(reason.to_owned());
        state.sentry_mut().unreadable(reason.to_owned());
        state.vercel_mut().unreadable(reason.to_owned());
        let payload = view(&state, QUOTA, RATES, CADENCE, NOW);
        assert_eq!(payload["loading"], false);
        assert_eq!(payload["message"]["text"], reason);
        assert!(payload["providers"].as_array().unwrap().is_empty());
    }

    // MARK: formatting

    #[test]
    fn token_counts_abbreviate_the_way_the_original_panel_does() {
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

    /// The Claude footer's staleness window follows the rollup cadence the loop
    /// last told the state (#615): 150s with none set and at 60s, 750s at 300s.
    #[test]
    fn the_claude_stale_window_follows_the_rollup_cadence() {
        let footer = |cadence: Option<u64>, age: u64| {
            let mut state = fixture_state(Fixture::Measured, NOW - age, NOW);
            if let Some(secs) = cadence {
                state.set_claude_cadence_secs(secs);
            }
            anthropic(&state)["footer"].clone()
        };
        for cadence in [None, Some(60)] {
            assert!(footer(cadence, 150).is_null(), "{cadence:?} at the edge");
            assert!(!footer(cadence, 151).is_null(), "{cadence:?} past it");
        }
        assert!(footer(Some(300), 151).is_null(), "a healthy 5-minute gap");
        assert!(footer(Some(300), 750).is_null(), "2.5 cycles is the edge");
        assert!(
            !footer(Some(300), 751).is_null(),
            "past it the walk looks stuck"
        );
    }

    #[test]
    fn before_the_first_walk_the_panel_says_it_is_reading() {
        let payload = anthropic(&UsageState::new());
        assert_eq!(payload["message"]["text"], LOADING_MESSAGE);
        assert_eq!(payload["loading"], true);
        assert_eq!(payload["trailing"], "");
        assert!(payload["windows"].as_array().unwrap().is_empty());
        assert!(payload["projects"].is_null());
        assert!(
            payload["machineHeading"].is_null(),
            "no heading over no rows"
        );
        assert!(payload["footer"].is_null(), "nothing to be stale yet");
    }

    /// A provider whose key nobody has read yet contributes no section — the
    /// same silence as one that is genuinely unconfigured, and deliberately so:
    /// materialising a section here would put a row of em dashes on screen for
    /// a provider we cannot yet say exists.
    #[test]
    fn a_provider_contributes_nothing_until_a_pass_has_read_its_key() {
        let payload = view(&UsageState::new(), 0, NeonRates::default(), CADENCE, NOW);
        assert!(payload["providers"]
            .as_array()
            .expect("providers")
            .is_empty());
    }

    /// …and once a pass holds the key the section appears *loading*, not as the
    /// row of em dashes under an error it used to be. `failed()` flipping the
    /// provider configured is what made a first failure its debut.
    #[test]
    fn a_provider_section_appears_as_loading_before_its_first_read_settles() {
        let mut state = UsageState::new();
        state.neon_mut().begin();
        let payload = view(&state, 0, NeonRates::default(), CADENCE, NOW);
        assert!(
            section(&payload, "neon").is_some(),
            "the section is present"
        );
        assert_eq!(payload["loading"], true);
        assert!(state.neon.is_loading());
        // Before the first read there is no age to show: no warning, and no
        // `ago` anywhere in the section's freshness.
        assert!(payload["footer"].is_null());
        let text = payload["freshness"]["text"].as_str().unwrap_or_default();
        assert!(!text.contains("ago"), "no fabricated age: {text}");
    }

    /// A read that begins drops the credential store's refusal, so a recovered
    /// keychain is not shown under its old error for one more read.
    #[test]
    fn beginning_a_read_clears_a_credential_store_reason() {
        let mut state = UsageState::new();
        state.neon_mut().begin();
        state
            .neon_mut()
            .unreadable("the keychain is locked".to_owned());
        assert!(!state.neon.is_loading());
        state.neon_mut().begin();
        assert!(state.neon.last_error.is_none());
        assert!(state.neon.is_loading());
    }

    /// A vendor failure that persists keeps its own reason while the next read
    /// is in flight, rather than flipping to loading for each round trip.
    #[test]
    fn beginning_a_read_keeps_a_vendor_failures_reason() {
        let mut state = UsageState::new();
        state.neon_mut().begin();
        state
            .neon_mut()
            .failed("Neon API request failed".to_owned());
        state.neon_mut().begin();
        assert_eq!(
            state.neon.last_error.as_deref(),
            Some("Neon API request failed")
        );
        assert!(!state.neon.is_loading());
    }

    /// The log root could not even be located, so nothing was read. Distinct
    /// from an empty week, which *is* a measurement.
    #[test]
    fn a_missing_log_root_says_no_usage_data_and_names_itself_in_the_footer() {
        let payload = anthropic_on(&fixture_state(Fixture::Empty, NOW, NOW));
        assert_eq!(payload["message"]["text"], NO_DATA_MESSAGE);
        // No `· last ok 0s ago`. There is no log root, so no walk has ever
        // succeeded, and the suffix used to be a reassurance about a reading
        // that never happened — restated on every pass, forever.
        assert_eq!(
            payload["footer"]["text"],
            format!("⚠ {NO_LOG_ROOT_MESSAGE}")
        );
    }

    #[test]
    fn a_measured_but_empty_week_says_so_rather_than_rendering_zero_rows() {
        let mut state = UsageState::new();
        state.apply_claude(Some(UsageSummary::default()), NOW, None);
        let payload = anthropic_on(&state);
        assert_eq!(payload["message"]["text"], EMPTY_MESSAGE);
        assert!(payload["windows"].as_array().unwrap().is_empty());
        // The trailing label still reports today's measured zero: the walk
        // succeeded, so "0 today" is a fact, not a fabrication.
        assert_eq!(payload["trailing"], "0 today");
    }

    #[test]
    fn content_renders_both_windows_and_at_most_four_projects() {
        let payload = anthropic(&measured());
        assert!(payload["message"].is_null());

        let windows = payload["windows"].as_array().unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0]["label"], "LAST 5H");
        assert_eq!(windows[0]["value"], "820k");
        assert_eq!(windows[1]["label"], "LAST 7D");
        assert_eq!(windows[1]["value"], "4.3M");
        assert_eq!(windows[0]["valueColor"], color::hex(color::GREEN));

        assert_eq!(payload["projects"]["label"], "TOP PROJECTS (7D)");
        let rows = payload["projects"]["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 4, "the fifth project is cut");
        assert_eq!(rows[0]["name"], "solador");
        assert_eq!(rows[0]["value"], "2.1M");
        assert_eq!(rows[0]["dotColor"], color::hex(color::GREEN_DIM));

        assert_eq!(payload["trailing"], "1.2M today");
    }

    /// The subscription publishes no ceiling, so the window rows carry no bar
    /// at all — not a null one, which would read as a feature half-wired.
    #[test]
    fn window_rows_carry_no_progress_bar() {
        let payload = anthropic(&measured());
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
        assert!(anthropic(&state)["projects"].is_null());
    }

    // MARK: provider sections

    /// An unconfigured provider contributes nothing — no heading, no em dash,
    /// no divider.
    #[test]
    fn an_unconfigured_provider_renders_no_section() {
        let payload = view(
            &fixture_state(Fixture::Empty, NOW, NOW),
            QUOTA,
            NeonRates::default(),
            CADENCE,
            NOW,
        );
        assert!(payload["providers"].as_array().unwrap().is_empty());
    }

    #[test]
    fn a_measured_provider_renders_its_figures_in_ink() {
        let payload = view(&measured(), QUOTA, RATES, CADENCE, NOW);
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
            &fixture_state(Fixture::Unmeasured, NOW, NOW),
            QUOTA,
            NeonRates::default(),
            CADENCE,
            NOW,
        );

        let neon = section(&payload, "neon").expect("neon section");
        for row in neon["rows"].as_array().unwrap() {
            assert_eq!(row["value"], "—", "got {row}");
            assert_eq!(row["valueColor"], color::hex(color::MUTED));
        }
        // …in the header, naming the section it belongs to. Not under the rows,
        // where it would make the card taller than the panel beside it.
        assert!(neon["footer"].is_null(), "no section carries its own line");
        assert!(payload["footer"]["text"]
            .as_str()
            .expect("a header line")
            .contains(&format!(
                "⚠ neon: {NEON_NO_CONSUMPTION_MESSAGE} · last ok 0s ago"
            )));

        let sentry = section(&payload, "sentry").expect("sentry section");
        assert_eq!(sentry["rows"][0]["value"], "—");
    }

    // MARK: Neon estimate + invoice rows

    /// The estimate row appears only when rates are set AND usage is measured.
    #[test]
    fn the_estimate_row_needs_rates_and_a_measurement() {
        let payload = view(&measured(), QUOTA, RATES, CADENCE, NOW);
        let neon = section(&payload, "neon").expect("neon section");
        let rows = neon["rows"].as_array().unwrap();
        let est = rows
            .iter()
            .find(|r| r["label"] == "NEON EST. CHARGES (MTD)")
            .expect("estimate row");
        // Measured fixture: 12.4 CU-h × 0.106 + 3.25 GiB × 0.35 = 2.4519
        assert_eq!(est["value"], "≈ $2.45");

        let unpriced = view(&measured(), QUOTA, NeonRates::default(), CADENCE, NOW);
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
        let payload = view(&state, QUOTA, NeonRates::default(), CADENCE, NOW);
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
        let payload = view(&fresh, QUOTA, NeonRates::default(), CADENCE, NOW);
        let neon = section(&payload, "neon").expect("neon section");
        let row = neon["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["label"] == "NEON LAST INVOICE")
            .expect("invoice row");
        assert_eq!(row["value"], "—");
    }

    /// Consumption's error owns the warning; the invoice's shows only when
    /// consumption is healthy. The rule is unchanged by the hoist — only where
    /// the line it produces ends up (`payload["footer"]`, not the section's).
    #[test]
    fn the_consumption_error_outranks_the_invoice_error_in_the_footer() {
        let mut state = measured();
        state
            .neon_invoice_mut()
            .failed("invoices: Neon API request failed (HTTP 404)".to_owned());
        state
            .neon_mut()
            .failed("Neon API request failed (HTTP 500)".to_owned());
        let payload = view(&state, QUOTA, NeonRates::default(), CADENCE, NOW);
        let text = payload["footer"]["text"].as_str().unwrap();
        assert!(text.contains("HTTP 500"));
        assert!(
            !text.contains("404"),
            "the invoice's error text must be absent when consumption's error wins: {text}"
        );
        assert!(text.contains("neon:"), "and it says whose failure it is");
    }

    /// When consumption is healthy but the invoice read failed, the invoice's
    /// error is the only one available and must still reach the header line —
    /// the `.or(invoice.last_error.as_deref())` fallback in `neon_section`.
    #[test]
    fn the_invoice_error_reaches_the_footer_when_consumption_is_healthy() {
        let mut state = measured();
        state
            .neon_invoice_mut()
            .failed("invoices: Neon API request failed (HTTP 404)".to_owned());
        let payload = view(&state, QUOTA, NeonRates::default(), CADENCE, NOW);
        assert!(payload["footer"]["text"]
            .as_str()
            .unwrap()
            .contains("invoices:"));
    }

    // MARK: the quota bar

    #[test]
    fn the_quota_bar_needs_both_a_quota_and_a_known_count() {
        // Quota set, count known -> a bar.
        let with_bar = view(&measured(), QUOTA, RATES, CADENCE, NOW);
        let bar = &section(&with_bar, "sentry").expect("sentry")["bar"];
        assert_eq!(bar["fraction"], 0.94);
        assert_eq!(bar["color"], color::hex(color::AMBER), "9400/10000 is 94%");

        // No quota -> no bar, however well measured the count is.
        let no_quota = view(&measured(), 0, RATES, CADENCE, NOW);
        assert!(section(&no_quota, "sentry").expect("sentry")["bar"].is_null());

        // Quota set, count unknown -> no bar. A bar at a defaulted 0 would
        // read "comfortably under quota" when the truth is "we don't know".
        let unknown = view(
            &fixture_state(Fixture::Unmeasured, NOW, NOW),
            QUOTA,
            NeonRates::default(),
            CADENCE,
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
        let payload = view(&state, QUOTA, RATES, CADENCE, NOW);
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
        let payload = view(&state, QUOTA, RATES, CADENCE, NOW);
        let sentry = section(&payload, "sentry").expect("sentry");
        assert_eq!(sentry["rows"][0]["value"], "0");
        assert_eq!(sentry["bar"]["fraction"], 0.0);
    }

    // MARK: freshness

    /// The two clocks, side by side. Ten minutes into an hourly cycle the
    /// figures are still current and nothing is added; one second past the
    /// cadence they are dated, in amber, while the footer stays silent —
    /// the window neither field can express on its own.
    #[test]
    fn each_metered_section_dates_its_figures_once_a_whole_cycle_has_passed() {
        let live = view(&measured(), QUOTA, RATES, CADENCE, NOW + 600);
        assert!(
            live["freshness"].is_null(),
            "current figures paint as they always did"
        );

        let at = NOW + CADENCE + 1;
        let stale = view(&measured(), QUOTA, RATES, CADENCE, at);
        assert_eq!(
            stale["freshness"]["text"],
            "neon: as of 1h ago · sentry: as of 1h ago"
        );
        assert_eq!(stale["freshness"]["color"], color::hex(color::AMBER));
        // …and the *warning* has not fired: the window is 90m, the cadence an
        // hour. Folding the two into one field would have to pick which of
        // those two edges to keep.
        assert!(
            stale["footer"].is_null(),
            "the panel's warning stays clean: dated is not late"
        );
    }

    /// Neither line may live under the body. Both used to — the warning until
    /// #351, the clock until #355 — and both appear only when something is
    /// wrong, so each cost the card a line of height the moment it fired, which
    /// `.panel-row` then spent on every other card in the row.
    #[test]
    fn a_section_carries_neither_line_of_its_own() {
        // Far enough past the 90m window that both lines are firing, so the
        // assertions below are about where they went rather than about a
        // fixture with nothing to say.
        let at = NOW + 23 * 3_600;
        let payload = view(&measured(), QUOTA, RATES, CADENCE, at);
        assert_eq!(
            payload["footer"]["text"],
            "⚠ neon: stale · updated 23h ago ⚠ sentry: stale · updated 23h ago"
        );
        assert_eq!(
            payload["freshness"]["text"],
            "neon: as of 23h ago · sentry: as of 23h ago"
        );
        for id in [NEON_ID, SENTRY_ID] {
            let provider = section(&payload, id).expect(id);
            assert!(provider["footer"].is_null(), "{id} warning");
            assert!(provider["freshness"].is_null(), "{id} clock");
        }
    }

    /// The failure this field exists to prevent: a section that has never once
    /// answered publishing an age of zero, which would paint it as the freshest
    /// thing on the card — and, in the header, a named blank reserving a line
    /// for a measurement nobody took.
    #[test]
    fn a_section_that_has_never_read_publishes_no_age_rather_than_a_zero() {
        let mut state = UsageState::new();
        state.neon_mut().begin();
        let payload = view(&state, QUOTA, RATES, CADENCE, NOW);
        assert!(
            section(&payload, NEON_ID).is_some(),
            "the section itself is rendered"
        );
        assert!(
            payload["freshness"].is_null(),
            "there is no figure to qualify"
        );
    }

    /// `freshness` is additive: a failed read still dates the figure it is
    /// carrying forward, and the footer still names the failure. Two fields,
    /// two strings, both in the header, and never joined.
    #[test]
    fn a_dated_figure_and_a_failure_are_reported_side_by_side() {
        let at = NOW + CADENCE + 1;
        let mut state = measured();
        state
            .neon
            .failed("Neon API request failed (HTTP 500)".to_owned());
        let payload = view(&state, QUOTA, RATES, CADENCE, at);
        let neon = section(&payload, "neon").expect("neon");
        assert_eq!(neon["rows"][0]["value"], "12.4 CU-h", "last good is kept");
        assert_eq!(
            payload["freshness"]["text"], "neon: as of 1h ago · sentry: as of 1h ago",
            "both sections are dated; only one of them failed"
        );
        assert_eq!(
            payload["footer"]["text"],
            "⚠ neon: Neon API request failed (HTTP 500) · last ok 1h ago"
        );
        assert_ne!(payload["freshness"]["text"], payload["footer"]["text"]);
    }

    /// The age is classified against the operator's cadence, not a constant
    /// here: an operator who polls every four hours must not be told a
    /// two-hour-old reading is stale.
    #[test]
    fn a_longer_configured_cadence_keeps_a_reading_live_for_longer() {
        let two_hours = 2 * 3_600;
        let payload = view(&measured(), QUOTA, RATES, 4 * 3_600, NOW + two_hours);
        assert!(payload["freshness"].is_null(), "still live at four hours");
        let payload = view(&measured(), QUOTA, RATES, CADENCE, NOW + two_hours);
        assert_eq!(
            payload["freshness"]["text"],
            "neon: as of 2h ago · sentry: as of 2h ago"
        );
    }

    /// Two sections, two clocks, one line — and each names the block it dates.
    /// Unattributed they are the byte-identical `as of 23h ago` twice over: a
    /// header saying the same thing twice and identifying neither, which is the
    /// failure that made the warnings' attribution load-bearing in #351.
    #[test]
    fn two_dated_sections_are_distinguishable_on_the_one_clock_line() {
        let at = NOW + 23 * 3_600;
        let payload = view(&measured(), QUOTA, RATES, CADENCE, at);
        let line = payload["freshness"]["text"].as_str().expect("a clock line");
        for id in [NEON_ID, SENTRY_ID] {
            assert!(line.contains(&format!("{id}: ")), "{id} is named");
            assert!(
                section(&payload, id).is_some(),
                "{id} is named after a section that exists"
            );
        }
        // Two questions, two strings — same header, never the same line.
        assert_ne!(payload["freshness"]["text"], payload["footer"]["text"]);
    }

    // MARK: footers

    /// Each reading is still measured against **its own window**, the whole
    /// reason the window is an argument: 10 minutes is stale for Claude (150s)
    /// and perfectly fine for the hourly providers, so only Anthropic warns.
    #[test]
    fn each_panel_measures_its_reading_on_its_own_window() {
        let later = NOW + 600;
        let payload = view(&measured(), QUOTA, RATES, CADENCE, later);
        assert!(payload["footer"].is_null(), "{}", payload["footer"]);
        for id in [NEON_ID, SENTRY_ID] {
            assert!(section(&payload, id).expect(id)["footer"].is_null(), "{id}");
        }
        assert_eq!(
            anthropic_view(&measured(), LOGS_ONLY, later)["footer"]["text"],
            "⚠ stale · updated 10m ago"
        );
    }

    /// Two sections degrading at once. Hoisted unattributed these were the
    /// byte-identical `⚠ stale · updated 23h ago` twice over — a header line
    /// that said the same thing twice and identified neither. Each names itself
    /// with the very id its section is keyed by, so the line points at a block a
    /// reader can find.
    #[test]
    fn two_degraded_sections_are_distinguishable_on_the_one_line() {
        let at = NOW + 23 * 3_600;
        let payload = view(&measured(), QUOTA, RATES, CADENCE, at);
        assert_eq!(
            payload["footer"]["text"],
            "⚠ neon: stale · updated 23h ago ⚠ sentry: stale · updated 23h ago"
        );
        for id in [NEON_ID, SENTRY_ID] {
            assert!(
                section(&payload, id).is_some(),
                "{id} is named after a section that exists"
            );
        }
    }

    #[test]
    fn a_provider_failure_keeps_the_last_good_figure_and_dates_it() {
        let mut state = measured();
        state
            .neon
            .failed("Neon API request failed (HTTP 500)".to_owned());
        let payload = view(&state, QUOTA, RATES, CADENCE, NOW + 300);
        let neon = section(&payload, "neon").expect("neon");
        assert_eq!(neon["rows"][0]["value"], "12.4 CU-h", "last good is kept");
        assert_eq!(
            payload["footer"]["text"],
            "⚠ neon: Neon API request failed (HTTP 500) · last ok 5m ago"
        );
    }

    /// Clearing a credential must take the figures with it, or the next time
    /// the key is saved the section reappears showing an hour-old number as if
    /// it had just been read. That includes the invoice: a stale dollar figure
    /// must not outlive the credential that read it, so the drop goes through
    /// `neon_unconfigure()`, not a bare `state.neon.unconfigure()`.
    #[test]
    fn unconfiguring_a_provider_drops_its_retained_figure() {
        let mut state = measured();
        state.neon_unconfigure();
        let payload = view(&state, QUOTA, RATES, CADENCE, NOW);
        assert!(section(&payload, "neon").is_none());

        state.neon.begin();
        let payload = view(&state, QUOTA, RATES, CADENCE, NOW);
        let neon = section(&payload, "neon").expect("neon");
        assert_eq!(neon["rows"][0]["value"], "—");
        assert_eq!(
            neon["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["label"] == "NEON LAST INVOICE")
                .expect("invoice row")["value"],
            "—",
            "the invoice figure must not outlive the credential that read it"
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

        let payload = view(&state, QUOTA, RATES, CADENCE, NOW + 120);
        let neon = section(&payload, "neon").expect("the section stays");
        assert_eq!(neon["rows"][0]["value"], "12.4 CU-h", "last good is kept");
        assert_eq!(
            payload["footer"]["text"],
            "⚠ neon: couldn't read the credential store · last ok 2m ago"
        );
    }

    /// …and must not conjure a section for a provider nobody configured.
    #[test]
    fn an_unreadable_credential_store_stays_silent_when_nothing_was_configured() {
        let mut state = fixture_state(Fixture::Empty, NOW, NOW);
        state
            .sentry_mut()
            .unreadable("couldn't read the credential store".to_owned());
        assert!(
            view(&state, QUOTA, NeonRates::default(), CADENCE, NOW)["providers"]
                .as_array()
                .expect("providers")
                .is_empty()
        );
    }

    // MARK: the fixtures

    /// The showcase is `Measured` plus a Vercel reading (#685): the website's
    /// Usage card is framed on the three providers.
    #[test]
    fn the_showcase_fixture_is_measured_plus_a_vercel_reading() {
        let measured = view(
            &fixture_state(Fixture::Measured, NOW, NOW),
            QUOTA,
            RATES,
            CADENCE,
            NOW,
        );
        let showcase = view(
            &fixture_state(Fixture::Showcase, NOW, NOW),
            QUOTA,
            RATES,
            CADENCE,
            NOW,
        );
        let ids: Vec<&str> = showcase["providers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["neon", "sentry", "vercel"]);
        for id in ["neon", "sentry"] {
            assert_eq!(section(&showcase, id), section(&measured, id), "{id}");
        }
        let rows = section(&showcase, "vercel").expect("vercel")["rows"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(rows[0]["label"], "VERCEL SPEND (MTD)");
        assert_eq!(rows[1]["label"], "VERCEL BEYOND PLAN");
        assert_eq!(rows.len(), 5, "two totals then three service rows");
        let labels: Vec<&str> = rows[2..]
            .iter()
            .map(|r| r["label"].as_str().unwrap())
            .collect();
        assert_eq!(labels, ["PRO", "FUNCTION INVOCATIONS", "FLUID ACTIVE CPU"]);
        for row in &rows {
            let value = row["value"].as_str().expect("a string value");
            assert!(value.starts_with('$'), "{row}");
        }
    }

    /// The Playwright suite renders these payloads, so a fixture that quietly
    /// lost the case it claims to exercise would leave that suite green while
    /// covering nothing. Asserted here, where a Rust test can see it.
    #[test]
    fn the_fixtures_cover_every_rendering_the_panel_has() {
        let state = fixture_state(Fixture::Measured, NOW, NOW);
        let rollups = anthropic(&state);
        assert!(rollups["message"].is_null(), "content, not a state line");
        assert_eq!(rollups["windows"].as_array().unwrap().len(), 2);
        assert!(!rollups["projects"]["rows"].as_array().unwrap().is_empty());
        let measured = view(&state, QUOTA, RATES, CADENCE, NOW);
        assert!(measured["message"].is_null(), "content, not a state line");
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
            &fixture_state(Fixture::Unmeasured, NOW, NOW),
            QUOTA,
            NeonRates::default(),
            CADENCE,
            NOW,
        );
        assert_eq!(unmeasured["providers"].as_array().unwrap().len(), 2);
        assert!(
            section(&unmeasured, "sentry").expect("sentry")["bar"].is_null(),
            "a quota with no count must suppress the bar"
        );
        let unmeasured_line = unmeasured["footer"]["text"]
            .as_str()
            .expect("both blank sections explain themselves");
        for id in [NEON_ID, SENTRY_ID] {
            let s = section(&unmeasured, id).expect(id);
            assert!(
                s["footer"].is_null(),
                "{id} must not carry a line of its own"
            );
            assert!(
                unmeasured_line.contains(&format!("⚠ {id}: ")),
                "{id} explains why it is blank, and says it is {id}: {unmeasured_line}"
            );
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

        let state = fixture_state(Fixture::Empty, NOW, NOW);
        let empty = anthropic_on(&state);
        assert_eq!(empty["message"]["text"], NO_DATA_MESSAGE);
        assert_eq!(empty["trailing"], "");
        let empty = view(&state, QUOTA, NeonRates::default(), CADENCE, NOW);
        assert!(empty["providers"].as_array().unwrap().is_empty());
        assert_eq!(empty["loading"], false, "a pass has read every key");
    }
}
