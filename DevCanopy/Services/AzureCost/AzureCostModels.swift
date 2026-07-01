import Foundation

/// Spend for a single Azure resource group, in USD. Names are normalized to
/// lowercase (Azure RG names are case-insensitive — see `aggregateCostCsv`), so
/// the lowercase name doubles as a stable identity for `ForEach`.
struct AzureResourceCost: Identifiable, Equatable {
    let name: String
    let cost: Double

    var id: String {
        name
    }
}

/// Intermediate aggregation of one or more export CSV partitions: the total USD and
/// the two top-N breakdowns (by resource group, by resource type). Produced by
/// `aggregateCostCsv` / `AzureCostReader.readExport` and folded into
/// `AzureCostSummary`.
struct AzureCostAggregate: Equatable {
    let total: Double
    let byResource: [AzureResourceCost]
    let byType: [AzureResourceCost]
}

/// Month-to-date Azure spend (USD), the prior calendar month's total for a MoM
/// reference, the MTD figure linearly projected to a full-month total, and the top
/// resource groups + resource types. Unlike Claude usage (subscription, so USD is
/// hidden), Azure spend *is* the headline figure, so this carries dollars.
///
/// `spendProjected` is computed at fetch time (see `projectMonthlySpend`) and frozen
/// here, so a carried-forward stale snapshot keeps a correct projection rather than
/// re-extrapolating against the wrong month.
///
/// `asOfMonth` is nil for the normal case (figures are the current month-to-date).
/// It is set to the first instant of the covered month when we fell back to the last
/// completed month because the current month hasn't been exported yet (the 1st-of-
/// month rollover gap) — the panel uses it to stamp which month the figures cover.
///
/// `error` is set only when the whole fetch fails; a missing prior-month export is
/// best-effort (prior = 0, `error` stays nil) so a partial outage never blanks the
/// card. Mirrors the server-side `AzureCostSnapshot` in mission-control.
struct AzureCostSummary: Equatable {
    let spendMTD: Double
    let spendPriorMonth: Double
    let spendProjected: Double
    let byResource: [AzureResourceCost]
    let byType: [AzureResourceCost]
    var asOfMonth: Date?
    var error: String?
}
