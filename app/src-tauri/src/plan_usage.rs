//! The AI Usage panel's **plan** rows (Claude's, then Codex's, then Copilot's).
//! The gate and schedule described below are Claude's and Codex's: a
//! subscription's own limit windows, read from the sign-in Claude Code or Codex
//! already holds. Copilot's reading is taken in `main`'s `poll_copilot_personal`,
//! and only its row is built here (`copilot_seat_section`).
//!
//! Three things live here, and only the first is a decision:
//!
//! 1. **The opt-in gate.** Nothing reads another program's sign-in until the
//!    operator turns that provider on in Settings (`store.json`'s
//!    `claude_plan_usage_enabled` / `codex_plan_usage_enabled`, both off by
//!    default). [`poll`] asks [`gate`] before it calls the credential reader,
//!    and a provider that is off has its retained reading dropped. On a Mac,
//!    Claude Code's sign-in is a keychain item, and reading it from another
//!    binary can raise a keychain prompt. A prompt nobody asked for, on launch,
//!    is the failure this gate exists to make impossible.
//! 2. **The schedule.** One cadence for both providers
//!    (`PanelInterval::PlanUsage`, 15 minutes by default, floor 5), tracked per
//!    provider so turning one on never spends a request on the other. A 429
//!    **backs the provider off**: after the n-th in a row the next read waits
//!    `cadence × 2^n`, capped at `8 × cadence` ([`backoff_secs`]), and a longer
//!    `Retry-After` still wins; a success resets n and any other failure leaves
//!    it. The schedule outlives a relaunch (`subscription::schedule`, written by
//!    the poll loop beside `store.json`), so a launch inside a wait waits. A
//!    refused keychain **parks** Claude until the toggle is flipped, so a
//!    "Deny" is not asked again every 15 minutes.
//! 3. **The rows.** Words, colours and pace come from `viewmodel::plan_usage`;
//!    this module only assembles them into the panel's payload: one row per
//!    plan (`Claude · Ultra`), carrying a meter per limit. While Claude's latest
//!    read was a 429, its row shows Claude Code's status-line figures instead
//!    (`subscription::statusline`), labelled as such, until a read succeeds.
//!
//! # Never the token
//!
//! [`poll`] hands the credential straight from the reader to the request and
//! keeps nothing: no field here holds a token, the state records only the
//! reading and the sentence for a failure, and the token is dropped when the
//! request returns. It is never copied into Solador's own credential store.
//!
//! # A subscription is not the machine
//!
//! These rows are a different reading from the panel's token rollups, which
//! are this machine's consumption from local logs and are attributed to no
//! account. A plan row is named by its provider and plan, its meters by their
//! limit (`5h`, `Week`, `Budget`), and the rollups sit apart under the panel's
//! "This machine" heading as `LAST 5H` / `LAST 7D`, so the two never read as
//! one figure.

use std::future::Future;
use std::path::Path;
use std::sync::Mutex;

use serde_json::{json, Value};
use subscription::schedule::{Found, Schedule, Schedules};
use subscription::statusline::{Kind as StatusLineKind, StatusLine};
use subscription::{
    Money, PlanErrorKind, PlanUsage, PlanUsageError, PlanWindow, Provider, Slot, WindowSpan,
    NO_WINDOWS_MESSAGE, SESSION_MINUTES, WEEK_MINUTES,
};
use viewmodel::color;
use viewmodel::freshness::Freshness;
use viewmodel::plan_usage as vm;

use crate::panel::{attributed_freshness_payload, attributed_status_footer};
use crate::usage::{ProviderState, UsageState};

/// The Claude plan section's id. Also its attribution in the header line.
pub const CLAUDE_ID: &str = "claude-plan";
/// The Codex plan section's id.
pub const CODEX_ID: &str = "codex-plan";

/// The longest a run of 429s backs a provider off, as a multiple of the cadence.
pub const BACKOFF_CAP_FACTOR: u64 = 8;

/// How long the next read waits after the `consecutive`-th 429 in a row:
/// `cadence × 2^n`, capped at [`BACKOFF_CAP_FACTOR`] × the cadence. At the
/// default 15 minutes that is 30m, 1h, 2h, then 2h.
#[must_use]
pub fn backoff_secs(consecutive: u32, cadence_secs: u64) -> u64 {
    let factor = 1_u64 << consecutive.min(BACKOFF_CAP_FACTOR.trailing_zeros());
    cadence_secs.saturating_mul(factor.min(BACKOFF_CAP_FACTOR))
}

/// The longest a wait can honestly be: the backoff's cap, or the longest
/// `Retry-After` the crate passes on, whichever is more.
#[must_use]
pub fn longest_wait_secs(cadence_secs: u64) -> u64 {
    cadence_secs
        .saturating_mul(BACKOFF_CAP_FACTOR)
        .max(subscription::MAX_RETRY_AFTER_SECS)
}

/// This source's own recommendation, mirrored by `PanelInterval::PlanUsage`'s
/// default (orca's 15 minutes). `cfg(test)` for the reason
/// `usage::PROVIDER_POLL_INTERVAL_SECS` gives: the loop reads the store, and
/// this exists so a test can say the two have parted.
#[cfg(test)]
pub const RECOMMENDED_POLL_INTERVAL_SECS: u64 = 15 * 60;

/// The section id for a provider.
#[must_use]
pub const fn section_id(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => CLAUDE_ID,
        Provider::Codex => CODEX_ID,
    }
}

/// The provider's name, in its plan row.
const fn plain_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "Claude",
        Provider::Codex => "Codex",
    }
}

/// One provider's published state, and its schedule.
#[derive(Debug, Default)]
pub struct PlanSource {
    /// The reading and the failure sentence. `Absent` when the provider is off.
    pub state: ProviderState<PlanUsage>,
    /// The keychain refused. No read is attempted until [`reset`](Self::reset).
    parked: bool,
    /// When the last read was started, for the cadence.
    last_attempt: Option<u64>,
    /// No read before this instant: the backoff after a 429, or the service's
    /// longer `Retry-After`.
    not_before: Option<u64>,
    /// Consecutive 429s since the last success. Other failures leave it alone.
    rate_limited: u32,
    /// Claude Code's status-line figures, held only while the latest read was a
    /// 429 ([`refresh_status_line`]). A success, a reset or any other outcome
    /// drops them.
    pub(crate) status_line: Option<StatusLine>,
    /// The kind of the last read's failure, or `None` after a success (or a
    /// reset). The memory the failure-transition log line is decided against.
    last_failure: Option<String>,
    /// Bumped by every reset, so a read that was in flight when the operator
    /// turned the provider off cannot land its result afterwards.
    generation: u64,
}

impl PlanSource {
    /// Back to "off": no reading, no schedule, not parked. What a toggle in
    /// either direction does, so turning a provider back on starts clean and
    /// a parked one is asked again.
    pub fn reset(&mut self) {
        let generation = self.generation.wrapping_add(1);
        *self = PlanSource {
            generation,
            ..PlanSource::default()
        };
        self.state.unconfigure();
    }

    /// Parks the provider, as a refused keychain does. For tests elsewhere in
    /// the crate that need a parked provider without a keychain.
    #[cfg(test)]
    pub fn park_for_test(&mut self) {
        self.parked = true;
    }

    /// The part of this schedule that outlives a relaunch.
    #[must_use]
    pub fn schedule(&self) -> Schedule {
        Schedule {
            last_attempt: self.last_attempt,
            rate_limited: self.rate_limited,
            not_before: self.not_before,
            last_was_rate_limited: self.is_rate_limited(),
        }
    }

    /// Puts a persisted schedule back, at launch. A latest outcome of 429 comes
    /// back as the failure it was, so the card says so before the first read.
    ///
    /// A time the file cannot honestly hold is dropped, because [`gate`] would
    /// otherwise wait on it forever (a clock set ahead and corrected, a VM
    /// resume, a hand edit): a `last_attempt` after `now`, and a `not_before`
    /// further ahead than any wait can be ([`longest_wait_secs`]). Returns
    /// whether either was dropped.
    pub fn restore(
        &mut self,
        provider: Provider,
        schedule: Schedule,
        now: u64,
        cadence_secs: u64,
    ) -> bool {
        let last_attempt = schedule.last_attempt.filter(|at| *at <= now);
        let not_before = schedule
            .not_before
            .filter(|at| *at <= now.saturating_add(longest_wait_secs(cadence_secs)));
        let dropped = last_attempt != schedule.last_attempt || not_before != schedule.not_before;
        self.last_attempt = last_attempt;
        self.rate_limited = schedule.rate_limited;
        self.not_before = not_before;
        if schedule.last_was_rate_limited {
            let throttled = PlanUsageError::new(
                provider,
                PlanErrorKind::RateLimited {
                    retry_after_secs: None,
                },
            );
            self.last_failure = Some(kind_label(&throttled.kind));
            self.state.failed(throttled.user_message());
        }
        dropped
    }

    /// Whether the latest read's outcome was a 429.
    #[must_use]
    pub fn is_rate_limited(&self) -> bool {
        self.last_failure.as_deref() == Some(RATE_LIMITED_LABEL)
    }

    /// Whether this provider is on and still waiting on its first answer.
    #[must_use]
    pub fn is_loading(&self, enabled: bool) -> bool {
        enabled && self.state.summary.is_none() && self.state.last_error.is_none()
    }
}

/// [`kind_label`] of a 429.
const RATE_LIMITED_LABEL: &str = "rate-limited";

/// A name for a failure kind that is safe to write to the log: a fixed word per
/// variant, never the `Unreachable` / `Undecodable` detail (which can carry
/// transport text) and never a header, a URL or a token. `Http` carries its
/// status code (a `u16` from a fixed field, not server text), so a 401 turning
/// into a 503 is a different kind.
fn kind_label(kind: &PlanErrorKind) -> String {
    let word = match kind {
        PlanErrorKind::NotSignedIn => "not-signed-in",
        PlanErrorKind::NoSubscription => "no-subscription",
        PlanErrorKind::KeychainRefused => "keychain-refused",
        PlanErrorKind::CredentialUnreadable => "credential-unreadable",
        PlanErrorKind::SignInExpired => "sign-in-expired",
        PlanErrorKind::MissingScope => "missing-scope",
        PlanErrorKind::RateLimited { .. } => RATE_LIMITED_LABEL,
        PlanErrorKind::Http { status } => return format!("http-status {status}"),
        PlanErrorKind::Unreachable(_) => "unreachable",
        PlanErrorKind::Undecodable(_) => "undecodable",
    };
    word.to_owned()
}

