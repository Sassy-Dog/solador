//! Opt-in, off-by-default crash reporting.
//!
//! `CLAUDE.md` commits to "No telemetry or analytics by default", and #18
//! settled the policy for the integration that used to live in the deleted
//! macOS app: **opt-in, off by default**. This crate is the Tauri port of that
//! decision, and the toggle is the opt-in — nothing else is.
//!
//! Not to be confused with `crates/usage/src/sentry.rs`, which shares only a
//! vendor name: that one *reads* Sentry's REST API for the Usage and Sentry
//! Crons panels, using an `org:read` token. It reports nothing. This crate is
//! the only place in the workspace that carries the Sentry **SDK**.
//!
//! # The gate
//!
//! [`decide`] is the whole permission model, and it is a positive check.
//! Reporting starts only when the operator has explicitly turned it on **and**
//! this build carries a DSN **and** that DSN parses. Every other combination —
//! including the one where the opt-in state could not be read at all — resolves
//! to [`Decision::Disabled`], and a `Disabled` decision never calls
//! `sentry::init`, so no client exists, no panic hook is installed, and no
//! network code is reachable. There is no path that falls back to sending.
//!
//! # Turning it off
//!
//! Off takes effect **immediately**: [`Reporting::set_consent`] flips a flag
//! that `before_send` reads on every event, so revoking consent stops reports
//! within the same session. Turning it *on* takes effect on the next launch,
//! because `sentry::init` binds a client to the hub of the thread that calls it
//! and the Settings command runs on a worker thread. That asymmetry is the
//! right way round — the direction that needs to be instant is the one that
//! withdraws permission — and [`Reporting::needs_restart`] exists so the UI can
//! say which state it is actually in instead of implying the toggle did more
//! than it did.
//!
//! # The DSN
//!
//! [`BUILD_DSN`] is read from the environment **at compile time**, which is
//! what #307 wires for the release build. A `SENTRY_DSN` in the operator's
//! runtime environment is deliberately *not* consulted: the SDK's own
//! `apply_defaults` would happily take one, and a shell variable that can
//! redirect this app's crash reports to a third party is not something to leave
//! lying around. No DSN is the common case for a local build, and it is a
//! silent no-op — not an error, not a warning on every launch.

mod transport;

pub mod scrub;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use sentry::types::Dsn;
use sentry::ClientOptions;

/// The DSN compiled into this build, if any.
///
/// `None` on a clean clone, and on every local build that has not exported
/// `SENTRY_DSN` — which is the documented common case. `build.rs` declares the
/// rerun dependency so changing the variable actually rebuilds this crate.
pub const BUILD_DSN: Option<&str> = option_env!("SENTRY_DSN");

/// Whether the operator has turned crash reporting on.
///
/// Three states, not a `bool`, for the reason `panel::Configured` is three
/// states: "we have not been able to look" is a real answer and it is not the
/// same as "they said no". Both refuse to report, but only one of them is a
/// preference, and a defaulted state is as much a fabrication as a defaulted
/// number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OptIn {
    /// The store could not be read. Reports nothing, and says so.
    #[default]
    Unknown,
    /// Read, and off — the default for a fresh store, and for every store
    /// written before this feature existed.
    Declined,
    /// Read, and explicitly on. The only value that permits a report.
    Granted,
}

/// Why reporting is not running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisabledReason {
    /// The operator has not turned it on. The default, and the common case.
    OptedOut,
    /// The opt-in state could not be read, so consent cannot be assumed.
    ConsentUnknown,
    /// Turned on, but this build carries no DSN. A silent no-op: local builds
    /// are like this, and there is nothing wrong.
    NoDsn,
    /// Turned on, with a DSN this build could not parse. Reported once at
    /// startup, because unlike the case above it is a mistake someone made.
    UnusableDsn,
}

