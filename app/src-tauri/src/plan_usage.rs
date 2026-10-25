//! The Usage panel's **plan** rows: a Claude or Codex subscription's own limit
//! windows, read from the sign-in Claude Code or Codex already holds.
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
//!    provider so turning one on never spends a request on the other. A 429's
//!    `Retry-After` pushes that provider's next read out; a refused keychain
//!    **parks** Claude until the toggle is flipped, so a "Deny" is not asked
//!    again every 15 minutes.
//! 3. **The rows.** Words and colours come from `viewmodel::plan_usage`; this
//!    module only assembles them into the panel's payload.
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
//! account. Plan rows are labelled `CLAUDE PLAN (…)` / `CODEX PLAN (…)`, render
//! in their own block, and never mix with the rollups.

use std::future::Future;
use std::sync::Mutex;

use serde_json::{json, Value};
use subscription::{
    PlanErrorKind, PlanUsage, PlanUsageError, PlanWindow, Provider, Slot, WindowSpan,
    NO_WINDOWS_MESSAGE,
};
use viewmodel::color;
use viewmodel::freshness::Freshness;
use viewmodel::plan_usage as vm;

use crate::panel::{attributed_freshness_payload, attributed_status_footer, progress_bar};
use crate::usage::{ProviderState, UsageState};

/// The Claude plan section's id. Also its attribution in the header line.
pub const CLAUDE_ID: &str = "claude-plan";
/// The Codex plan section's id.
pub const CODEX_ID: &str = "codex-plan";

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

/// The provider's name as its rows are labelled.
const fn row_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "CLAUDE",
        Provider::Codex => "CODEX",
    }
}

/// The provider's name in the setup line.
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
    /// No read before this instant: the service's `Retry-After`.
    not_before: Option<u64>,
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

    /// Whether this provider is on and still waiting on its first answer.
    #[must_use]
    pub fn is_loading(&self, enabled: bool) -> bool {
        enabled && self.state.summary.is_none() && self.state.last_error.is_none()
    }
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
/// Returns the gate this pass acted on.
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
    match outcome {
        Ok(reading) => {
            source.not_before = None;
            let note = reading.is_empty().then(|| NO_WINDOWS_MESSAGE.to_owned());
            source.state.succeeded(reading, now, note);
        }
        Err(error) => {
            if error.kind == PlanErrorKind::KeychainRefused {
                source.parked = true;
            }
            source.not_before = error
                .retry_after_secs()
                .map(|secs| now.saturating_add(secs));
            source.state.failed(error.user_message());
        }
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

/// What the Usage panel reads at render time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanInputs {
    pub claude: bool,
    pub codex: bool,
    /// `PanelInterval::PlanUsage`, what each section's clock is dated against.
    pub cadence_secs: u64,
}

impl PlanInputs {
    /// Both providers off, as every store starts.
    pub const OFF: PlanInputs = PlanInputs {
        claude: false,
        codex: false,
        cadence_secs: 15 * 60,
    };

    #[must_use]
    pub const fn enabled(self, provider: Provider) -> bool {
        match provider {
            Provider::Claude => self.claude,
            Provider::Codex => self.codex,
        }
    }
}

/// The plan block's contribution to the Usage panel.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanView {
    /// One section per provider that is on, in provider order.
    pub sections: Vec<Value>,
    /// The single setup line naming every provider that is off, or `Null`.
    pub off_line: Value,
    /// Each section's header warning, for `merged_footer`.
    pub warnings: Vec<Value>,
    /// Each section's clock, for `merged_freshness`.
    pub clocks: Vec<Value>,
    /// Any provider that is on and has not answered yet.
    pub loading: bool,
}

/// The plan block, at render time `now`.
#[must_use]
pub fn view(claude: &PlanSource, codex: &PlanSource, inputs: PlanInputs, now: u64) -> PlanView {
    let mut plan = PlanView {
        sections: Vec::new(),
        off_line: Value::Null,
        warnings: Vec::new(),
        clocks: Vec::new(),
        loading: false,
    };
    let mut off = Vec::new();
    for (provider, source) in [(Provider::Claude, claude), (Provider::Codex, codex)] {
        let enabled = inputs.enabled(provider);
        if !enabled {
            off.push(plain_name(provider));
            continue;
        }
        let id = section_id(provider);
        plan.sections
            .push(json!({ "id": id, "rows": rows(provider, source, enabled, now) }));
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
        plan.loading |= source.is_loading(enabled);
    }
    if let Some(text) = vm::off_line(&off) {
        plan.off_line = json!({ "text": text, "color": color::hex(color::MUTED) });
    }
    plan
}

/// One provider's rows: one per window, or a single placeholder.
fn rows(provider: Provider, source: &PlanSource, enabled: bool, now: u64) -> Vec<Value> {
    let name = row_name(provider);
    let windows = source
        .state
        .summary
        .as_ref()
        .map(|reading| reading.windows.as_slice())
        .unwrap_or_default();
    if windows.is_empty() {
        return vec![row_json(&vm::placeholder_row(
            name,
            source.is_loading(enabled),
        ))];
    }
    let now = i64::try_from(now).unwrap_or(i64::MAX);
    windows
        .iter()
        .map(|window| row_json(&vm::row(name, &mirror(window), now)))
        .collect()
}