/// Records one read's outcome on `source` and returns the log lines it earns:
/// one when the failure kind changes (ok to failed, failed to a different
/// kind, failed to ok), one when this outcome parks the provider, and one when
/// it backs the provider off after a 429: one line per 429, naming the wait and
/// whether it is the service's `Retry-After` (when that is longer than the
/// backoff) or the backoff. Polls the gate skips never get here, so a parked or
/// waiting provider logs nothing more. The lines name the provider and fixed
/// words only.
pub(crate) fn record_outcome(
    source: &mut PlanSource,
    provider: Provider,
    failure: Option<&PlanUsageError>,
    now: u64,
    cadence_secs: u64,
) -> Vec<String> {
    let id = section_id(provider);
    let mut lines = Vec::new();
    let label = failure.map(|error| kind_label(&error.kind));
    if label != source.last_failure {
        match (source.last_failure.as_deref(), label.as_deref()) {
            (None, Some(new)) => lines.push(format!("plan usage: {id} read failed ({new})")),
            (Some(old), Some(new)) => {
                lines.push(format!("plan usage: {id} failure changed ({old} -> {new})"));
            }
            (Some(old), None) => {
                lines.push(format!("plan usage: {id} read ok again (was {old})"));
            }
            (None, None) => {}
        }
        source.last_failure = label;
    }
    if let Some(error) = failure {
        if error.kind == PlanErrorKind::KeychainRefused {
            source.parked = true;
            lines.push(format!(
                "plan usage: {id} parked until Subscription is removed and added back"
            ));
        }
        if matches!(error.kind, PlanErrorKind::RateLimited { .. }) {
            source.rate_limited = source.rate_limited.saturating_add(1);
            let backoff = backoff_secs(source.rate_limited, cadence_secs);
            let retry_after = error.retry_after_secs();
            // The service's own word wins when it asks for longer.
            let wait = backoff.max(retry_after.unwrap_or(0));
            source.not_before = Some(now.saturating_add(wait));
            lines.push(match retry_after.filter(|secs| *secs > backoff) {
                Some(secs) => format!("plan usage: {id} retry-after {secs}s"),
                None => format!(
                    "plan usage: {id} backing off {wait}s after {} rate limit(s) in a row",
                    source.rate_limited
                ),
            });
        } else {
            // Another failure waits only the cadence, and leaves the count.
            source.not_before = None;
        }
    } else {
        source.not_before = None;
        source.rate_limited = 0;
    }
    if !source.is_rate_limited() {
        source.status_line = None;
    }
    lines
}

/// What a pass does for one provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// The operator has not turned it on. Nothing is read.
    Off,
    /// The keychain refused; nothing is read until the toggle is flipped.
    Parked,
    /// Not due for this many seconds.
    Wait(u64),
    /// Read now.
    Due,
}

/// Whether `source` may be read at `now`. Pure: the whole opt-in and schedule
/// decision, with no I/O.
#[must_use]
pub fn gate(enabled: bool, source: &PlanSource, now: u64, cadence_secs: u64) -> Gate {
    if !enabled {
        return Gate::Off;
    }
    if source.parked {
        return Gate::Parked;
    }
    let due_at = source
        .last_attempt
        .map_or(now, |at| at.saturating_add(cadence_secs))
        .max(source.not_before.unwrap_or(0));
    if due_at <= now {
        Gate::Due
    } else {
        Gate::Wait(due_at - now)
    }
}

/// One pass for one provider.
///
/// `read` borrows the credential and `fetch` sends the one request; the
/// credential goes from one to the other and is dropped. Neither is called
/// unless [`gate`] says [`Gate::Due`], which it never does for a provider the
/// operator has not turned on.
///
/// `enabled` is asked **here, at this provider's turn**, not taken as a value
/// the caller read earlier. The loop polls Claude before Codex, and Claude's
/// reader can wait up to a minute on a keychain prompt: an opt-in read once for
/// the whole pass would let Codex be read after the operator had turned it off
/// during that wait, which is the one thing the opt-in promises cannot happen.
///
/// Returns the gate this pass acted on. The log lines go to stderr.
pub async fn poll<T, E, R, RF, F, FF>(
    usage: &Mutex<UsageState>,
    provider: Provider,
    enabled: E,
    now: u64,
    cadence_secs: u64,
    read: R,
    fetch: F,
) -> Gate
where
    E: FnOnce() -> bool,
    R: FnOnce() -> RF,
    RF: Future<Output = Result<T, PlanErrorKind>>,
    F: FnOnce(T) -> FF,
    FF: Future<Output = Result<PlanUsage, PlanUsageError>>,
{
    poll_logged(
        usage,
        provider,
        enabled,
        now,
        cadence_secs,
        read,
        fetch,
        |line| eprintln!("{line}"),
    )
    .await
}

/// [`poll`] with the log sink as a parameter, so a test can read the lines.
/// `log` is called after the usage lock is released: `eprintln!` panics on a
/// broken stderr pipe, and a panic under the lock would poison `UsageState`.
#[allow(clippy::too_many_arguments)]
pub async fn poll_logged<T, E, R, RF, F, FF, L>(
    usage: &Mutex<UsageState>,
    provider: Provider,
    enabled: E,
    now: u64,
    cadence_secs: u64,
    read: R,
    fetch: F,
    mut log: L,
) -> Gate
where
    L: FnMut(&str),
    E: FnOnce() -> bool,
    R: FnOnce() -> RF,
    RF: Future<Output = Result<T, PlanErrorKind>>,
    F: FnOnce(T) -> FF,
    FF: Future<Output = Result<PlanUsage, PlanUsageError>>,
{
    // Before the usage lock, not under it: the caller's answer reads the store,
    // and holding one lock while taking the other is how an ordering deadlock
    // starts.
    let enabled = enabled();
    let generation = {
        let mut state = usage.lock().expect("usage state poisoned");
        let source = state.plan_mut(provider);
        match gate(enabled, source, now, cadence_secs) {
            Gate::Off => {
                // The opt-in. Return before anything is read, and drop what an
                // earlier "on" left behind.
                source.reset();
                return Gate::Off;
            }
            Gate::Due => {
                // Before the request, like every metered provider, so the
                // section says it is reading rather than waiting on the round
                // trip to learn it exists.
                source.state.begin();
                source.last_attempt = Some(now);
                source.generation
            }
            other => return other,
        }
    };

    let outcome = match read().await {
        Ok(credential) => fetch(credential).await,
        Err(kind) => Err(PlanUsageError::new(provider, kind)),
    };

    let mut state = usage.lock().expect("usage state poisoned");
    let source = state.plan_mut(provider);
    if source.generation != generation {
        // Turned off (or off and on) while this read was in flight.
        return Gate::Due;
    }
    let lines = match outcome {
        Ok(reading) => {
            let lines = record_outcome(source, provider, None, now, cadence_secs);
            let note = reading.is_empty().then(|| NO_WINDOWS_MESSAGE.to_owned());
            source.state.succeeded(reading, now, note);
            lines
        }
        Err(error) => {
            let lines = record_outcome(source, provider, Some(&error), now, cadence_secs);
            source.state.failed(error.user_message());
            lines
        }
    };
    drop(state);
    for line in lines {
        log(&line);
    }
    Gate::Due
}

/// How long the loop may sleep before some provider is due, at most
/// `cadence_secs` and at least one second.
#[must_use]
pub fn next_wake(
    sources: [(bool, &PlanSource); 2],
    now: u64,
    cadence_secs: u64,
) -> std::time::Duration {
    let soonest = sources
        .into_iter()
        .filter_map(
            |(enabled, source)| match gate(enabled, source, now, cadence_secs) {
                Gate::Wait(secs) => Some(secs),
                Gate::Due => Some(0),
                Gate::Off | Gate::Parked => None,
            },
        )
        .min()
        .unwrap_or(cadence_secs);
    std::time::Duration::from_secs(soonest.clamp(1, cadence_secs.max(1)))
}

/// Both providers' schedules, as the poll loop persists them.
#[must_use]
pub fn snapshot(usage: &UsageState) -> Schedules {
    let mut schedules = Schedules::default();
    for provider in [Provider::Claude, Provider::Codex] {
        schedules.set(provider, usage.plan(provider).schedule());
    }
    schedules
}

/// Puts persisted schedules back, once, at launch. A provider that is off drops
/// what this restores at its first pass ([`gate`] says [`Gate::Off`]). Returns
/// the log lines for any wait it dropped as impossible.
pub fn restore(
    usage: &mut UsageState,
    schedules: &Schedules,
    now: u64,
    cadence_secs: u64,
) -> Vec<String> {
    let mut lines = Vec::new();
    for provider in [Provider::Claude, Provider::Codex] {
        if usage
            .plan_mut(provider)
            .restore(provider, schedules.get(provider), now, cadence_secs)
        {
            lines.push(format!(
                "plan usage: {} saved wait was dated past any possible wait, dropped",
                section_id(provider)
            ));
        }
    }
    lines
}

/// The schedule file as the poll loop keeps it: loaded and restored once at
/// launch, rewritten after a pass only when it changed. A failed write is kept
/// as unsaved and tried again on the next pass.
#[derive(Debug)]
pub struct SchedulePersistence {
    dir: Option<std::path::PathBuf>,
    saved: Schedules,
    failing: bool,
}

impl SchedulePersistence {
    /// Loads the file in `dir` and restores it into `usage`. The lines to log.
    pub fn open(
        dir: Option<std::path::PathBuf>,
        usage: &Mutex<UsageState>,
        now: u64,
        cadence_secs: u64,
    ) -> (Self, Vec<String>) {
        let (saved, found) = dir
            .as_deref()
            .map_or((Schedules::default(), Found::Absent), |dir| {
                subscription::schedule::load_checked(dir)
            });
        let mut lines = Vec::new();
        if found == Found::Unusable {
            // No wait is owed either way; the line only says the file was there.
            lines.push(
                "plan usage: the saved read schedule could not be read, starting fresh".to_owned(),
            );
        }
        lines.extend(restore(
            &mut usage.lock().expect("usage state poisoned"),
            &saved,
            now,
            cadence_secs,
        ));
        let persistence = SchedulePersistence {
            dir,
            saved,
            failing: false,
        };
        (persistence, lines)
    }

    /// Writes the schedules if they differ from what was last saved. Logs once
    /// when saving starts failing and once when it recovers, never the path or
    /// the error's text.
    pub fn persist(&mut self, usage: &Mutex<UsageState>) -> Vec<String> {
        let now = snapshot(&usage.lock().expect("usage state poisoned"));
        let Some(dir) = self.dir.as_deref() else {
            return Vec::new();
        };
        if now == self.saved {
            return Vec::new();
        }
        match subscription::schedule::save(dir, &now) {
            Ok(()) => {
                self.saved = now;
                if std::mem::take(&mut self.failing) {
                    vec!["plan usage: the read schedule is saving again".to_owned()]
                } else {
                    Vec::new()
                }
            }
            Err(_) if self.failing => Vec::new(),
            Err(_) => {
                self.failing = true;
                vec!["plan usage: could not save the read schedule".to_owned()]
            }
        }
    }
}

