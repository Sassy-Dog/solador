import HostMetricsKit

/// Lays volumes into display rows for a host card.
///
/// One column → each volume is its own (full-width) row. Two columns → an odd
/// count gives the most-important (first, importance-sorted) volume a full-width
/// top row and pairs the rest; an even count pairs all. Pure so it can be tested
/// and so sibling cards reserve a consistent number of rows (alignment).
enum VolumeGridLayout {
    /// How many rows `count` volumes occupy with `columns` columns.
    static func rowCount(_ count: Int, columns: Int) -> Int {
        guard count > 0 else { return 0 }
        if columns <= 1 { return count }
        if count <= 1 { return 1 }
        return count.isMultiple(of: 2) ? count / 2 : 1 + (count - 1) / 2
    }

    /// Split importance-sorted volumes into rows of one (full width) or two.
    static func rows(_ volumes: [VolumeUsage], columns: Int) -> [[VolumeUsage]] {
        guard columns >= 2 else { return volumes.map { [$0] } }
        var rows: [[VolumeUsage]] = []
        var rest = volumes
        if !rest.count.isMultiple(of: 2) {
            rows.append([rest.removeFirst()])   // most-important, full-width top row
        }
        var i = 0
        while i < rest.count {
            rows.append(Array(rest[i ..< min(i + 2, rest.count)]))
            i += 2
        }
        return rows
    }
}