fn mirror(window: &PlanWindow) -> vm::Window {
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

/// A row as the panel's other rows are shaped, plus its own `bar`.
fn row_json(row: &vm::Row) -> Value {
    json!({
        "label": row.label,
        "value": row.value,
        "valueColor": color::hex(row.color),
        "bar": row.bar.map_or(Value::Null, |fraction| {
            progress_bar(fraction, vm::BAR_AMBER_AT, vm::BAR_RED_AT)
        }),
    })
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
        let empty = PlanUsage { windows: vec![] };
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
        }
    }

    #[test]
    fn off_is_one_setup_line_and_no_section() {
        let off = view(
            &PlanSource::default(),
            &PlanSource::default(),
            PlanInputs::OFF,
            NOW,
        );
        assert!(off.sections.is_empty());
        assert_eq!(
            off.off_line["text"],
            "Claude and Codex plan limits are off — turn on in Settings"
        );
        assert_eq!(off.off_line["color"], color::hex(color::MUTED));
        assert!(!off.loading);
        assert!(off.warnings.iter().all(Value::is_null));

        let codex_off = view(
            &PlanSource::default(),
            &PlanSource::default(),
            on(true, false),
            NOW,
        );
        assert_eq!(codex_off.sections.len(), 1);
        assert_eq!(
            codex_off.off_line["text"],
            "Codex plan limits are off — turn on in Settings"
        );
        assert!(view(
            &PlanSource::default(),
            &PlanSource::default(),
            on(true, true),
            NOW
        )
        .off_line
        .is_null());
    }

    /// A reading retained from before the toggle went off is not shown: the
    /// preference is read at render time.
    #[test]
    fn a_provider_switched_off_shows_nothing_of_its_last_reading() {
        let mut claude = PlanSource::default();
        claude.state.succeeded(reading(), NOW, None);
        let off = view(&claude, &PlanSource::default(), PlanInputs::OFF, NOW);
        assert!(off.sections.is_empty());
    }

    #[test]
    fn on_and_unanswered_reads_loading_not_a_dash() {
        let v = view(
            &PlanSource::default(),
            &PlanSource::default(),
            on(true, false),
            NOW,
        );
        assert!(v.loading);
        assert_eq!(v.sections[0]["id"], CLAUDE_ID);
        let row = &v.sections[0]["rows"][0];
        assert_eq!(row["label"], "CLAUDE PLAN");
        assert_eq!(row["value"], vm::LOADING_VALUE);
        assert!(row["bar"].is_null());
    }

    #[test]
    fn a_failure_before_any_reading_is_a_dash_with_its_reason_in_the_header() {
        let mut codex = PlanSource::default();
        codex
            .state
            .failed("Codex not found — install it and sign in".into());
        let v = view(&PlanSource::default(), &codex, on(false, true), NOW);
        assert!(!v.loading);
        assert_eq!(v.sections[0]["rows"][0]["value"], "—");
        assert_eq!(
            v.warnings[0]["text"],
            "⚠ codex-plan: Codex not found — install it and sign in"
        );
    }

    #[test]
    fn a_reading_renders_one_row_per_window_with_a_meter_coloured_bar() {
        let mut claude = PlanSource::default();
        claude.state.succeeded(reading(), NOW, None);
        let v = view(&claude, &PlanSource::default(), on(true, false), NOW);
        let rows = v.sections[0]["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["label"], "CLAUDE PLAN (5H)");
        assert_eq!(rows[0]["value"], "42% · resets in 2h 14m");
        assert_eq!(rows[0]["valueColor"], color::hex(color::GREEN));
        assert_eq!(rows[0]["bar"]["fraction"], 0.42);
        assert_eq!(rows[1]["label"], "CLAUDE PLAN (WEEK · FABLE)");
        assert_eq!(rows[1]["valueColor"], color::hex(color::RED));
        assert_eq!(rows[1]["bar"]["color"], color::hex(color::RED));
        assert!(v.warnings[0].is_null(), "a healthy section raises nothing");
    }

    /// An empty answer is the dash, and the header says why.
    #[test]
    fn an_empty_reading_is_a_dash_and_says_no_windows() {
        let mut codex = PlanSource::default();
        codex.state.succeeded(
            PlanUsage { windows: vec![] },
            NOW,
            Some(NO_WINDOWS_MESSAGE.into()),
        );
        let v = view(&PlanSource::default(), &codex, on(false, true), NOW);
        assert_eq!(v.sections[0]["rows"][0]["value"], "—");
        assert!(v.warnings[0]["text"]
            .as_str()
            .unwrap()
            .contains(NO_WINDOWS_MESSAGE));
    }

    /// A Codex window with no stated length keeps its slot name.
    #[test]
    fn an_unstated_window_keeps_its_slot_label() {
        let mut codex = PlanSource::default();
        codex.state.succeeded(
            PlanUsage {
                windows: vec![PlanWindow {
                    span: WindowSpan::Unstated(Slot::Secondary),
                    scope: None,
                    used_percent: 5.0,
                    resets_at: None,
                }],
            },
            NOW,
            None,
        );
        let v = view(&PlanSource::default(), &codex, on(false, true), NOW);
        assert_eq!(v.sections[0]["rows"][0]["label"], "CODEX PLAN (SECONDARY)");
        assert_eq!(v.sections[0]["rows"][0]["value"], "5%");
    }

    /// The staleness warning sits at twice the cadence, so a longer cadence
    /// does not warn permanently, and the clock is dated against the cadence.
    #[test]
    fn staleness_is_judged_against_the_plan_cadence() {
        let mut claude = PlanSource::default();
        claude.state.succeeded(reading(), NOW, None);
        let inside = view(
            &claude,
            &PlanSource::default(),
            on(true, false),
            NOW + 2 * CADENCE,
        );
        assert!(inside.warnings[0].is_null());
        let past = view(
            &claude,
            &PlanSource::default(),
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
