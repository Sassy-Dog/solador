import SwiftUI

/// The primary cockpit surface. Renders a `CockpitLayout` as rows of panels —
/// the layout is data, so swapping arrangements later is a value change, not a
/// rewrite of this view.
struct CockpitView: View {
    var layout: CockpitLayout = .hostsForward

    var body: some View {
        ScrollView {
            VStack(spacing: 16) {
                ForEach(Array(layout.rows.enumerated()), id: \.offset) { _, row in
                    HStack(alignment: .top, spacing: 16) {
                        ForEach(row) { placement in
                            panel(for: placement.kind)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                    }
                }
            }
            .padding(20)
        }
        .frame(minWidth: 880, minHeight: 600)
        .background(CockpitTheme.background)
    }

    @ViewBuilder
    private func panel(for kind: CockpitPanelKind) -> some View {
        switch kind {
        case .hosts: HostsPanel()
        case .containers: ContainersPanel()
        case .portfolioCI: PortfolioCIPanel()
        case .gitWorktrees: GitWorktreesPanel()
        case .claudeUsage: ClaudeUsagePanel()
        }
    }
}

#Preview {
    CockpitView()
        .frame(width: 1200, height: 800)
}
