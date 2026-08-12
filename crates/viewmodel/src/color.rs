//! The cockpit palette.
//!
//! Azulejo-derived: a cobalt ground, glaze-cream ink and terracotta warmth —
//! the tilework a *solador* lays. Ported from
//! `CockpitTheme` (a phosphor-green CRT scheme)
//! and re-toned here; the original app still carries the original.
//!
//! Two constants left blue behind: `CPU` and `NET` moved to turquoise because
//! a cobalt ground claims blue for itself, and a metric series the colour of
//! the panel behind it is not a series.
//!
//! `GPU`, `READ`, `WRITE`, `NET_UP` and `CORE_COLORS` are deliberately **not**
//! re-toned. They are legend swatches: `app.css` uses each exactly once as a
//! `.sw-*` background beside its own label, so they need to be distinct from
//! *each other*, not from the ground. Re-toning them would be churn without a
//! reader who benefits.

pub const BACKGROUND: u32 = 0x0000_0000;
pub const PANEL: u32 = 0x000B_1020;
pub const PANEL_ALT: u32 = 0x0013_1B30;
pub const LINE: u32 = 0x001C_2B4A;
pub const GREEN: u32 = 0x004E_C98A;
pub const GREEN_DIM: u32 = 0x002F_7A5C;
pub const AMBER: u32 = 0x00E0_A03A;
pub const RED: u32 = 0x00E0_614F;
pub const MUTED: u32 = 0x0080_90AC;
/// The mark's own ground (`brand/icon.svg`), borrowed back into the UI.
///
/// The rest of the palette went the other way — the mark was re-toned to these
/// constants rather than these constants to the mark, because they carry
/// meaning and it does not. This is the one value that travelled inward, and it
/// could because ink is not semantic: no threshold reads it, so nothing about
/// *good/warning/error* moves when it does. It reads 15.4:1 on `PANEL`, a shade
/// better than the `#E8E2D4` it replaced.
pub const INK: u32 = 0x00F5_E6CD;

pub const CPU: u32 = 0x003F_C8D4;
pub const MEM: u32 = 0x00A9_7CE8;
pub const GPU: u32 = 0x0033_C7C7;
pub const READ: u32 = 0x003F_B950;
pub const WRITE: u32 = 0x00E0_922A;
pub const NET: u32 = 0x003F_C8D4;
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
        // Synthetic inputs on purpose: this is a test of the *formatter*, and
        // feeding it palette constants made it fail on a re-palette for a
        // reason that had nothing to do with formatting.
        assert_eq!(hex(0x00AB_CDEF), "#abcdef");
        // Leading zeroes in any channel must survive.
        assert_eq!(hex(0x0000_0102), "#000102");
        // Black must survive as six zeroes, not collapse to "#0".
        assert_eq!(hex(BACKGROUND), "#000000");
    }

    /// `greenDim` is the resting/among-the-lines green — it must stay visibly
    /// darker than `green`, or the two stop reading as a pair.
    #[test]
    fn green_dim_is_darker_than_green() {
        let lum = |c: u32| ((c >> 16) & 0xFF) + ((c >> 8) & 0xFF) + (c & 0xFF);
        assert!(lum(GREEN_DIM) < lum(GREEN));
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

#[cfg(test)]
mod css_sync {
    use super::*;

    /// `app/ui/app.css` mirrors a handful of these constants for the two chrome
    /// rules that cannot read them through CSSOM (a border and a keyframe). The
    /// comment there says they must stay equal to the Rust constant — this is
    /// what makes that true rather than hoped.
    ///
    /// Worth stating why it is a test and not a convention: every colour that
    /// *carries meaning* is published from Rust at render time, so a drifted
    /// token does not break a panel loudly. It desynchronises one border from
    /// every value beside it, which is exactly the kind of defect that survives
    /// review and ships.
    #[test]
    fn the_css_mirror_matches_the_rust_constants() {
        let css = include_str!("../../../app/ui/app.css");
        for (token, value) in [
            ("--panel:", PANEL),
            ("--panelAlt:", PANEL_ALT),
            ("--line:", LINE),
            ("--green:", GREEN),
            ("--red:", RED),
            ("--muted:", MUTED),
            ("--ink:", INK),
            ("--read:", READ),
            ("--write:", WRITE),
            ("--net:", NET),
            ("--netup:", NET_UP),
        ] {
            let at = css
                .find(token)
                .unwrap_or_else(|| panic!("{token} is missing from app.css"));
            let got: String = css[at + token.len()..]
                .trim_start()
                .chars()
                .take(7)
                .collect();
            assert_eq!(
                got,
                hex(value),
                "{token} in app.css drifted from its Rust constant"
            );
        }
    }

    /// `app/ui/mark.svg` is a copy of `brand/mark.svg`, made by
    /// `scripts/generate-icons.sh`. The copy exists because the frontend's dist
    /// root is `app/ui` and the app's CSP is `img-src 'self' data:`, so it
    /// cannot reach out of the tree to `brand/`.
    ///
    /// A copy with nothing watching it is a copy that goes stale, and this one
    /// would go stale invisibly: the page keeps rendering, just the previous
    /// mark. This is the same guard the CSS mirror above provides, for the same
    /// reason -- see also the six drifted hex values recorded in
    /// `brand/README.md`, which is what an unwatched duplicate looks like.
    #[test]
    fn the_frontend_mark_matches_the_brand_mark() {
        assert_eq!(
            include_str!("../../../app/ui/mark.svg"),
            include_str!("../../../brand/mark.svg"),
            "app/ui/mark.svg has drifted from brand/mark.svg -- \
             re-run ./scripts/generate-icons.sh"
        );
    }

    /// The Playwright crons suite writes four of these out as literals,
    /// because two of them reach it under the same key and telling them apart
    /// in JS would mean re-deriving the panel's own logic there. That is a
    /// reasonable trade *only* while something fails when they drift — which
    /// is this.
    #[test]
    fn the_crons_spec_mirror_matches_the_rust_constants() {
        let js = include_str!("../../../tests/frontend/crons.spec.js");
        for (decl, value) in [
            ("const RED = ", RED),
            ("const AMBER = ", AMBER),
            ("const MUTED = ", MUTED),
            ("const GREEN_DIM = ", GREEN_DIM),
        ] {
            let at = js
                .find(decl)
                .unwrap_or_else(|| panic!("{decl} is missing from crons.spec.js"));
            let got: String = js[at + decl.len()..]
                .trim_start()
                .trim_start_matches('"')
                .chars()
                .take(7)
                .collect();
            assert_eq!(
                got,
                hex(value),
                "{decl}in crons.spec.js drifted from its Rust constant"
            );
        }
    }
}
