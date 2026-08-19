//! The in-app updater's state, its Settings surface, and the one banner it may
//! show.
//!
//! # Everything here is pure
//!
//! The plugin call lives in `main.rs`, where the `AppHandle` is; this module
//! holds what the app learned from it. That split is the same one
//! `github::notify` makes and for the same reason — the rules about *what the
//! operator is told* have to be testable without a network, a release, or a
//! notification centre.
//!
//! # The ACL is untouched, deliberately
//!
//! `tauri-plugin-updater` is granted **nothing** in
//! `capabilities/default.json`, exactly like `tauri-plugin-notification`: every
//! call into it is made from Rust ([`crate::run_update_check`],
//! [`crate::run_update_install`]), so the webview never invokes it and needs no
//! permission to. The frontend's whole share of this feature is three
//! app-defined commands, which Tauri's ACL always permits. An `updater:default`
//! grant would hand the one seam with no automated coverage (#123) the ability
//! to download and execute code.
//!
//! # One banner per version, per process
//!
//! The launch check is the only automatic one, so at most one banner can fire
//! per launch anyway — but a manual re-check must not re-announce something
//! already announced. [`UpdateState::notice`] is the ledger, the same shape as
//! `ApprovalWatch`: it decides, `main.rs` delivers.
//!
//! # Some builds never get that far
//!
//! [`not_updatable`] is asked before the plugin is reached at all (#353). It
//! supplies the one fact `viewmodel::update::not_updatable` cannot know — which
//! cargo profile this is — and its answer is the whole of this module's share
//! of that decision; the rule, the sentence and the colour are all in
//! `viewmodel`.

use serde_json::{json, Value};
use viewmodel::color;
use viewmodel::update::{self, Check, Install, NotUpdatable, Provenance};

/// The Settings group's heading.
pub const HEADING: &str = "Updates";

/// The manual re-check button.
pub const CHECK_LABEL: &str = "Check for updates";

/// The policy, stated on screen rather than left to be discovered.
///
/// Two promises, both of which the code keeps: the check happens once at
/// launch, and nothing installs itself. A cockpit that sits full-screen on a
/// second monitor for days must not restart itself, so even a downloaded update
/// waits for a relaunch the operator chooses.
pub const HELP: &str = "Solador checks once when it starts. Nothing is downloaded or installed until you ask, and an installed update takes effect the next time you open the app.";

/// Said when the plugin cannot be reached because the app is not up yet.
///
/// Only reachable if a check is ever started before `setup` has handed over the
/// `AppHandle`. It is recorded as a *failure* rather than skipped, because "we
/// never looked" is not "there is nothing there" — and it is deliberately not
/// [`NotUpdatable`], which is a statement about the build rather than about the
/// moment.
pub const NO_HANDLE: &str = "the app is not up yet";

/// Why this build is outside the update path, or `None` when it is inside it.
///
/// The rule is [`update::not_updatable`]; this supplies the one fact only the
/// shell can know. `cfg!(debug_assertions)` is that fact: `./dev` and `./dev
/// run --tauri` build the debug profile, and the signed bundle an update is
/// unpacked over only ever comes off `cargo tauri build --release`.
///
/// It is a compile-time constant rather than something sniffed off disk at
/// runtime, and that choice has a direction. Deriving it from the running
/// `.app`'s path, or from whether `package_info().version` still reads
/// `0.1.0`, would misclassify a *shipped* build the first time either
/// assumption broke — and a shipped build misread as development stops updates
/// reaching users silently, which is a far worse failure than the launch-time
/// nag being fixed here. So the ambiguous cases fall towards checking. The one
/// build this does not catch is `./dev run --release`, a release-profile build
/// made at the operator's own request; [`update::is_newer`] is what keeps that
/// one honest, by comparing the CalVer it actually is.
#[must_use]
pub fn not_updatable(current: Option<&str>) -> Option<NotUpdatable> {
    let provenance = if cfg!(debug_assertions) {
        Provenance::Development
    } else {
        Provenance::Released
    };
    update::not_updatable(current, provenance)
}

/// Everything the Updates group renders from, plus the banner ledger.
#[derive(Debug, Default)]
pub struct UpdateState {
    check: Check,
    install: Install,
    /// The version already announced by a desktop banner, if any. Not a `bool`:
    /// a second, newer version found by a manual re-check is a second thing
    /// worth saying.
    announced: Option<String>,
}

