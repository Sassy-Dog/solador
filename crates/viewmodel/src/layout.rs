//! Layout policy. Values, not rendering — the shell applies these via CSS.
//!
//! Ports `coreColumns` and the `coreRowSpan * coreRowUnit` block arithmetic
//! from `HostMetricsPanel`, and generalises the first: instead of one
//! column count for all widths, a ladder of every count `coreColumns` would
//! accept, so a wider container buys more columns without ever buying a
//! layout `coreColumns` itself would refuse.

/// Height of one cockpit "section row" (`HostMetricsPanel.coreRowUnit`).
pub const CORE_ROW_UNIT: f64 = 110.0;
/// Default `@AppStorage("coreRowSpan")`.
pub const CORE_ROW_SPAN_DEFAULT: usize = 2;
pub const CORE_GAP: f64 = 8.0;
/// Narrowest legible core cell; below this the `Core N xx%` label truncates.
pub const CORE_MIN_CELL: f64 = 104.0;
/// Widest per-core grid the cockpit will ask for (`coreColumns`' cap).
pub const CORE_MAX_COLUMNS: usize = 10;

/// Samples retained. Deliberately larger than any chart can show, so widening
/// a chart reveals more history rather than stretching the same samples.
pub const HISTORY_CAPACITY: usize = 600;
/// On-screen width of one sample, held constant at every chart width.
pub const PX_PER_SAMPLE: f64 = 4.0;

/// Every column count this grid may legibly use, with the container width each
/// needs. Ascending by width.
///
/// A rung is usable on exactly the terms [`core_columns`] documents: it divides
/// `count` evenly, it stays at or below `max_columns`, and it leaves >= 2 rows.
/// The two must agree because the ladder is what the shell actually renders --
/// `core_columns` only answers for a fixed width, so a rung it would never pick
/// is a layout no test of `core_columns` can reach. That gap is how a 10-core
/// Mac came to be painted as a single row of ten stretched cells on a wide
/// card while `core_columns` kept insisting the answer was 5 x 2.
///
/// Filtering alone would strand the primes: 11 has no proper divisor, so the
/// ladder would keep only its 1-column rung and render an 11-core M3 Pro as
/// eleven rows of one -- a worse grid than the one this fixes. So the top rung
/// is always [`core_columns`]'s answer, unioned in: it already knows the
/// least-waste, >= 2 rows, ties-toward-fewer-rows fallback, and restating that
/// here would be a second copy to drift.
///
/// `count == 1` keeps its lone 1-column rung -- one row is all there is, and
/// the >= 2 rows rule has nothing to say about it. `count == 0` has no ladder.
pub fn core_column_ladder(
    count: usize,
    min_cell: f64,
    gap: f64,
    max_columns: usize,
) -> Vec<(f64, usize)> {
    if count == 0 {
        return vec![];
    }
    let mut cols: Vec<usize> = (1..count)
        .filter(|d| count.is_multiple_of(*d) && *d <= max_columns)
        .collect();
    cols.push(core_columns(count, max_columns));
    cols.sort_unstable();
    cols.dedup();

    let mut l: Vec<(f64, usize)> = cols
        .into_iter()
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

/// The tightest core cell the original cockpit ever renders: 36 cores at its
/// fixed 9 columns is 4 rows in the 2-row-span block, `core_cell_height(220,
/// 4, 8)` = 49. Below this the label + padding consume the whole cell and the
/// sparkline's flexed remainder hits 0 — the chart vanishes while the
/// percentages keep rendering, which reads as working when it isn't.
pub const CORE_CELL_MIN_H: f64 = 49.0;

/// Height of the cores block on one ladder rung. the original can hold its block
/// fixed because its column count never drops below `core_columns`; the
/// ladder's narrow rungs produce row counts the original can't (6, 9, 12 ... rows),
/// where dividing the fixed block erases the plots. So: the fixed block while
/// cells stay at or above the squeeze floor, growing only past it.
pub fn core_rung_height(row_span: usize, rows: usize) -> f64 {
    let rows = rows.max(1) as f64;
    core_block_height(row_span).max(rows * CORE_CELL_MIN_H + (rows - 1.0) * CORE_GAP)
}

/// One rung of a core ladder: the container width it needs, the columns it
/// asks for, and the height the block takes there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoreRung {
    pub min_width: f64,
    pub cols: usize,
    pub height: f64,
}

