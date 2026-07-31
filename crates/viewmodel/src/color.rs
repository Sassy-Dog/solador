//! CockpitTheme, verbatim from `DevCanopy/Views/Cockpit/CockpitTheme.swift`,
//! plus the chart hues from `HostMetricsPanel.swift`.

pub const PANEL: u32 = 0x0005_0805;
pub const PANEL_ALT: u32 = 0x000A_0F0C;
pub const LINE: u32 = 0x0013_301F;
pub const GREEN: u32 = 0x0033_D17A;
pub const AMBER: u32 = 0x00E0_9A26;
pub const RED: u32 = 0x00E0_5A4F;
pub const MUTED: u32 = 0x005A_6B60;
pub const INK: u32 = 0x00CF_E9D8;

pub const CPU: u32 = 0x005B_8DEF;
pub const MEM: u32 = 0x00B0_66F0;
pub const GPU: u32 = 0x0033_C7C7;
pub const READ: u32 = 0x003F_B950;
pub const WRITE: u32 = 0x00E0_922A;
pub const NET: u32 = 0x005B_8DEF;
pub const NET_UP: u32 = 0x009B_D34A;

/// The 10 cycling per-core hues.
pub const CORE_COLORS: [u32; 10] = [
    0x005B_8DEF,
    0x003F_B950,
    0x00E0_922A,
    0x00B0_66F0,
    0x00E0_584F,
    0x0033_C7C7,
    0x00E0_6AB0,
    0x009B_D34A,
    0x004F_B0E0,
    0x00D0_C24A,
];

pub fn hex(c: u32) -> String {
    format!(
        "#{:02x}{:02x}{:02x}",
        (c >> 16) & 0xFF,
        (c >> 8) & 0xFF,
        c & 0xFF
    )
}

pub fn usage_color(v: f64) -> u32 {
    if v < 70.0 {
        GREEN
    } else if v < 90.0 {
        AMBER
    } else {
        RED
    }
}

pub fn pressure_color(v: f64) -> u32 {
    if v < 60.0 {
        GREEN
    } else if v < 85.0 {
        AMBER
    } else {
        RED
    }
}

/// Volumes warn earlier than CPU — a full volume fails outright.
pub fn volume_color(pct: f64) -> u32 {
    if pct < 85.0 {
        GREEN
    } else if pct < 95.0 {
        AMBER
    } else {
        RED
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThermalState {
    Nominal,
    Fair,
    Serious,
    Critical,
}

impl ThermalState {
    /// The agent sends `thermalState` as an integer.
    pub fn from_wire(v: i64) -> Self {
        match v {
            0 => ThermalState::Nominal,
            1 => ThermalState::Fair,
            2 => ThermalState::Serious,
            _ => ThermalState::Critical,
        }
    }
}

/// Mirrors `thermalBadge`. Nominal and Fair are both green by design.
pub fn thermal_badge(s: ThermalState) -> (&'static str, u32) {
    match s {
        ThermalState::Nominal => ("Normal", GREEN),
        ThermalState::Fair => ("Fair", GREEN),
        ThermalState::Serious => ("Hot", AMBER),
        ThermalState::Critical => ("Critical", RED),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_renders_six_digit_lowercase() {
        assert_eq!(hex(GREEN), "#33d17a");
        assert_eq!(hex(PANEL), "#050805");
    }

    #[test]
    fn usage_thresholds_are_70_and_90() {
        assert_eq!(usage_color(69.9), GREEN);
        assert_eq!(usage_color(70.0), AMBER);
        assert_eq!(usage_color(89.9), AMBER);
        assert_eq!(usage_color(90.0), RED);
    }

    #[test]
    fn pressure_thresholds_are_60_and_85() {
        assert_eq!(pressure_color(59.9), GREEN);
        assert_eq!(pressure_color(60.0), AMBER);
        assert_eq!(pressure_color(85.0), RED);
    }

    #[test]
    fn volumes_warn_earlier_than_cpu_because_a_full_volume_fails() {
        assert_eq!(volume_color(84.9), GREEN);
        assert_eq!(volume_color(85.0), AMBER);
        assert_eq!(volume_color(95.0), RED);
        // 88% is amber for both, but a volume at 90% is amber while a CPU is red
        assert_eq!(volume_color(90.0), AMBER);
        assert_eq!(usage_color(90.0), RED);
    }

    #[test]
    fn nominal_and_fair_both_render_green() {
        assert_eq!(thermal_badge(ThermalState::Nominal), ("Normal", GREEN));
        assert_eq!(thermal_badge(ThermalState::Fair), ("Fair", GREEN));
        assert_eq!(thermal_badge(ThermalState::Serious), ("Hot", AMBER));
        assert_eq!(thermal_badge(ThermalState::Critical), ("Critical", RED));
    }

    /// `card::host_card` picks a core's hue with `CORE_COLORS[i % len]`, so
    /// "cycling" means two things, and this asserts both: the 10 hues are
    /// pairwise distinct (otherwise a shorter cycle would be invisible), and
    /// index 10 wraps onto index 0 while index 9 -- the last one before the
    /// wrap -- does not.
    ///
    /// The previous body compared `CORE_COLORS[0]` to
    /// `CORE_COLORS[10 % CORE_COLORS.len()]`; `10 % 10 == 0`, so it compared
    /// element zero to itself and could not fail.
    #[test]
    fn ten_core_hues_cycle() {
        assert_eq!(CORE_COLORS.len(), 10);

        for (i, a) in CORE_COLORS.iter().enumerate() {
            for (j, b) in CORE_COLORS.iter().enumerate().skip(i + 1) {
                assert_ne!(a, b, "cores {i} and {j} share a hue");
            }
        }

        let hue = |i: usize| CORE_COLORS[i % CORE_COLORS.len()];
        assert_eq!(hue(10), hue(0), "core 10 must wrap onto core 0's hue");
        assert_ne!(hue(9), hue(0), "core 9 is still inside the first cycle");
        assert_eq!(hue(11), hue(1));
        assert_eq!(hue(20), hue(0));
    }
}
