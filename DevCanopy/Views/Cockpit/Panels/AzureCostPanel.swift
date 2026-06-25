import SwiftUI

/// Azure Cost panel — month-to-date Azure spend in USD (the headline), the prior
/// calendar month's total for a MoM reference, and the top resource groups. Reads
/// `AzureCostService`, which sources a platform-owned daily Cost Management export
/// over a Keychain-stored SAS URL. Unlike Claude usage, USD *is* the point here.
struct AzureCostPanel: CockpitPanelView {
    static let kind: CockpitPanelKind = .azureCost

    @EnvironmentObject private var service: AzureCostService

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
        return "\(Self.usd(summary.spendMTD)) MTD"
    }

    private func content(_ summary: AzureCostSummary) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(Self.usd(summary.spendMTD))
                    .font(CockpitTheme.mono(22, weight: .bold))
                    .foregroundStyle(CockpitTheme.green)
                Text("month-to-date")
                    .font(CockpitTheme.mono(10))
                    .foregroundStyle(CockpitTheme.muted)
                Spacer()
            }

            HStack(spacing: 7) {
                Text("PRIOR MONTH")
                    .font(CockpitTheme.mono(9, weight: .bold))
                    .foregroundStyle(CockpitTheme.muted)
                Spacer()
                Text(Self.usd(summary.spendPriorMonth))
                    .font(CockpitTheme.mono(12, weight: .bold))
                    .foregroundStyle(CockpitTheme.ink)
            }

            if !summary.byResource.isEmpty {
                Divider().overlay(CockpitTheme.line)
                Text("TOP RESOURCE GROUPS")
                    .font(CockpitTheme.mono(9, weight: .bold))
                    .foregroundStyle(CockpitTheme.muted)
                ForEach(summary.byResource.prefix(5)) { resource in
                    resourceRow(resource)
                }
            }
        }
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
}