impl UpdateState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A check is in flight.
    ///
    /// Returns the check to [`Check::Unknown`], which is the honest state: what
    /// the last check found is no longer what we are claiming, and the panel
    /// renders "checking…" rather than a stale verdict beside a spinner.
    pub fn checking(&mut self) {
        self.check = Check::Unknown;
    }

    /// A check completed and the feed offers something newer.
    pub fn found(&mut self, version: String, notes: Option<String>) {
        self.check = Check::Available { version, notes };
    }

    /// A check completed and there is nothing newer.
    ///
    /// This also clears any install outcome: an applied install that the
    /// operator has since relaunched into shows up here as "up to date", and
    /// leaving the old "installed, relaunch to apply" line beside it would be
    /// describing a relaunch that already happened.
    pub fn up_to_date(&mut self) {
        self.check = Check::UpToDate;
        if matches!(self.install, Install::Applied) {
            self.install = Install::Idle;
        }
    }

    /// A check ran and failed. **Never** folded into [`up_to_date`](Self::up_to_date).
    pub fn check_failed(&mut self, reason: String) {
        self.check = Check::Failed { reason };
    }

    /// No check will run: this build is not one a release can replace (#353).
    ///
    /// Set *instead of* [`checking`](Self::checking), never after it. Nothing
    /// is in flight, and "Checking for updates…" beside a check that will never
    /// happen is the stale-verdict problem in the other direction — a sentence
    /// promising an answer that is not coming.
    pub fn not_updatable(&mut self, why: NotUpdatable) {
        self.check = Check::NotUpdatable(why);
    }

    pub fn installing(&mut self) {
        self.install = Install::Running;
    }

    pub fn installed(&mut self) {
        self.install = Install::Applied;
    }

    pub fn install_failed(&mut self, reason: String) {
        self.install = Install::Failed { reason };
    }

    /// Whether an install may be started right now.
    ///
    /// Asked by the command before it spawns anything, so two clicks in quick
    /// succession cannot start two downloads over the same bundle.
    #[must_use]
    pub fn can_install(&self) -> bool {
        update::install_label(&self.check, &self.install).is_some()
    }

    /// The banner for a newly-found version, or `None`.
    ///
    /// Mutating, like `ApprovalWatch::observe`: answering *and* recording is
    /// what makes "once per version" true rather than aspirational.
    pub fn notice(&mut self) -> Option<(String, String)> {
        let Check::Available { version, .. } = &self.check else {
            return None;
        };
        if self.announced.as_deref() == Some(version.as_str()) {
            return None;
        }
        self.announced = Some(version.clone());
        Some((
            format!("Solador {version} is available"),
            "Open Settings → About to install it.".to_string(),
        ))
    }
}

