//! The **Usage** panel: Claude Code token rollups, the Claude and Codex
//! **plan** windows when the operator has turned those on, and a Neon, Sentry
//! and Vercel section when those providers are configured.
//!
//! The plan block is a different reading from the rollups and is never merged
//! with them: the rollups are this machine's consumption from local logs, the
//! plan rows a subscription's utilisation of its own limits
//! ([`crate::plan_usage`]). It renders between the two, and its sections and
//! setup line are their own payload keys (`plans`, `plansMessage`).
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
//! **An unconfigured provider has no section at all.** No Neon key means the
//! panel is pixel-identical to its Claude-only self: no heading, no em dash, no
//! layout shift. The em dash is for "configured, and we could not find out".
//!
//! **Three clocks, one warning line.** Claude reads local files on its own
//! rollup cadence (`staleAfter` 2.5 cycles, 150s at the default 60s); Neon and Sentry are hourly API reads
//! whose staleness window is 1.5 cycles of their own cadence (90m at the default), so a warning means
//! a stuck poller rather than the normal gap between polls. Each section decides
//! its own warning because each can fail on its own — and every one of them is
//! then folded into the panel's **single header line** by
//! [`merged_footer`](crate::panel::merged_footer), naming the section it came
//! from.
//!
//! **Nothing a section can say may live under the section** (#351). A warning in
//! the body makes the card a line taller the moment it fires, `.panel-row`
//! stretches every other card in the row to match, and the rows below are pushed
//! down: one Neon read going stale rearranged the cockpit. The header is always
//! rendered, so a warning there costs no height at all. That is also why the
//! attribution is load-bearing rather than decorative — the hoisted lines are
//! byte-identical without it, and `⚠ stale · updated 23h ago` twice over says
//! the same thing twice and identifies neither section.
//!
//! **And each metered section carries a second clock** (#338, the Azure Cost
//! panel's arrangement one panel over). `freshness` is [`Freshness`] over the
//! age of the last **success** and answers *"how old is the figure on screen"*;
//! the warning answers *"did the last attempt fail, or is the poller late"*. On
//! an hourly read the first speaks a full 30 minutes before the second does, and
//! a nearly-hour-old dollar figure rendering identically to one measured a
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
    NeonInvoiceSummary, NeonUsageSummary, SentryUsageSummary, UsageTotals, VercelUsageSummary,
};
use viewmodel::cockpit::PanelKind;
use viewmodel::color;
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
pub use usage::{summarize_logs, NeonClient, SentryClient, UsageSummary, VercelClient};

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

/// How often the Neon and Sentry reads run by default — hourly, on a cadence of their own, not the
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

/// A summary that walked the logs successfully and found nothing in the window.
/// Distinct from [`NO_DATA_MESSAGE`]: this one is a measurement.
pub const EMPTY_MESSAGE: &str = "no Claude usage in the last 7 days";

/// What the footer says when `~/.claude/projects` is not there at all. the original
/// reports this from its own existence check, which is why it is the shell's
/// string and not the crate's.
pub const NO_LOG_ROOT_MESSAGE: &str = "no ~/.claude/projects";

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
}

impl<S> Default for ProviderState<S> {
    fn default() -> Self {
        ProviderState {
            configured: Configured::Unknown,
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
        self.configured = Configured::Absent;
        self.summary = None;
        self.last_updated = None;
        self.last_error = None;
    }

    /// A pass read this provider's key. Called before the request goes out, so
    /// the section can say it is loading rather than waiting on the round trip
    /// to learn it exists at all.
    pub fn begin(&mut self) {
        self.configured = Configured::Present;
    }

    /// A successful read. `error` carries the "answered, but measured nothing"
    /// explanation, which is a successful read with an empty result — not a
    /// failure.
    pub fn succeeded(&mut self, summary: S, at: u64, error: Option<String>) {
        self.configured = Configured::Present;
        self.summary = Some(summary);
        self.last_updated = Some(at);
        self.last_error = error;
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
    }

    /// Whether this provider is still waiting on the answer to its first read.
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
    /// that was never configured still stays silent — a hiccup must not conjure
    /// a section for a provider nobody set up.
    pub fn unreadable(&mut self, error: String) {
        if self.configured.is_present() {
            self.last_error = Some(error);
        }
    }
}

/// Everything the Usage panel renders from.
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

