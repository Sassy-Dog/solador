import Foundation

/// Chooses how many columns the per-core CPU grid uses for a given core count.
///
/// Unlike Lupita's `CoreLayout` (a switch hand-tuned for known Mac core counts), this is a
/// general rule for arbitrary hosts. Two goals:
///   1. Divide evenly so the last row is full — no stranded trailing cell.
///   2. Use **at least 2 rows**, so low-core hosts don't render as one giant row of tall cells
///      stretched over the fixed block height.
///
/// So: pick the **largest proper divisor of `coreCount` that is ≤ `maxColumns` and leaves ≥ 2
/// rows**. Examples (maxColumns = 10): 36 → 9 (×4), 16 → 8 (×2), 12 → 6 (×2), 10 → 5 (×2),
/// 8 → 4 (×2), 9 → 3 (×3), 4 → 2 (×2).
///
/// For prime core counts (e.g. 5, 7, 11, 13) there is no usable divisor, so we fall back to the
/// column count with the least last-row waste, keeping ≥ 2 rows and ties broken toward fewer
/// rows: 13 → 7 (×2), 11 → 6 (×2), 7 → 4 (×2). This never collapses to a single row.
func coreColumns(coreCount: Int, maxColumns: Int = 10) -> Int {
    // 1 or 2 cores can't sensibly fill two columns: a single column gives ≤ 2 rows.
    guard coreCount > 2 else { return 1 }

    // Largest proper divisor (rows = coreCount / c ≥ 2) within the cap → fewest rows, no waste.
    var best = 0
    for c in 1 ... min(maxColumns, coreCount) where coreCount % c == 0 && coreCount / c >= 2 {
        best = c
    }
    if best >= 2 { return best }

    // Prime core count: no even split. Least-waste columns that still leave ≥ 2 rows.
    var bestColumns = 2
    var bestWaste = Int.max
    var bestRows = Int.max
    for c in 2 ... max(2, maxColumns) {
        let rows = (coreCount + c - 1) / c
        guard rows >= 2 else { continue }
        let waste = rows * c - coreCount
        if waste < bestWaste || (waste == bestWaste && rows < bestRows) {
            bestWaste = waste
            bestRows = rows
            bestColumns = c
        }
    }
    return bestColumns
}
