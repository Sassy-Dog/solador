import SwiftUI

/// Claude Usage panel — today's tokens (header), rolling 5h + weekly windows, and
/// a compact top-projects breakdown. Reads from `ClaudeUsageService`; no network.
/// Subscription-based, so token counts (not USD costs) are what's shown.
struct ClaudeUsagePanel: CockpitPanelView {
    static let kind: CockpitPanelKind = .claudeUsage

    @EnvironmentObject private var service: ClaudeUsageService

    var body: some View {
        CockpitPanelContainer(kind: Self.kind, trailing: trailingLabel) {
            VStack(alignment: .leading, spacing: 10) {
                if let summary = service.summary {
                    if summary.last7d.totalTokens == 0 {
                        emptyState
                    } else {
                        content(summary)
                    }
                } else {
                    Text(service.isLoading ? "reading logs…" : "no usage data")
                        .font(CockpitTheme.mono(11))
                        .foregroundStyle(CockpitTheme.muted)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                PanelStatusFooter(lastUpdated: service.lastUpdated, error: service.lastError, staleAfter: 150)
            }
        }
    }

    private var emptyState: some View {
        Text("no Claude usage in the last 7 days")
            .font(CockpitTheme.mono(11))
            .foregroundStyle(CockpitTheme.muted)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func content(_ summary: UsageSummary) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            windowRow(label: "5H", totals: summary.last5h, limit: service.fiveHourTokenLimit)
            windowRow(label: "WEEK", totals: summary.last7d, limit: service.weeklyTokenLimit)

            if !summary.projectsLast7d.isEmpty {
                Divider().overlay(CockpitTheme.line)
                Text("TOP PROJECTS (7D)")
                    .font(CockpitTheme.mono(9, weight: .bold))
                    .foregroundStyle(CockpitTheme.muted)
                ForEach(summary.projectsLast7d.prefix(4)) { item in
                    breakdownRow(name: item.name, totals: item.totals)
                }
            }
        }
    }

    private var trailingLabel: String {
        guard let summary = service.summary else { return "" }
        return "\(Self.tokens(summary.today.totalTokens)) today"
    }

    private func windowRow(label: String, totals: UsageTotals, limit: Int?) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 7) {
                Text(label)
                    .font(CockpitTheme.mono(10, weight: .bold))
                    .foregroundStyle(CockpitTheme.muted)
                    .frame(width: 42, alignment: .leading)
                Spacer()
                Text(Self.tokens(totals.totalTokens))
                    .font(CockpitTheme.mono(12, weight: .bold))
                    .foregroundStyle(CockpitTheme.green)
            }
            if let limit, limit > 0 {
                let fraction = min(Double(totals.totalTokens) / Double(limit), 1.0)
                progressBar(fraction: fraction)
            }
        }
    }

    private func breakdownRow(name: String, totals: UsageTotals) -> some View {
        HStack(spacing: 7) {
            Circle()
                .fill(CockpitTheme.greenDim)
                .frame(width: 5, height: 5)
            Text(name)
                .font(CockpitTheme.mono(11))
                .foregroundStyle(CockpitTheme.ink)
                .lineLimit(1)
            Spacer()
            Text(Self.tokens(totals.totalTokens))
                .font(CockpitTheme.mono(10))
                .foregroundStyle(CockpitTheme.muted)
        }
    }

    private func progressBar(fraction: Double) -> some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                RoundedRectangle(cornerRadius: 2)
                    .fill(CockpitTheme.panelAlt)
                RoundedRectangle(cornerRadius: 2)
                    .fill(Self.fillColor(fraction))
                    .frame(width: max(2, geo.size.width * fraction))
            }
        }
        .frame(height: 4)
        .padding(.leading, 49)
    }

    private static func fillColor(_ fraction: Double) -> Color {
        if fraction >= 0.9 { return CockpitTheme.red }
        if fraction >= 0.6 { return CockpitTheme.amber }
        return CockpitTheme.green
    }

    // MARK: - Formatting

    /// Abbreviated token count: 1_234 -> "1.2k", 1_200_000 -> "1.2M".
    static func tokens(_ count: Int) -> String {
        let n = Double(count)
        if n >= 1_000_000 {
            return String(format: "%.1fM", n / 1_000_000)
        }
        if n >= 1000 {
            return String(format: "%.0fk", n / 1000)
        }
        return "\(count)"
    }
}
