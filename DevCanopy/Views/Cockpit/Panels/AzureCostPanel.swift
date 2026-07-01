import SwiftUI

/// Azure Cost panel — month-to-date Azure spend in USD (the headline), the prior
/// calendar month's total for a MoM reference, and the top resource groups. Reads
/// `AzureCostService`, which sources a platform-owned daily Cost Management export
/// over a Keychain-stored SAS URL. Unlike Claude usage, USD *is* the point here.
struct AzureCostPanel: CockpitPanelView {
    static let kind: CockpitPanelKind = .azureCost

    @EnvironmentObject private var service: AzureCostService

    /// Monthly budget (USD) for the projected-vs-budget bar, set in Settings → Azure
    /// Cost. 0 (the default) hides the bar. `@AppStorage` — the app's scalar store,
    /// same as `refreshInterval`; not a secret, so no Keychain.
    @AppStorage("azureMonthlyBudgetUSD") private var budgetUSD: Double = 0

    var body: some View {
        CockpitPanelContainer(kind: Self.kind, trailing: trailing) {
            VStack(alignment: .leading, spacing: 10) {
                if !service.isConfigured {
                    muted("Add an Azure Cost SAS URL in Settings")
                } else if let summary = service.summary {
                    content(summary)
                    PanelStatusFooter(lastUpdated: service.lastUpdated, error: service.lastError, staleAfter: 600)
                } else if let error = service.lastError {
                    Text(error)
                        .font(CockpitTheme.mono(11))
                        .foregroundStyle(CockpitTheme.red)
                        .frame(maxWidth: .infinity, alignment: .leading)
                } else {
                    muted(service.isLoading ? "reading export…" : "no cost data")
                }
            }
        }
    }

    private var trailing: String? {
        guard service.isConfigured, let summary = service.summary else { return nil }
        let money = Self.usd(summary.spendMTD)
        guard let month = summary.asOfMonth else { return "\(money) MTD" }
        return "\(money) · \(Self.monthName(month, "MMM"))"
    }

    private func content(_ summary: AzureCostSummary) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(Self.usd(summary.spendMTD))
                    .font(CockpitTheme.mono(22, weight: .bold))
                    .foregroundStyle(CockpitTheme.green)
                Text(caption(summary))
                    .font(CockpitTheme.mono(10))
                    .foregroundStyle(summary.asOfMonth == nil ? CockpitTheme.muted : CockpitTheme.amber)
                Spacer()
            }

            statRow("PRIOR MONTH", summary.spendPriorMonth)
            statRow("PROJECTED", summary.spendProjected)

            if budgetUSD > 0 {
                budgetBar(projected: summary.spendProjected, budget: budgetUSD)
            }

            if !summary.byResource.isEmpty || !summary.byType.isEmpty {
                Divider().overlay(CockpitTheme.line)
                HStack(alignment: .top, spacing: 16) {
                    if !summary.byResource.isEmpty {
                        breakdownColumn("TOP RESOURCE GROUPS", summary.byResource)
                    }
                    if !summary.byType.isEmpty {
                        breakdownColumn("TOP RESOURCE TYPES", summary.byType)
                    }
                }
            }
        }
    }

    /// Caption under the headline figure: "month-to-date" normally, or a stamp of the
    /// covered month when we fell back to the last completed month (rollover gap).
    private func caption(_ summary: AzureCostSummary) -> String {
        guard let month = summary.asOfMonth else { return "month-to-date" }
        return "\(Self.monthName(month, "MMMM")) · this month pending"
    }

    /// A label / right-aligned USD row (PRIOR MONTH, PROJECTED).
    private func statRow(_ label: String, _ value: Double) -> some View {
        HStack(spacing: 7) {
            Text(label)
                .font(CockpitTheme.mono(9, weight: .bold))
                .foregroundStyle(CockpitTheme.muted)
            Spacer()
            Text(Self.usd(value))
                .font(CockpitTheme.mono(12, weight: .bold))
                .foregroundStyle(CockpitTheme.ink)
        }
    }

    /// Projected-vs-budget: a `$projected / $budget` header over a bar that greens →
    /// ambers (≥90%) → reds (≥100%). Mirrors mission-control's BudgetBar.
    private func budgetBar(projected: Double, budget: Double) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 7) {
                Text("PROJECTED VS BUDGET")
                    .font(CockpitTheme.mono(9, weight: .bold))
                    .foregroundStyle(CockpitTheme.muted)
                Spacer()
                Text("\(Self.usd(projected)) / \(Self.usd(budget))")
                    .font(CockpitTheme.mono(10))
                    .foregroundStyle(CockpitTheme.muted)
            }
            CockpitProgressBar(fraction: projected / budget, amberAt: 0.9, redAt: 1.0)
        }
    }

    /// A titled top-N breakdown column (resource groups or resource types). Sized to
    /// share the row width evenly so the two lists sit side by side.
    private func breakdownColumn(_ title: String, _ resources: [AzureResourceCost]) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title)
                .font(CockpitTheme.mono(9, weight: .bold))
                .foregroundStyle(CockpitTheme.muted)
            ForEach(resources.prefix(5)) { resource in
                resourceRow(resource)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func resourceRow(_ resource: AzureResourceCost) -> some View {
        HStack(spacing: 7) {
            Circle()
                .fill(CockpitTheme.greenDim)
                .frame(width: 5, height: 5)
            Text(resource.name)
                .font(CockpitTheme.mono(11))
                .foregroundStyle(CockpitTheme.ink)
                .lineLimit(1)
            Spacer()
            Text(Self.usd(resource.cost))
                .font(CockpitTheme.mono(10))
                .foregroundStyle(CockpitTheme.muted)
        }
    }

    private func muted(_ text: String) -> some View {
        Text(text)
            .font(CockpitTheme.mono(11))
            .foregroundStyle(CockpitTheme.muted)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: - Formatting

    /// USD with a thousands separator and cents, e.g. `$1,234.56`.
    static func usd(_ amount: Double) -> String {
        let formatter = NumberFormatter()
        formatter.numberStyle = .currency
        formatter.currencyCode = "USD"
        formatter.locale = Locale(identifier: "en_US")
        formatter.maximumFractionDigits = 2
        formatter.minimumFractionDigits = 2
        return formatter.string(from: NSNumber(value: amount)) ?? String(format: "$%.2f", amount)
    }

    /// A calendar-month name in UTC (matches the export's month math), e.g. "MMMM" →
    /// "June", "MMM" → "Jun".
    static func monthName(_ date: Date, _ format: String) -> String {
        let formatter = DateFormatter()
        formatter.calendar = Calendar(identifier: .gregorian)
        formatter.timeZone = TimeZone(identifier: "UTC")
        formatter.locale = Locale(identifier: "en_US")
        formatter.dateFormat = format
        return formatter.string(from: date)
    }
}
