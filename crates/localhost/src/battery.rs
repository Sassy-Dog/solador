//! Battery readout.
//!
//! Deliberately the wire contract's floor — `level` and `isCharging`, the only
//! two fields a generic host agent can produce (see the `wire::Battery` doc
//! comment and `tests/fixtures/battery_contract.json`). The Swift collector
//! enriches its local reading with cycle count, health, wattage and time
//! remaining via IOKit; none of that is portable, and none of it is on the wire,
//! so none of it is collected here.

use starship_battery::{Manager, State};
use wire::Battery;

/// Reads the first battery the platform reports.
///
/// `None` when there is no battery (desktops, most CI runners) or the platform
/// refused to say — the same `nil` the Swift collector returns when
/// `getBatteryInfo()` finds no power source, and what `wire::Snapshot::battery`
/// being an `Option` already means.
pub(crate) fn read() -> Option<Battery> {
    let manager = Manager::new().ok()?;
    let battery = manager.batteries().ok()?.next()?.ok()?;
    Some(Battery {
        // `state_of_charge` is a 0.0–1.0 ratio; the wire contract is 0–100, the
        // same scale `BatteryMetrics.level` carries.
        level: f64::from(battery.state_of_charge().value) * 100.0,
        is_charging: battery.state() == State::Charging,
    })
}