/// Ladders for every core count on screen, with **one block height shared
/// across all of them at every width**.
///
/// [`core_rung_height`] is a function of a card's own row count, so two cards
/// of different core counts legitimately disagree: at a 900pt-class card a
/// 36-core host takes 6 rows (334pt) where a 16-core host takes 4 (220pt), and
/// every section below the block — Memory, Disk, Volumes, the process lists —
/// inherits that 114pt of skew. Cards in a cockpit row are equal width, so the
/// fix is to give each count *its own* columns (its grid must still be one
/// [`core_column_ladder`] would offer) but *everyone's* height: the max across
/// the counts present.
///
/// Only the height is shared. Taking the widest card's column count too would
/// hand a 36-core host a 16-core grid and orphan the last row, which is the
/// thing [`core_column_ladder`] exists to prevent.
///
/// The result is never shorter than any card's own rung — it is a `max` over
/// values that are themselves `max`es against [`CORE_CELL_MIN_H`] — so no card
/// is ever squeezed below the floor that keeps its sparklines visible. The
/// slack lands in the shorter card's cells, which `grid-auto-rows: 1fr` grows
/// into taller tiles rather than leaving as a void.
///
/// One count in gives exactly [`core_column_ladder`]'s answer with
/// [`core_rung_height`]'s heights — the single-card case is unchanged.
#[must_use]
pub fn aligned_core_ladders(
    counts: &[usize],
    row_span: usize,
    min_cell: f64,
    gap: f64,
    max_columns: usize,
) -> Vec<(usize, Vec<CoreRung>)> {
    let mut distinct: Vec<usize> = counts.iter().copied().filter(|c| *c > 0).collect();
    distinct.sort_unstable();
    distinct.dedup();
    if distinct.is_empty() {
        return vec![];
    }

    let ladders: Vec<(usize, Vec<(f64, usize)>)> = distinct
        .iter()
        .map(|&n| (n, core_column_ladder(n, min_cell, gap, max_columns)))
        .collect();

    // A card's layout only changes at one of ITS rung boundaries, but the
    // shared height changes at any card's — so every ladder is evaluated at
    // the union of them all.
    let mut widths: Vec<f64> = ladders
        .iter()
        .flat_map(|(_, rungs)| rungs.iter().map(|(w, _)| *w))
        .collect();
    widths.sort_by(f64::total_cmp);
    widths.dedup_by(|a, b| a == b);

    ladders
        .iter()
        .map(|(n, rungs)| {
            let at = |rungs: &[(f64, usize)], w: f64| {
                // The widest rung this width affords; below the narrowest, the
                // single-column rung is all there is.
                rungs
                    .iter()
                    .filter(|(rw, _)| *rw <= w)
                    .map(|(_, c)| *c)
                    .next_back()
                    .unwrap_or(1)
            };
            let out = widths
                .iter()
                .map(|&w| {
                    let cols = at(rungs, w);
                    let height = ladders
                        .iter()
                        .map(|(other, other_rungs)| {
                            let other_cols = at(other_rungs, w);
                            core_rung_height(row_span, core_visual_rows(*other, other_cols))
                        })
                        .fold(0.0_f64, f64::max);
                    CoreRung {
                        min_width: w,
                        cols,
                        height,
                    }
                })
                .collect();
            (*n, out)
        })
        .collect()
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

/// Chooses how many columns the per-core CPU grid uses for a given core count —
/// `coreColumns`, ported from `CoreGridLayout`.
/// Where [`core_column_ladder`] offers every rung so the shell can pick by
/// width, this picks the single count for a fixed-width grid.
///
/// Two goals:
///   1. Divide evenly so the last row is full — no stranded trailing cell.
///   2. Use **at least 2 rows**, so low-core hosts don't render as one giant
///      row of tall cells stretched over the fixed block height.
///
/// So: the **largest proper divisor of `core_count` that is <= `max_columns`
/// and leaves >= 2 rows**. Examples (`max_columns` = 10): 36 -> 9 (x4),
/// 16 -> 8 (x2), 12 -> 6 (x2), 10 -> 5 (x2), 8 -> 4 (x2), 9 -> 3 (x3),
/// 4 -> 2 (x2).
///
/// For prime core counts (5, 7, 11, 13) there is no usable divisor, so we fall
/// back to the column count with the least last-row waste, keeping >= 2 rows
/// and ties broken toward fewer rows: 13 -> 7 (x2), 11 -> 6 (x2), 7 -> 4 (x2).
/// This never collapses to a single row.
pub fn core_columns(core_count: usize, max_columns: usize) -> usize {
    // 1 or 2 cores can't sensibly fill two columns: a single column gives <= 2 rows.
    if core_count <= 2 {
        return 1;
    }

    // Largest proper divisor (rows = core_count / c >= 2) within the cap ->
    // fewest rows, no waste.
    let best = (1..=max_columns.min(core_count))
        .rfind(|c| core_count.is_multiple_of(*c) && core_count / c >= 2)
        .unwrap_or(0);
    if best >= 2 {
        return best;
    }

    // Prime core count: no even split. Least-waste columns that still leave
    // >= 2 rows.
    let mut best_columns = 2;
    let mut best_waste = usize::MAX;
    let mut best_rows = usize::MAX;
    for c in 2..=max_columns.max(2) {
        let rows = core_count.div_ceil(c);
        if rows < 2 {
            continue;
        }
        let waste = rows * c - core_count;
        if waste < best_waste || (waste == best_waste && rows < best_rows) {
            best_waste = waste;
            best_rows = rows;
            best_columns = c;
        }
    }
    best_columns
}

/// How many rows `count` volumes occupy with `columns` columns —
/// `VolumeGridLayout.rowCount`. Sibling host cards reserve the same number of
/// slots from this, which is what keeps their volume blocks aligned.
pub fn volume_row_count(count: usize, columns: usize) -> usize {
    if count == 0 {
        return 0;
    }
    if columns <= 1 {
        return count;
    }
    if count <= 1 {
        return 1;
    }
    if count.is_multiple_of(2) {
        count / 2
    } else {
        1 + (count - 1) / 2
    }
}

/// Splits importance-sorted volumes into rows of one (full width) or two —
/// `VolumeGridLayout.rows`.
///
/// One column -> each volume is its own full-width row. Two columns -> an odd
/// count gives the most-important (first) volume a full-width top row and pairs
/// the rest; an even count pairs all.
pub fn volume_rows<T: Clone>(volumes: &[T], columns: usize) -> Vec<Vec<T>> {
    if columns < 2 {
        return volumes.iter().map(|v| vec![v.clone()]).collect();
    }
    let mut rows: Vec<Vec<T>> = Vec::new();
    let rest = if volumes.len().is_multiple_of(2) {
        volumes
    } else {
        // most-important, full-width top row
        rows.push(vec![volumes[0].clone()]);
        &volumes[1..]
    };
    rows.extend(rest.chunks(2).map(<[T]>::to_vec));
    rows
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

    fn ladder(count: usize) -> Vec<(f64, usize)> {
        core_column_ladder(count, CORE_MIN_CELL, CORE_GAP, CORE_MAX_COLUMNS)
    }

    fn widest_rung(count: usize) -> usize {
        ladder(count).last().map_or(0, |(_, c)| *c)
    }

    /// This used to assert only "every rung divides evenly", which every
    /// divisor satisfies -- including `count` itself. That let the shell offer
    /// a 1-row grid of stretched cells on a wide card (a 10-core M1 Max as one
    /// row of ten) while `core_columns` was answering 5 x 2 and passing all its
    /// own tests. A full last row was never the whole rule: the rung must also
    /// fit under the cap and leave >= 2 rows, exactly as `core_columns` says.
    #[test]
    fn ladder_only_yields_column_counts_core_columns_would_accept() {
        for count in [4usize, 8, 10, 12, 16, 20, 24, 32, 64] {
            for (_, cols) in ladder(count) {
                assert_eq!(count % cols, 0, "count {count} cols {cols}");
                assert!(cols <= CORE_MAX_COLUMNS, "count {count} cols {cols}");
                assert!(
                    core_visual_rows(count, cols) >= 2,
                    "count {count} cols {cols} is a single row"
                );
            }
        }
    }

    /// The widest rung is the grid a maximally wide card renders, so it has to
    /// be the answer `core_columns` would give -- including for the primes,
    /// where the even-divisor filter leaves nothing but the 1-column rung.
    #[test]
    fn the_widest_rung_is_the_grid_core_columns_would_choose() {
        for (count, want) in [
            (8usize, 4usize),
            (10, 5),
            (12, 6),
            (16, 8),
            (36, 9),
            (11, 6),
            (7, 4),
            (4, 2),
            (2, 1),
            (1, 1),
        ] {
            assert_eq!(widest_rung(count), want, "count {count}");
            assert_eq!(
                widest_rung(count),
                core_columns(count, CORE_MAX_COLUMNS),
                "count {count} drifted from core_columns"
            );
        }
    }

    /// No width can buy a card a grid over the cap or a single stretched row.
    /// One core is the sole exception: it has one row and no second to reach.
    #[test]
    fn no_rung_exceeds_the_cap_or_collapses_to_one_row() {
        for count in 1..=64usize {
            let rungs = ladder(count);
            assert!(!rungs.is_empty(), "count {count} has no ladder");
            for (_, cols) in rungs {
                assert!(
                    cols <= CORE_MAX_COLUMNS,
                    "count {count} cols {cols} exceeds the cap"
                );
                if count > 1 {
                    assert!(
                        core_visual_rows(count, cols) >= 2,
                        "count {count} cols {cols} is a single row"
                    );
                }
            }
        }
        assert_eq!(ladder(1), vec![(CORE_MIN_CELL, 1)], "one core, one rung");
    }

    #[test]
    fn ladder_for_16_cores_matches_the_documented_rungs() {
        // 16 (one row) is gone; 8 x 2 is as wide as this grid legibly goes.
        let l = core_column_ladder(16, 104.0, 8.0, CORE_MAX_COLUMNS);
        assert_eq!(l, vec![(104.0, 1), (216.0, 2), (440.0, 4), (888.0, 8)]);
    }

    #[test]
    fn block_height_is_fixed_regardless_of_core_count() {
        let h = core_block_height(CORE_ROW_SPAN_DEFAULT);
        assert_eq!(h, 220.0);
        for count in [4usize, 8, 16, 32, 64] {
            let cols = ladder(count)
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
    fn cell_height_matches_the_original_arithmetic() {
        let h = core_block_height(2);
        assert_eq!(core_cell_height(h, 1, CORE_GAP), 220.0);
        assert_eq!(core_cell_height(h, 2, CORE_GAP), 106.0);
        assert_eq!(core_cell_height(h, 4, CORE_GAP), 49.0);
    }

    #[test]
    fn rung_height_holds_the_block_through_the_original_squeeze() {
        // 1..=4 rows fit the 2-row block by squeezing, exactly as the original does
        // (4 rows is its 36-core case): the block must not move.
        for rows in 1..=4 {
            assert_eq!(core_rung_height(2, rows), 220.0, "rows {rows}");
        }
    }

    #[test]
    fn rung_height_grows_past_the_squeeze_floor_instead_of_erasing_plots() {
        // Deeper rungs keep every cell at the floor: rows*49 + (rows-1)*8.
        assert_eq!(core_rung_height(2, 6), 334.0); // 36 cores @ 6 cols
        assert_eq!(core_rung_height(2, 8), 448.0); // 16 cores @ 2 cols
        assert_eq!(core_rung_height(2, 16), 904.0); // 16 cores @ 1 col
        for rows in 5..=64 {
            let cell = core_cell_height(core_rung_height(2, rows), rows, CORE_GAP);
            assert!(
                (cell - CORE_CELL_MIN_H).abs() < 1e-9,
                "rows {rows}: cell {cell}"
            );
        }
    }

    #[test]
    fn rung_height_survives_zero_rows() {
        assert_eq!(core_rung_height(2, 0), 220.0);
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
    /// order, so the ladder still comes back -- every usable rung present, in
    /// ascending column order, because a stable sort leaves equal keys alone.
    #[test]
    fn a_nan_cell_width_sorts_instead_of_panicking() {
        let l = core_column_ladder(16, f64::NAN, CORE_GAP, CORE_MAX_COLUMNS);
        let cols: Vec<usize> = l.iter().map(|(_, c)| *c).collect();
        assert_eq!(cols, vec![1, 2, 4, 8]);
        assert!(l.iter().all(|(w, _)| w.is_nan()));
    }

    #[test]
    fn zero_cores_does_not_panic() {
        assert!(core_column_ladder(0, CORE_MIN_CELL, CORE_GAP, CORE_MAX_COLUMNS).is_empty());
        assert_eq!(core_visual_rows(0, 0), 1);
    }

    // MARK: core_columns

    fn cols(count: usize) -> usize {
        core_columns(count, CORE_MAX_COLUMNS)
    }

    /// Largest proper divisor <= 10 with >= 2 rows — divides evenly, never a
    /// single tall row.
    #[test]
    fn core_columns_prefers_the_largest_even_divisor() {
        assert_eq!(cols(36), 9); // ubu-01: 9 x 4
        assert_eq!(cols(16), 8); // 8 x 2
        assert_eq!(cols(12), 6); // 6 x 2
        assert_eq!(cols(10), 5); // 5 x 2
        assert_eq!(cols(9), 3); // 3 x 3
        assert_eq!(cols(8), 4); // 4 x 2
        assert_eq!(cols(4), 2); // 2 x 2
        assert_eq!(cols(64), 8); // 8 x 8 (10 is not a divisor)
    }

    /// Prime core counts have no even split: least-waste columns keeping >= 2
    /// rows, ties broken toward fewer rows.
    #[test]
    fn core_columns_falls_back_to_least_waste_for_primes() {
        assert_eq!(cols(7), 4); // 4 x 2, waste 1
        assert_eq!(cols(13), 7); // 7 x 2, waste 1
        assert_eq!(cols(11), 6); // 6 x 2, waste 1
    }

    /// No host should ever render as a single row (rows >= 2 whenever there are
    /// >= 3 cores).
    #[test]
    fn core_columns_always_leaves_at_least_two_rows() {
        for n in 3..=128usize {
            let c = cols(n);
            assert!(c <= CORE_MAX_COLUMNS, "core count {n} exceeded the cap");
            assert!(
                core_visual_rows(n, c) >= 2,
                "core count {n} produced a single row"
            );
        }
    }

    #[test]
    fn core_columns_edge_cases() {
        assert_eq!(cols(1), 1);
        assert_eq!(cols(0), 1);
        assert_eq!(cols(2), 1); // 1 x 2
    }

    // MARK: the volume grid

    fn vols(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("/v{i}")).collect()
    }

    #[test]
    fn volume_row_count_single_column_is_one_row_per_volume() {
        assert_eq!(volume_row_count(1, 1), 1);
        assert_eq!(volume_row_count(5, 1), 5);
        assert_eq!(volume_row_count(0, 1), 0);
    }

    #[test]
    fn volume_row_count_two_columns() {
        assert_eq!(volume_row_count(1, 2), 1); // single -> one full-width row
        assert_eq!(volume_row_count(2, 2), 1); // a pair
        assert_eq!(volume_row_count(3, 2), 2); // full-width top + 1 pair
        assert_eq!(volume_row_count(8, 2), 4); // four pairs
        assert_eq!(volume_row_count(7, 2), 4); // full-width top + 3 pairs
    }

    #[test]
    fn a_single_volume_is_one_full_width_row() {
        let rows = volume_rows(&vols(1), 2);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), 1, "a lone volume spans the row");
    }

    #[test]
    fn an_odd_count_gives_the_most_important_volume_a_full_width_top_row() {
        let rows = volume_rows(&vols(5), 2);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].len(), 1, "top row is the most-important volume");
        let rest: Vec<usize> = rows[1..].iter().map(Vec::len).collect();
        assert_eq!(rest, vec![2, 2], "rest are paired");
        assert_eq!(rows[0][0], "/v0", "first (importance-sorted) leads");
    }

    #[test]
    fn an_even_count_pairs_all() {
        let rows = volume_rows(&vols(6), 2);
        assert_eq!(rows.iter().map(Vec::len).collect::<Vec<_>>(), vec![2, 2, 2]);
    }

    #[test]
    fn a_single_column_makes_every_volume_full_width() {
        let rows = volume_rows(&vols(3), 1);
        assert_eq!(rows.iter().map(Vec::len).collect::<Vec<_>>(), vec![1, 1, 1]);
    }

    /// Every volume appears exactly once regardless of layout, and the rendered
    /// row count matches the count sibling cards reserved slots from.
    #[test]
    fn no_volume_lost_or_duplicated() {
        for n in 0..=9usize {
            for c in 1..=2usize {
                let rows = volume_rows(&vols(n), c);
                let flat: Vec<&String> = rows.iter().flatten().collect();
                assert_eq!(flat.len(), n, "n={n} cols={c}");
                let unique: std::collections::HashSet<&&String> = flat.iter().collect();
                assert_eq!(unique.len(), n, "n={n} cols={c}");
                assert_eq!(
                    rows.len(),
                    volume_row_count(n, c),
                    "reserved slots must match rendered rows at n={n} cols={c}"
                );
            }
        }
    }

    // MARK: the shared core-block height

    fn aligned(counts: &[usize]) -> Vec<(usize, Vec<CoreRung>)> {
        aligned_core_ladders(counts, 2, CORE_MIN_CELL, CORE_GAP, CORE_MAX_COLUMNS)
    }

    /// Alone, a count gets exactly what it got before: `core_column_ladder`'s
    /// rungs with `core_rung_height`'s heights. The whole feature has to be
    /// invisible to a single-card cockpit.
    #[test]
    fn one_count_reproduces_the_unaligned_ladder() {
        for n in [1_usize, 8, 10, 16, 36, 64] {
            let aligned = aligned(&[n]);
            assert_eq!(aligned.len(), 1, "n {n}");
            let (got_n, rungs) = &aligned[0];
            assert_eq!(*got_n, n);
            let plain = ladder(n);
            assert_eq!(rungs.len(), plain.len(), "n {n}");
            for (rung, (w, c)) in rungs.iter().zip(plain.iter()) {
                assert_eq!(rung.min_width, *w, "n {n}");
                assert_eq!(rung.cols, *c, "n {n}");
                assert_eq!(
                    rung.height,
                    core_rung_height(2, core_visual_rows(n, *c)),
                    "n {n} cols {c}"
                );
            }
        }
    }

    /// The case this exists for: Chris's cockpit, a 10-core Mac beside the
    /// 36-core ubu-01. At a 900pt-class card the Mac takes 2 rows (220) and
    /// ubu takes 6 (334); aligned, both blocks are 334 while each keeps the
    /// column count its own core count divides into.
    #[test]
    fn a_mac_beside_a_36_core_host_shares_the_taller_block() {
        let aligned = aligned(&[10, 36]);
        let at = |n: usize, width: f64| {
            let (_, rungs) = aligned.iter().find(|(c, _)| *c == n).expect("count");
            *rungs
                .iter()
                .rfind(|r| r.min_width <= width)
                .expect("a rung at this width")
        };
        let mac = at(10, 899.0);
        let ubu = at(36, 899.0);
        assert_eq!(mac.cols, 5, "10 cores divide into 5, not into 6");
        assert_eq!(ubu.cols, 6);
        assert_eq!(ubu.height, 334.0, "6 rows of 36 cores clears the floor");
        assert_eq!(mac.height, ubu.height, "the blocks must match");
    }

    /// Sharing a height never shortens anyone: the value is a max over values
    /// that are themselves maxes against the squeeze floor, so no card can be
    /// pushed below the height that keeps its sparklines visible (#219).
    #[test]
    fn the_shared_height_never_drops_a_card_below_its_own() {
        let counts = [4_usize, 8, 10, 16, 36, 64];
        let aligned = aligned(&counts);
        for (n, rungs) in &aligned {
            for rung in rungs {
                let own = core_rung_height(2, core_visual_rows(*n, rung.cols));
                assert!(
                    rung.height >= own,
                    "n {n} at {}pt: shared {} < own {own}",
                    rung.min_width,
                    rung.height
                );
            }
        }
    }

    /// Every count keeps a column count it divides evenly into — taking the
    /// widest card's columns as well as its height would orphan a last row,
    /// which is the thing the ladder exists to prevent.
    #[test]
    fn sharing_a_height_never_shares_a_column_count() {
        let aligned = aligned(&[8, 36]);
        for (n, rungs) in &aligned {
            for rung in rungs {
                assert_eq!(n % rung.cols, 0, "n {n} cols {}", rung.cols);
            }
        }
    }

    /// At any one width every count reports the same height — the property the
    /// cockpit actually renders.
    #[test]
    fn all_counts_agree_on_the_height_at_every_width() {
        let aligned = aligned(&[8, 16, 36]);
        let widths: Vec<f64> = aligned[0].1.iter().map(|r| r.min_width).collect();
        for w in widths {
            let heights: Vec<f64> = aligned
                .iter()
                .map(|(_, rungs)| {
                    rungs
                        .iter()
                        .rfind(|r| r.min_width <= w)
                        .expect("rung")
                        .height
                })
                .collect();
            assert!(
                heights.windows(2).all(|p| p[0] == p[1]),
                "at {w}pt heights disagree: {heights:?}"
            );
        }
    }

    /// Degenerate inputs: no counts, and zero-core cards (an unreachable host
    /// reports none) must not invent a ladder or panic.
    #[test]
    fn empty_and_zero_counts_yield_no_ladders() {
        assert!(aligned(&[]).is_empty());
        assert!(aligned(&[0, 0]).is_empty());
        // A zero-core card alongside a real one drops out rather than
        // dragging the set down.
        let aligned = aligned(&[0, 8]);
        assert_eq!(aligned.len(), 1);
        assert_eq!(aligned[0].0, 8);
    }
}