/// Brings Claude's held status-line figures up to date for a render at `now`:
/// read from `dir` while the opt-in is on and the latest read was a 429, dropped
/// otherwise. The file is read outside the usage lock. Called by whatever builds
/// the panel, so the figures are as new as the status line's last write and not
/// as old as the last poll.
pub fn refresh_status_line(usage: &Mutex<UsageState>, enabled: bool, dir: Option<&Path>, now: u64) {
    let wanted = enabled
        && usage
            .lock()
            .expect("usage state poisoned")
            .plan(Provider::Claude)
            .is_rate_limited();
    let line = dir
        .filter(|_| wanted)
        .and_then(|dir| subscription::statusline::read(dir, now));
    usage
        .lock()
        .expect("usage state poisoned")
        .plan_mut(Provider::Claude)
        .status_line = line;
}

/// What the AI Usage panel reads at render time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanInputs {
    pub claude: bool,
    pub codex: bool,
    /// `PanelInterval::PlanUsage`, what each section's clock is dated against.
    pub cadence_secs: u64,
    /// Whether this machine's Claude Code logs are read at all.
    pub claude_local_logs: bool,
    /// `PanelInterval::UsageProviders`, what API spend's and Copilot's clocks are
    /// dated against.
    pub spend_cadence_secs: u64,
    /// The Copilot source's organization half is on. Read at render time, so
    /// unticking it hides the row on this frame; the half owns no key to go
    /// missing.
    pub copilot_org: bool,
}

impl PlanInputs {
    /// Everything off, as a new store starts.
    pub const OFF: PlanInputs = PlanInputs {
        claude: false,
        codex: false,
        cadence_secs: 15 * 60,
        claude_local_logs: false,
        spend_cadence_secs: 60 * 60,
        copilot_org: false,
    };

    #[must_use]
    pub const fn enabled(self, provider: Provider) -> bool {
        match provider {
            Provider::Claude => self.claude,
            Provider::Codex => self.codex,
        }
    }
}

/// One provider's contribution to the AI Usage panel.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanView {
    /// The provider's section while it is on, or nothing.
    pub sections: Vec<Value>,
    /// The section's header warning, for `merged_footer`.
    pub warnings: Vec<Value>,
    /// The section's clock, for `merged_freshness`.
    pub clocks: Vec<Value>,
    /// The provider is on and has not answered yet.
    pub loading: bool,
}

/// One provider's plan block, at render time `now`. The AI Usage panel puts
/// both providers' blocks side by side, so nothing here may speak for the
/// other: every line is attributed by the section's own id.
#[must_use]
pub fn view(provider: Provider, source: &PlanSource, inputs: PlanInputs, now: u64) -> PlanView {
    let mut plan = PlanView {
        sections: Vec::new(),
        warnings: Vec::new(),
        clocks: Vec::new(),
        loading: false,
    };
    let enabled = inputs.enabled(provider);
    if !enabled {
        return plan;
    }
    let id = section_id(provider);
    plan.sections
        .push(section(provider, source, enabled, now, inputs.cadence_secs));
    // Twice the cadence: one missed read is not yet a stuck poller.
    plan.warnings.push(attributed_status_footer(
        id,
        source.state.last_updated,
        source.state.last_error.as_deref(),
        now,
        inputs.cadence_secs.saturating_mul(2),
    ));
    plan.clocks.push(attributed_freshness_payload(
        id,
        Freshness::classify(
            source.state.last_updated.map(|at| now.saturating_sub(at)),
            inputs.cadence_secs,
        ),
    ));
    plan.loading = source.is_loading(enabled);
    plan
}

/// One provider's section: its plan row and that row's meters, or a single
/// placeholder row with no meters while there is nothing to show.
fn section(
    provider: Provider,
    source: &PlanSource,
    enabled: bool,
    now: u64,
    cadence_secs: u64,
) -> Value {
    let id = section_id(provider);
    let name = plain_name(provider);
    if provider == Provider::Claude && source.is_rate_limited() {
        if let Some(row) = status_line_section(source, now, cadence_secs) {
            return row;
        }
    }
    let reading = source
        .state
        .summary
        .as_ref()
        .filter(|reading| !reading.is_empty());
    let Some(reading) = reading else {
        // A plan name with no limits is not a reading, but it is still the
        // plan: `Claude · Ultra  —` says more than `Claude  —`.
        let plan = source
            .state
            .summary
            .as_ref()
            .and_then(|reading| reading.plan.as_deref());
        let label = vm::provider_label(name, plan);
        return row_json(id, &vm::placeholder_row(&label, source.is_loading(enabled)));
    };
    // Pace is judged at the reading's own time, and only while it is inside the
    // same stale edge the header warns at. A reading with no time is not fresh.
    let (read_at, fresh) = match source.state.last_updated {
        Some(at) => (at, now.saturating_sub(at) <= cadence_secs.saturating_mul(2)),
        None => (now, false),
    };
    let secs = |at: u64| i64::try_from(at).unwrap_or(i64::MAX);
    let row = vm::plan_row(name, &mirror_plan(reading), secs(now), secs(read_at), fresh);
    row_json(id, &row)
}

/// The Claude row while Anthropic is refusing the plan read: Claude Code's
/// status-line figures (5h and Week only), the plan's name from the last reading
/// that had one, and the sentence that says where the figures are from and how
/// old they are. `None` when there is nothing to show, which leaves the card as
/// it was: a dash under the header's warning, or the older reading kept.
///
/// The status line is used only when it is the newer of the two: a plan read
/// that landed after the file was written is the fresher figure. Pace is judged
/// at the file's write time, inside the same stale edge the header warns at.
fn status_line_section(source: &PlanSource, now: u64, cadence_secs: u64) -> Option<Value> {
    let line = source.status_line?;
    if source
        .state
        .last_updated
        .is_some_and(|read| read > line.written_at)
    {
        return None;
    }
    let windows: Vec<vm::Window> = line
        .windows(now)
        .into_iter()
        .map(|(kind, window)| vm::Window {
            span: vm::Span::Minutes(match kind {
                StatusLineKind::FiveHour => SESSION_MINUTES,
                StatusLineKind::SevenDay => WEEK_MINUTES,
            }),
            scope: None,
            used_percent: window.used_percent,
            resets_at: window.resets_at,
        })
        .collect();
    if windows.is_empty() {
        return None;
    }
    let plan = vm::Plan {
        name: source
            .state
            .summary
            .as_ref()
            .and_then(|reading| reading.plan.clone()),
        windows,
        spend: None,
    };
    let age = line.age_secs(now);
    let fresh = age <= cadence_secs.saturating_mul(2);
    let secs = |at: u64| i64::try_from(at).unwrap_or(i64::MAX);
    let read_at = line.written_at.min(now);
    let row = vm::plan_row(
        plain_name(Provider::Claude),
        &plan,
        secs(now),
        secs(read_at),
        fresh,
    );
    let note = vm::status_line_note(age);
    let mut json = row_json(CLAUDE_ID, &row);
    json["detail"] = json!(format!("{} {note}", row.detail));
    json["source"] = json!(note);
    Some(json)
}

fn mirror_plan(reading: &PlanUsage) -> vm::Plan {
    vm::Plan {
        name: reading.plan.clone(),
        windows: reading.windows.iter().map(mirror_window).collect(),
        spend: reading.spend.as_ref().map(|spend| vm::Spend {
            used: spend.used.as_ref().map(mirror_money),
            limit: spend.limit.as_ref().map(mirror_money),
            stated_percent: spend.stated_percent,
            enabled: spend.enabled,
            resets_at: None,
            reset_unreadable: false,
        }),
    }
}

/// The Copilot plan row: the token owner's own plan or employer seat, one spend
/// cap in US dollars (a credit is $0.01). Built by [`vm::plan_row`], the
/// function Claude's and Codex's rows are built by, so it takes their meter,
/// their colours and the red rule `dashboard::plan_rows` reads, with no
/// attention code of its own.
///
/// With no reading (loading, or a failure before any figure, or one that took
/// its figure with it) it is the placeholder row: `Copilot  —`, and the header
/// line carries the reason. `Copilot · Business` once there is a reading; the
/// plan's name is GitHub's string with its first letter raised, a display
/// transform and nothing more.
pub fn copilot_seat_section(state: &crate::usage::CopilotPlanState, now: u64) -> Value {
    let id = crate::usage::COPILOT_PLAN_ID;
    let Some(seat) = state.read.summary.as_ref() else {
        let label = vm::provider_label(COPILOT_NAME, None);
        return row_json(id, &vm::placeholder_row(&label, state.read.is_loading()));
    };
    let usd = |credits: copilotusage::Credits| vm::Money {
        minor: credits.usd_minor(),
        exponent: copilotusage::USD_EXPONENT,
        currency: Some("USD".to_owned()),
    };
    let plan = vm::Plan {
        name: seat.plan.clone(),
        windows: Vec::new(),
        spend: Some(vm::Spend {
            used: Some(usd(seat.used)),
            limit: match seat.allowance {
                copilotusage::Allowance::Capped(limit) => Some(usd(limit)),
                copilotusage::Allowance::Unlimited => None,
            },
            stated_percent: None,
            enabled: None,
            resets_at: seat.resets_at,
            reset_unreadable: seat.reset_unreadable,
        }),
    };
    let secs = |at: u64| i64::try_from(at).unwrap_or(i64::MAX);
    let read_at = state.read.last_updated.unwrap_or(now);
    row_json(
        id,
        &vm::plan_row(COPILOT_NAME, &plan, secs(now), secs(read_at), true),
    )
}

/// The provider's name in the Copilot plan row's label.
const COPILOT_NAME: &str = "Copilot";

fn mirror_window(window: &PlanWindow) -> vm::Window {
    vm::Window {
        span: match window.span {
            WindowSpan::Minutes(minutes) => vm::Span::Minutes(minutes),
            WindowSpan::Unstated(Slot::Primary) => vm::Span::Primary,
            WindowSpan::Unstated(Slot::Secondary) => vm::Span::Secondary,
        },
        scope: window.scope.clone(),
        used_percent: window.used_percent,
        resets_at: window.resets_at.map(|at| at.timestamp()),
    }
}

fn mirror_money(money: &Money) -> vm::Money {
    vm::Money {
        minor: money.minor,
        exponent: money.exponent,
        currency: money.currency.clone(),
    }
}

/// A plan section as the panel and the tile both read it: the row the tile
/// lists, and the meters under it.
fn row_json(id: &str, row: &vm::PlanRow) -> Value {
    json!({
        "id": id,
        "label": row.label,
        "value": row.value,
        "valueColor": color::hex(row.value_color),
        "color": color::hex(row.color),
        "detail": row.detail,
        "metrics": row.metrics.iter().map(metric_json).collect::<Vec<_>>(),
    })
}

