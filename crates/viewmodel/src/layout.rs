//! Layout policy. Values, not rendering — the shell applies these via CSS.
//!
//! Ports `coreColumns` and the `coreRowSpan * coreRowUnit` block arithmetic
//! from `HostMetricsPanel.swift`, and generalises the first: instead of one
//! column count for all widths, a ladder of every count that divides the core
//! count evenly, so the last row is never an orphan at any width.

/// Height of one cockpit "section row" (`HostMetricsPanel.coreRowUnit`).
pub const CORE_ROW_UNIT: f64 = 110.0;
/// Default `@AppStorage("coreRowSpan")`.
pub const CORE_ROW_SPAN_DEFAULT: usize = 2;
pub const CORE_GAP: f64 = 8.0;
/// Narrowest legible core cell; below this the `Core N xx%` label truncates.
pub const CORE_MIN_CELL: f64 = 104.0;

/// Samples retained. Deliberately larger than any chart can show, so widening
/// a chart reveals more history rather than stretching the same samples.
pub const HISTORY_CAPACITY: usize = 600;
/// On-screen width of one sample, held constant at every chart width.
pub const PX_PER_SAMPLE: f64 = 4.0;

/// Every column count that divides `count` evenly, with the container width
/// each needs. Ascending by width.
pub fn core_column_ladder(count: usize, min_cell: f64, gap: f64) -> Vec<(f64, usize)> {
    if count == 0 {
        return vec![];
    }
    let mut l: Vec<(f64, usize)> = (1..=count)
        .filter(|d| count.is_multiple_of(*d))
        .map(|d| (d as f64 * min_cell + (d.saturating_sub(1)) as f64 * gap, d))
        .collect();
    // `total_cmp`, not `partial_cmp(..).unwrap()`: the latter panics the
    // moment any width is NaN. No caller produces one today, but this is
    // public API and a total order costs nothing.
    l.sort_by(|a, b| a.0.total_cmp(&b.0));
    l
}

/// The cores block reserves a fixed height regardless of core count, so host
/// cards line up in the cockpit grid. Cells divide it; the block never grows.
pub fn core_block_height(row_span: usize) -> f64 {
    row_span.max(1) as f64 * CORE_ROW_UNIT
}

pub fn core_cell_height(block_height: f64, rows: usize, gap: f64) -> f64 {
    let rows = rows.max(1);
    (block_height - gap * (rows - 1) as f64) / rows as f64
}

pub fn core_visual_rows(count: usize, cols: usize) -> usize {
    if cols == 0 {
        return 1;
    }
    count.div_ceil(cols).max(1)
}

/// How many retained samples fit `width_px` at fixed density.
pub fn visible_samples(width_px: f64, px_per_sample: f64, retained: usize) -> usize {
    if px_per_sample <= 0.0 || width_px <= 0.0 {
        return retained.min(2);
    }
    ((width_px / px_per_sample).floor().max(2.0) as usize).min(retained)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_only_yields_column_counts_that_leave_a_full_last_row() {
        for count in [4usize, 8, 10, 12, 16, 20, 24, 32, 64] {
            for (_, cols) in core_column_ladder(count, CORE_MIN_CELL, CORE_GAP) {
                assert_eq!(count % cols, 0, "count {count} cols {cols}");
            }
        }
    }

    #[test]
    fn ladder_for_16_cores_matches_the_documented_rungs() {
        let l = core_column_ladder(16, 104.0, 8.0);
        assert_eq!(
            l,
            vec![(104.0, 1), (216.0, 2), (440.0, 4), (888.0, 8), (1784.0, 16)]
        );
    }

    #[test]
    fn block_height_is_fixed_regardless_of_core_count() {
        let h = core_block_height(CORE_ROW_SPAN_DEFAULT);
        assert_eq!(h, 220.0);
        for count in [4usize, 8, 16, 32, 64] {
            let cols = core_column_ladder(count, CORE_MIN_CELL, CORE_GAP)
                .into_iter()
                .filter(|(w, _)| *w <= 940.0)
                .map(|(_, c)| c)
                .max()
                .unwrap_or(1);
            let rows = core_visual_rows(count, cols);
            let cell = core_cell_height(h, rows, CORE_GAP);
            let total = cell * rows as f64 + CORE_GAP * (rows - 1) as f64;
            assert!(
                (total - h).abs() < 1e-9,
                "count {count}: block drifted to {total}"
            );
        }
    }

    #[test]
    fn cell_height_matches_the_swift_arithmetic() {
        let h = core_block_height(2);
        assert_eq!(core_cell_height(h, 1, CORE_GAP), 220.0);
        assert_eq!(core_cell_height(h, 2, CORE_GAP), 106.0);
        assert_eq!(core_cell_height(h, 4, CORE_GAP), 49.0);
    }

    #[test]
    fn wider_charts_show_more_time_not_stretched_pixels() {
        let narrow = visible_samples(400.0, PX_PER_SAMPLE, HISTORY_CAPACITY);
        let wide = visible_samples(1600.0, PX_PER_SAMPLE, HISTORY_CAPACITY);
        assert_eq!(narrow, 100);
        assert_eq!(wide, 400);
        assert!((400.0 / narrow as f64 - 1600.0 / wide as f64).abs() < 1e-9);
    }

    #[test]
    fn visible_window_is_clamped_to_the_buffer() {
        assert_eq!(
            visible_samples(999_999.0, PX_PER_SAMPLE, HISTORY_CAPACITY),
            HISTORY_CAPACITY
        );
        assert_eq!(visible_samples(0.0, PX_PER_SAMPLE, HISTORY_CAPACITY), 2);
    }

    /// A NaN width used to abort the whole render: `sort_by` reached
    /// `partial_cmp(..).unwrap()` on it and panicked. `total_cmp` is a total
    /// order, so the ladder still comes back -- every divisor present, in
    /// insertion order, because a stable sort leaves equal keys alone.
    #[test]
    fn a_nan_cell_width_sorts_instead_of_panicking() {
        let l = core_column_ladder(16, f64::NAN, CORE_GAP);
        let cols: Vec<usize> = l.iter().map(|(_, c)| *c).collect();
        assert_eq!(cols, vec![1, 2, 4, 8, 16]);
        assert!(l.iter().all(|(w, _)| w.is_nan()));
    }

    #[test]
    fn zero_cores_does_not_panic() {
        assert!(core_column_ladder(0, CORE_MIN_CELL, CORE_GAP).is_empty());
        assert_eq!(core_visual_rows(0, 0), 1);
    }
}
