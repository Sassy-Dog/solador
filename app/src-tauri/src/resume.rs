//! Noticing that the machine woke up.
//!
//! Every poll task is suspended while a laptop sleeps, so the four slow loops —
//! GitHub (the store's refresh interval), usage (1h), Azure (4h) and OpenClaw —
//! come back on their own schedule. On 2026-08-07 that meant opening the lid to
//! a cockpit painting the previous night's runner list: all twelve runners were
//! online, the panel said ten, and the availability chip read a red
//! "Fleet Down" off data hours old. Waiting out a poll interval to find that
//! out is the whole problem.
//!
//! # Two clocks, and the gap between them
//!
//! Sleep is detected by comparing how much **wall-clock** time passed against
//! how much **monotonic** time passed. A process that ran normally sees the two
//! advance together; a process that was suspended sees the wall clock jump
//! ahead. Nothing platform-specific, no new dependency, and it works on Windows
//! for free.
//!
//! The property that makes this worth preferring to
//! `NSWorkspace.didWakeNotification` is that **it is correct whichever way the
//! monotonic clock behaves**, which is a genuinely contested detail on macOS:
//!
//! - If the monotonic clock *excludes* sleep, a timer armed before the nap
//!   still has most of its interval to run when the machine wakes. The gap
//!   appears here, the watchdog fires, and every loop is nudged.
//! - If it *includes* sleep, those timers are already past their deadline and
//!   tokio fires them the moment the process resumes — nothing needs nudging.
//!   The two clocks then agree, and this correctly stays silent.
//!
//! So the answer never has to be guessed at, and a future OS changing its mind
//! about it cannot break the feature in either direction.
//!
//! A wall clock that jumps for some *other* reason — an NTP correction after a
//! long offline stretch — reads as a resume too. That is the harmless direction
//! to be wrong in: the cost is one extra poll of each source.

use std::time::{Duration, Instant, SystemTime};

/// How far the two clocks must diverge before this calls it a resume.
///
/// Comfortably above any scheduling jitter a 2s tick can accumulate (that is
/// milliseconds), and comfortably below the shortest nap worth reacting to. A
/// suspend of less than this leaves the data barely stale anyway.
pub const RESUME_GAP: Duration = Duration::from_secs(20);

/// How often [`resumed`] is sampled.
///
/// Two seconds, so a lid-open is noticed about as fast as a human can focus on
/// the screen. Deliberately not sixty: the cadence table in `main.rs` forbids a
/// fixed cadence equal to the default refresh interval, precisely so the two
/// can never be confused for one another.
pub const TICK: Duration = Duration::from_secs(2);

/// Whether the process was suspended between two samples.
///
/// `saturating_sub` rather than a signed difference: a wall clock that went
/// *backwards* (an NTP correction the other way) yields zero, never a huge
/// unsigned wrap that would read as a decade-long nap.
#[must_use]
pub fn resumed(monotonic: Duration, wall: Duration) -> bool {
    wall.saturating_sub(monotonic) >= RESUME_GAP
}

/// The two clocks, sampled together.
///
/// Held by the watchdog across ticks. `SystemTime` can fail to subtract when it
/// moves backwards, which [`Sample::since`] folds into a zero elapsed — the
/// same "degrade to the reading that cannot be alarming" rule the rest of this
/// codebase follows for clocks.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    monotonic: Instant,
    wall: SystemTime,
}

impl Sample {
    #[must_use]
    pub fn now() -> Self {
        Sample {
            monotonic: Instant::now(),
            wall: SystemTime::now(),
        }
    }

    /// Elapsed monotonic and wall time since `earlier`.
    #[must_use]
    pub fn since(self, earlier: Sample) -> (Duration, Duration) {
        (
            self.monotonic.saturating_duration_since(earlier.monotonic),
            self.wall
                .duration_since(earlier.wall)
                .unwrap_or(Duration::ZERO),
        )
    }

    /// Whether the process was suspended between `earlier` and this sample.
    #[must_use]
    pub fn resumed_since(self, earlier: Sample) -> bool {
        let (monotonic, wall) = self.since(earlier);
        resumed(monotonic, wall)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ordinary tick: both clocks advanced together, give or take the
    /// scheduling jitter a 2s timer accumulates.
    #[test]
    fn a_normal_tick_is_not_a_resume() {
        assert!(!resumed(TICK, TICK));
        assert!(!resumed(TICK, TICK + Duration::from_millis(40),));
        assert!(!resumed(Duration::ZERO, Duration::ZERO));
    }

    /// A night's sleep: the wall clock ran, the process did not.
    #[test]
    fn a_wall_clock_that_ran_ahead_is_a_resume() {
        assert!(resumed(TICK, Duration::from_secs(8 * 60 * 60)));
        assert!(resumed(Duration::ZERO, RESUME_GAP));
    }

    /// The other branch of the macOS question: if the monotonic clock counts
    /// sleep too, both advanced by the same eight hours and there is nothing to
    /// detect — the loops' own deadlines have already passed, and tokio fires
    /// them without help. Reporting a resume here would be harmless but wrong.
    #[test]
    fn a_monotonic_clock_that_also_counted_the_sleep_reports_nothing() {
        let night = Duration::from_secs(8 * 60 * 60);
        assert!(!resumed(night, night));
    }

    #[test]
    fn the_threshold_is_inclusive_and_one_second_under_it_is_not() {
        assert!(resumed(Duration::ZERO, RESUME_GAP));
        assert!(!resumed(
            Duration::ZERO,
            RESUME_GAP - Duration::from_secs(1)
        ));
    }

    /// A wall clock corrected *backwards* must not wrap into a decade-long nap.
    #[test]
    fn a_backwards_wall_clock_is_not_a_resume() {
        assert!(!resumed(Duration::from_secs(3600), Duration::ZERO));
        assert!(!resumed(Duration::from_secs(3600), Duration::from_secs(1)));
    }

    /// The sampled path, which is what the watchdog actually calls. Two samples
    /// taken back to back can never look like a resume.
    #[test]
    fn two_samples_taken_together_are_not_a_resume() {
        let first = Sample::now();
        let second = Sample::now();
        assert!(!second.resumed_since(first));
        let (monotonic, wall) = second.since(first);
        assert!(monotonic < RESUME_GAP && wall < RESUME_GAP);
    }

    /// `TICK` must stay clear of the default refresh interval — `main.rs`'s
    /// cadence table forbids a fixed cadence of 60s so the two can never be
    /// read as the same number.
    #[test]
    fn the_tick_is_not_confusable_with_the_default_refresh_interval() {
        assert_ne!(
            TICK.as_secs(),
            u64::from(store::settings::DEFAULT_REFRESH_INTERVAL_SECS)
        );
        assert!(
            TICK < RESUME_GAP,
            "a tick longer than the gap could miss one"
        );
    }
}
