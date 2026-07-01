import Foundation

/// Pure, network-free parsing + aggregation for the platform-owned Azure Cost
/// Management export (a CSV in blob). Ported from mission-control's
/// `azure-cost/query.ts` so the two consumers read the export identically. Kept
/// free of I/O so it is fully unit-testable.
///
/// Columns read (Cost Management "ActualCost" export schema): the dashboard shows
/// USD, so we sum `costInUsd`; one top-N tile groups by `resourceGroupName`, the
/// other by `meterCategory` (the human-readable service type — "Virtual Network",
/// "NAT Gateway", "SQL Database", "Storage", …).
private let costColumn = "costInUsd"
private let resourceGroupColumn = "resourceGroupName"
private let meterCategoryColumn = "meterCategory"

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

/// Linearly project month-to-date spend to a full-month total: spend so far, scaled
/// by (days in month / elapsed days). Elapsed counts the current (partial) day so
/// day 1 never divides by zero. Ported from mission-control's `projectMonthlySpend`
/// so both consumers extrapolate identically. Computed at fetch time and frozen onto
/// the summary — on the last day of a month elapsed == daysInMonth, so a carried-
/// forward end-of-month snapshot correctly shows projected == MTD.
func projectMonthlySpend(_ spendMTD: Double, now: Date = Date()) -> Double {
    var calendar = Calendar(identifier: .gregorian)
    calendar.timeZone = TimeZone(identifier: "UTC")!
    let elapsedDays = calendar.component(.day, from: now) // 1-based, includes today
    let daysInMonth = calendar.range(of: .day, in: .month, for: now)?.count ?? 30
    guard elapsedDays > 0 else { return spendMTD }
    return spendMTD / Double(elapsedDays) * Double(daysInMonth)
}

/// Parse RFC4180 CSV into rows of fields. A full character state machine (not a
/// line split) because the export's `tags` column embeds quoted JSON with commas
/// — and quoted fields may, in principle, contain newlines. Strips a leading
/// UTF-8 BOM (the export's header starts with one).
///
/// Scans Unicode *scalars*, not `Character`s: Swift folds a CRLF (`"\r\n"`) into a
/// single grapheme-cluster `Character` that equals neither `"\r"` nor `"\n"`, so a
/// `Character`-based scan never breaks rows on the export's CRLF line endings and
/// collapses the whole file into one row. Scalars keep `\r` and `\n` separate.
func parseCsv(_ text: String) -> [[String]] {
    var rows: [[String]] = []
    var field = ""
    var row: [String] = []
    var inQuotes = false

    var source = Substring(text)
    if source.first == "\u{FEFF}" { source = source.dropFirst() }
    let scalars = Array(source.unicodeScalars)
    let quote: Unicode.Scalar = "\""
    let comma: Unicode.Scalar = ","
    let newline: Unicode.Scalar = "\n"
    let carriageReturn: Unicode.Scalar = "\r"

    var i = 0
    while i < scalars.count {
        let c = scalars[i]
        if inQuotes {
            if c == quote {
                if i + 1 < scalars.count, scalars[i + 1] == quote {
                    field.unicodeScalars.append(quote)
                    i += 1 // escaped quote
                } else {
                    inQuotes = false
                }
            } else {
                field.unicodeScalars.append(c)
            }
        } else if c == quote {
            inQuotes = true
        } else if c == comma {
            row.append(field)
            field = ""
        } else if c == newline || c == carriageReturn {
            // End the row on \n; swallow \r and \r\n.
            if c == carriageReturn, i + 1 < scalars.count, scalars[i + 1] == newline { i += 1 }
            row.append(field)
            field = ""
            rows.append(row)
            row = []
        } else {
            field.unicodeScalars.append(c)
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

/// Sum an export CSV: total `costInUsd`, plus two top-N breakdowns (each sorted
/// desc) — by resource group and by resource type (`meterCategory`).
///
/// Resource groups: Azure names are case-INSENSITIVE (the export can report the same
/// group as both `rg-sassydog` and `RG-SASSYDOG` across rows), so we group by — and
/// display — the lowercased name; rows with no group (subscription-level charges)
/// fold into "(unassigned)".
///
/// Resource types: `meterCategory` values are human-readable display names
/// ("Virtual Network", "SQL Database"), so we keep their original casing rather than
/// lowercasing; blank rows (e.g. reservation/capacity charges) fold into "(other)".
///
/// - Throws: when the `costInUsd` column is absent from the header.
func aggregateCostCsv(_ csv: String) throws -> AzureCostAggregate {
    let rows = parseCsv(csv)
    guard let header = rows.first else { return AzureCostAggregate(total: 0, byResource: [], byType: []) }

    guard let costIdx = header.firstIndex(of: costColumn) else {
        throw AzureCostError.missingColumn(costColumn)
    }
    let rgIdx = header.firstIndex(of: resourceGroupColumn)
    let typeIdx = header.firstIndex(of: meterCategoryColumn)

    var total = 0.0
    var byResourceGroup: [String: Double] = [:]
    var byMeterCategory: [String: Double] = [:]
    for row in rows.dropFirst() {
        guard row.count > costIdx else { continue } // skip blank/short trailing lines
        let cost = num(row[costIdx])
        total += cost

        let rg = (rgIdx.map { row.count > $0 ? row[$0] : "" } ?? "").trimmingCharacters(in: .whitespaces)
        byResourceGroup[(rg.isEmpty ? "(unassigned)" : rg).lowercased(), default: 0] += cost

        let type = (typeIdx.map { row.count > $0 ? row[$0] : "" } ?? "").trimmingCharacters(in: .whitespaces)
        byMeterCategory[type.isEmpty ? "(other)" : type, default: 0] += cost
    }

    return AzureCostAggregate(
        total: total,
        byResource: sortedCosts(byResourceGroup),
        byType: sortedCosts(byMeterCategory)
    )
}

/// Fold a `name → cost` map into `AzureResourceCost`s sorted by cost desc.
private func sortedCosts(_ totals: [String: Double]) -> [AzureResourceCost] {
    totals
        .map { AzureResourceCost(name: $0.key, cost: $0.value) }
        .sorted { $0.cost > $1.cost }
}
