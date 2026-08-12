//! The auto-learned runner roster. Port of
//! `GHRunnerRoster`.
//!
//! The org's ephemeral runners de-register between jobs, so GitHub's registered
//! list alone can never say "mac-s2 *should* exist" — it only ever says what is
//! registered right now. This roster is the missing memory: every name GitHub
//! has shown us, with when we last saw it.
//!
//! Pure arithmetic — no I/O, no wall clock. Every entry point takes `now`, and
//! the caller must only pass a **successful** fetch's timestamp. That is what
//! freezes absence clocks while GitHub is unreachable: an outage must not age a
//! healthy runner into a red "missing 40m", because the runner never went
//! anywhere — our view of it did.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::presence::{self, PresenceState, DEFAULT_GRACE_SECS};
use crate::runners::{GhRunner, RunnerOs, RunnerSummary};

/// Names absent this long are silently dropped — covers decommissioned runners
/// and rotated hash-suffixed names without manual cleanup.
pub const AGE_OUT_SECS: i64 = 24 * 3_600;

/// One remembered self-hosted runner name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerRosterEntry {
    pub name: String,
    pub os: RunnerOs,
    pub last_seen: DateTime<Utc>,
}

/// A remembered runner currently absent from GitHub's registered list: amber
/// while recycling (normal ephemeral churn), red once missing beyond grace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhRunnerAbsence {
    pub name: String,
    pub os: RunnerOs,
    pub state: PresenceState,
}

/// One renderable Runners-panel row. Identity is the NAME for both cases —
/// ephemeral runners get a fresh numeric id on every re-registration, so
/// id-keyed rows would tear down and rebuild each cycle even while nothing
/// visibly changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhRunnerDisplayRow {
    Registered(GhRunner),
    Absent(GhRunnerAbsence),
}

impl GhRunnerDisplayRow {
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            GhRunnerDisplayRow::Registered(r) => &r.name,
            GhRunnerDisplayRow::Absent(a) => &a.name,
        }
    }

    #[must_use]
    pub fn os(&self) -> RunnerOs {
        match self {
            GhRunnerDisplayRow::Registered(r) => r.os,
            GhRunnerDisplayRow::Absent(a) => a.os,
        }
    }
}

/// Everything one successful org-runners fetch produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterUpdate {
    pub runners: Vec<GhRunner>,
    pub summary: RunnerSummary,
    pub roster: Vec<RunnerRosterEntry>,
    pub absent: Vec<GhRunnerAbsence>,
}

/// Fold a **successful** registered-runners fetch into the roster.
///
/// There is deliberately no failure counterpart: a failed fetch is handled by
/// not calling this at all, which leaves the caller's roster and absence list
/// exactly as they were. Every absence clock therefore reads from the last
/// successful poll, not from wall time.
#[must_use]
pub fn apply_fetch(
    roster: &[RunnerRosterEntry],
    registered: &[GhRunner],
    now: DateTime<Utc>,
    grace_secs: i64,
) -> RosterUpdate {
    let roster = updated(roster, registered, now);
    let absent = absences(&roster, registered, now, grace_secs);
    RosterUpdate {
        summary: crate::runners::summarize(registered),
        runners: registered.to_vec(),
        roster,
        absent,
    }
}

/// Upsert a sighting for every registered name, keep absent entries untouched,
/// and age out long-gone ones. Sorted by name.
#[must_use]
pub fn updated(
    roster: &[RunnerRosterEntry],
    registered: &[GhRunner],
    now: DateTime<Utc>,
) -> Vec<RunnerRosterEntry> {
    // A BTreeMap both de-duplicates by name (last wins, as the original
    // `uniquingKeysWith` does) and yields the by-name ordering for free.
    let mut by_name: BTreeMap<String, RunnerRosterEntry> = roster
        .iter()
        .map(|entry| (entry.name.clone(), entry.clone()))
        .collect();
    for runner in registered {
        by_name.insert(
            runner.name.clone(),
            RunnerRosterEntry {
                name: runner.name.clone(),
                os: runner.os,
                last_seen: now,
            },
        );
    }
    by_name
        .into_values()
        .filter(|entry| (now - entry.last_seen).num_seconds() < AGE_OUT_SECS)
        .collect()
}

