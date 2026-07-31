import SwiftUI

/// Usage panel — today's Claude tokens (header), rolling 5h + weekly windows, and a
/// compact top-projects breakdown, plus per-provider usage sections beneath. Reads
/// from `ClaudeUsageService` (local logs, no network), `NeonUsageService`, and
/// `SentryUsageService`. Claude is subscription-based, so token counts (not USD costs)
/// are what's shown.
struct ClaudeUsagePanel: CockpitPanelView {
    static let kind: CockpitPanelKind = .claudeUsage

    @EnvironmentObject private var service: ClaudeUsageService
    @EnvironmentObject private var neon: NeonUsageService
    @EnvironmentObject private var sentry: SentryUsageService

    /// Monthly accepted-error quota for the Sentry bar. 0 (the default) hides the bar;
    /// non-secret, so it comes from the same `@AppStorage` key Settings writes.
    @AppStorage(SentryUsageService.monthlyEventQuotaDefaultsKey) private var sentryEventQuota: Int = 0

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
                // No Neon API key ⇒ no section at all, so the panel is pixel-identical
                // to its Claude-only self.
                if neon.isConfigured {
                    neonSection
                }
                // Same guarantee for Sentry: no token ⇒ no section, no layout shift.
                if sentry.isConfigured {
                    sentrySection
                }
                PanelStatusFooter(lastUpdated: service.lastUpdated, error: service.lastError, staleAfter: 150)
            }
        }
    }

    // MARK: - Neon

    /// Month-to-date Neon compute + branch storage. Rendering only — the fold and the
    /// unit conversion live in `NeonUsageMapping`. An unknown figure renders `—`,
    /// never a defaulted 0.
    @ViewBuilder
    private var neonSection: some View {
        Divider().overlay(CockpitTheme.line)
        providerRow("NEON COMPUTE (MTD)", Self.cuHours(neon.summary?.computeUnitHours))
        providerRow("NEON STORAGE", Self.gibibytes(neon.summary?.storageGiB))
        // Poll cadence is 1h, so the stale threshold sits above it — 90m flags a
        // genuinely stuck poller rather than the normal gap between polls.
        PanelStatusFooter(lastUpdated: neon.lastUpdated, error: neon.lastError, staleAfter: 90 * 60)
    }

    // MARK: - Sentry

    /// Accepted error events over the rolling 30d window, with an optional quota bar.
    /// Rendering only — the fold lives in `SentryUsageMapping`. An unknown figure
    /// renders `—`, and the bar is suppressed rather than drawn at a defaulted 0.
    @ViewBuilder
    private var sentrySection: some View {
        Divider().overlay(CockpitTheme.line)
        providerRow(
            "SENTRY ERRORS (\(SentryStatsQuery.windowDays)D)",
            Self.events(sentry.summary?.acceptedErrorEvents)
        )
        if sentryEventQuota > 0, let accepted = sentry.summary?.acceptedErrorEvents {
            // Same thresholds as the Azure budget bar: amber at 90%, red at quota.
            CockpitProgressBar(
                fraction: Double(accepted) / Double(sentryEventQuota),
                amberAt: 0.9,
                redAt: 1.0
            )
        }
        PanelStatusFooter(lastUpdated: sentry.lastUpdated, error: sentry.lastError, staleAfter: 90 * 60)
    }

    private func providerRow(_ label: String, _ value: String?) -> some View {
        HStack(spacing: 7) {
            Text(label)
                .font(CockpitTheme.mono(9, weight: .bold))
                .foregroundStyle(CockpitTheme.muted)
            Spacer()
            Text(value ?? "—")
                .font(CockpitTheme.mono(12, weight: .bold))
                .foregroundStyle(value == nil ? CockpitTheme.muted : CockpitTheme.ink)
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
                CockpitProgressBar(fraction: fraction)
                    .padding(.leading, 49)
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

    // MARK: - Formatting

    /// Compute-unit hours to one decimal, e.g. `12.4 CU-h`. nil in ⇒ nil out, which
    /// the row renders as `—`.
    static func cuHours(_ hours: Double?) -> String? {
        guard let hours else { return nil }
        return String(format: "%.1f CU-h", hours)
    }

    /// Storage in GiB to one decimal, e.g. `3.2 GiB`. Real units only — the API
    /// exposes no quota, so there is no percentage to show.
    static func gibibytes(_ gib: Double?) -> String? {
        guard let gib else { return nil }
        return String(format: "%.1f GiB", gib)
    }

    /// Accepted error events in the panel's abbreviated-count style, e.g. `12k`. nil in
    /// ⇒ nil out, which the row renders as `—`.
    static func events(_ count: Int?) -> String? {
        guard let count else { return nil }
        return tokens(count)
    }

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