/// The Updates group, as the About tab paints it.
///
/// `current` is `settings::VERSION` — `None` on a build whose CalVer could not
/// be derived. It is passed rather than read here so this stays a pure function
/// of its arguments, and so a test can name a version.
#[must_use]
pub fn view(state: &UpdateState, current: Option<&str>) -> Value {
    let status = update::status(&state.check, &state.install, current);
    json!({
        "heading": HEADING,
        "status": { "text": status.text, "color": color::hex(status.color) },
        "notes": update::notes(&state.check),
        // Null suppresses the button entirely rather than disabling it: a
        // greyed-out control invites a click that will do nothing, and the two
        // moments it is suppressed (the first check, and a running install) are
        // both moments where the honest answer is "not yet".
        "checkLabel": update::can_check(&state.check, &state.install).then_some(CHECK_LABEL),
        "installLabel": update::install_label(&state.check, &state.install),
        "help": HELP,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(state: &mut UpdateState) {
        state.found("2026.8.114".to_string(), Some("notes".to_string()));
    }

    #[test]
    fn a_fresh_state_is_checking_not_up_to_date() {
        let view = view(&UpdateState::new(), Some("2026.8.113"));
        assert_eq!(view["status"]["text"], json!("Checking for updates…"));
        assert_eq!(view["status"]["color"], json!(color::hex(color::MUTED)));
        assert_eq!(view["checkLabel"], Value::Null);
        assert_eq!(view["installLabel"], Value::Null);
        assert_eq!(view["notes"], Value::Null);
    }

    #[test]
    fn an_available_update_offers_exactly_one_install_button() {
        let mut state = UpdateState::new();
        found(&mut state);
        let view = view(&state, Some("2026.8.113"));
        assert_eq!(view["installLabel"], json!("Install 2026.8.114"));
        assert_eq!(view["checkLabel"], json!(CHECK_LABEL));
        assert_eq!(view["notes"], json!("notes"));
    }

    /// The one that matters: a check that could not run must not paint the same
    /// thing as a check that ran and found nothing.
    #[test]
    fn a_failed_check_is_a_different_payload_from_up_to_date() {
        let mut failed = UpdateState::new();
        failed.check_failed("the network is unreachable".to_string());
        let mut ok = UpdateState::new();
        ok.up_to_date();

        let failed = view(&failed, Some("2026.8.113"));
        let ok = view(&ok, Some("2026.8.113"));
        assert_ne!(failed["status"]["text"], ok["status"]["text"]);
        assert_ne!(failed["status"]["color"], ok["status"]["color"]);
        assert!(failed["status"]["text"]
            .as_str()
            .expect("a status sentence")
            .contains("the network is unreachable"));
        // Both still offer a re-check: a failure is the state most worth
        // retrying, and being up to date is cheap to re-confirm.
        assert_eq!(failed["checkLabel"], json!(CHECK_LABEL));
        assert_eq!(ok["checkLabel"], json!(CHECK_LABEL));
    }

    #[test]
    fn a_running_install_cannot_be_started_a_second_time() {
        let mut state = UpdateState::new();
        found(&mut state);
        assert!(state.can_install());
        state.installing();
        assert!(!state.can_install());
        let view = view(&state, Some("2026.8.113"));
        assert_eq!(view["installLabel"], Value::Null);
        assert_eq!(view["checkLabel"], Value::Null);
    }

    #[test]
    fn one_banner_per_version_and_a_newer_one_still_gets_its_own() {
        let mut state = UpdateState::new();
        assert_eq!(state.notice(), None, "nothing found yet, nothing to say");

        found(&mut state);
        let (title, body) = state.notice().expect("the first sighting announces");
        assert!(title.contains("2026.8.114"), "{title}");
        assert!(body.contains("Settings"), "{body}");
        assert_eq!(
            state.notice(),
            None,
            "the same version must not re-announce"
        );

        // A manual re-check that finds the SAME version stays quiet…
        state.checking();
        found(&mut state);
        assert_eq!(state.notice(), None);

        // …and one that finds a newer one does not.
        state.found("2026.9.1".to_string(), None);
        assert!(state
            .notice()
            .expect("a newer version is a new thing to say")
            .0
            .contains("2026.9.1"));
    }

    #[test]
    fn being_up_to_date_clears_a_stale_relaunch_notice() {
        let mut state = UpdateState::new();
        found(&mut state);
        state.installed();
        assert!(view(&state, Some("2026.8.113"))["status"]["text"]
            .as_str()
            .expect("a status sentence")
            .contains("next time"));

        // The operator relaunched; the launch check now finds nothing newer.
        state.checking();
        state.up_to_date();
        let view = view(&state, Some("2026.8.114"));
        assert_eq!(
            view["status"]["text"],
            json!("Solador 2026.8.114 is the latest version.")
        );
    }

    #[test]
    fn a_failed_check_does_not_erase_an_install_that_did_work() {
        let mut state = UpdateState::new();
        found(&mut state);
        state.installed();
        state.check_failed("offline".to_string());
        // The install outcome outranks the check, and it is still true.
        assert!(view(&state, None)["status"]["text"]
            .as_str()
            .expect("a status sentence")
            .contains("next time"));
    }

    // MARK: - #353

    /// The bug this fixes, at the seam that would have delivered the banner.
    ///
    /// `notice()` is the only thing `main.rs` will deliver an `update-available`
    /// banner from, so a `None` here is the property the acceptance asks for:
    /// no banner can fire on a build with nothing to replace, however the feed
    /// answers.
    #[test]
    fn a_build_with_nothing_to_replace_can_never_deliver_a_banner() {
        for why in [
            NotUpdatable::DevelopmentBuild,
            NotUpdatable::UnnameableBuild,
        ] {
            let mut state = UpdateState::new();
            state.not_updatable(why);
            assert_eq!(state.notice(), None, "{why:?}");
            // …and pressing it a second time cannot shake one loose either.
            assert_eq!(state.notice(), None, "{why:?}");
            assert!(!state.can_install(), "{why:?}");
        }
    }

    /// The payload the About tab paints: one sentence, no buttons, no notes.
    #[test]
    fn a_build_with_nothing_to_replace_renders_its_own_payload() {
        let mut state = UpdateState::new();
        state.not_updatable(NotUpdatable::DevelopmentBuild);
        let mine = view(&state, Some("2026.8.113"));

        assert_eq!(mine["installLabel"], Value::Null);
        assert_eq!(mine["checkLabel"], Value::Null);
        assert_eq!(mine["notes"], Value::Null);

        // And it is none of the other four sentences — most of all not the
        // green one, which would be a verdict nothing looked for.
        let mut fresh = UpdateState::new();
        fresh.up_to_date();
        for other in [
            view(&UpdateState::new(), Some("2026.8.113")),
            view(&fresh, Some("2026.8.113")),
        ] {
            assert_ne!(mine["status"]["text"], other["status"]["text"]);
            assert_ne!(mine["status"]["color"], other["status"]["color"]);
        }
    }

    /// The shell's whole share of the decision: which profile this is.
    ///
    /// Asserted against the profile the test itself was compiled at, because
    /// that is the only honest thing to assert about a `cfg!` — the rule it
    /// feeds is covered exhaustively in `viewmodel::update`.
    #[test]
    fn the_cargo_profile_is_what_decides_whether_this_build_checks() {
        let named = Some("2026.8.113");
        if cfg!(debug_assertions) {
            assert_eq!(not_updatable(named), Some(NotUpdatable::DevelopmentBuild));
        } else {
            assert_eq!(not_updatable(named), None);
        }
        // Either way, a build that cannot name itself never compares itself
        // against anything.
        assert!(not_updatable(None).is_some());
    }
}
