//! Cumulative-counter → per-second-rate conversion.
//!
//! The OS publishes network and disk I/O as *cumulative* byte totals, while the
//! wire contract publishes MiB/s. Every rate is therefore a delta against the
//! previous reading — and the first reading has no previous one, so it is
//! **unknown**, never `0.0`.
//!
//! That line is drawn the same way in `ProcessDiskIOSampler.bytesPerSecond`
//! (`Packages/HostMetricsKit/Sources/HostMetricsKit/ProcessDiskIOSampler.swift`):
//! a counter that did not move is a *measured* zero and honest to publish; a
//! counter with nothing to diff against is not a zero at all.

use std::time::{Duration, Instant};

const BYTES_PER_MIB: f64 = 1024.0 * 1024.0;

/// Cumulative byte totals for one two-way subsystem, read at a single instant.
///
/// `inbound` is bytes received (network) or read (disk); `outbound` is bytes
/// transmitted or written. One type serves both because the arithmetic is
/// identical and the wire contract pairs them the same way
/// (`wire::Network`, `wire::Disk`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Counters {
    pub(crate) inbound: u64,
    pub(crate) outbound: u64,
}

/// What one [`Counters`] delta worked out to, in MiB/s.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rates {
    pub(crate) inbound_mib_s: f64,
    pub(crate) outbound_mib_s: f64,
}

/// Holds the previous reading so the next one can become a rate.
#[derive(Debug, Default)]
pub(crate) struct RateTracker {
    previous: Option<(Counters, Instant)>,
}

impl RateTracker {
    /// Records `counters` as read at `at` and returns the rates since the
    /// previous reading.
    ///
    /// `None` — unknown, never a fabricated `0.0` — when:
    ///
    /// * there is no previous reading (the first sample);
    /// * no time elapsed between the two readings; or
    /// * either counter went backwards. An interface disappeared, a disk was
    ///   re-enumerated, or the counter wrapped: the cached total no longer
    ///   describes the same thing, so nothing honest can be derived from it.
    ///
    /// Unknown is all-or-nothing across the pair because the wire contract
    /// renders the pair together — half a rate would read as "the other
    /// direction was idle".
    ///
    /// The reading is stored either way, so the *next* call has a baseline even
    /// when this one came back unknown.
    pub(crate) fn update(&mut self, counters: Counters, at: Instant) -> Option<Rates> {
        let (previous, previous_at) = self.previous.replace((counters, at))?;
        let elapsed = at.checked_duration_since(previous_at)?;
        Some(Rates {
            inbound_mib_s: mib_per_second(counters.inbound, previous.inbound, elapsed)?,
            outbound_mib_s: mib_per_second(counters.outbound, previous.outbound, elapsed)?,
        })
    }
}

/// One cumulative-counter delta as MiB/s.
///
/// `None` when the counter went backwards, when no time elapsed, or when the
/// arithmetic did not land on a finite number. A counter that simply did not
/// move yields `Some(0.0)` — a measured zero, which is honest to publish.
fn mib_per_second(current: u64, previous: u64, elapsed: Duration) -> Option<f64> {
    let delta = current.checked_sub(previous)?;
    let seconds = elapsed.as_secs_f64();
    if seconds <= 0.0 {
        return None;
    }
    let rate = delta as f64 / seconds / BYTES_PER_MIB;
    rate.is_finite().then_some(rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    fn counters(inbound: u64, outbound: u64) -> Counters {
        Counters { inbound, outbound }
    }

    #[test]
    fn the_first_reading_has_no_rate_at_all() {
        let mut tracker = RateTracker::default();
        assert_eq!(tracker.update(counters(MIB, MIB), Instant::now()), None);
    }

    #[test]
    fn the_second_reading_rates_the_delta_not_the_total() {
        let t0 = Instant::now();
        let mut tracker = RateTracker::default();
        tracker.update(counters(10 * MIB, 4 * MIB), t0);

        let rates = tracker
            .update(counters(12 * MIB, 5 * MIB), t0 + Duration::from_secs(2))
            .expect("a second reading two seconds later is a rate");

        assert_eq!(rates.inbound_mib_s, 1.0);
        assert_eq!(rates.outbound_mib_s, 0.5);
    }

    /// The distinction the whole module exists for: a counter that did not move
    /// is a *measured* 0.0, and must not be conflated with the unknown above.
    #[test]
    fn an_unmoved_counter_is_a_measured_zero_not_an_unknown() {
        let t0 = Instant::now();
        let mut tracker = RateTracker::default();
        tracker.update(counters(7 * MIB, 7 * MIB), t0);

        let rates = tracker
            .update(counters(7 * MIB, 7 * MIB), t0 + Duration::from_secs(1))
            .expect("an idle interval is measured, not unknown");

        assert_eq!(rates.inbound_mib_s, 0.0);
        assert_eq!(rates.outbound_mib_s, 0.0);
    }

    #[test]
    fn a_backwards_counter_is_unknown_rather_than_a_huge_or_negative_rate() {
        let t0 = Instant::now();
        let mut tracker = RateTracker::default();
        tracker.update(counters(100 * MIB, 100 * MIB), t0);

        assert_eq!(
            tracker.update(counters(MIB, 100 * MIB), t0 + Duration::from_secs(1)),
            None
        );
    }

    /// One direction going backwards takes the pair down with it, rather than
    /// publishing the other direction beside a fabricated counterpart.
    #[test]
    fn one_backwards_direction_makes_the_whole_pair_unknown() {
        let t0 = Instant::now();
        let mut tracker = RateTracker::default();
        tracker.update(counters(100 * MIB, 100 * MIB), t0);

        assert_eq!(
            tracker.update(counters(200 * MIB, MIB), t0 + Duration::from_secs(1)),
            None
        );
    }

    #[test]
    fn two_readings_at_the_same_instant_are_unknown_not_infinite() {
        let t0 = Instant::now();
        let mut tracker = RateTracker::default();
        tracker.update(counters(0, 0), t0);

        assert_eq!(tracker.update(counters(MIB, MIB), t0), None);
    }

    /// A `None` result must not poison the tracker: the reading that could not
    /// be rated is still the baseline for the one after it.
    #[test]
    fn an_unknown_reading_still_rebaselines_for_the_next_one() {
        let t0 = Instant::now();
        let mut tracker = RateTracker::default();
        tracker.update(counters(100 * MIB, 100 * MIB), t0);
        // Backwards: unknown, but it re-baselines at 1 MiB.
        tracker.update(counters(MIB, MIB), t0 + Duration::from_secs(1));

        let rates = tracker
            .update(counters(3 * MIB, 2 * MIB), t0 + Duration::from_secs(2))
            .expect("the reading after an unknown one has a baseline again");

        assert_eq!(rates.inbound_mib_s, 2.0);
        assert_eq!(rates.outbound_mib_s, 1.0);
    }

    #[test]
    fn sub_second_intervals_scale_up_to_a_per_second_rate() {
        let t0 = Instant::now();
        let mut tracker = RateTracker::default();
        tracker.update(counters(0, 0), t0);

        let rates = tracker
            .update(counters(MIB, 2 * MIB), t0 + Duration::from_millis(500))
            .expect("half a second is still time elapsed");

        assert_eq!(rates.inbound_mib_s, 2.0);
        assert_eq!(rates.outbound_mib_s, 4.0);
    }
}