/// One meter, in the Machines tile's metric shape plus a pace tick and a note.
fn metric_json(metric: &vm::Metric) -> Value {
    let mut out = json!({
        "label": metric.label,
        "value": metric.value,
        "fraction": metric.fraction,
        "color": color::hex(metric.color),
        "pace": metric.pace,
        "note": metric.note,
    });
    if metric.full {
        out["span"] = json!("full");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use subscription::{SESSION_MINUTES, WEEK_MINUTES};

    const NOW: u64 = 1_792_843_200; // 2026-10-24T12:00:00Z
    const CADENCE: u64 = 15 * 60;

    fn at(secs: u64) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(i64::try_from(secs).unwrap(), 0).unwrap()
    }

    fn reading() -> PlanUsage {
        PlanUsage {
            windows: vec![
                PlanWindow {
                    span: WindowSpan::Minutes(SESSION_MINUTES),
                    scope: None,
                    used_percent: 42.0,
                    resets_at: Some(at(NOW + 2 * 3600 + 14 * 60)),
                },
                PlanWindow {
                    span: WindowSpan::Minutes(WEEK_MINUTES),
                    scope: Some("Fable".into()),
                    used_percent: 93.0,
                    resets_at: Some(at(NOW + 3 * 86_400)),
                },
            ],
            ..PlanUsage::default()
        }
    }

    /// An Enterprise seat: no windows, a dollar budget.
    fn enterprise() -> PlanUsage {
        let usd = |minor| Money {
            minor,
            exponent: 2,
            currency: Some("USD".into()),
        };
        PlanUsage {
            plan: Some("Enterprise".into()),
            windows: vec![],
            spend: Some(subscription::PlanSpend {
                used: Some(usd(31_245)),
                limit: Some(usd(50_000)),
                stated_percent: Some(62.49),
                enabled: None,
            }),
        }
    }

    /// A reader and a fetcher that count their calls.
    struct Calls {
        reads: AtomicUsize,
        fetches: AtomicUsize,
    }

    impl Calls {
        fn new() -> Self {
            Calls {
                reads: AtomicUsize::new(0),
                fetches: AtomicUsize::new(0),
            }
        }
    }

    async fn pass(
        usage: &Mutex<UsageState>,
        provider: Provider,
        enabled: bool,
        now: u64,
        calls: &Calls,
        read: Result<&'static str, PlanErrorKind>,
        fetched: Result<PlanUsage, PlanErrorKind>,
    ) -> Gate {
        poll(
            usage,
            provider,
            || enabled,
            now,
            CADENCE,
            || async {
                calls.reads.fetch_add(1, Ordering::SeqCst);
                read
            },
            |credential: &'static str| async move {
                calls.fetches.fetch_add(1, Ordering::SeqCst);
                assert_eq!(
                    credential, "token",
                    "the credential goes straight to the request"
                );
                fetched.map_err(|kind| PlanUsageError::new(provider, kind))
            },
        )
        .await
    }

    // ---- the log lines ---------------------------------------------------

    fn failure(kind: PlanErrorKind) -> PlanUsageError {
        PlanUsageError::new(Provider::Claude, kind)
    }

    #[test]
    fn a_failure_logs_on_each_change_of_kind_and_on_recovery_never_per_poll() {
        let mut source = PlanSource::default();
        let p = Provider::Claude;
        let expired = failure(PlanErrorKind::SignInExpired);
        let first = record_outcome(&mut source, p, Some(&expired), NOW, CADENCE);
        assert_eq!(
            first,
            ["plan usage: claude-plan read failed (sign-in-expired)"]
        );
        assert!(
            record_outcome(&mut source, p, Some(&expired), NOW + 1, CADENCE).is_empty(),
            "the same kind again is not news"
        );
        let other = failure(PlanErrorKind::Http { status: 503 });
        assert_eq!(
            record_outcome(&mut source, p, Some(&other), NOW + 2, CADENCE),
            ["plan usage: claude-plan failure changed (sign-in-expired -> http-status 503)"]
        );
        assert_eq!(
            record_outcome(&mut source, p, None, NOW + 3, CADENCE),
            ["plan usage: claude-plan read ok again (was http-status 503)"]
        );
        assert!(record_outcome(&mut source, p, None, NOW + 4, CADENCE).is_empty());
    }

    /// Negative control: label `Http` as a bare `http-status` and the
    /// 401-to-503 change below logs nothing, and the first line loses its code.
    #[test]
    fn an_http_failure_carries_its_status_and_a_changed_status_is_a_changed_kind() {
        let mut source = PlanSource::default();
        let p = Provider::Codex;
        let unauthorized = failure(PlanErrorKind::Http { status: 401 });
        let unavailable = failure(PlanErrorKind::Http { status: 503 });
        assert_eq!(
            record_outcome(&mut source, p, Some(&unauthorized), NOW, CADENCE),
            ["plan usage: codex-plan read failed (http-status 401)"]
        );
        assert!(record_outcome(&mut source, p, Some(&unauthorized), NOW + 1, CADENCE).is_empty());
        assert_eq!(
            record_outcome(&mut source, p, Some(&unavailable), NOW + 2, CADENCE),
            ["plan usage: codex-plan failure changed (http-status 401 -> http-status 503)"]
        );
    }

    #[test]
    fn a_park_logs_once_and_parks() {
        let mut source = PlanSource::default();
        let refused = failure(PlanErrorKind::KeychainRefused);
        let lines = record_outcome(&mut source, Provider::Codex, Some(&refused), NOW, CADENCE);
        assert_eq!(
            lines.iter().filter(|l| l.contains("parked")).count(),
            1,
            "{lines:?}"
        );
        assert!(source.parked);
    }

    /// Negative control for "once": delete the `label != last_failure` guard
    /// and the second call returns a `read failed` line again.
    #[test]
    fn a_failure_repeated_logs_its_read_failed_line_once_and_carries_no_detail() {
        let p = Provider::Codex;
        let mut source = PlanSource::default();
        let down = failure(PlanErrorKind::Http { status: 503 });
        let first = record_outcome(&mut source, p, Some(&down), NOW, CADENCE);
        assert!(first.iter().any(|l| l.contains("read failed")), "{first:?}");
        let second = record_outcome(&mut source, p, Some(&down), NOW + 1, CADENCE);
        assert!(
            second.is_empty(),
            "the same failure twice logged twice: {second:?}"
        );

        let leaky = failure(PlanErrorKind::Unreachable(
            "https://example.test/?token=SECRET".into(),
        ));
        let lines = record_outcome(&mut PlanSource::default(), p, Some(&leaky), NOW, CADENCE);
        assert_eq!(lines, ["plan usage: codex-plan read failed (unreachable)"]);
    }

    fn throttled(retry_after_secs: Option<u64>) -> PlanUsageError {
        failure(PlanErrorKind::RateLimited { retry_after_secs })
    }

    /// Records `n` 429s in a row at `NOW` and returns the source.
    fn after_throttles(n: u32, retry_after_secs: Option<u64>) -> PlanSource {
        let mut source = PlanSource::default();
        for _ in 0..n {
            record_outcome(
                &mut source,
                Provider::Claude,
                Some(&throttled(retry_after_secs)),
                NOW,
                CADENCE,
            );
        }
        source
    }

    /// Negative control: cap at `4 *` and the fourth rung reads 1h; drop the
    /// cap and the sixth reads 8h.
    #[test]
    fn consecutive_429s_back_off_cadence_times_two_to_the_n_capped_at_eight_cadences() {
        let ladder: Vec<u64> = (1..=6).map(|n| backoff_secs(n, CADENCE)).collect();
        assert_eq!(
            ladder,
            [30 * 60, 60 * 60, 120 * 60, 120 * 60, 120 * 60, 120 * 60],
            "30m, 1h, 2h, then 2h at the default 15 minutes"
        );
        assert_eq!(backoff_secs(1, 5 * 60), 10 * 60, "it follows the cadence");
        assert_eq!(
            backoff_secs(u32::MAX, CADENCE),
            8 * CADENCE,
            "never overflows"
        );

        // The source walks the same ladder, and the gate enforces it.
        let mut source = PlanSource::default();
        for (n, wait) in [(1, 30 * 60), (2, 60 * 60), (3, 120 * 60), (4, 120 * 60)] {
            record_outcome(
                &mut source,
                Provider::Claude,
                Some(&throttled(None)),
                NOW,
                CADENCE,
            );
            assert_eq!(source.rate_limited, n);
            assert_eq!(source.not_before, Some(NOW + wait), "after {n}");
            source.last_attempt = Some(NOW);
            assert_eq!(gate(true, &source, NOW + wait - 1, CADENCE), Gate::Wait(1));
            assert_eq!(gate(true, &source, NOW + wait, CADENCE), Gate::Due);
        }
    }

    /// A 429 with no `Retry-After` used to leave the next read at the cadence,
    /// which is what kept spending requests.
    #[test]
    fn the_first_429_without_a_retry_after_already_waits_past_the_cadence() {
        let source = after_throttles(1, None);
        assert_eq!(source.not_before, Some(NOW + 2 * CADENCE));
    }

    /// Negative control: use `retry_after` instead of the larger of the two and
    /// the 60s case waits 60s; use `backoff` alone and the 3-cadence case waits 2.
    #[test]
    fn a_longer_retry_after_wins_and_a_shorter_one_shortens_nothing() {
        assert_eq!(
            after_throttles(1, Some(3 * CADENCE)).not_before,
            Some(NOW + 3 * CADENCE)
        );
        assert_eq!(
            after_throttles(1, Some(60)).not_before,
            Some(NOW + 2 * CADENCE),
            "the backoff is a floor"
        );
        assert_eq!(
            after_throttles(3, Some(60)).not_before,
            Some(NOW + 8 * CADENCE)
        );
        assert_eq!(
            after_throttles(3, Some(9 * CADENCE)).not_before,
            Some(NOW + 9 * CADENCE),
            "the service may ask for more than the cap"
        );
    }

    /// The log says which of the two set the wait. Negative control: log the
    /// `retry-after` line whenever one is sent and the 60s half fails.
    #[test]
    fn a_429_logs_its_wait_and_says_retry_after_only_when_that_is_what_set_it() {
        let p = Provider::Codex;
        let mut source = PlanSource::default();
        let lines = record_outcome(&mut source, p, Some(&throttled(Some(60))), NOW, CADENCE);
        assert_eq!(
            lines,
            [
                "plan usage: codex-plan read failed (rate-limited)",
                "plan usage: codex-plan backing off 1800s after 1 rate limit(s) in a row"
            ]
        );
        let lines = record_outcome(
            &mut source,
            p,
            Some(&throttled(Some(5 * CADENCE))),
            NOW,
            CADENCE,
        );
        assert_eq!(
            lines,
            [format!(
                "plan usage: codex-plan retry-after {}s",
                5 * CADENCE
            )],
            "an unchanged failure kind adds no `read failed` line"
        );
    }

    /// Negative control: leave `rate_limited` alone on success and the next 429
    /// waits 2h, not 30m.
    #[test]
    fn a_success_resets_the_count_and_the_wait() {
        let mut source = after_throttles(2, None);
        assert_eq!(source.rate_limited, 2);
        record_outcome(&mut source, Provider::Claude, None, NOW + 1, CADENCE);
        assert_eq!(source.rate_limited, 0);
        assert_eq!(source.not_before, None);
        record_outcome(
            &mut source,
            Provider::Claude,
            Some(&throttled(None)),
            NOW + 2,
            CADENCE,
        );
        assert_eq!(source.not_before, Some(NOW + 2 + 2 * CADENCE), "30m again");
    }

    /// Negative control: zero the count in the other-failure arm and the final
    /// 429 waits 1h, not 2h.
    #[test]
    fn other_failures_leave_the_count_and_wait_only_the_cadence() {
        let mut source = after_throttles(2, None);
        let unavailable = failure(PlanErrorKind::Http { status: 503 });
        record_outcome(
            &mut source,
            Provider::Claude,
            Some(&unavailable),
            NOW + 10,
            CADENCE,
        );
        assert_eq!(source.rate_limited, 2, "a 503 is not a success");
        assert_eq!(source.not_before, None, "and not a 429's backoff either");
        record_outcome(
            &mut source,
            Provider::Claude,
            Some(&throttled(None)),
            NOW + 20,
            CADENCE,
        );
        assert_eq!(source.rate_limited, 3);
        assert_eq!(source.not_before, Some(NOW + 20 + 8 * CADENCE));
    }

    /// A relaunch inside the wait waits. Negative controls: skip `restore` and
    /// the second launch reads at once; drop `not_before` from `Schedule` and it
    /// reads at the cadence, 15 minutes in.
    #[tokio::test]
    async fn a_relaunch_inside_a_429s_backoff_waits_it_out() {
        let dir = tempfile::tempdir().unwrap();
        let first = Mutex::new(UsageState::new());
        let (mut persistence, _) =
            SchedulePersistence::open(Some(dir.path().to_path_buf()), &first, NOW, CADENCE);
        let calls = Calls::new();
        pass(
            &first,
            Provider::Claude,
            true,
            NOW,
            &calls,
            Ok("token"),
            Err(PlanErrorKind::RateLimited {
                retry_after_secs: None,
            }),
        )
        .await;
        assert!(persistence.persist(&first).is_empty());

        // The app is quit and started again, a cadence later.
        let relaunched = Mutex::new(UsageState::new());
        let (_, lines) = SchedulePersistence::open(
            Some(dir.path().to_path_buf()),
            &relaunched,
            NOW + CADENCE,
            CADENCE,
        );
        assert!(lines.is_empty(), "{lines:?}");
        let calls = Calls::new();
        let inside = pass(
            &relaunched,
            Provider::Claude,
            true,
            NOW + CADENCE,
            &calls,
            Ok("token"),
            Ok(reading()),
        )
        .await;
        assert_eq!(inside, Gate::Wait(CADENCE), "30m from the 429, 15m gone");
        assert_eq!(calls.reads.load(Ordering::SeqCst), 0, "no sign-in read");
        assert_eq!(calls.fetches.load(Ordering::SeqCst), 0, "no request");
        {
            let state = relaunched.lock().unwrap();
            let claude = state.plan(Provider::Claude);
            assert!(claude.is_rate_limited(), "the 429 is remembered");
            assert_eq!(claude.rate_limited, 1);
            assert!(
                claude.state.last_error.is_some(),
                "and the header says so before any read"
            );
        }

        // Past the wait it reads, and a second launch with no file would have
        // read at once.
        let after = pass(
            &relaunched,
            Provider::Claude,
            true,
            NOW + 2 * CADENCE,
            &calls,
            Ok("token"),
            Ok(reading()),
        )
        .await;
        assert_eq!(after, Gate::Due);
        assert_eq!(calls.fetches.load(Ordering::SeqCst), 1);
        let fresh = Mutex::new(UsageState::new());
        assert_eq!(
            gate(
                true,
                fresh.lock().unwrap().plan(Provider::Claude),
                NOW,
                CADENCE
            ),
            Gate::Due
        );
    }

    #[tokio::test]
    async fn the_schedule_that_is_saved_holds_no_token_and_nothing_of_the_reply() {
        let dir = tempfile::tempdir().unwrap();
        let usage = Mutex::new(UsageState::new());
        pass(
            &usage,
            Provider::Claude,
            true,
            NOW,
            &Calls::new(),
            Ok("token"),
            Err(PlanErrorKind::Unreachable(
                "https://example.test/?t=SECRET".into(),
            )),
        )
        .await;
        subscription::schedule::save(dir.path(), &snapshot(&usage.lock().unwrap())).unwrap();
        let text =
            std::fs::read_to_string(dir.path().join(subscription::schedule::FILE_NAME)).unwrap();
        for forbidden in ["token", "SECRET", "example.test", "unreachable"] {
            assert!(!text.contains(forbidden), "{forbidden} in {text}");
        }
    }

    /// A provider that is off at launch drops what the file restored.
    #[tokio::test]
    async fn a_restored_schedule_for_a_provider_that_is_off_is_dropped() {
        let usage = Mutex::new(UsageState::new());
        let mut schedules = Schedules::default();
        schedules.set(
            Provider::Claude,
            Schedule {
                last_attempt: Some(NOW),
                rate_limited: 2,
                not_before: Some(NOW + 3_600),
                last_was_rate_limited: true,
            },
        );
        restore(&mut usage.lock().unwrap(), &schedules, NOW, CADENCE);
        let gate = pass(
            &usage,
            Provider::Claude,
            false,
            NOW,
            &Calls::new(),
            Ok("token"),
            Ok(reading()),
        )
        .await;
        assert_eq!(gate, Gate::Off);
        assert_eq!(
            snapshot(&usage.lock().unwrap()).get(Provider::Claude),
            Schedule::default()
        );
    }

    #[test]
    fn a_reset_forgets_the_last_failure_so_the_next_one_logs_again() {
        let mut source = PlanSource::default();
        let expired = failure(PlanErrorKind::SignInExpired);
        record_outcome(&mut source, Provider::Claude, Some(&expired), NOW, CADENCE);
        source.reset();
        assert_eq!(
            record_outcome(&mut source, Provider::Claude, Some(&expired), NOW, CADENCE).len(),
            1
        );
    }

    /// Runs `poll_logged` for Claude with a read that succeeds and a fetch of
    /// `fetched`; returns the lines the sink saw and whether the usage mutex
    /// was free while each was written.
    async fn logged_pass(
        usage: &Mutex<UsageState>,
        now: u64,
        fetched: Result<PlanUsage, PlanErrorKind>,
    ) -> (Vec<String>, Vec<bool>) {
        let mut lines = Vec::new();
        let mut free = Vec::new();
        poll_logged(
            usage,
            Provider::Claude,
            || true,
            now,
            CADENCE,
            || async { Ok("token") },
            |_| async { fetched.map_err(|kind| PlanUsageError::new(Provider::Claude, kind)) },
            |line| {
                free.push(usage.try_lock().is_ok());
                lines.push(line.to_owned());
            },
        )
        .await;
        (lines, free)
    }

    /// Negative controls: delete the sink loop and `lines` is empty; drop
    /// `record_outcome` from the `Err` arm and the line is missing; hold the
    /// `state` guard through the loop and `free` holds a `false`.
    #[tokio::test]
    async fn poll_logs_a_failure_transition_without_holding_the_usage_lock() {
        let usage = Mutex::new(UsageState::new());
        let (lines, free) = logged_pass(&usage, NOW, Err(PlanErrorKind::SignInExpired)).await;
        assert_eq!(
            lines,
            ["plan usage: claude-plan read failed (sign-in-expired)"]
        );
        assert_eq!(free, [true], "a line was written under the usage mutex");

        // Recovery is the second line `poll` can write; this one needs the
        // cadence to have passed.
        let (lines, free) = logged_pass(&usage, NOW + CADENCE, Ok(reading())).await;
        assert_eq!(
            lines,
            ["plan usage: claude-plan read ok again (was sign-in-expired)"]
        );
        assert_eq!(free, [true]);
    }

    /// A reading that lands after `reset()` is stale: no line, and
    /// `last_failure` is not touched. Negative controls: moving
    /// `record_outcome` above the generation check trips only the
    /// `last_failure` assertion (the stale path still returns before the sink
    /// loop); deleting the generation check entirely trips `lines.is_empty()`.
    #[tokio::test]
    async fn a_failure_that_lands_after_a_reset_logs_nothing_and_leaves_last_failure() {
        let usage = Mutex::new(UsageState::new());
        let mut lines = Vec::new();
        poll_logged(
            &usage,
            Provider::Claude,
            || true,
            NOW,
            CADENCE,
            || async { Ok("token") },
            |_| async {
                usage.lock().unwrap().plan_mut(Provider::Claude).reset();
                Err(PlanUsageError::new(
                    Provider::Claude,
                    PlanErrorKind::SignInExpired,
                ))
            },
            |line: &str| lines.push(line.to_owned()),
        )
        .await;
        assert!(lines.is_empty(), "{lines:?}");
        assert_eq!(
            usage.lock().unwrap().plan(Provider::Claude).last_failure,
            None
        );
    }

    /// Negative control: restore both times unchecked and the gate waits a
    /// year; drop only `not_before`'s check and it still waits on the other.
    #[tokio::test]
    async fn a_restored_wait_dated_impossibly_far_ahead_is_dropped_and_reads_at_once() {
        let year = 365 * 24 * 3_600;
        let usage = Mutex::new(UsageState::new());
        let mut schedules = Schedules::default();
        schedules.set(
            Provider::Claude,
            Schedule {
                last_attempt: Some(NOW + year),
                rate_limited: 3,
                not_before: Some(NOW + year),
                last_was_rate_limited: true,
            },
        );
        let lines = restore(&mut usage.lock().unwrap(), &schedules, NOW, CADENCE);
        assert_eq!(
            lines,
            ["plan usage: claude-plan saved wait was dated past any possible wait, dropped"]
        );
        {
            let state = usage.lock().unwrap();
            let claude = state.plan(Provider::Claude);
            assert_eq!(gate(true, claude, NOW, CADENCE), Gate::Due);
            assert_eq!(claude.rate_limited, 3, "the count is not a time");
        }
        let calls = Calls::new();
        let gate = pass(
            &usage,
            Provider::Claude,
            true,
            NOW,
            &calls,
            Ok("token"),
            Ok(reading()),
        )
        .await;
        assert_eq!(gate, Gate::Due);
        assert_eq!(calls.fetches.load(Ordering::SeqCst), 1);
    }

    /// Each field alone, and the edges: a wait up to the longest honest one is
    /// kept, one second past it is not.
    #[test]
    fn a_restored_time_is_kept_up_to_the_longest_honest_wait() {
        let longest = longest_wait_secs(CADENCE);
        assert_eq!(longest, 24 * 3_600, "a day beats 8 cadences at 15 minutes");
        assert_eq!(
            longest_wait_secs(4 * 3_600),
            32 * 3_600,
            "8 cadences beat a day at 4 hours"
        );
        let restored = |last_attempt, not_before| {
            let mut source = PlanSource::default();
            let dropped = source.restore(
                Provider::Claude,
                Schedule {
                    last_attempt,
                    not_before,
                    ..Schedule::default()
                },
                NOW,
                CADENCE,
            );
            (source.last_attempt, source.not_before, dropped)
        };
        assert_eq!(
            restored(Some(NOW), Some(NOW + longest)),
            (Some(NOW), Some(NOW + longest), false)
        );
        assert_eq!(restored(Some(NOW + 1), None), (None, None, true));
        assert_eq!(restored(None, Some(NOW + longest + 1)), (None, None, true));
        assert_eq!(restored(None, None), (None, None, false));
    }

    #[test]
    fn an_unreadable_schedule_file_is_said_once_and_an_absent_one_is_silent() {
        let dir = tempfile::tempdir().unwrap();
        let usage = Mutex::new(UsageState::new());
        let (_, lines) =
            SchedulePersistence::open(Some(dir.path().to_path_buf()), &usage, NOW, CADENCE);
        assert!(lines.is_empty(), "absent is silent: {lines:?}");
        std::fs::write(dir.path().join(subscription::schedule::FILE_NAME), "{oops").unwrap();
        let (_, lines) =
            SchedulePersistence::open(Some(dir.path().to_path_buf()), &usage, NOW, CADENCE);
        assert_eq!(
            lines,
            ["plan usage: the saved read schedule could not be read, starting fresh"]
        );
        assert_eq!(
            snapshot(&usage.lock().unwrap()),
            Schedules::default(),
            "and no wait is owed"
        );
    }

    /// The loop's load, restore, compare and save, and a failed save tried
    /// again on the next pass. Negative control: set `saved` before the write
    /// succeeds and the retry never happens.
    #[tokio::test]
    async fn the_schedule_is_saved_only_when_it_changed_and_a_failed_save_is_retried() {
        let dir = tempfile::tempdir().unwrap();
        // A file where the directory should be: creating it fails.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "x").unwrap();
        let target = blocker.join("data");
        let usage = Mutex::new(UsageState::new());
        let (mut persistence, lines) =
            SchedulePersistence::open(Some(target.clone()), &usage, NOW, CADENCE);
        assert_eq!(
            lines.len(),
            1,
            "a path that cannot be read is said once: {lines:?}"
        );
        assert!(persistence.persist(&usage).is_empty(), "nothing changed");

        pass(
            &usage,
            Provider::Claude,
            true,
            NOW,
            &Calls::new(),
            Ok("token"),
            Err(PlanErrorKind::RateLimited {
                retry_after_secs: None,
            }),
        )
        .await;
        assert_eq!(
            persistence.persist(&usage),
            ["plan usage: could not save the read schedule"]
        );
        assert!(
            persistence.persist(&usage).is_empty(),
            "still failing: said once, not every pass"
        );

        // The directory becomes writable: the next pass saves it and says so.
        std::fs::remove_file(&blocker).unwrap();
        assert_eq!(
            persistence.persist(&usage),
            ["plan usage: the read schedule is saving again"]
        );
        let loaded = subscription::schedule::load(&target);
        assert_eq!(loaded.claude.rate_limited, 1);
        assert!(loaded.claude.last_was_rate_limited);
        assert!(
            persistence.persist(&usage).is_empty(),
            "unchanged, no write"
        );
    }

    // ---- the status-line fallback ----------------------------------------

    const CLAUDE_ON: PlanInputs = PlanInputs {
        claude: true,
        ..PlanInputs::OFF
    };

    /// A status line written `age` ago with both windows open.
    fn status_line(age: u64) -> StatusLine {
        use subscription::statusline::Window;
        StatusLine {
            written_at: NOW - age,
            five_hour: Some(Window {
                used_percent: 23.5,
                resets_at: i64::try_from(NOW + 2 * 3600).ok(),
            }),
            seven_day: Some(Window {
                used_percent: 41.2,
                resets_at: i64::try_from(NOW + 3 * 86_400).ok(),
            }),
        }
    }

    /// Claude whose latest read was a 429, holding `line`.
    fn throttled_with(line: Option<StatusLine>) -> PlanSource {
        let mut source = PlanSource::default();
        source.state.begin();
        let error = failure(PlanErrorKind::RateLimited {
            retry_after_secs: None,
        });
        record_outcome(
            &mut source,
            Provider::Claude,
            Some(&error),
            NOW - 600,
            CADENCE,
        );
        source.state.failed(error.user_message());
        source.status_line = line;
        source
    }

    fn claude_section(source: &PlanSource) -> Value {
        view(Provider::Claude, source, CLAUDE_ON, NOW)
            .sections
            .remove(0)
    }

    fn labels(section: &Value) -> Vec<String> {
        section["metrics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["label"].as_str().unwrap().to_owned())
            .collect()
    }

    /// Negative control: drop the `is_rate_limited` guard in `section` and the
    /// 503 half shows the status line too.
    #[test]
    fn the_claude_card_shows_the_status_line_only_while_its_latest_read_was_a_429() {
        let throttled = throttled_with(Some(status_line(120)));
        let section = claude_section(&throttled);
        assert_eq!(
            labels(&section),
            ["5h", "Week"],
            "5h and Week, nothing else"
        );
        assert_eq!(section["metrics"][0]["value"], "24%");
        assert_eq!(section["metrics"][1]["value"], "41%");
        assert_eq!(section["label"], "Claude");

        // A 503 is not a 429: the figures held are not shown.
        let mut unavailable = PlanSource::default();
        record_outcome(
            &mut unavailable,
            Provider::Claude,
            Some(&failure(PlanErrorKind::Http { status: 503 })),
            NOW - 600,
            CADENCE,
        );
        unavailable.status_line = Some(status_line(120));
        let section = claude_section(&unavailable);
        assert_eq!(labels(&section), Vec::<String>::new());
        assert!(section.get("source").is_none());
    }

    /// The words are Rust's: where the figures come from, and how old.
    #[test]
    fn the_card_says_the_figures_are_claude_codes_and_how_old_and_the_header_still_warns() {
        let source = throttled_with(Some(status_line(12 * 60)));
        let plan = view(Provider::Claude, &source, CLAUDE_ON, NOW);
        let section = &plan.sections[0];
        let note = section["source"].as_str().unwrap();
        assert!(note.contains("Claude Code's status line"), "{note}");
        assert!(note.contains("written 12m ago"), "{note}");
        assert!(
            section["detail"].as_str().unwrap().ends_with(note),
            "the tile's detail carries it too"
        );
        let header = plan.warnings[0]["text"].as_str().unwrap();
        assert!(
            header.contains("rate-limit"),
            "the header still says: {header}"
        );
    }

    /// Negative control: stop dropping the held figures in `record_outcome`'s
    /// success path and the second half still shows the status line.
    #[test]
    fn a_successful_read_replaces_the_status_line_and_the_card_goes_back_to_the_plan() {
        let mut source = throttled_with(Some(status_line(120)));
        assert!(claude_section(&source).get("source").is_some());

        record_outcome(&mut source, Provider::Claude, None, NOW, CADENCE);
        source.state.succeeded(reading(), NOW, None);
        let section = claude_section(&source);
        assert!(section.get("source").is_none(), "no status-line label");
        assert_eq!(
            labels(&section),
            ["5h", "Week · Fable"],
            "the plan read's own figures"
        );
        assert_eq!(source.status_line, None);
    }

    /// Negative control: show a window whose `resets_at` has passed and the
    /// 5h meter appears.
    #[test]
    fn an_expired_window_is_not_shown_and_a_card_with_none_left_is_the_dash() {
        let mut line = status_line(120);
        line.five_hour.as_mut().unwrap().resets_at = i64::try_from(NOW - 1).ok();
        let section = claude_section(&throttled_with(Some(line)));
        assert_eq!(labels(&section), ["Week"]);

        line.seven_day.as_mut().unwrap().resets_at = i64::try_from(NOW).ok();
        let section = claude_section(&throttled_with(Some(line)));
        assert_eq!(labels(&section), Vec::<String>::new());
        assert_eq!(section["value"], vm::NO_READING_VALUE);
        assert!(section.get("source").is_none());
    }

    /// Absent is no meter, never 0%.
    #[test]
    fn an_absent_window_is_no_meter() {
        let mut line = status_line(120);
        line.five_hour = None;
        let section = claude_section(&throttled_with(Some(line)));
        assert_eq!(labels(&section), ["Week"]);
        assert!(!section.to_string().contains("\"0%\""));
        let section = claude_section(&throttled_with(None));
        assert_eq!(labels(&section), Vec::<String>::new());
        assert_eq!(section["value"], vm::NO_READING_VALUE);
    }

    /// The plan's name is kept when a reading had one, and a plan read that
    /// landed after the file was written is the fresher figure.
    #[test]
    fn the_plan_name_stays_and_a_newer_plan_read_outranks_an_older_file() {
        let mut source = throttled_with(Some(status_line(120)));
        source.state.summary = Some(PlanUsage {
            plan: Some("Ultra".into()),
            ..reading()
        });
        source.state.last_updated = Some(NOW - 3_600);
        assert_eq!(claude_section(&source)["label"], "Claude · Ultra");

        // The plan read was 1 minute ago; the file is 2 minutes old.
        source.state.last_updated = Some(NOW - 60);
        let section = claude_section(&source);
        assert!(section.get("source").is_none());
        assert_eq!(labels(&section), ["5h", "Week · Fable"]);
    }

    /// Codex has no status line, whatever it holds.
    #[test]
    fn codex_never_shows_a_status_line() {
        let mut source = PlanSource::default();
        source.state.begin();
        record_outcome(
            &mut source,
            Provider::Codex,
            Some(&PlanUsageError::new(
                Provider::Codex,
                PlanErrorKind::RateLimited {
                    retry_after_secs: None,
                },
            )),
            NOW - 600,
            CADENCE,
        );
        source.status_line = Some(status_line(60));
        let inputs = PlanInputs {
            codex: true,
            ..PlanInputs::OFF
        };
        let section = view(Provider::Codex, &source, inputs, NOW)
            .sections
            .remove(0);
        assert_eq!(labels(&section), Vec::<String>::new());
    }

    /// The file is read only while the opt-in is on and the latest read was a
    /// 429, and what was held is dropped otherwise.
    #[test]
    fn the_file_is_read_only_while_on_and_throttled_and_dropped_otherwise() {
        let dir = tempfile::tempdir().unwrap();
        let text = format!(
            "{{\"written_at\":{},\"rate_limits\":{{\"seven_day\":{{\"used_percentage\":41.2,\"resets_at\":{}}}}}}}",
            NOW - 30,
            NOW + 86_400
        );
        std::fs::write(dir.path().join(subscription::statusline::FILE_NAME), text).unwrap();
        let usage = Mutex::new(UsageState::new());
        usage
            .lock()
            .unwrap()
            .plan_mut(Provider::Claude)
            .state
            .begin();

        // Not throttled yet: nothing is read.
        refresh_status_line(&usage, true, Some(dir.path()), NOW);
        assert_eq!(
            usage.lock().unwrap().plan(Provider::Claude).status_line,
            None
        );

        let error = failure(PlanErrorKind::RateLimited {
            retry_after_secs: None,
        });
        record_outcome(
            usage.lock().unwrap().plan_mut(Provider::Claude),
            Provider::Claude,
            Some(&error),
            NOW,
            CADENCE,
        );
        refresh_status_line(&usage, true, Some(dir.path()), NOW);
        let held = usage.lock().unwrap().plan(Provider::Claude).status_line;
        assert_eq!(held.map(|line| line.written_at), Some(NOW - 30));

        // The opt-in turned off, a missing directory, a recovery: all drop it.
        refresh_status_line(&usage, false, Some(dir.path()), NOW);
        assert_eq!(
            usage.lock().unwrap().plan(Provider::Claude).status_line,
            None
        );
        refresh_status_line(&usage, true, Some(dir.path()), NOW);
        refresh_status_line(&usage, true, None, NOW);
        assert_eq!(
            usage.lock().unwrap().plan(Provider::Claude).status_line,
            None
        );
        refresh_status_line(&usage, true, Some(dir.path()), NOW);
        record_outcome(
            usage.lock().unwrap().plan_mut(Provider::Claude),
            Provider::Claude,
            None,
            NOW + 1,
            CADENCE,
        );
        assert_eq!(
            usage.lock().unwrap().plan(Provider::Claude).status_line,
            None
        );

        // Too old to use: not held.
        let stale = format!(
            "{{\"written_at\":{},\"rate_limits\":{{\"seven_day\":{{\"used_percentage\":41.2,\"resets_at\":{}}}}}}}",
            NOW - subscription::statusline::MAX_AGE_SECS - 1,
            NOW + 86_400
        );
        std::fs::write(dir.path().join(subscription::statusline::FILE_NAME), stale).unwrap();
        record_outcome(
            usage.lock().unwrap().plan_mut(Provider::Claude),
            Provider::Claude,
            Some(&error),
            NOW + 2,
            CADENCE,
        );
        refresh_status_line(&usage, true, Some(dir.path()), NOW);
        assert_eq!(
            usage.lock().unwrap().plan(Provider::Claude).status_line,
            None
        );
    }

    // ---- the gate --------------------------------------------------------

    #[test]
    fn the_gate_reads_nothing_while_off_or_parked_and_keeps_the_cadence() {
        let mut source = PlanSource::default();
        assert_eq!(gate(false, &source, NOW, CADENCE), Gate::Off);
        assert_eq!(gate(true, &source, NOW, CADENCE), Gate::Due);

        source.last_attempt = Some(NOW - 60);
        assert_eq!(gate(true, &source, NOW, CADENCE), Gate::Wait(CADENCE - 60));
        assert_eq!(gate(true, &source, NOW - 60 + CADENCE, CADENCE), Gate::Due);
        // Off outranks everything: a due provider that is off is still off.
        assert_eq!(gate(false, &source, NOW + CADENCE, CADENCE), Gate::Off);

        // A Retry-After past the cadence wins.
        source.not_before = Some(NOW + 3_600);
        assert_eq!(
            gate(true, &source, NOW + CADENCE, CADENCE),
            Gate::Wait(3_600 - CADENCE)
        );

        source.parked = true;
        assert_eq!(gate(true, &source, NOW + 7_200, CADENCE), Gate::Parked);
        assert_eq!(gate(false, &source, NOW + 7_200, CADENCE), Gate::Off);
    }

    /// **The opt-in gate.** A provider the operator has not turned on never
    /// has its credential read, and loses any reading it held. This is the
    /// test whose negative control is recorded on the change: remove the
    /// `Gate::Off` early return in [`poll`] and it fails, because the reader
    /// runs.
    #[tokio::test]
    async fn a_provider_that_is_off_never_reads_the_sign_in_and_drops_its_reading() {
        let usage = Mutex::new(UsageState::new());
        let calls = Calls::new();
        // Something to drop: a reading from when it was on.
        usage
            .lock()
            .unwrap()
            .plan_mut(Provider::Claude)
            .state
            .succeeded(reading(), NOW - 60, None);

        let mut gates = Vec::new();
        for provider in [Provider::Claude, Provider::Codex] {
            gates.push(
                pass(
                    &usage,
                    provider,
                    false,
                    NOW,
                    &calls,
                    Ok("token"),
                    Ok(reading()),
                )
                .await,
            );
        }
        assert_eq!(
            calls.reads.load(Ordering::SeqCst),
            0,
            "no sign-in read while off"
        );
        assert_eq!(calls.fetches.load(Ordering::SeqCst), 0);
        assert_eq!(gates, [Gate::Off, Gate::Off]);
        let state = usage.lock().unwrap();
        let claude = &state.plan(Provider::Claude).state;
        assert!(claude.configured.is_absent(), "off is Configured::Absent");
        assert!(
            claude.summary.is_none(),
            "a reading does not outlive the opt-in"
        );
    }

    #[tokio::test]
    async fn a_provider_that_is_on_reads_once_per_cadence() {
        let usage = Mutex::new(UsageState::new());
        let calls = Calls::new();
        assert_eq!(
            pass(
                &usage,
                Provider::Codex,
                true,
                NOW,
                &calls,
                Ok("token"),
                Ok(reading())
            )
            .await,
            Gate::Due
        );
        {
            let state = usage.lock().unwrap();
            let codex = &state.plan(Provider::Codex).state;
            assert_eq!(codex.summary, Some(reading()));
            assert_eq!(codex.last_updated, Some(NOW));
            assert_eq!(codex.last_error, None);
        }
        // Inside the cadence: nothing is read.
        assert_eq!(
            pass(
                &usage,
                Provider::Codex,
                true,
                NOW + 60,
                &calls,
                Ok("token"),
                Ok(reading())
            )
            .await,
            Gate::Wait(CADENCE - 60)
        );
        assert_eq!(calls.reads.load(Ordering::SeqCst), 1);
        assert_eq!(calls.fetches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_answer_with_no_windows_is_a_reading_that_says_so() {
        let usage = Mutex::new(UsageState::new());
        // A plan name alone is not a reading.
        let empty = PlanUsage {
            plan: Some("Ultra".into()),
            ..PlanUsage::default()
        };
        pass(
            &usage,
            Provider::Claude,
            true,
            NOW,
            &Calls::new(),
            Ok("token"),
            Ok(empty.clone()),
        )
        .await;
        let state = usage.lock().unwrap();
        let claude = &state.plan(Provider::Claude).state;
        assert_eq!(claude.summary, Some(empty));
        assert_eq!(claude.last_error.as_deref(), Some(NO_WINDOWS_MESSAGE));
    }

    /// A refusal is asked once. Turning the provider off and on again is the
    /// retry.
    #[tokio::test]
    async fn a_refused_keychain_parks_the_provider_until_it_is_reset() {
        let usage = Mutex::new(UsageState::new());
        let calls = Calls::new();
        pass(
            &usage,
            Provider::Claude,
            true,
            NOW,
            &calls,
            Err(PlanErrorKind::KeychainRefused),
            Ok(reading()),
        )
        .await;
        assert_eq!(
            calls.fetches.load(Ordering::SeqCst),
            0,
            "no token, no request"
        );
        let later = NOW + 10 * CADENCE;
        assert_eq!(
            pass(
                &usage,
                Provider::Claude,
                true,
                later,
                &calls,
                Ok("token"),
                Ok(reading())
            )
            .await,
            Gate::Parked
        );
        assert_eq!(
            calls.reads.load(Ordering::SeqCst),
            1,
            "a Deny is not asked again"
        );
        assert_eq!(
            usage
                .lock()
                .unwrap()
                .plan(Provider::Claude)
                .state
                .last_error,
            Some(
                PlanUsageError::new(Provider::Claude, PlanErrorKind::KeychainRefused)
                    .user_message()
            )
        );

        usage.lock().unwrap().plan_mut(Provider::Claude).reset();
        assert_eq!(
            pass(
                &usage,
                Provider::Claude,
                true,
                later,
                &calls,
                Ok("token"),
                Ok(reading())
            )
            .await,
            Gate::Due
        );
    }

    /// A failure keeps the last good reading, and an expired sign-in says so
    /// in its own words.
    #[tokio::test]
    async fn a_failure_keeps_the_last_reading_and_states_its_reason() {
        let usage = Mutex::new(UsageState::new());
        let calls = Calls::new();
        pass(
            &usage,
            Provider::Codex,
            true,
            NOW,
            &calls,
            Ok("token"),
            Ok(reading()),
        )
        .await;
        pass(
            &usage,
            Provider::Codex,
            true,
            NOW + CADENCE,
            &calls,
            Ok("token"),
            Err(PlanErrorKind::SignInExpired),
        )
        .await;
        let state = usage.lock().unwrap();
        let codex = &state.plan(Provider::Codex).state;
        assert_eq!(codex.summary, Some(reading()));
        assert_eq!(codex.last_updated, Some(NOW));
        assert_eq!(
            codex.last_error.as_deref(),
            Some("Codex's sign-in has expired — run it to refresh")
        );
    }

    #[tokio::test]
    async fn a_throttled_read_waits_out_the_retry_after() {
        let usage = Mutex::new(UsageState::new());
        let calls = Calls::new();
        pass(
            &usage,
            Provider::Claude,
            true,
            NOW,
            &calls,
            Ok("token"),
            Err(PlanErrorKind::RateLimited {
                retry_after_secs: Some(3 * CADENCE),
            }),
        )
        .await;
        assert_eq!(
            pass(
                &usage,
                Provider::Claude,
                true,
                NOW + CADENCE,
                &calls,
                Ok("token"),
                Ok(reading())
            )
            .await,
            Gate::Wait(2 * CADENCE)
        );
        assert_eq!(calls.fetches.load(Ordering::SeqCst), 1);
    }

    /// Turned off while a read was in flight: the result is discarded.
    #[tokio::test]
    async fn a_result_that_lands_after_the_provider_was_turned_off_is_dropped() {
        let usage = Mutex::new(UsageState::new());
        poll(
            &usage,
            Provider::Claude,
            || true,
            NOW,
            CADENCE,
            || async { Ok("token") },
            |_| async {
                usage.lock().unwrap().plan_mut(Provider::Claude).reset();
                Ok(reading())
            },
        )
        .await;
        let state = usage.lock().unwrap();
        let claude = &state.plan(Provider::Claude).state;
        assert!(claude.summary.is_none());
        assert!(claude.configured.is_absent());
    }

    /// The loop's shape: Claude first, then Codex. The operator turns Codex off
    /// while Claude's reader is still waiting (a keychain prompt can hold it for
    /// a minute). Codex's opt-in is asked at its own turn, so its sign-in is
    /// never read and nothing is sent.
    #[tokio::test]
    async fn a_provider_turned_off_during_the_other_ones_read_is_not_read() {
        let usage = Mutex::new(UsageState::new());
        let codex_on = std::sync::atomic::AtomicBool::new(true);
        poll(
            &usage,
            Provider::Claude,
            || true,
            NOW,
            CADENCE,
            || async {
                codex_on.store(false, Ordering::SeqCst);
                Ok("token")
            },
            |_| async { Ok(reading()) },
        )
        .await;
        let calls = Calls::new();
        let gate = poll(
            &usage,
            Provider::Codex,
            || codex_on.load(Ordering::SeqCst),
            NOW,
            CADENCE,
            || async {
                calls.reads.fetch_add(1, Ordering::SeqCst);
                Ok("token")
            },
            |_: &'static str| async {
                calls.fetches.fetch_add(1, Ordering::SeqCst);
                Ok(reading())
            },
        )
        .await;
        assert_eq!(gate, Gate::Off);
        assert_eq!(
            calls.reads.load(Ordering::SeqCst),
            0,
            "Codex's sign-in was read"
        );
        assert_eq!(calls.fetches.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn the_loop_sleeps_until_the_soonest_due_provider() {
        let mut claude = PlanSource::default();
        let mut codex = PlanSource::default();
        claude.last_attempt = Some(NOW - 100);
        codex.last_attempt = Some(NOW - 800);
        fn secs(sources: [(bool, &PlanSource); 2]) -> u64 {
            next_wake(sources, NOW, CADENCE).as_secs()
        }
        assert_eq!(secs([(true, &claude), (true, &codex)]), CADENCE - 800);
        // An off or parked provider does not set the alarm.
        assert_eq!(secs([(true, &claude), (false, &codex)]), CADENCE - 100);
        assert_eq!(secs([(false, &claude), (false, &codex)]), CADENCE);
        codex.last_attempt = Some(NOW - 5 * CADENCE);
        assert_eq!(
            secs([(true, &claude), (true, &codex)]),
            1,
            "never a zero-length sleep"
        );
    }

    // ---- the view --------------------------------------------------------

    fn on(claude: bool, codex: bool) -> PlanInputs {
        PlanInputs {
            claude,
            codex,
            cadence_secs: CADENCE,
            ..PlanInputs::OFF
        }
    }

    #[test]
    fn off_is_no_section_and_nothing_to_say() {
        let source = PlanSource::default();
        let off = view(Provider::Claude, &source, PlanInputs::OFF, NOW);
        assert!(off.sections.is_empty());
        assert!(!off.loading);
        assert!(off.warnings.iter().all(Value::is_null));
    }

    /// Each tile answers for its own provider.
    #[test]
    fn a_view_says_nothing_about_the_other_provider() {
        let source = PlanSource::default();
        let claude_on = on(true, false);
        let claude = view(Provider::Claude, &source, claude_on, NOW);
        assert_eq!(claude.sections.len(), 1);
        assert_eq!(
            view(Provider::Codex, &source, claude_on, NOW)
                .sections
                .len(),
            0
        );
    }

    /// A reading retained from before the toggle went off is not shown: the
    /// preference is read at render time.
    #[test]
    fn a_provider_switched_off_shows_nothing_of_its_last_reading() {
        let mut claude = PlanSource::default();
        claude.state.succeeded(reading(), NOW, None);
        let off = view(Provider::Claude, &claude, PlanInputs::OFF, NOW);
        assert!(off.sections.is_empty());
    }

    #[test]
    fn on_and_unanswered_reads_loading_not_a_dash() {
        let v = view(
            Provider::Claude,
            &PlanSource::default(),
            on(true, false),
            NOW,
        );
        assert!(v.loading);
        let plan = &v.sections[0];
        assert_eq!(plan["id"], CLAUDE_ID);
        assert_eq!(plan["label"], "Claude");
        assert_eq!(plan["value"], vm::LOADING_VALUE);
        assert_eq!(plan["metrics"], json!([]));
    }

    #[test]
    fn a_failure_before_any_reading_is_a_dash_with_its_reason_in_the_header() {
        let mut codex = PlanSource::default();
        codex
            .state
            .failed("Codex not found — install it and sign in".into());
        let v = view(Provider::Codex, &codex, on(false, true), NOW);
        assert!(!v.loading);
        assert_eq!(v.sections[0]["id"], CODEX_ID);
        assert_eq!(v.sections[0]["value"], vm::NO_READING_VALUE);
        assert_eq!(v.sections[0]["metrics"], json!([]));
        assert_eq!(
            v.warnings[0]["text"],
            "⚠ codex-plan: Codex not found — install it and sign in"
        );
    }

    /// One row per plan, one meter per window, the verdict the tightest.
    #[test]
    fn a_reading_is_one_plan_row_with_a_meter_per_window() {
        let mut claude = PlanSource::default();
        let mut max = reading();
        max.plan = Some("Max".into());
        claude.state.succeeded(max, NOW, None);
        let v = view(Provider::Claude, &claude, on(true, false), NOW);
        let plan = &v.sections[0];
        assert_eq!(plan["label"], "Claude · Max");
        // The Fable week: 93% with 4 of 7 days gone runs out in ~7h 13m.
        assert_eq!(plan["value"], "out in ~7h 13m");
        assert_eq!(plan["valueColor"], color::hex(color::AMBER));
        assert_eq!(plan["color"], color::hex(color::RED), "the worst meter");
        assert!(plan["detail"]
            .as_str()
            .unwrap()
            .starts_with("Week · Fable is at 93%"));

        let metrics = plan["metrics"].as_array().unwrap();
        assert_eq!(metrics.len(), 2);
        assert_eq!(metrics[0]["label"], "5h");
        assert_eq!(metrics[0]["value"], "42%");
        assert_eq!(metrics[0]["fraction"], 0.42);
        assert_eq!(metrics[0]["color"], color::hex(color::GREEN));
        assert_eq!(metrics[0]["note"], "resets in 2h 14m · on pace for 76%");
        let tick = metrics[0]["pace"].as_f64().unwrap();
        assert!((tick - 166.0 / 300.0).abs() < 1e-9, "{tick}");
        assert!(metrics[0].get("span").is_none());
        assert_eq!(metrics[1]["label"], "Week · Fable");
        assert_eq!(metrics[1]["color"], color::hex(color::RED));
        assert!(v.warnings[0].is_null(), "a healthy section raises nothing");
    }

    /// The pace is the reading's, not the clock's: the tick stays put as time
    /// passes, and past the stale edge there is no pace at all.
    #[test]
    fn pace_is_frozen_at_the_read_and_hidden_once_stale() {
        let mut claude = PlanSource::default();
        claude.state.succeeded(reading(), NOW, None);
        let later = view(Provider::Claude, &claude, on(true, false), NOW + CADENCE);
        let session = &later.sections[0]["metrics"][0];
        let tick = session["pace"].as_f64().unwrap();
        assert!((tick - 166.0 / 300.0).abs() < 1e-9, "{tick}");
        assert_eq!(session["note"], "resets in 1h 59m · on pace for 76%");

        let stale = view(
            Provider::Claude,
            &claude,
            on(true, false),
            NOW + 2 * CADENCE + 1,
        );
        let plan = &stale.sections[0];
        assert!(plan["metrics"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["pace"].is_null()));
        assert_eq!(plan["value"], "resets in 2d 23h");
        assert_eq!(plan["valueColor"], color::hex(color::MUTED));
    }

    /// An Enterprise seat is a budget: one full-width meter, no pace, and no
    /// "no windows" warning, because a spend is a reading.
    #[tokio::test]
    async fn a_budget_with_no_windows_is_a_reading_with_one_budget_meter() {
        let usage = Mutex::new(UsageState::new());
        pass(
            &usage,
            Provider::Claude,
            true,
            NOW,
            &Calls::new(),
            Ok("token"),
            Ok(enterprise()),
        )
        .await;
        let state = usage.lock().unwrap();
        let claude = state.plan(Provider::Claude);
        assert_eq!(claude.state.last_error, None);
        let v = view(Provider::Claude, claude, on(true, false), NOW);
        let plan = &v.sections[0];
        assert_eq!(plan["label"], "Claude · Enterprise");
        assert_eq!(plan["value"], "62% of budget");
        assert_eq!(plan["valueColor"], color::hex(color::GREEN));
        let budget = &plan["metrics"][0];
        assert_eq!(budget["label"], "Budget");
        assert_eq!(budget["value"], "$312 / $500 · 62%");
        assert_eq!(budget["span"], "full");
        assert!(budget["pace"].is_null());
        assert!(v.warnings[0].is_null());
    }

    /// An empty answer is the dash, and the header says why.
    #[test]
    fn an_empty_reading_is_a_dash_and_says_no_windows() {
        let mut codex = PlanSource::default();
        codex
            .state
            .succeeded(PlanUsage::default(), NOW, Some(NO_WINDOWS_MESSAGE.into()));
        let v = view(Provider::Codex, &codex, on(false, true), NOW);
        assert_eq!(v.sections[0]["value"], vm::NO_READING_VALUE);
        assert_eq!(v.sections[0]["metrics"], json!([]));
        assert!(v.warnings[0]["text"]
            .as_str()
            .unwrap()
            .contains(NO_WINDOWS_MESSAGE));
    }

    /// A Codex window with no stated length keeps its slot name, and has no pace.
    #[test]
    fn an_unstated_window_keeps_its_slot_label() {
        let mut codex = PlanSource::default();
        codex.state.succeeded(
            PlanUsage {
                plan: Some("plus".into()),
                windows: vec![PlanWindow {
                    span: WindowSpan::Unstated(Slot::Secondary),
                    scope: None,
                    used_percent: 5.0,
                    resets_at: None,
                }],
                spend: None,
            },
            NOW,
            None,
        );
        let v = view(Provider::Codex, &codex, on(false, true), NOW);
        assert_eq!(v.sections[0]["label"], "Codex · Plus");
        let metric = &v.sections[0]["metrics"][0];
        assert_eq!(metric["label"], "Secondary");
        assert_eq!(metric["value"], "5%");
        assert!(metric["pace"].is_null());
    }

    /// The staleness warning sits at twice the cadence, so a longer cadence
    /// does not warn permanently, and the clock is dated against the cadence.
    #[test]
    fn staleness_is_judged_against_the_plan_cadence() {
        let mut claude = PlanSource::default();
        claude.state.succeeded(reading(), NOW, None);
        let inside = view(
            Provider::Claude,
            &claude,
            on(true, false),
            NOW + 2 * CADENCE,
        );
        assert!(inside.warnings[0].is_null());
        let past = view(
            Provider::Claude,
            &claude,
            on(true, false),
            NOW + 2 * CADENCE + 1,
        );
        assert!(past.warnings[0]["text"]
            .as_str()
            .unwrap()
            .starts_with("⚠ claude-plan: stale"));
        assert_eq!(past.clocks.len(), 1);
    }
}