impl DisabledReason {
    /// One line, in the app's voice, for the log and for Settings.
    #[must_use]
    pub const fn user_message(self) -> &'static str {
        match self {
            DisabledReason::OptedOut => "Crash reporting is off.",
            DisabledReason::ConsentUnknown => {
                "Crash reporting is off: the setting could not be read, so nothing is sent."
            }
            DisabledReason::NoDsn => {
                "Crash reporting is on, but this build carries no reporting destination, so nothing is sent."
            }
            DisabledReason::UnusableDsn => {
                "Crash reporting is on, but this build's reporting destination could not be read, so nothing is sent."
            }
        }
    }
}

/// What [`decide`] resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Report, to this destination.
    Enabled(Box<Dsn>),
    /// Do not report, for this reason.
    Disabled(DisabledReason),
}

impl Decision {
    /// Whether this decision permits a report. The only place that answers it.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        matches!(self, Decision::Enabled(_))
    }
}

/// Resolve the opt-in state and the build's DSN into one verdict.
///
/// The order of the checks is the argument: consent is asked first, so a build
/// with no DSN and no consent reports `OptedOut` rather than `NoDsn` — the
/// operator's answer is the reason, and the missing DSN is incidental. Pure,
/// so the gate is testable without a client, a network or a store.
#[must_use]
pub fn decide(opt_in: OptIn, dsn: Option<&str>) -> Decision {
    match opt_in {
        OptIn::Declined => return Decision::Disabled(DisabledReason::OptedOut),
        OptIn::Unknown => return Decision::Disabled(DisabledReason::ConsentUnknown),
        OptIn::Granted => {}
    }
    // An empty or whitespace-only value is "unset spelled badly", not a
    // destination — CI and shell scripts export empty strings all the time.
    let Some(dsn) = dsn.map(str::trim).filter(|dsn| !dsn.is_empty()) else {
        return Decision::Disabled(DisabledReason::NoDsn);
    };
    dsn.parse::<Dsn>()
        .map_or(Decision::Disabled(DisabledReason::UnusableDsn), |dsn| {
            Decision::Enabled(Box::new(dsn))
        })
}

/// Everything [`start`] needs, so nothing is read from ambient state.
#[derive(Debug, Clone, Copy)]
pub struct Config<'a> {
    /// The operator's answer, read from the store.
    pub opt_in: OptIn,
    /// The DSN this build carries — [`BUILD_DSN`] in the app, an explicit value
    /// in tests. Never read from the runtime environment.
    pub dsn: Option<&'a str>,
    /// The version this build reports itself as, or `None` when it could not be
    /// derived (a shallow clone, or no git at all — see `app/src-tauri/build.rs`).
    ///
    /// `None` attaches **no** release to the event rather than a placeholder.
    /// Sentry groups and regresses issues by release, so a stand-in string does
    /// not degrade gracefully here: every build that could not name itself
    /// would share one release, and a crash fixed in one would read as
    /// regressed by the next.
    pub release: Option<&'a str>,
    /// `debug` or `release`; never a hostname, a user or a machine name.
    pub environment: &'a str,
}

/// A live (or deliberately dead) reporter.
///
/// Holding this value is what keeps the client alive; dropping it flushes and
/// closes. When the decision was [`Decision::Disabled`] it holds no client, and
/// every method still answers honestly.
pub struct Reporting {
    decision: Decision,
    /// Read by `before_send` on every event. An `Arc` rather than a global so
    /// the gate is a value someone owns, and so tests can hold two.
    consent: Arc<AtomicBool>,
    guard: Option<sentry::ClientInitGuard>,
}

impl std::fmt::Debug for Reporting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reporting")
            .field("decision", &self.decision)
            .field("consent", &self.consent.load(Ordering::SeqCst))
            .field("client", &self.guard.is_some())
            .finish()
    }
}

impl Reporting {
    /// What was resolved at startup.
    #[must_use]
    pub const fn decision(&self) -> &Decision {
        &self.decision
    }

