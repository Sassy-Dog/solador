//! What the app knows about a newer version of itself, and how that is said.
//!
//! # "Up to date" and "the check failed" are different sentences
//!
//! This is the whole reason this module exists as a pair of enums rather than
//! an `Option<String>`. An update check that errored — no network, a 404 on the
//! feed, a manifest the plugin could not parse — is *not* a machine that is
//! running the latest build; it is a machine that does not know. Collapsing
//! them gives an operator a green "you're up to date" on the exact failure that
//! stops updates reaching them, which is the same class of bug as an unmeasured
//! metric rendering as zero.
//!
//! So there are five check states, and the last two are refusals to give a
//! verdict rather than verdicts. [`Check::Unknown`] is the first frame — before
//! the launch check has settled, the app has nothing to say and says so, the
//! same discipline `panel::Configured` applies to credentials.
//!
//! # A build with nothing to replace gets no verdict at all
//!
//! [`Check::NotUpdatable`] is that discipline one step earlier (#353). Two
//! builds have no business being compared against a release feed:
//!
//! * a **development build** — `./dev` runs plain `cargo build` and
//!   `scripts/run.sh` wraps the binary in a throwaway `.app` under
//!   `target/<profile>/`. A release `.app.tar.gz` unpacked over that is not an
//!   update, so "you are behind" would be a verdict implying an install that
//!   cannot work; and
//! * a build that **cannot name its own version** — a shallow clone, where
//!   `settings::VERSION` is `None` and About already renders `—`. A build that
//!   cannot say what it is must not be told it is behind something else.
//!
//! It is not [`Check::UpToDate`] (nothing was compared), not [`Check::Failed`]
//! (nothing broke) and not [`Check::Unknown`] (which promises a check is
//! running). It is its own sentence in its own colour, and it offers no buttons
//! — there is nothing to install, and nothing a re-check could change.
//!
//! # The version compared is the one the app actually is
//!
//! [`is_newer`] exists because the updater plugin's own comparison uses the
//! wrong number. `tauri-plugin-updater` compares `release.version` against
//! `app.package_info().version`, which comes from `tauri.conf.json` — and this
//! repo deliberately authors no `version` key there (#303,
//! `docs/VERSIONING.md`), so Tauri falls back to `app/src-tauri/Cargo.toml`'s
//! unpublished `0.1.0`. The real CalVer only ever arrives as a `--config`
//! overlay on the bundling build, so a locally built cockpit compared every
//! release ever cut against `0.1.0`, found all of them greater, and announced
//! an update on every launch, forever (#353).
//!
//! `UpdaterBuilder` has no `current_version` setter — the field is private and
//! set from `package_info()` — so the comparator is the supported seam, and
//! this is it. The `current` the plugin passes in is that wrong number, and is
//! discarded.
//!
//! # Never auto-install
//!
//! [`Check::Available`] is an *offer*. This cockpit is built to sit full-screen
//! on a second monitor for days; an app that restarts itself to apply an update
//! takes the display down at a moment nobody chose. The install is a separate
//! state machine ([`Install`]) that only an operator action starts.

use crate::color;
use semver::Version;

/// The result of the most recent update check.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Check {
    /// No check has completed. The launch check is in flight.
    #[default]
    Unknown,
    /// A check completed and the feed offers nothing newer.
    UpToDate,
    /// A check completed and a newer version is available.
    Available {
        /// The feed's `version` — the marketing CalVer.
        version: String,
        /// The release notes, if the feed carried any. `None` is *no notes*,
        /// never an empty string standing in for them.
        notes: Option<String>,
    },
    /// A check ran and failed. **Not** [`UpToDate`](Self::UpToDate).
    Failed {
        /// Already user-facing; the shell passes the plugin's error through
        /// `user_message`-style wording.
        reason: String,
    },
    /// No check was run at all, because this build is not one a release can
    /// replace. **Not** [`UpToDate`](Self::UpToDate) — nothing was compared.
    ///
    /// Decided from the build itself by [`not_updatable`] *before* any request
    /// is made, so a `./dev` cockpit never reaches the release server.
    NotUpdatable(NotUpdatable),
}

