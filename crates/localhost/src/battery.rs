//! Battery readout.
//!
//! Deliberately the wire contract's floor — `level` and `isCharging`, the only
//! two fields a generic host agent can produce (see the `wire::Battery` doc
//! comment and `tests/fixtures/battery_contract.json`). The original collector
//! enriches its local reading with cycle count, health, wattage and time
//! remaining via IOKit; none of that is portable, and none of it is on the wire,
//! so none of it is collected here.

use starship_battery::{Manager, State};
use wire::Battery;

/// Reads the first battery the platform reports.
///
/// `None` when there is no battery (desktops, most CI runners) or the platform
/// refused to say — the same `nil` the original collector returns when
/// `getBatteryInfo()` finds no power source, and what `wire::Snapshot::battery`
/// being an `Option` already means.
pub(crate) fn read() -> Option<Battery> {
    let manager = Manager::new().ok()?;
    let battery = manager.batteries().ok()?.next()?.ok()?;
    lower(
        f64::from(battery.state_of_charge().value),
        f64::from(battery.energy_full().value),
        battery.state(),
    )
}

/// Decides whether a platform entry is a battery at all, and lowers it onto the
/// wire.
///
/// IOKit publishes an `AppleSmartBattery` service even on a desktop Mac, with
/// `BatteryInstalled = No` and zero capacities, and starship-battery returns it
/// as a real battery whose state of charge is `0 / 0 = NaN`. A battery that
/// cannot report a capacity is no battery — `None`, never a fabricated level
/// (`NaN` would serialise as `null` and not decode back into an `f64`).
///
/// Zero `energy_full` is refused even when the ratio is finite: starship-battery
/// clamps the ratio, so a zero-capacity entry with current charge reads as a
/// plausible 1.0. A Linux battery that reports only a percentage (no energy
/// figures) also lands here — accepted, since the cockpit ships on macOS and
/// Windows and a level with no capacity behind it cannot be vouched for.
fn lower(state_of_charge: f64, energy_full: f64, state: State) -> Option<Battery> {
    if !state_of_charge.is_finite() || energy_full == 0.0 {
        return None;
    }
    Some(Battery {
        // `state_of_charge` is a 0.0–1.0 ratio; the wire contract is 0–100, the
        // same scale `BatteryMetrics.level` carries.
        level: state_of_charge * 100.0,
        is_charging: state == State::Charging,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_full_energy_is_no_battery() {
        assert!(lower(0.5, 0.0, State::Empty).is_none());
        assert!(lower(1.0, 0.0, State::Full).is_none());
    }

    #[test]
    fn nan_state_of_charge_is_no_battery() {
        assert!(lower(f64::NAN, 0.0, State::Empty).is_none());
        assert!(lower(f64::NAN, 50.0, State::Empty).is_none());
        assert!(lower(f64::INFINITY, 50.0, State::Full).is_none());
    }

    #[test]
    fn a_real_reading_lowers_to_a_percentage() {
        let b = lower(0.5, 50.0, State::Charging).expect("a real battery");
        assert_eq!(b.level, 50.0);
        assert!(b.is_charging);
        assert!(!lower(0.5, 50.0, State::Discharging).unwrap().is_charging);
    }
}
