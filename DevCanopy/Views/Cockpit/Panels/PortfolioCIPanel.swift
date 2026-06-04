import SwiftUI

/// Portfolio CI panel — latest CI/Release run health per configured repo.
/// Authenticates with a fine-grained PAT from the Keychain (set in Settings).
struct PortfolioCIPanel: CockpitPanelView {
    static let kind: CockpitPanelKind = .portfolioCI

    @EnvironmentObject private var service: PortfolioCIService

    var body: some View {
        CockpitPanelContainer(kind: Self.kind, trailing: trailingLabel) {
            if !service.isAuthenticated {
                Text("connect a GitHub token in Settings")
                    .font(CockpitTheme.mono(11))
                    .foregroundStyle(CockpitTheme.muted)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(service.statuses) { status in
                        row(status)
                    }
                }
            }
        }
    }

    private var trailingLabel: String {
        "\(PortfolioCIService.configuredRepos.count) repos"
    }

    private func row(_ status: RepoCIStatus) -> some View {
        HStack(spacing: 7) {
            Circle()
                .fill(color(for: status.health))
                .frame(width: 6, height: 6)
            Text(status.shortName)
                .font(CockpitTheme.mono(11, weight: .bold))
                .foregroundStyle(CockpitTheme.ink)
                .lineLimit(1)
            Spacer()
            Text(label(for: status))
                .font(CockpitTheme.mono(9))
                .foregroundStyle(color(for: status.health))
                .lineLimit(1)
        }
    }

    private func color(for health: RepoHealth) -> Color {
        switch health {
        case .good: return CockpitTheme.green
        case .bad: return CockpitTheme.red
        case .running: return CockpitTheme.amber
        case .unknown: return CockpitTheme.muted
        }
    }

    private func label(for status: RepoCIStatus) -> String {
        switch status.health {
        case .good: return "CI ✓"
        case .bad: return "CI ✗"
        case .running: return "running…"
        case .unknown: return "—"
        }
    }
}
