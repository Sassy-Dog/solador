//! Reconnect pacing.
//!
//! Rust port of the backoff arithmetic in `OpenClawService.reconnectLoop`
//! (`DevCanopy/Services/OpenClaw/OpenClawService.swift`). Pure and clock-free:
//! the caller reports how the session ended and gets back how long to wait, so
//! every rule here is testable without a socket or a timer.
//!
//! Three rules, each earning its place:
//!
//! - **Exponential, floored and capped.** 0.5s doubling to 30s.
//! - **Reset only after a session that actually lived.** A gateway that accepts
//!   the handshake and immediately drops would otherwise reset the backoff on
//!   every cycle and get hammered at the 0.5s floor forever. Only a session
//!   that survived [`STABLE_SESSION`] past `hello-ok` counts as healthy.
//! - **Pairing waits on a human.** `PAIRING_REQUIRED` means an operator has to
//!   run `openclaw devices approve`; retrying fast is pointless and floods both
//!   sides' logs, so it gets a fixed, long delay and leaves the exponential
//!   state untouched.

use std::time::Duration;

/// First delay after an unhealthy session, and the value a healthy one resets
/// to.
pub const INITIAL_BACKOFF: Duration = Duration::from_millis(500);
/// Ceiling for the exponential growth.
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Fixed delay while device pairing is pending approval.
pub const PAIRING_BACKOFF: Duration = Duration::from_secs(15);
/// How long a session must live past `hello-ok` to count as healthy.
pub const STABLE_SESSION: Duration = Duration::from_secs(10);

/// How a session ended, as far as pacing is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionOutcome {
    /// The gateway is waiting on an out-of-band approval.
    PairingRequired,
    /// Anything else — a clean close, a handshake failure, a dropped socket.
    ///
    /// `connected_for` is how long the session ran *after* `hello-ok`, or
    /// `None` when it never got that far.
    Ended { connected_for: Option<Duration> },
}

/// The reconnect delay state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    current: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff::new()
    }
}

impl Backoff {
    #[must_use]
    pub fn new() -> Self {
        Backoff {
            current: INITIAL_BACKOFF,
        }
    }

    /// Advance the policy for a session that ended with `outcome`, and return
    /// how long to wait before reconnecting.
    pub fn delay_after(&mut self, outcome: SessionOutcome) -> Duration {
        match outcome {
            // Deliberately does not touch `current`: a pending approval says
            // nothing about whether the network is healthy, so the exponential
            // state that was building before it must survive.
            SessionOutcome::PairingRequired => PAIRING_BACKOFF,
            SessionOutcome::Ended { connected_for } => {
                self.current = if connected_for.is_some_and(|lived| lived > STABLE_SESSION) {
                    INITIAL_BACKOFF
                } else {
                    (self.current * 2).min(MAX_BACKOFF)
                };
                self.current
            }
        }
    }

    /// The delay the next unhealthy session would double from.
    #[must_use]
    pub fn current(&self) -> Duration {
        self.current
    }

    /// Back to the floor — for a deliberate restart (a Settings change), which
    /// is not a failure and should reconnect immediately.
    pub fn reset(&mut self) {
        self.current = INITIAL_BACKOFF;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ended(connected_for: Option<Duration>) -> SessionOutcome {
        SessionOutcome::Ended { connected_for }
    }

    #[test]
    fn constants_match_the_swift_reconnect_loop() {
        assert_eq!(INITIAL_BACKOFF, Duration::from_millis(500));
        assert_eq!(MAX_BACKOFF, Duration::from_secs(30));
        assert_eq!(PAIRING_BACKOFF, Duration::from_secs(15));
        assert_eq!(STABLE_SESSION, Duration::from_secs(10));
    }

    #[test]
    fn a_failing_session_doubles_up_to_the_ceiling() {
        let mut backoff = Backoff::new();
        assert_eq!(backoff.current(), INITIAL_BACKOFF);

        let delays: Vec<Duration> = (0..8).map(|_| backoff.delay_after(ended(None))).collect();
        assert_eq!(
            delays,
            [
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(16),
                Duration::from_secs(30),
                Duration::from_secs(30),
                Duration::from_secs(30),
            ],
            "first retry waits 1s, not the 0.5s floor, and it caps at 30s"
        );
    }

    #[test]
    fn a_short_lived_connection_keeps_the_backoff_climbing() {
        // The rule that matters: a gateway that accepts the handshake then
        // drops must NOT reset the backoff, or we hammer it at the floor.
        let mut backoff = Backoff::new();
        backoff.delay_after(ended(None));
        let delay = backoff.delay_after(ended(Some(Duration::from_secs(3))));
        assert_eq!(delay, Duration::from_secs(2));

        // Exactly at the threshold is still not "lived a while" — the Swift
        // comparison is strictly greater-than.
        let delay = backoff.delay_after(ended(Some(STABLE_SESSION)));
        assert_eq!(delay, Duration::from_secs(4));
    }

    #[test]
    fn a_session_that_lived_resets_to_the_floor() {
        let mut backoff = Backoff::new();
        for _ in 0..5 {
            backoff.delay_after(ended(None));
        }
        assert_eq!(backoff.current(), Duration::from_secs(16));

        let delay = backoff.delay_after(ended(Some(Duration::from_secs(11))));
        assert_eq!(delay, INITIAL_BACKOFF);
        assert_eq!(backoff.current(), INITIAL_BACKOFF);
    }

    #[test]
    fn pairing_is_a_fixed_wait_that_does_not_disturb_the_ladder() {
        let mut backoff = Backoff::new();
        backoff.delay_after(ended(None));
        backoff.delay_after(ended(None));
        assert_eq!(backoff.current(), Duration::from_secs(2));

        for _ in 0..3 {
            assert_eq!(
                backoff.delay_after(SessionOutcome::PairingRequired),
                PAIRING_BACKOFF
            );
        }
        assert_eq!(
            backoff.current(),
            Duration::from_secs(2),
            "waiting on a human says nothing about the network"
        );
        assert_eq!(backoff.delay_after(ended(None)), Duration::from_secs(4));
    }

    #[test]
    fn reset_returns_to_the_floor() {
        let mut backoff = Backoff::new();
        for _ in 0..4 {
            backoff.delay_after(ended(None));
        }
        backoff.reset();
        assert_eq!(backoff.current(), INITIAL_BACKOFF);
        assert_eq!(backoff.delay_after(ended(None)), Duration::from_secs(1));
    }
}
