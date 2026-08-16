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
//! So there are four check states, and the fourth ([`Check::Unknown`]) is the
//! first frame — before the launch check has settled, the app has nothing to
//! say and says so, the same discipline `panel::Configured` applies to
//! credentials.
//!
//! # Never auto-install
//!
//! [`Check::Available`] is an *offer*. This cockpit is built to sit full-screen
//! on a second monitor for days; an app that restarts itself to apply an update
//! takes the display down at a moment nobody chose. The install is a separate
//! state machine ([`Install`]) that only an operator action starts.

use crate::color;

/// The result of the most recent update check.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Check {
    /// No check has completed. The launch check is in flight, or this build
    /// cannot check at all.
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
/// Suppressed only while work is in flight. It stays available after a failure
/// — retrying is the obvious next thing an operator wants — and after an
/// install, because a check is harmless.
#[must_use]
pub fn can_check(check: &Check, install: &Install) -> bool {
    !matches!(install, Install::Running) && !matches!(check, Check::Unknown)
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
}
