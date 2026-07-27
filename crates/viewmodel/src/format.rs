//! Ported verbatim from the private methods on `HostMetricsPanel` (Swift).
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
}