/// Why a build is outside the update path.
///
/// Both cases are properties of the build, fixed for the life of the process:
/// nothing an operator can do moves a build from one to the other, which is why
/// the state offers no "Check for updates" button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotUpdatable {
    /// Built locally by `./dev`. There is no installed release here to replace.
    DevelopmentBuild,
    /// The build could not derive its own CalVer — a shallow clone, where
    /// `settings::VERSION` is `None` and About renders `—`.
    UnnameableBuild,
}

/// How the running build was produced, as far as replacing it goes.
///
/// The shell decides this from the cargo profile. The ambiguity is deliberately
/// resolved *towards* checking: a release-profile build is always
/// [`Released`](Self::Released), because a rule that could silently classify a
/// shipped build as development would stop updates reaching users — a far worse
/// failure than the launch-time nag it is replacing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// Produced by the release path: there is an installed bundle on disk that
    /// an update can be unpacked over.
    Released,
    /// A local development build.
    Development,
}

/// How far an operator-started install has got.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Install {
    /// Nothing has been asked for.
    #[default]
    Idle,
    /// Downloading and unpacking. The button is gone while this is true, so a
    /// second click cannot start a second install over the first.
    Running,
    /// Applied on disk. The new version takes effect on the next launch —
    /// which the operator chooses, because relaunching a cockpit unasked is
    /// the thing this feature must not do.
    Applied,
    /// The install failed. The offer stays, so it can be retried.
    Failed { reason: String },
}

/// One rendered line: the sentence, and the colour that qualifies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub text: String,
    pub color: u32,
}

/// What the running build calls itself, for the sentences below.
///
/// `None` is a build whose CalVer could not be derived (a shallow clone — see
/// `settings::VERSION`). It is carried as an `Option` rather than defaulted to
/// a plausible number for the same reason About renders `Version —`.
pub type CurrentVersion<'a> = Option<&'a str>;

/// Why this build must not be compared against the release feed, or `None` when
/// it may be.
///
/// Asked **before** the request rather than after it: a build with nothing to
/// replace has no verdict to render whatever the feed says, and asking anyway
/// would mean every developer launch pinging a release server to discard the
/// answer.
///
/// [`NotUpdatable::DevelopmentBuild`] wins when both apply. It is the reason
/// that names what the operator did, and a shallow clone that is also a `./dev`
/// build is still, first, a `./dev` build.
#[must_use]
pub fn not_updatable(current: CurrentVersion<'_>, provenance: Provenance) -> Option<NotUpdatable> {
    match (provenance, current) {
        (Provenance::Development, _) => Some(NotUpdatable::DevelopmentBuild),
        (Provenance::Released, None) => Some(NotUpdatable::UnnameableBuild),
        (Provenance::Released, Some(_)) => None,
    }
}

/// Whether the feed's newest `release` is newer than the build that is running.
///
/// This is what `tauri-plugin-updater` is registered with as its version
/// comparator; the module header carries why its own comparison cannot be used.
/// `current` is `settings::VERSION` — the git-derived CalVer `build.rs`
/// publishes for **every** profile, debug included — and never
/// `CARGO_PKG_VERSION` / `package_info().version`.
///
/// `semver` is the same crate, at the same `^1` requirement, that the plugin
/// resolves, so cargo unifies them to one and this cannot order two versions
/// differently from the code that would otherwise have compared them. That is
/// the reasoning `crates/updatefeed` takes `minisign-verify` for.
///
/// **It fails closed.** A build that cannot name itself, or a version string
/// neither side can parse, answers `false` — no offer — rather than one that
/// could not be justified. A missed offer is an update that arrives late; a
/// fabricated one is an install prompt for a comparison that never happened.
#[must_use]
pub fn is_newer(current: CurrentVersion<'_>, release: &str) -> bool {
    let (Some(current), Ok(release)) = (current, Version::parse(release)) else {
        return false;
    };
    Version::parse(current).is_ok_and(|current| release > current)
}