    /// Whether a report would actually be sent right now.
    ///
    /// Both halves have to hold: a client exists (so the decision permitted
    /// one), and consent has not since been withdrawn.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.guard.is_some() && self.consent.load(Ordering::SeqCst)
    }

    /// Whether the operator has turned reporting on in a session that started
    /// with it off — the one case where the toggle needs a restart.
    #[must_use]
    pub fn needs_restart(&self) -> bool {
        self.guard.is_none() && self.consent.load(Ordering::SeqCst)
    }

    /// Apply a change made in Settings.
    ///
    /// `false` takes effect immediately, for every event, whether or not a
    /// client exists. `true` records the consent but cannot conjure a client
    /// that was never initialised — see [`Reporting::needs_restart`].
    pub fn set_consent(&self, granted: bool) {
        self.consent.store(granted, Ordering::SeqCst);
    }

    /// One honest sentence about the current state, for the log and for the
    /// Settings surface. Never claims more than is true.
    #[must_use]
    pub fn user_message(&self) -> &'static str {
        match &self.decision {
            Decision::Disabled(reason) => reason.user_message(),
            Decision::Enabled(_) if self.consent.load(Ordering::SeqCst) => {
                "Crash reporting is on. Panics are scrubbed and sent."
            }
            Decision::Enabled(_) => "Crash reporting is off.",
        }
    }
}

/// Resolve [`Config`] and, only if it permits, start the SDK.
///
/// # Panic-hook ordering
///
/// `sentry`'s panic integration installs the hook from inside `sentry::init`,
/// and it **chains**: it calls `panic::take_hook()` and re-installs a hook that
/// captures the event and then calls the previous one, so the standard
/// "thread panicked at …" message still reaches stderr. Nothing else in this
/// app's stack competes for it — `tauri`, `wry` and `tao` install no panic hook
/// at all (`tao` uses `catch_unwind` around its macOS event-loop callback,
/// which runs *after* the hook, since hooks fire at panic time and before
/// unwinding). Calling this from `main` before `tauri::Builder::run` therefore
/// puts the hook in place ahead of every panic the app can produce, and nothing
/// replaces it afterwards.
///
/// The one gap is honest and unavoidable: panics *before* this call — the store
/// opening, the credential migrations — are not captured, because the opt-in
/// state has to be read before there is permission to report anything.
#[must_use]
pub fn start(config: Config<'_>) -> Reporting {
    start_with_transport(config, Arc::new(transport::HttpTransportFactory))
}