    /// Whether a log walk has actually produced Claude rollups.
    ///
    /// The Services panel's evidence that Anthropic is worth watching
    /// (`services::active_vendors`): Anthropic is the one vendor with no
    /// credential to look for, so the signal is that this machine has Claude
    /// usage in it. Deliberately reads the *summary* rather than
    /// `!claude_loading`: a completed walk that found nothing is a machine that
    /// does not use Claude, and before the first walk this answers `false`
    /// rather than guessing — the tile appears when the data does, never ahead
    /// of it.
    #[must_use]
    pub fn claude_present(&self) -> bool {
        self.claude.is_some()
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

/// The whole panel payload.
///
/// `quota` is the store's `sentry_monthly_event_quota` and `rates` its
/// `neon_usd_per_cu_hour` / `neon_usd_per_gib_month` — both read at render time
/// rather than captured by the poller, because changing either must repaint the
/// panel without waiting out an hourly cadence for a number no API call is
/// involved in.
///
/// `plans` is the two plan opt-ins and their cadence, read at render time for
/// the same reason: turning a provider off must hide its rows now, not after
/// the next poll has dropped them.
///
/// `cadence_secs` is the operator's cadence for the **metered providers**
/// (`PanelInterval::UsageProviders`), and it is what each section's clock on the
/// panel's `freshness` line is classified against. Read at render time for the
/// same reason as the two
/// above: a shortened interval must re-date the sections now rather than an hour
/// from now. Claude's own rollups are not measured against it — they are on the
/// much faster `PanelInterval::ClaudeRollups` cadence, and their footer already covers that
/// clock.
#[must_use]
pub fn view(
    state: &UsageState,
    quota: u64,
    rates: NeonRates,
    plans: PlanInputs,
    cadence_secs: u64,
    now: u64,
) -> Value {
    let kind = PanelKind::ClaudeUsage;

    // Three states, in the original's own order: no summary at all (loading, or a log
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
            // Absent, not empty: the original renders the divider and the heading only
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
    // about today, and the original's `trailingLabel` returns the empty string there.
    let trailing = state.claude.as_ref().map_or_else(String::new, |summary| {
        format!("{} today", tokens(summary.today.total_tokens()))
    });

    // The panel's one warning line starts with Claude's own, so the header reads
    // in the body's order: the rollups first, then each metered section down the
    // card. Claude's is unattributed — it is the panel's own subject, and
    // `⚠ claude: no ~/.claude/projects` would name a section that is not one.
    let mut warnings = vec![status_footer(
        state.claude_succeeded,
        state.claude_error.as_deref(),
        now,
        state.claude_stale_after_secs(),
    )];

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
    // Claude contributes no clock, only a warning. Its rollups are on the
    // `ClaudeRollups` cadence rather than `cadence_secs`, so classifying them
    // against the metered providers' cycle would date a local log walk by a
    // stranger's edge — and its own 150s window is already what the warning
    // above measures.
    let mut clocks: Vec<Value> = Vec::new();
    let mut providers: Vec<Value> = Vec::new();
    // The plan block renders between the rollups and the metered providers,
    // so its warnings and clocks join the header in that order too.
    let plan = crate::plan_usage::view(
        state.plan(Provider::Claude),
        state.plan(Provider::Codex),
        plans,
        now,
    );
    warnings.extend(plan.warnings);
    clocks.extend(plan.clocks);
    for provider in configured.into_iter().flatten() {
        providers.push(provider.section);
        warnings.push(provider.warning);
        clocks.push(provider.freshness);
    }

    json!({
        "id": kind.id(),
        "title": kind.title(),
        "trailing": trailing,
        "message": message,
        "windows": windows,
        "projects": projects,
        // ONE line for the whole panel — Claude's warning and every metered
        // section's, each naming itself. There is deliberately no `footer` on a
        // section: a warning under the body is what made the card grow and
        // shove the rest of the cockpit around (#351), so the payload gives the
        // frontend nowhere to put one.
        "footer": merged_footer(&warnings),
        // And ONE clock line, beside it in the same header and never folded
        // into it: `footer` says the poller is late, this says how old the
        // figures are, and between the two edges only this one speaks (#338).
        // A section carries no `freshness` key for the same reason it carries no
        // `footer` — the line appeared and disappeared with a missed poll, and
        // in the body that cost the card a line of height (#355).
        "freshness": merged_freshness(&clocks),
        // The plan windows, one section per provider that is on, and the one
        // muted setup line for every provider that is off (`Null` when both
        // are on). Off is an instruction, never the dash a failure renders.
        "plans": plan.sections,
        "plansMessage": plan.off_line,
        "providers": providers,
        // Any half of the panel still waiting on its first answer keeps the
        // frontend on its fast refresh. Published rather than inferred from the
        // message text, which this boundary never asks the frontend to read.
        "loading": state.claude_loading
            || plan.loading
            || state.neon.is_loading()
            || state.sentry.is_loading()
            || state.vercel.is_loading(),
    })
}

// MARK: - Fixtures

/// Which rendering `--dump-usage` should produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fixture {
    /// Everything measured: Claude content, both plans' windows (well inside
    /// their limits), Neon figures + last invoice, Sentry count + bar.
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
        Fixture::Measured => {
            state.apply_claude(Some(fixture_summary()), claude_at, None);
            // Read at the providers' clock, reset times from the render's: a
            // stale dump moves only when the plans were read, so its rows and
            // bars are the healthy dump's.
            for (provider, reading) in fixture_plans(claude_at) {
                state.plan_mut(provider).state.succeeded(reading, at, None);
            }
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
    state
}

/// The fixture's plan readings, with every reset counted from `at`: green,
/// mid-window, every reset ahead. Shown only when the dump turns the plans on
/// ([`FIXTURE_PLANS`]).
fn fixture_plans(at: u64) -> [(Provider, subscription::PlanUsage); 2] {
    use subscription::{PlanUsage, PlanWindow, WindowSpan, SESSION_MINUTES, WEEK_MINUTES};
    let reset = |secs: u64| {
        chrono::DateTime::from_timestamp(i64::try_from(at + secs).unwrap_or(i64::MAX), 0)
    };
    let window = |minutes, scope: Option<&str>, used, resets_in| PlanWindow {
        span: WindowSpan::Minutes(minutes),
        scope: scope.map(str::to_owned),
        used_percent: used,
        resets_at: reset(resets_in),
    };
    [
        (
            Provider::Claude,
            PlanUsage {
                windows: vec![
                    window(SESSION_MINUTES, None, 34.0, 2 * 3_600 + 14 * 60),
                    window(WEEK_MINUTES, None, 41.0, 3 * 86_400 + 5 * 3_600),
                ],
            },
        ),
        (
            Provider::Codex,
            PlanUsage {
                windows: vec![
                    window(SESSION_MINUTES, None, 12.0, 3_600 + 40 * 60),
                    window(WEEK_MINUTES, None, 22.0, 4 * 86_400 + 2 * 3_600),
                ],
            },
        ),
    ]
}

/// The plan opt-ins the measured dumps render with: both on, so they show the
/// plan rows. The other dumps render with both off, as a fresh store does,
/// which is what puts the setup line in front of the browser suite.
pub const FIXTURE_PLANS: PlanInputs = PlanInputs {
    claude: true,
    codex: true,
    cadence_secs: 15 * 60,
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

    fn measured() -> UsageState {
        fixture_state(Fixture::Measured, NOW, NOW)
    }

    /// The panel as every store starts: both plan opt-ins off. A local item
    /// that shadows the glob-imported [`super::view`], so the tests written
    /// before the plan block keep describing exactly the panel they always
    /// did. The plan block's own tests call `super::view` with it on.
    fn view(state: &UsageState, quota: u64, rates: NeonRates, cadence: u64, now: u64) -> Value {
        super::view(state, quota, rates, PlanInputs::OFF, cadence, now)
    }

    /// The measured fixture with Claude's walk dated `at` instead of `NOW`.
    ///
    /// The panel now has **one** warning line, so a test that advances the clock
    /// past the metered providers' window would otherwise pick up Claude's own
    /// 150s staleness too — 10 minutes is late for a local log walk — and every
    /// assertion about a provider would be an assertion about two clocks at
    /// once. Keeping Claude current is what lets these tests name the exact line
    /// the header renders rather than matching a substring of it.
    fn measured_with_claude_at(at: u64) -> UsageState {
        fixture_state(Fixture::Measured, at, NOW)
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

    /// Off is one muted setup line and no plan section; on, the measured
    /// fixture's two plans render as their own sections, and the token
    /// rollups are untouched either way.
    #[test]
    fn the_plan_block_is_its_own_reading_beside_the_rollups() {
        let off = view(&measured(), QUOTA, RATES, CADENCE, NOW);
        assert_eq!(off["plans"], json!([]));
        assert_eq!(
            off["plansMessage"]["text"],
            "Claude and Codex plan limits are off — turn on in Settings"
        );

        let on = super::view(&measured(), QUOTA, RATES, FIXTURE_PLANS, CADENCE, NOW);
        assert!(on["plansMessage"].is_null());
        let ids: Vec<&str> = on["plans"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            [crate::plan_usage::CLAUDE_ID, crate::plan_usage::CODEX_ID]
        );
        assert_eq!(on["plans"][0]["rows"][0]["label"], "CLAUDE PLAN (5H)");
        assert_eq!(on["windows"], off["windows"], "the rollups do not move");
        assert_eq!(on["windows"][0]["label"], "5H");
    }

    /// A plan's failure reads in the header in body order: after Claude's own
    /// line, before the metered providers'.
    #[test]
    fn a_plan_warning_joins_the_header_between_the_rollups_and_the_providers() {
        let mut state = measured();
        state.neon_mut().failed("couldn't reach Neon".into());
        state
            .plan_mut(Provider::Codex)
            .state
            .failed("Codex's sign-in has expired — run it to refresh".into());
        let inputs = PlanInputs {
            codex: true,
            ..PlanInputs::OFF
        };
        let payload = super::view(&state, QUOTA, RATES, inputs, CADENCE, NOW);
        let text = payload["footer"]["text"].as_str().unwrap();
        let plan = text.find("codex-plan:").expect("plan warning");
        let neon = text.find("neon:").expect("neon warning");
        assert!(plan < neon, "{text}");
    }

    #[test]
    fn a_plan_that_is_on_and_unanswered_keeps_the_panel_loading() {
        let mut state = fixture_state(Fixture::Measured, NOW, NOW);
        state.plan_mut(Provider::Claude).reset();
        let inputs = PlanInputs {
            claude: true,
            ..PlanInputs::OFF
        };
        assert_eq!(
            super::view(&state, 0, RATES, inputs, CADENCE, NOW)["loading"],
            true
        );
        assert_eq!(view(&state, 0, RATES, CADENCE, NOW)["loading"], false);
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
            let mut state = measured_with_claude_at(NOW - age);
            if let Some(secs) = cadence {
                state.set_claude_cadence_secs(secs);
            }
            view(&state, QUOTA, RATES, CADENCE, NOW)["footer"].clone()
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
        let payload = view(&UsageState::new(), 0, NeonRates::default(), CADENCE, NOW);
        assert_eq!(payload["message"]["text"], LOADING_MESSAGE);
        assert_eq!(payload["loading"], true);
        assert_eq!(payload["trailing"], "");
        assert!(payload["windows"].as_array().unwrap().is_empty());
        assert!(payload["projects"].is_null());
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

    /// The log root could not even be located, so nothing was read. Distinct
    /// from an empty week, which *is* a measurement.
    #[test]
    fn a_missing_log_root_says_no_usage_data_and_names_itself_in_the_footer() {
        let payload = view(
            &fixture_state(Fixture::Empty, NOW, NOW),
            0,
            NeonRates::default(),
            CADENCE,
            NOW,
        );
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
        let payload = view(&state, 0, NeonRates::default(), CADENCE, NOW);
        assert_eq!(payload["message"]["text"], EMPTY_MESSAGE);
        assert!(payload["windows"].as_array().unwrap().is_empty());
        // The trailing label still reports today's measured zero: the walk
        // succeeded, so "0 today" is a fact, not a fabrication.
        assert_eq!(payload["trailing"], "0 today");
    }

    #[test]
    fn content_renders_both_windows_and_at_most_four_projects() {
        let payload = view(&measured(), 0, RATES, CADENCE, NOW);
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
        assert_eq!(rows[0]["name"], "solador");
        assert_eq!(rows[0]["value"], "2.1M");
        assert_eq!(rows[0]["dotColor"], color::hex(color::GREEN_DIM));

        assert_eq!(payload["trailing"], "1.2M today");
    }

    /// The subscription publishes no ceiling, so the window rows carry no bar
    /// at all — not a null one, which would read as a feature half-wired.
    #[test]
    fn window_rows_carry_no_progress_bar() {
        let payload = view(&measured(), 0, RATES, CADENCE, NOW);
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
        assert!(view(&state, 0, NeonRates::default(), CADENCE, NOW)["projects"].is_null());
    }

    // MARK: provider sections

    /// An unconfigured provider contributes nothing — no heading, no em dash,
    /// no divider. The panel is pixel-identical to its Claude-only self.
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
        let live = view(
            &measured_with_claude_at(NOW + 600),
            QUOTA,
            RATES,
            CADENCE,
            NOW + 600,
        );
        assert!(
            live["freshness"].is_null(),
            "current figures paint as they always did"
        );

        let at = NOW + CADENCE + 1;
        let stale = view(&measured_with_claude_at(at), QUOTA, RATES, CADENCE, at);
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
        let payload = view(&measured_with_claude_at(at), QUOTA, RATES, CADENCE, at);
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
        let mut state = measured_with_claude_at(at);
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
        let payload = view(&measured_with_claude_at(at), QUOTA, RATES, CADENCE, at);
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

    /// Each section is still measured against **its own window** — the whole
    /// reason the window is an argument. Only the place the resulting line
    /// lands changed: 10 minutes is stale for Claude (150s) and perfectly fine
    /// for the hourly providers, so the one header line carries Claude's
    /// warning and nothing else.
    #[test]
    fn each_section_carries_its_own_footer_on_its_own_window() {
        let payload = view(&measured(), QUOTA, RATES, CADENCE, NOW + 600);
        assert_eq!(payload["footer"]["text"], "⚠ stale · updated 10m ago");
        assert!(
            !payload["footer"]["text"]
                .as_str()
                .unwrap()
                .contains(&format!("{NEON_ID}:")),
            "a fresh section must not be named on the line"
        );
        for id in [NEON_ID, SENTRY_ID] {
            assert!(section(&payload, id).expect(id)["footer"].is_null(), "{id}");
        }
    }

    /// Two sections degrading at once. Hoisted unattributed these were the
    /// byte-identical `⚠ stale · updated 23h ago` twice over — a header line
    /// that said the same thing twice and identified neither. Each names itself
    /// with the very id its section is keyed by, so the line points at a block a
    /// reader can find.
    #[test]
    fn two_degraded_sections_are_distinguishable_on_the_one_line() {
        let at = NOW + 23 * 3_600;
        let payload = view(&measured_with_claude_at(at), QUOTA, RATES, CADENCE, at);
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

    /// Claude's warning and the sections' share the line, and neither is
    /// dropped when both are present.
    #[test]
    fn claudes_warning_and_a_providers_both_reach_the_header() {
        let mut state = fixture_state(Fixture::Empty, NOW, NOW);
        state.neon_mut().begin();
        state
            .neon_mut()
            .failed("Neon API request failed (HTTP 500)".to_owned());
        let payload = view(&state, QUOTA, RATES, CADENCE, NOW);
        let text = payload["footer"]["text"].as_str().expect("a header line");
        assert!(text.contains(NO_LOG_ROOT_MESSAGE), "Claude's: {text}");
        assert!(
            text.contains("neon: Neon API request failed"),
            "Neon's: {text}"
        );
        assert!(
            text.starts_with(&format!("⚠ {NO_LOG_ROOT_MESSAGE}")),
            "the panel's own subject leads, then the sections in body order: {text}"
        );
    }

    #[test]
    fn a_provider_failure_keeps_the_last_good_figure_and_dates_it() {
        let mut state = measured_with_claude_at(NOW + 300);
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

    /// The Playwright suite renders these payloads, so a fixture that quietly
    /// lost the case it claims to exercise would leave that suite green while
    /// covering nothing. Asserted here, where a Rust test can see it.
    #[test]
    fn the_fixtures_cover_every_rendering_the_panel_has() {
        let measured = view(
            &fixture_state(Fixture::Measured, NOW, NOW),
            QUOTA,
            RATES,
            CADENCE,
            NOW,
        );
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

        let empty = view(
            &fixture_state(Fixture::Empty, NOW, NOW),
            QUOTA,
            NeonRates::default(),
            CADENCE,
            NOW,
        );
        assert_eq!(empty["message"]["text"], NO_DATA_MESSAGE);
        assert!(empty["providers"].as_array().unwrap().is_empty());
        assert_eq!(empty["trailing"], "");
    }
}
