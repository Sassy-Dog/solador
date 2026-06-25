import Foundation

/// Pure, network-free parsing + aggregation for the platform-owned Azure Cost
/// Management export (a CSV in blob). Ported from mission-control's
/// `azure-cost/query.ts` so the two consumers read the export identically. Kept
/// free of I/O so it is fully unit-testable.
///
/// Columns read (Cost Management "ActualCost" export schema): the dashboard shows
/// USD, so we sum `costInUsd`; the top-N tile groups by `resourceGroupName`.
private let costColumn = "costInUsd"
private let resourceGroupColumn = "resourceGroupName"

/// Lenient numeric parse matching JS `Number(v)`: blanks / non-numbers fold to 0
/// rather than throwing, so a stray short row never poisons the sum.
private func num(_ value: String) -> Double {
    let trimmed = value.trimmingCharacters(in: .whitespaces)
    guard let parsed = Double(trimmed), parsed.isFinite else { return 0 }
    return parsed
}

/// The export partitions output by month into a folder named for the calendar
/// month's date range, e.g. `20260601-20260630`. Compute it for the month
/// containing `date` (UTC) so we can list just that month's runs instead of the
/// export's whole history.
func monthRangeFolder(_ date: Date) -> String {
    var calendar = Calendar(identifier: .gregorian)
    calendar.timeZone = TimeZone(identifier: "UTC")!
    let components = calendar.dateComponents([.year, .month], from: date)
    let year = components.year!
    let month = components.month!
    let lastDay = calendar.range(of: .day, in: .month, for: date)?.count ?? 28
    return String(format: "%04d%02d01-%04d%02d%02d", year, month, year, month, lastDay)
}

/// First instant of the calendar month before the one containing `date` (UTC).
/// Feeds `monthRangeFolder` for the prior-month (`last-month`) export.
func priorMonthDate(from date: Date) -> Date {
    var calendar = Calendar(identifier: .gregorian)
    calendar.timeZone = TimeZone(identifier: "UTC")!
    let startOfMonth = calendar.date(from: calendar.dateComponents([.year, .month], from: date))!
    return calendar.date(byAdding: .month, value: -1, to: startOfMonth)!
}

/// Parse RFC4180 CSV into rows of fields. A full character state machine (not a
/// line split) because the export's `tags` column embeds quoted JSON with commas
/// — and quoted fields may, in principle, contain newlines. Strips a leading
/// UTF-8 BOM (the export's header starts with one).
func parseCsv(_ text: String) -> [[String]] {
    var rows: [[String]] = []
    var field = ""
    var row: [String] = []
    var inQuotes = false

    var source = Substring(text)
    if source.first == "\u{FEFF}" { source = source.dropFirst() }
    let chars = Array(source)

    var i = 0
    while i < chars.count {
        let c = chars[i]
        if inQuotes {
            if c == "\"" {
                if i + 1 < chars.count, chars[i + 1] == "\"" {
                    field.append("\"")
                    i += 1 // escaped quote
                } else {
                    inQuotes = false
                }
            } else {
                field.append(c)
            }
        } else if c == "\"" {
            inQuotes = true
        } else if c == "," {
            row.append(field)
            field = ""
        } else if c == "\n" || c == "\r" {
            // End the row on \n; swallow \r and \r\n.
            if c == "\r", i + 1 < chars.count, chars[i + 1] == "\n" { i += 1 }
            row.append(field)
            field = ""
            rows.append(row)
            row = []
        } else {
            field.append(c)
        }
        i += 1
    }
    // Flush a trailing field/row with no terminating newline.
    if !field.isEmpty || !row.isEmpty {
        row.append(field)
        rows.append(row)
    }
    return rows
}

/// Sum an export CSV: total `costInUsd`, plus a per-resource-group breakdown
/// (sorted desc). Rows with no resource group (subscription-level charges) fold
/// into "(unassigned)".
///
/// Azure resource-group names are case-INSENSITIVE: the export can report the same
/// group as both `rg-sassydog` and `RG-SASSYDOG` across rows. Group by — and
/// display — the lowercased name so we neither split one group into two tiles nor
/// show an inconsistent mix of casings (lossless here: this org's groups are
/// lowercase-canonical).
///
/// - Throws: when the `costInUsd` column is absent from the header.
func aggregateCostCsv(_ csv: String) throws -> (total: Double, byResource: [AzureResourceCost]) {
    let rows = parseCsv(csv)
    guard let header = rows.first else { return (0, []) }

    guard let costIdx = header.firstIndex(of: costColumn) else {
        throw AzureCostError.missingColumn(costColumn)
    }
    let rgIdx = header.firstIndex(of: resourceGroupColumn)

    var total = 0.0
    var byResourceGroup: [String: Double] = [:]
    for row in rows.dropFirst() {
        guard row.count > costIdx else { continue } // skip blank/short trailing lines
        let cost = num(row[costIdx])
        total += cost
        let raw = (rgIdx.map { row.count > $0 ? row[$0] : "" } ?? "").trimmingCharacters(in: .whitespaces)
        let key = (raw.isEmpty ? "(unassigned)" : raw).lowercased()
        byResourceGroup[key, default: 0] += cost
    }

    let byResource = byResourceGroup
        .map { AzureResourceCost(name: $0.key, cost: $0.value) }
        .sorted { $0.cost > $1.cost }
    return (total, byResource)
}