/// Roster entries missing from the registered list, as presence states. Sorted
/// by name.
#[must_use]
pub fn absences(
    roster: &[RunnerRosterEntry],
    registered: &[GhRunner],
    now: DateTime<Utc>,
    grace_secs: i64,
) -> Vec<GhRunnerAbsence> {
    let registered_names: HashSet<&str> = registered.iter().map(|r| r.name.as_str()).collect();
    let mut absent: Vec<GhRunnerAbsence> = roster
        .iter()
        .filter(|entry| !registered_names.contains(entry.name.as_str()))
        .map(|entry| GhRunnerAbsence {
            name: entry.name.clone(),
            os: entry.os,
            state: presence::state(false, entry.last_seen, now, grace_secs),
        })
        .collect();
    absent.sort_by(|a, b| a.name.cmp(&b.name));
    absent
}

/// Drop one remembered runner immediately — the right-click "Forget" for a
/// decommissioned name that shouldn't wait out the 24h age-out.
#[must_use]
pub fn forget(roster: &[RunnerRosterEntry], name: &str) -> Vec<RunnerRosterEntry> {
    roster
        .iter()
        .filter(|entry| entry.name != name)
        .cloned()
        .collect()
}

/// Registered + absent merged in the panel's display order: macOS first, then
/// Linux, then other; name within — so an absent runner holds the exact slot it
/// occupied while registered.
#[must_use]
pub fn display_rows(
    registered: &[GhRunner],
    absent: &[GhRunnerAbsence],
) -> Vec<GhRunnerDisplayRow> {
    let mut rows: Vec<GhRunnerDisplayRow> = registered
        .iter()
        .cloned()
        .map(GhRunnerDisplayRow::Registered)
        .chain(absent.iter().cloned().map(GhRunnerDisplayRow::Absent))
        .collect();
    rows.sort_by(|a, b| {
        a.os()
            .rank()
            .cmp(&b.os().rank())
            .then_with(|| natural_cmp(a.name(), b.name()))
    });
    rows
}

/// Digit-aware name ordering — the stand-in for the original's
/// `localizedStandardCompare`, which is what keeps `mac-s2` ahead of `mac-s10`
/// instead of sorting them as the strings "2" and "10".
///
/// ASCII-scoped on purpose: runner names are machine-assigned slugs
/// (`mac-s1`, `ubu-01`), not user prose, so full Unicode collation would buy
/// nothing this input can exercise.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut left = a.chars().peekable();
    let mut right = b.chars().peekable();
    loop {
        match (left.peek().copied(), right.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let (lx, rx) = (take_digits(&mut left), take_digits(&mut right));
                // Longer run of significant digits == larger number; equal
                // lengths compare lexically, which for digits is numerically.
                let ord = lx.len().cmp(&rx.len()).then_with(|| lx.cmp(&rx));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                left.next();
                right.next();
                let ord = x
                    .to_ascii_lowercase()
                    .cmp(&y.to_ascii_lowercase())
                    .then_with(|| x.cmp(&y));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
        }
    }
}

/// Consume one run of digits, dropping leading zeros so `007` and `7` compare
/// as the same magnitude.
fn take_digits(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut digits = String::new();
    while let Some(c) = chars.peek().copied() {
        if !c.is_ascii_digit() {
            break;
        }
        chars.next();
        if !(digits.is_empty() && c == '0') {
            digits.push(c);
        }
    }
    digits
}

