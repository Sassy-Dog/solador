//! Ported verbatim from the private methods on `HostMetricsPanel` (ported).
//! They were unreachable from tests there; here they are free functions.

/// One decimal below 100, integral above.
pub fn fmt(v: f64) -> String {
    if v >= 100.0 {
        format!("{}", v as i64)
    } else {
        format!("{v:.1}")
    }
}

/// MB/s up to 1000, then GB/s, so a burst can't widen the anchored legend.
pub fn fmt_rate(mbps: f64) -> String {
    if mbps >= 1000.0 {
        format!("{:.1} GB/s", mbps / 1024.0)
    } else {
        format!("{} MB/s", fmt(mbps))
    }
}

/// Collapses thousands to `k` so an auto-scaled axis never shifts the plot.
pub fn fmt_axis(v: f64) -> String {
    if v >= 1000.0 {
        format!("{:.0}k", v / 1000.0)
    } else {
        fmt(v)
    }
}

pub fn memory_label(mb: f64) -> String {
    if mb >= 1024.0 {
        format!("{} GB", fmt(mb / 1024.0))
    } else {
        format!("{} MB", mb.round() as i64)
    }
}

/// "12s" / "3m" / "1h" / "2d" — one unit, largest that fits.
///
/// The ladder the original spells twice: `PanelStatusFooter.relative` adds " ago" to
/// it, `Presence.duration` uses it bare ("recycling 40s"). One definition here,
/// so a panel footer and a presence row can never disagree about where a
/// minute becomes an hour.
pub fn duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// "12s ago" / "3m ago" / "1h ago" / "2d ago" — same bucket boundaries as
/// `PanelStatusFooter.relative` (ported), so a stale reading reads the same
/// way here as it does in the shipped app.
pub fn relative_age(secs: u64) -> String {
    format!("{} ago", duration(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_decimal_below_100_integral_above() {
        assert_eq!(fmt(18.24), "18.2");
        assert_eq!(fmt(412.0), "412");
    }

    #[test]
    fn rate_switches_to_gb_at_1000() {
        assert_eq!(fmt_rate(88.1), "88.1 MB/s");
        assert_eq!(fmt_rate(1024.0), "1.0 GB/s");
    }

    #[test]
    fn axis_collapses_thousands_so_the_column_never_widens() {
        assert_eq!(fmt_axis(17151.0), "17k");
        assert_eq!(fmt_axis(88.1), "88.1");
    }

    #[test]
    fn memory_label_switches_unit_at_1024_mb() {
        assert_eq!(memory_label(612.0), "612 MB");
        assert_eq!(memory_label(2150.0), "2.1 GB");
    }

    #[test]
    fn duration_is_relative_age_without_the_suffix() {
        for secs in [0, 59, 60, 3599, 3600, 86_399, 86_400, 200_000] {
            assert_eq!(relative_age(secs), format!("{} ago", duration(secs)));
        }
        assert_eq!(duration(40), "40s");
        assert_eq!(duration(720), "12m");
        assert_eq!(duration(7_200), "2h");
        assert_eq!(duration(200_000), "2d");
    }

    #[test]
    fn relative_age_buckets_at_minutes_hours_and_days() {
        assert_eq!(relative_age(0), "0s ago");
        assert_eq!(relative_age(59), "59s ago");
        assert_eq!(relative_age(60), "1m ago");
        assert_eq!(relative_age(3599), "59m ago");
        assert_eq!(relative_age(3600), "1h ago");
        assert_eq!(relative_age(86_399), "23h ago");
        assert_eq!(relative_age(86_400), "1d ago");
    }
}
