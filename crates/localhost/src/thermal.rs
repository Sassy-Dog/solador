//! Host thermal pressure.

/// How hard the machine is thermally throttling.
///
/// Mirrors `ProcessInfo.thermalState` — the source the original collector reads
/// (`HostMetricsCollector.mapThermalState`) — and carries the same integer
/// encoding the view-model decodes
/// (`crates/viewmodel/src/color.rs::ThermalState::from_wire`), so the local card
/// and a remote agent's card colour identically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThermalState {
    Nominal,
    Fair,
    Serious,
    Critical,
}

impl ThermalState {
    /// This state as `wire::Cpu::thermal_state`.
    #[must_use]
    pub fn to_wire(self) -> i64 {
        match self {
            ThermalState::Nominal => 0,
            ThermalState::Fair => 1,
            ThermalState::Serious => 2,
            ThermalState::Critical => 3,
        }
    }
}

/// Reads the host's thermal state, or `None` where the platform exposes none.
///
/// `None` is *unknown*, and is why [`crate::LocalCpu::thermal_state`] is an
/// `Option`: the wire contract encodes nominal as `0`, so a platform with no
/// thermal source that answered "0" would render a permanently reassuring
/// "Normal" badge on a machine nobody is measuring.
#[cfg(target_os = "macos")]
pub(crate) fn read() -> Option<ThermalState> {
    use objc2_foundation::{NSProcessInfo, NSProcessInfoThermalState};

    let state = NSProcessInfo::processInfo().thermalState();
    match state {
        NSProcessInfoThermalState::Nominal => Some(ThermalState::Nominal),
        NSProcessInfoThermalState::Fair => Some(ThermalState::Fair),
        NSProcessInfoThermalState::Serious => Some(ThermalState::Serious),
        NSProcessInfoThermalState::Critical => Some(ThermalState::Critical),
        // A state added by a future macOS. Unknown beats guessing "Nominal",
        // which is what `mapThermalState`'s `@unknown default` settles for and
        // the one place this crate improves on it.
        _ => None,
    }
}

/// Windows (and anything else) has no cheap `NSProcessInfo.thermalState`
/// equivalent: thermal zones live behind WMI/ACPI, which costs a WMI round trip
/// per sample and reports raw kelvin rather than a pressure level. Unknown until
/// a slice decides that trade is worth making.
#[cfg(not(target_os = "macos"))]
pub(crate) fn read() -> Option<ThermalState> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the encoding against `ThermalState::from_wire` in the view-model.
    /// Renumbering here silently recolours every thermal badge.
    #[test]
    fn the_wire_encoding_matches_the_view_models_decoder() {
        assert_eq!(ThermalState::Nominal.to_wire(), 0);
        assert_eq!(ThermalState::Fair.to_wire(), 1);
        assert_eq!(ThermalState::Serious.to_wire(), 2);
        assert_eq!(ThermalState::Critical.to_wire(), 3);
    }

    /// macOS reads a real state; every other platform is honest about having
    /// none. Both assertions are the *same* rule — a platform answers or admits
    /// it cannot — so both arms are checked wherever the tests run.
    #[test]
    fn a_platform_without_a_thermal_source_reports_unknown_not_nominal() {
        let state = read();
        if cfg!(target_os = "macos") {
            assert!(state.is_some(), "macOS reads NSProcessInfo.thermalState");
        } else {
            assert_eq!(
                state, None,
                "no thermal source must be unknown, never a fabricated Nominal"
            );
        }
    }
}