/// Convenience for the common case: the shipped 5-minute grace.
#[must_use]
pub fn absences_with_default_grace(
    roster: &[RunnerRosterEntry],
    registered: &[GhRunner],
    now: DateTime<Utc>,
) -> Vec<GhRunnerAbsence> {
    absences(roster, registered, now, DEFAULT_GRACE_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runners::RunnerState;
    use chrono::TimeDelta;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(4_000_000, 0).expect("valid timestamp")
    }

    fn runner(name: &str, os: RunnerOs) -> GhRunner {
        GhRunner {
            id: 1,
            name: name.to_string(),
            os,
            state: RunnerState::Idle,
        }
    }

    fn entry(name: &str, os: RunnerOs, absent_for: i64) -> RunnerRosterEntry {
        RunnerRosterEntry {
            name: name.to_string(),
            os,
            last_seen: now() - TimeDelta::seconds(absent_for),
        }
    }

    #[test]
    fn updated_learns_and_refreshes_registered_runners() {
        let roster = updated(
            &[entry("mac-s1", RunnerOs::MacOs, 900)],
            &[
                runner("mac-s1", RunnerOs::MacOs),
                runner("ubu-1", RunnerOs::Linux),
            ],
            now(),
        );
        assert_eq!(
            roster.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["mac-s1", "ubu-1"]
        );
        // Every registered name is a sighting.
        assert!(roster.iter().all(|e| e.last_seen == now()));
    }

    /// A de-registered ephemeral runner stays remembered — that memory is the
    /// entire point of the roster.
    #[test]
    fn updated_keeps_absent_entries_unchanged() {
        let absent = entry("mac-s2", RunnerOs::MacOs, 120);
        assert_eq!(
            updated(std::slice::from_ref(&absent), &[], now()),
            vec![absent]
        );
    }

    #[test]
    fn updated_ages_out_entries_absent_beyond_a_day() {
        let roster = updated(
            &[
                entry("stale", RunnerOs::MacOs, 86_400),
                entry("fresh", RunnerOs::MacOs, 86_399),
            ],
            &[],
            now(),
        );
        // 24h absent → silently forgotten (rotated hash names).
        assert_eq!(
            roster.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["fresh"]
        );
    }

    #[test]
    fn absences_are_empty_when_everything_is_registered() {
        let absent = absences_with_default_grace(
            &[entry("mac-s1", RunnerOs::MacOs, 0)],
            &[runner("mac-s1", RunnerOs::MacOs)],
            now(),
        );
        assert!(absent.is_empty());
    }

    #[test]
    fn absences_escalate_recycling_to_missing_across_grace() {
        let recycling =
            absences_with_default_grace(&[entry("mac-s2", RunnerOs::MacOs, 90)], &[], now());
        assert_eq!(
            recycling.first().map(|a| a.state),
            Some(PresenceState::Recycling { absence_secs: 90 })
        );

        let missing =
            absences_with_default_grace(&[entry("mac-s2", RunnerOs::MacOs, 420)], &[], now());
        assert_eq!(
            missing.first().map(|a| a.state),
            Some(PresenceState::Missing { absence_secs: 420 })
        );
    }

    #[test]
    fn absences_carry_the_roster_os() {
        let absent =
            absences_with_default_grace(&[entry("ubu-x", RunnerOs::Linux, 60)], &[], now());
        assert_eq!(absent.first().map(|a| a.os), Some(RunnerOs::Linux));
    }

    #[test]
    fn display_rows_interleave_by_os_rank_then_name() {
        let rows = display_rows(
            &[
                runner("mac-s1", RunnerOs::MacOs),
                runner("ubu-2", RunnerOs::Linux),
            ],
            &[
                GhRunnerAbsence {
                    name: "mac-s2".into(),
                    os: RunnerOs::MacOs,
                    state: PresenceState::Recycling { absence_secs: 60 },
                },
                GhRunnerAbsence {
                    name: "ubu-1".into(),
                    os: RunnerOs::Linux,
                    state: PresenceState::Missing { absence_secs: 400 },
                },
            ],
        );
        // macOS before Linux, name within — absent rows hold their slot.
        assert_eq!(
            rows.iter()
                .map(GhRunnerDisplayRow::name)
                .collect::<Vec<_>>(),
            vec!["mac-s1", "mac-s2", "ubu-1", "ubu-2"]
        );
    }

    /// Plain string ordering puts `mac-s10` before `mac-s2`; the panel's order
    /// is digit-aware, so it must not.
    #[test]
    fn display_rows_order_numeric_suffixes_by_magnitude() {
        let rows = display_rows(
            &[
                runner("mac-s10", RunnerOs::MacOs),
                runner("mac-s2", RunnerOs::MacOs),
                runner("mac-s1", RunnerOs::MacOs),
            ],
            &[],
        );
        assert_eq!(
            rows.iter()
                .map(GhRunnerDisplayRow::name)
                .collect::<Vec<_>>(),
            vec!["mac-s1", "mac-s2", "mac-s10"]
        );
    }

    #[test]
    fn forget_drops_one_remembered_name() {
        let roster = [
            entry("mac-s1", RunnerOs::MacOs, 0),
            entry("mac-s2", RunnerOs::MacOs, 60),
        ];
        let kept = forget(&roster, "mac-s2");
        assert_eq!(
            kept.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["mac-s1"]
        );
    }

    /// The roster is persisted between launches, so it has to survive a JSON
    /// round trip byte-for-byte.
    #[test]
    fn roster_entry_round_trips_through_json() {
        let original = vec![
            entry("mac-s1", RunnerOs::MacOs, 0),
            entry("ubu-1", RunnerOs::Linux, 60),
        ];
        let json = serde_json::to_string(&original).expect("encode");
        let decoded: Vec<RunnerRosterEntry> = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, original);
    }

    // MARK: - The clock-freeze contract

    /// The load-bearing rule: absence clocks advance only on a **successful**
    /// fetch. Two successful fetches an hour apart escalate an absent runner
    /// from recycling to missing…
    #[test]
    fn a_successful_fetch_advances_the_absence_clock() {
        let first = apply_fetch(
            &[],
            &[runner("mac-s2", RunnerOs::MacOs)],
            now(),
            DEFAULT_GRACE_SECS,
        );
        assert!(first.absent.is_empty(), "it was registered on this poll");

        let later = now() + TimeDelta::seconds(3_600);
        let second = apply_fetch(&first.roster, &[], later, DEFAULT_GRACE_SECS);
        assert_eq!(
            second.absent.first().map(|a| a.state),
            Some(PresenceState::Missing {
                absence_secs: 3_600
            })
        );
    }

    /// …but a failed fetch calls nothing, so an hour of GitHub being down must
    /// leave the very same state a moment ago produced. An outage must never
    /// age a healthy runner into a red alarm.
    #[test]
    fn a_failed_fetch_freezes_the_absence_clock() {
        let seeded = apply_fetch(
            &[],
            &[runner("mac-s2", RunnerOs::MacOs)],
            now(),
            DEFAULT_GRACE_SECS,
        );
        let observed = apply_fetch(
            &seeded.roster,
            &[],
            now() + TimeDelta::seconds(90),
            DEFAULT_GRACE_SECS,
        );
        assert_eq!(
            observed.absent.first().map(|a| a.state),
            Some(PresenceState::Recycling { absence_secs: 90 })
        );

        // GitHub is unreachable for the next hour: `apply_fetch` is not called,
        // so the caller re-renders exactly `observed` — nothing ages.
        let after_outage = observed.clone();
        assert_eq!(after_outage, observed);
        assert_eq!(
            after_outage.absent.first().map(|a| a.state),
            Some(PresenceState::Recycling { absence_secs: 90 }),
            "a failing source must not age a runner toward a false alarm"
        );
    }

    #[test]
    fn apply_fetch_summarises_the_registered_runners() {
        let update = apply_fetch(
            &[],
            &[
                runner("mac-s1", RunnerOs::MacOs),
                runner("ubu-1", RunnerOs::Linux),
            ],
            now(),
            DEFAULT_GRACE_SECS,
        );
        assert_eq!(update.summary.total, 2);
        assert_eq!(update.summary.idle, 2);
        assert_eq!(update.roster.len(), 2);
    }
}