/// The sentence describing where this app stands relative to the feed.
///
/// The [`Install`] state outranks the [`Check`] state whenever it has something
/// to say: once an operator has started an install, "an update is available" is
/// no longer the useful line.
#[must_use]
pub fn status(check: &Check, install: &Install, current: CurrentVersion<'_>) -> Status {
    match install {
        Install::Running => {
            return Status {
                text: "Downloading the update…".to_string(),
                color: color::MUTED,
            }
        }
        Install::Applied => {
            return Status {
                text: "Update installed. It takes effect the next time you open Solador."
                    .to_string(),
                color: color::GREEN,
            }
        }
        Install::Failed { reason } => {
            return Status {
                text: format!("Could not install the update: {reason}"),
                color: color::RED,
            }
        }
        Install::Idle => {}
    }

    match check {
        // Deliberately not "up to date". Nothing has looked yet.
        Check::Unknown => Status {
            text: "Checking for updates…".to_string(),
            color: color::MUTED,
        },
        Check::UpToDate => Status {
            text: match current {
                Some(v) => format!("Solador {v} is the latest version."),
                // An un-nameable build still knows the feed offered nothing
                // newer; it just cannot say which build it is.
                None => "This is the latest version.".to_string(),
            },
            color: color::GREEN_DIM,
        },
        Check::Available { version, .. } => Status {
            text: match current {
                Some(current) => format!("Solador {version} is available. You have {current}."),
                None => format!("Solador {version} is available."),
            },
            color: color::AMBER,
        },
        // Amber, not red: a failed check is a gap in knowledge, not a broken
        // app — and not green either, which is the mistake this whole module
        // exists to make impossible.
        Check::Failed { reason } => Status {
            text: format!("Could not check for updates: {reason}"),
            color: color::AMBER,
        },
        // Neither a verdict nor a failure, so neither a verdict's colour nor a
        // failure's. `color::INK` is the one palette value that is explicitly
        // *not* semantic — its own doc says no threshold reads it — which makes
        // it the honest paint for a line that declines to claim anything about
        // updates. It is also none of the four colours the states above use, so
        // this cannot be misread as any of them.
        Check::NotUpdatable(why) => Status {
            text: match (why, current) {
                (NotUpdatable::DevelopmentBuild, Some(v)) => format!(
                    "Solador {v}, built locally. Updates are not checked: there is no installed release here to replace."
                ),
                (NotUpdatable::DevelopmentBuild, None) => {
                    "This is a local build. Updates are not checked: there is no installed release here to replace."
                        .to_string()
                }
                (NotUpdatable::UnnameableBuild, _) => {
                    "This build could not derive its own version, so it is not compared against any release."
                        .to_string()
                }
            },
            color: color::INK,
        },
    }
}

/// The install button's label, or `None` when there must not be one.
///
/// There is nothing to install unless a check has *seen* a newer version, and
/// nothing to start while one is already running or has already been applied.
#[must_use]
pub fn install_label(check: &Check, install: &Install) -> Option<String> {
    let Check::Available { version, .. } = check else {
        return None;
    };
    match install {
        Install::Idle | Install::Failed { .. } => Some(format!("Install {version}")),
        Install::Running | Install::Applied => None,
    }
}

/// Whether the "Check for updates" button should be offered.
///
/// Suppressed while work is in flight, and on a build that is outside the
/// update path at all: [`Check::NotUpdatable`] is a property of the build, so a
/// button there would be a control whose only possible effect is to repaint the
/// same sentence. It stays available after a failure — retrying is the obvious
/// next thing an operator wants — and after an install, because a check is
/// harmless.
#[must_use]
pub fn can_check(check: &Check, install: &Install) -> bool {
    !matches!(install, Install::Running)
        && !matches!(check, Check::Unknown | Check::NotUpdatable(_))
}

