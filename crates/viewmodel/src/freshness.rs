//! How old the number on screen is — and whether that age is a measurement at
//! all.
//!
//! # This composes with `status_footer`, it does not replace it
//!
//! They are two clocks and they answer different questions. The shell's
//! `panel::status_footer` answers *"did the last attempt fail, and when did one
//! last succeed"*; [`Freshness`] answers *"how old is the figure I am painting
//! right now"*. A panel that needs both keeps them as separate fields — the
//! `local_last_updated` / `local_last_success` precedent — because collapsing
//! them is how a footer ends up promising `last ok` about a reading that never
//! existed.

/// The age of the reading a panel is painting, and **what kind of claim that
/// age is**.
///
/// [`Unknown`](Self::Unknown) is a variant rather than a sentinel duration on
/// purpose. Nothing has ever been read successfully, so there is no number to
/// report; `Stale { measured_secs_ago: 0 }` would be a fabricated one, and this
/// repo treats a defaulted state as exactly as much of a fabrication as a
/// defaulted number.
///
/// [`Stale`](Self::Stale) is likewise a *separate variant* rather than the same
/// `u64` from a different place — the same argument
/// `usage::sentry::CronAge` makes: the renderer has to mark it as the weaker
/// claim it is, and a plain `u64` would let it pass for the precise one. A
/// day-old figure rendering identically to a live one is the same class of bug
/// as unknown-rendered-as-zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Measured, and within this panel's cadence. The number is current.
    Live { measured_secs_ago: u64 },
    /// Measured, and older than the cadence. Real, but no longer a live claim.
    Stale { measured_secs_ago: u64 },
    /// Never successfully read. **Not a duration** — there is no age to give.
    Unknown,
}

impl Freshness {
    /// Classify a reading's age against the cadence of the panel showing it.
    ///
    /// `measured_secs_ago` is `None` when nothing has ever been read — that is
    /// [`Unknown`](Self::Unknown), never an age of zero.
    ///
    /// **The cadence itself counts as live**: a panel polling every 60s is
    /// expected to be showing a reading up to 60s old, so `60` is
    /// [`Live`](Self::Live) and only `61` is [`Stale`](Self::Stale). The
    /// comparison is therefore `> cadence_secs`, the same inclusive edge
    /// `status_footer` draws against its `stale_after`; the two describing the
    /// same reading differently at the boundary is a contradiction the operator
    /// would have to resolve on sight.
    ///
    /// The caller passes the age. Nothing here reads a clock — this crate is
    /// pure values, so a test can name the second it cares about.
    #[must_use]
    pub fn classify(measured_secs_ago: Option<u64>, cadence_secs: u64) -> Self {
        match measured_secs_ago {
            Some(measured_secs_ago) if measured_secs_ago > cadence_secs => {
                Freshness::Stale { measured_secs_ago }
            }
            Some(measured_secs_ago) => Freshness::Live { measured_secs_ago },
            None => Freshness::Unknown,
        }
    }

    /// Whether the figure may be presented as current.
    ///
    /// Only [`Live`](Self::Live) qualifies. [`Unknown`](Self::Unknown) is not
    /// live for the same reason it is not stale: there is no reading.
    #[must_use]
    pub fn is_live(self) -> bool {
        matches!(self, Freshness::Live { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_boundary_between_live_and_stale_is_the_cadence_itself() {
        assert_eq!(
            Freshness::classify(Some(60), 60),
            Freshness::Live {
                measured_secs_ago: 60
            }
        );
        assert_eq!(
            Freshness::classify(Some(61), 60),
            Freshness::Stale {
                measured_secs_ago: 61
            }
        );
    }

    #[test]
    fn never_measured_is_unknown_not_stale() {
        assert_eq!(Freshness::classify(None, 60), Freshness::Unknown);
    }

    #[test]
    fn stale_is_not_live_however_recent() {
        assert!(!Freshness::classify(Some(86_400), 3600).is_live());
    }

    #[test]
    fn unknown_is_not_live_and_carries_no_duration() {
        // The failure this type exists to prevent: never-measured degrading
        // into an age of zero, which would render as the freshest reading on
        // the panel.
        assert!(!Freshness::classify(None, 60).is_live());
        assert_ne!(
            Freshness::classify(None, 60),
            Freshness::Stale {
                measured_secs_ago: 0
            }
        );
        assert_ne!(
            Freshness::classify(None, 60),
            Freshness::Live {
                measured_secs_ago: 0
            }
        );
    }

    #[test]
    fn a_reading_measured_this_instant_is_live() {
        assert_eq!(
            Freshness::classify(Some(0), 60),
            Freshness::Live {
                measured_secs_ago: 0
            }
        );
        assert!(Freshness::classify(Some(0), 60).is_live());
    }

    #[test]
    fn the_measured_age_survives_classification_unchanged() {
        // Both real variants report the age they were given: the classification
        // qualifies the number, it never replaces or rounds it.
        assert_eq!(
            Freshness::classify(Some(3_599), 3_600),
            Freshness::Live {
                measured_secs_ago: 3_599
            }
        );
        assert_eq!(
            Freshness::classify(Some(90_061), 3_600),
            Freshness::Stale {
                measured_secs_ago: 90_061
            }
        );
    }
}
