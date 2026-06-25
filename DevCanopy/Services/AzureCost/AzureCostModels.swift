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

/// Month-to-date Azure spend (USD), the prior calendar month's total for a MoM
/// reference, and the top resource groups. Unlike Claude usage (subscription, so
/// USD is hidden), Azure spend *is* the headline figure, so this carries dollars.
///
/// `error` is set only when the whole fetch fails; a missing prior-month export is
/// best-effort (prior = 0, `error` stays nil) so a partial outage never blanks the
/// card. Mirrors the server-side `AzureCostResult` in mission-control.
struct AzureCostSummary: Equatable {
    let spendMTD: Double
    let spendPriorMonth: Double
    let byResource: [AzureResourceCost]
    var error: String?
}
