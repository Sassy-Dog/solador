//! Presence of an entity the user expects to exist. Port of
//! `PresenceState`.
//!
//! Pure evaluation: no I/O, no wall clock. Callers pass `now` as the owning
//! source's last **successful** poll time, never render time, so absence clocks
//! freeze while a source is failing instead of ageing toward a false alarm.

use chrono::{DateTime, Utc};

/// Absence grace before "recycling" escalates to "missing". The mac runner
/// slots recycle in 1–4 minutes, so 5 minutes of absence means wedged.
pub const DEFAULT_GRACE_SECS: i64 = 300;

/// Present in the source's current data, briefly absent (a normal recycle
/// window, amber), or absent beyond grace (alarm, red).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceState {
    Present,
    Recycling { absence_secs: i64 },
    Missing { absence_secs: i64 },
}

/// Classify one entity's presence.
///
/// Absence is clamped at zero: a backwards clock skew must read as "just seen",
/// never as a negative age that would sort or format as nonsense.
#[must_use]
pub fn state(
    is_present: bool,
    last_seen: DateTime<Utc>,
    now: DateTime<Utc>,
    grace_secs: i64,
) -> PresenceState {
    if is_present {
        return PresenceState::Present;
    }
    let absence_secs = (now - last_seen).num_seconds().max(0);
    if absence_secs < grace_secs {
        PresenceState::Recycling { absence_secs }
    } else {
        PresenceState::Missing { absence_secs }
    }
}

/// "recycling 40s" / "missing 12m" — `None` for present.
#[must_use]
pub fn label(state: PresenceState) -> Option<String> {
    match state {
        PresenceState::Present => None,
        PresenceState::Recycling { absence_secs } => {
            Some(format!("recycling {}", duration(absence_secs)))
        }
        PresenceState::Missing { absence_secs } => {
            Some(format!("missing {}", duration(absence_secs)))
        }
    }
}

/// Same unit ladder as the panel footers' relative age, without the "ago".
fn duration(secs: i64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> DateTime<Utc> {
        DateTime::from_timestamp(1_000_000, 0).expect("valid timestamp")
    }

    fn absent_for(secs: i64, grace_secs: i64) -> PresenceState {
        state(
            false,
            base(),
            base() + chrono::TimeDelta::seconds(secs),
            grace_secs,
        )
    }

    #[test]
    fn present_wins_regardless_of_last_seen() {
        let ancient = base() - chrono::TimeDelta::seconds(86_400);
        assert_eq!(
            state(true, ancient, base(), DEFAULT_GRACE_SECS),
            PresenceState::Present
        );
    }

    #[test]
    fn absence_just_under_grace_is_recycling() {
        assert_eq!(
            absent_for(299, DEFAULT_GRACE_SECS),
            PresenceState::Recycling { absence_secs: 299 }
        );
    }

    #[test]
    fn absence_exactly_at_grace_is_missing() {
        assert_eq!(
            absent_for(300, DEFAULT_GRACE_SECS),
            PresenceState::Missing { absence_secs: 300 }
        );
    }

    #[test]
    fn absence_beyond_grace_is_missing() {
        assert_eq!(
            absent_for(301, DEFAULT_GRACE_SECS),
            PresenceState::Missing { absence_secs: 301 }
        );
    }

    #[test]
    fn backward_clock_skew_clamps_to_zero_absence() {
        assert_eq!(
            absent_for(-50, DEFAULT_GRACE_SECS),
            PresenceState::Recycling { absence_secs: 0 }
        );
    }

    #[test]
    fn custom_grace_is_respected() {
        assert_eq!(
            absent_for(45, 30),
            PresenceState::Missing { absence_secs: 45 }
        );
        assert_eq!(
            absent_for(20, 30),
            PresenceState::Recycling { absence_secs: 20 }
        );
    }

    #[test]
    fn present_has_no_label() {
        assert_eq!(label(PresenceState::Present), None);
    }

    #[test]
    fn labels_walk_the_unit_ladder() {
        let recycling = |s| label(PresenceState::Recycling { absence_secs: s });
        let missing = |s| label(PresenceState::Missing { absence_secs: s });
        assert_eq!(recycling(40).as_deref(), Some("recycling 40s"));
        assert_eq!(recycling(180).as_deref(), Some("recycling 3m"));
        assert_eq!(missing(45).as_deref(), Some("missing 45s"));
        assert_eq!(missing(720).as_deref(), Some("missing 12m"));
        assert_eq!(missing(7_200).as_deref(), Some("missing 2h"));
        assert_eq!(missing(200_000).as_deref(), Some("missing 2d"));
    }
}
