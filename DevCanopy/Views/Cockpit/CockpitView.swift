import SwiftUI

/// The primary cockpit surface. Renders a `CockpitLayout` as rows of panels —
/// the layout is data, so swapping arrangements later is a value change, not a
/// rewrite of this view.
struct CockpitView: View {
    var layout: CockpitLayout = .hostsForward

    /// The cockpit grid is two columns wide: a `.half` panel occupies one column,
    /// a `.full` panel spans both. `Grid` sizes each row to its tallest cell and
    /// offers that height to the others, so cards in a row share a height.
    private let gridColumns = 2

    var body: some View {
        ScrollView {
            Grid(horizontalSpacing: 16, verticalSpacing: 16) {
                ForEach(Array(layout.rows.enumerated()), id: \.offset) { _, row in
                    GridRow {
                        ForEach(row) { placement in
                            panel(for: placement.kind)
                                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                                .gridCellColumns(columnSpan(placement.span))
                        }
                    }
                }
            }
            .padding(20)
        }
        .frame(minWidth: 880, minHeight: 600)
        .background(CockpitTheme.background)
    }

    /// How many grid columns a placement occupies. Only `.full`/`.half` appear in
    /// shipped layouts; `.third` falls back to one column in this two-column grid.
    private func columnSpan(_ span: PanelSpan) -> Int {
        switch span {
        case .full: return gridColumns
        case .half, .third: return 1
        }
    }

    @ViewBuilder
    private func panel(for kind: CockpitPanelKind) -> some View {
        switch kind {
        case .hosts: HostsPanel()
        case .ciRunners: CIRunnersPanel()
        case .containers: ContainersPanel()
        case .ciHealth: CIHealthPanel()
        case .gitWorktrees: GitWorktreesPanel()
        case .claudeUsage: ClaudeUsagePanel()
        }
    }
}

#Preview {
    CockpitView()
        .frame(width: 1200, height: 800)
}