/// The release notes to show, if any, for an offered update.
///
/// Only ever shown for [`Check::Available`]: notes belong to the version being
/// offered, and leaving them on screen after an install would describe a
/// release the operator already has.
#[must_use]
pub fn notes(check: &Check) -> Option<&str> {
    match check {
        Check::Available { notes, .. } => notes.as_deref().map(str::trim).filter(|n| !n.is_empty()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CURRENT: CurrentVersion<'static> = Some("2026.8.113");

    fn available() -> Check {
        Check::Available {
            version: "2026.8.114".to_string(),
            notes: Some("fixed a thing".to_string()),
        }
    }

    fn local_build() -> Check {
        Check::NotUpdatable(NotUpdatable::DevelopmentBuild)
    }

    /// The invariant this module exists for. Three states that a `bool` or an
    /// `Option` would have flattened into two must not paint the same line or
    /// the same colour.
    #[test]
    fn a_failed_check_never_reads_as_up_to_date() {
        let failed = status(
            &Check::Failed {
                reason: "the network is unreachable".to_string(),
            },
            &Install::Idle,
            CURRENT,
        );
        let ok = status(&Check::UpToDate, &Install::Idle, CURRENT);
        let unknown = status(&Check::Unknown, &Install::Idle, CURRENT);

        assert_ne!(failed.text, ok.text);
        assert_ne!(failed.color, ok.color);
        assert_ne!(unknown.text, ok.text);
        assert_ne!(unknown.color, ok.color);
        // And the failure names its cause rather than being a bare "error".
        assert!(failed.text.contains("the network is unreachable"));
        // Never green: green is the colour of a claim this state cannot make.
        assert_ne!(failed.color, color::GREEN);
        assert_ne!(failed.color, color::GREEN_DIM);
        assert_ne!(unknown.color, color::GREEN);
        assert_ne!(unknown.color, color::GREEN_DIM);
    }

    #[test]
    fn the_first_frame_says_it_is_looking_not_that_it_looked() {
        let s = status(&Check::Unknown, &Install::Idle, CURRENT);
        assert_eq!(s.text, "Checking for updates…");
        assert_eq!(s.color, color::MUTED);
        // And offers no button: there is nothing to re-check before the first
        // check has settled, and nothing to install.
        assert!(!can_check(&Check::Unknown, &Install::Idle));
        assert_eq!(install_label(&Check::Unknown, &Install::Idle), None);
    }

    #[test]
    fn an_available_update_names_both_versions() {
        let s = status(&available(), &Install::Idle, CURRENT);
        assert!(s.text.contains("2026.8.114"), "{}", s.text);
        assert!(s.text.contains("2026.8.113"), "{}", s.text);
        assert_eq!(s.color, color::AMBER);
        assert_eq!(
            install_label(&available(), &Install::Idle),
            Some("Install 2026.8.114".to_string())
        );
    }

    /// A build with no derivable CalVer (a shallow clone) can still be offered
    /// an update — it just must not invent a number to compare against.
    #[test]
    fn an_unnameable_build_omits_the_current_version_rather_than_inventing_one() {
        let s = status(&available(), &Install::Idle, None);
        assert_eq!(s.text, "Solador 2026.8.114 is available.");
        assert!(!s.text.contains("You have"));

        let up = status(&Check::UpToDate, &Install::Idle, None);
        assert_eq!(up.text, "This is the latest version.");
    }

    #[test]
    fn there_is_nothing_to_install_until_a_check_has_seen_something() {
        assert_eq!(install_label(&Check::UpToDate, &Install::Idle), None);
        assert_eq!(
            install_label(
                &Check::Failed {
                    reason: "boom".to_string()
                },
                &Install::Idle
            ),
            None
        );
    }

    #[test]
    fn a_running_install_takes_the_button_away_so_it_cannot_be_started_twice() {
        assert_eq!(install_label(&available(), &Install::Running), None);
        assert!(!can_check(&available(), &Install::Running));
        assert_eq!(
            status(&available(), &Install::Running, CURRENT).text,
            "Downloading the update…"
        );
    }

    /// The applied state must not claim the app is now running the new build —
    /// it is running the old one until somebody relaunches it, and saying
    /// otherwise is a fabricated state.
    #[test]
    fn an_applied_install_says_it_takes_effect_on_the_next_launch() {
        let s = status(&available(), &Install::Applied, CURRENT);
        assert!(s.text.contains("next time"), "{}", s.text);
        assert_eq!(s.color, color::GREEN);
        assert_eq!(install_label(&available(), &Install::Applied), None);
    }

    #[test]
    fn a_failed_install_keeps_the_offer_so_it_can_be_retried() {
        let failed = Install::Failed {
            reason: "the download was interrupted".to_string(),
        };
        let s = status(&available(), &failed, CURRENT);
        assert!(s.text.contains("the download was interrupted"));
        assert_eq!(s.color, color::RED);
        assert_eq!(
            install_label(&available(), &failed),
            Some("Install 2026.8.114".to_string())
        );
        assert!(can_check(&available(), &failed));
    }

    #[test]
    fn notes_are_shown_only_for_the_version_actually_being_offered() {
        assert_eq!(notes(&available()), Some("fixed a thing"));
        assert_eq!(notes(&Check::UpToDate), None);
        assert_eq!(notes(&Check::Unknown), None);
        assert_eq!(notes(&local_build()), None);
        // A feed that carried an empty `notes` string said nothing, and an
        // empty paragraph on screen is not a way of saying nothing.
        assert_eq!(
            notes(&Check::Available {
                version: "2026.8.114".to_string(),
                notes: Some("   \n ".to_string()),
            }),
            None
        );
    }

    // MARK: - #353

    /// The bug, pinned from both ends.
    ///
    /// `0.1.0` is `app/src-tauri/Cargo.toml`'s unpublished package version, and
    /// what `package_info().version` returns on any build that did not get the
    /// `--config` overlay. The first assertion *is* the defect: compared
    /// against that number, every release ever cut is an available update. The
    /// rest is what the comparator does instead.
    #[test]
    fn the_feed_is_compared_against_the_calver_not_the_cargo_version() {
        assert!(
            is_newer(Some("0.1.0"), "2026.8.110"),
            "0.1.0 loses to every CalVer — which is exactly why the plugin's own \
             comparison announced an update on every launch"
        );

        // The build in the report: newer than the newest release, so nothing is
        // offered. Not "nothing offered because the check failed" — it compared.
        assert!(!is_newer(Some("2026.8.113"), "2026.8.110"));
        // The same version is not a newer version.
        assert!(!is_newer(Some("2026.8.110"), "2026.8.110"));
        // And a genuinely newer release still wins, including across a month
        // roll, where the patch counter restarts and only the minor moves.
        assert!(is_newer(Some("2026.8.110"), "2026.8.113"));
        assert!(is_newer(Some("2026.8.113"), "2026.9.1"));
        assert!(!is_newer(Some("2026.9.1"), "2026.8.113"));
    }

    /// Fail closed: every input this cannot order answers "no offer".
    #[test]
    fn an_uncomparable_version_offers_nothing_rather_than_guessing() {
        assert!(!is_newer(None, "2026.8.113"));
        assert!(!is_newer(Some("2026.8.113"), "not-a-version"));
        assert!(!is_newer(Some("not-a-version"), "2026.8.113"));
        assert!(!is_newer(Some(""), "2026.8.113"));
    }

    /// Which builds are inside the update path, and which are not.
    #[test]
    fn only_a_released_build_that_can_name_itself_is_compared_at_all() {
        assert_eq!(not_updatable(CURRENT, Provenance::Released), None);
        assert_eq!(
            not_updatable(CURRENT, Provenance::Development),
            Some(NotUpdatable::DevelopmentBuild)
        );
        // A shallow clone lands in the state rather than comparing against
        // anything — the same refusal About makes when it renders `—`.
        assert_eq!(
            not_updatable(None, Provenance::Released),
            Some(NotUpdatable::UnnameableBuild)
        );
        // Both at once is still, first, a local build.
        assert_eq!(
            not_updatable(None, Provenance::Development),
            Some(NotUpdatable::DevelopmentBuild)
        );
    }

    /// The fifth state must not read like any of the four before it, which is
    /// the invariant `a_failed_check_never_reads_as_up_to_date` asserts one
    /// state earlier.
    #[test]
    fn a_build_with_nothing_to_replace_renders_as_none_of_the_other_four() {
        let mine = status(&local_build(), &Install::Idle, CURRENT);
        for other in [
            Check::Unknown,
            Check::UpToDate,
            available(),
            Check::Failed {
                reason: "the network is unreachable".to_string(),
            },
        ] {
            let s = status(&other, &Install::Idle, CURRENT);
            assert_ne!(mine.text, s.text, "{other:?}");
            assert_ne!(mine.color, s.color, "{other:?}");
        }
        // Never green, for the reason the whole module exists.
        assert_ne!(mine.color, color::GREEN);
        assert_ne!(mine.color, color::GREEN_DIM);
        // It names the running build rather than hiding it, and says why there
        // is no verdict.
        assert!(mine.text.contains("2026.8.113"), "{}", mine.text);
        assert!(mine.text.contains("built locally"), "{}", mine.text);
    }

    /// No install button, no re-check button, no notes — there is nothing to
    /// install and nothing a second look could change.
    #[test]
    fn a_build_with_nothing_to_replace_offers_no_buttons() {
        assert_eq!(install_label(&local_build(), &Install::Idle), None);
        assert!(!can_check(&local_build(), &Install::Idle));
        assert_eq!(notes(&local_build()), None);

        let unnameable = Check::NotUpdatable(NotUpdatable::UnnameableBuild);
        assert_eq!(install_label(&unnameable, &Install::Idle), None);
        assert!(!can_check(&unnameable, &Install::Idle));
    }

    /// The two reasons are different sentences: "you built this yourself" and
    /// "this checkout cannot say what it is" are different things to fix.
    #[test]
    fn the_two_reasons_for_having_nothing_to_replace_do_not_read_alike() {
        let local = status(&local_build(), &Install::Idle, None);
        let unnameable = status(
            &Check::NotUpdatable(NotUpdatable::UnnameableBuild),
            &Install::Idle,
            None,
        );
        assert_ne!(local.text, unnameable.text);
        assert!(unnameable.text.contains("could not derive"));
        // Neither invents a version it does not have.
        assert!(!local.text.contains("Solador "));
        assert!(!unnameable.text.contains("Solador "));
    }

    /// A real release build is untouched by all of the above: it checks, it is
    /// offered the newer release, and it is offered the button to install it.
    #[test]
    fn a_released_build_still_checks_offers_and_installs() {
        let current = Some("2026.8.113");
        assert_eq!(not_updatable(current, Provenance::Released), None);
        assert!(is_newer(current, "2026.8.114"));

        let s = status(&available(), &Install::Idle, current);
        assert_eq!(s.color, color::AMBER);
        assert_eq!(
            s.text,
            "Solador 2026.8.114 is available. You have 2026.8.113."
        );
        assert_eq!(
            install_label(&available(), &Install::Idle),
            Some("Install 2026.8.114".to_string())
        );
        assert!(can_check(&available(), &Install::Idle));
        assert_eq!(notes(&available()), Some("fixed a thing"));
    }
}