/// [`start`], sending through `transport` rather than the built-in one.
///
/// The gate and the scrubber are untouched — this swaps only what carries the
/// bytes, and it runs *after* `before_send`, so a transport cannot see anything
/// the allow-list did not already pass. It exists because the acceptance item
/// "a forced panic produces an event" has to be provable, and **no test in this
/// repo may send an event to a real Sentry project**: the test supplies a
/// recording transport and asserts against what the real SDK, the real panic
/// hook and the real scrubber handed it.
#[must_use]
pub fn start_with_transport(
    config: Config<'_>,
    transport: Arc<dyn sentry::TransportFactory>,
) -> Reporting {
    let decision = decide(config.opt_in, config.dsn);
    let consent = Arc::new(AtomicBool::new(config.opt_in == OptIn::Granted));

    let Decision::Enabled(dsn) = &decision else {
        return Reporting {
            decision,
            consent,
            guard: None,
        };
    };

    let before_send_consent = Arc::clone(&consent);
    // `ClientOptions` is `#[non_exhaustive]`, so this cannot be a struct
    // literal the way the scrubber's allow-list is. Every field that matters is
    // therefore set explicitly below rather than left to a default that could
    // move under us.
    let mut options = ClientOptions::default();
    options.dsn = Some((**dsn).clone());
    options.release = config.release.map(|r| r.to_owned().into());
    options.environment = Some(config.environment.to_owned().into());
    // Belt and braces around the scrubber: nothing should be collecting PII in
    // the first place, and the structural allow-list drops it anyway.
    options.send_default_pii = false;
    // Breadcrumbs are refused three times over: never collected (0), never
    // recorded (`before_breadcrumb`), never copied (`scrub_event`).
    options.max_breadcrumbs = 0;
    options.before_breadcrumb = Some(Arc::new(|_| None));
    options.attach_stacktrace = true;
    // The hostname field, explicitly empty. The `contexts` feature that would
    // fill it is not compiled in.
    options.server_name = None;
    options.before_send = Some(Arc::new(move |event| {
        // The gate, again, on the event path. `set_consent(false)` has to stop
        // reports in the session it is called in, and this is where that
        // happens — before any scrubbing, so a withdrawn consent costs nothing
        // and cannot be undone by a bug further down.
        if !before_send_consent.load(Ordering::SeqCst) {
            return None;
        }
        Some(scrub::scrub_event(event))
    }));
    // Ours, over the workspace's reqwest. See `transport.rs`.
    options.transport = Some(transport);

    let guard = sentry::init(options);
    Reporting {
        decision,
        consent,
        guard: Some(guard),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A DSN shape, not a destination. Nothing in this repo may hold a real one
    /// and no test may reach a real project.
    const TEST_DSN: &str = "https://0123456789abcdef0123456789abcdef@example.invalid/42";

    #[test]
    fn a_fresh_store_reports_nothing() {
        // The acceptance item, at the gate: the default `OptIn` and a store
        // that has never been configured both refuse, DSN or no DSN.
        assert_eq!(
            decide(OptIn::default(), Some(TEST_DSN)),
            Decision::Disabled(DisabledReason::ConsentUnknown)
        );
        assert_eq!(
            decide(OptIn::Declined, Some(TEST_DSN)),
            Decision::Disabled(DisabledReason::OptedOut)
        );
        assert!(!decide(OptIn::Declined, Some(TEST_DSN)).is_enabled());
    }

    #[test]
    fn consent_is_asked_before_the_dsn() {
        // Not cosmetic: if the DSN were checked first, an opted-out operator on
        // a local build would be told "no DSN", which reads as "turn it on and
        // it will work" rather than "you turned it off".
        assert_eq!(
            decide(OptIn::Declined, None),
            Decision::Disabled(DisabledReason::OptedOut)
        );
    }

    #[test]
    fn opting_in_without_a_dsn_is_a_no_op_not_an_error() {
        for absent in [None, Some(""), Some("   ")] {
            assert_eq!(
                decide(OptIn::Granted, absent),
                Decision::Disabled(DisabledReason::NoDsn),
                "an absent DSN must resolve to a silent no-op, not a failure"
            );
        }
    }

    #[test]
    fn an_unparseable_dsn_does_not_report() {
        assert_eq!(
            decide(OptIn::Granted, Some("not a dsn")),
            Decision::Disabled(DisabledReason::UnusableDsn)
        );
    }

    #[test]
    fn only_an_explicit_yes_with_a_usable_dsn_reports() {
        assert!(decide(OptIn::Granted, Some(TEST_DSN)).is_enabled());
    }

    #[test]
    fn a_disabled_reporter_holds_no_client_and_says_so() {
        let reporting = start(Config {
            opt_in: OptIn::Declined,
            dsn: Some(TEST_DSN),
            release: Some("test"),
            environment: "test",
        });
        assert!(!reporting.is_active());
        assert!(!reporting.needs_restart());
        assert_eq!(reporting.user_message(), "Crash reporting is off.");

        // Turning it on mid-session records the consent but cannot report:
        // there is no client, and the UI has to be able to say that.
        reporting.set_consent(true);
        assert!(!reporting.is_active());
        assert!(reporting.needs_restart());
    }

    #[test]
    fn no_dsn_reports_nothing_and_does_not_error() {
        let reporting = start(Config {
            opt_in: OptIn::Granted,
            dsn: None,
            release: Some("test"),
            environment: "test",
        });
        assert_eq!(
            reporting.decision(),
            &Decision::Disabled(DisabledReason::NoDsn)
        );
        assert!(!reporting.is_active());
    }
}
