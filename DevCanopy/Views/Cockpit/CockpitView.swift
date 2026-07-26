import SwiftUI

/// The primary cockpit surface. Renders a `CockpitLayout` as rows of panels —
/// the layout is data, so swapping arrangements later is a value change, not a
/// rewrite of this view.
///
/// Responsive: the authored rows are run through `CockpitBreakpoints.reflow` at the
/// available content width, so a row whose panels no longer fit side-by-side splits
/// into stacked rows while roomier rows stay paired.
///
/// One `GeometryReader` at the root is the cockpit's only width measurement. Every
/// panel's own width is then *derived* from it and handed down as
/// `\.cockpitPanelWidth` — panels never measure themselves.
struct CockpitView: View {
    var layout: CockpitLayout = .hostsForward

    /// The cockpit grid is two columns wide: a `.half` panel occupies one column,
    /// a `.full` panel spans both. `Grid` sizes each row to its tallest cell and
    /// offers that height to the others, so cards in a row share a height.
    private let gridColumns = 2

    /// Inset around the whole grid.
    private let pagePadding: CGFloat = 20

    var body: some View {
        GeometryReader { geo in
            let available = max(0, geo.size.width - pagePadding * 2)
            ScrollView {
                Grid(
                    horizontalSpacing: CockpitBreakpoints.spacing,
                    verticalSpacing: CockpitBreakpoints.spacing
                ) {
                    let rows = CockpitBreakpoints.reflow(layout.rows, available: available)
                    ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                        GridRow {
                            ForEach(row) { placement in
                                panel(for: placement.kind)
                                    .environment(\.cockpitPanelWidth, panelWidth(inRowOf: row.count, of: available))
                                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                                    .gridCellColumns(columnSpan(placement, inRowOf: row.count))
                            }
                        }
                    }
                }
                .padding(pagePadding)
                .frame(width: geo.size.width)
            }
        }
        .frame(minWidth: 880, minHeight: 600)
        .background(CockpitTheme.background)
    }

    /// The width one panel gets in a rendered row of `count`. Panels in a row share
    /// the space evenly, minus the gaps between them.
    private func panelWidth(inRowOf count: Int, of available: CGFloat) -> CGFloat {
        guard count > 1 else { return available }
        let gaps = CockpitBreakpoints.spacing * CGFloat(count - 1)
        return max(0, (available - gaps) / CGFloat(count))
    }

    /// How many grid columns a placement occupies. A panel alone in a rendered row
    /// always spans the full width — otherwise a row that reflow split would leave a
    /// hole beside it. Only `.full`/`.half` appear in shipped layouts; `.third` falls
    /// back to one column in this two-column grid.
    private func columnSpan(_ placement: CockpitPlacement, inRowOf count: Int) -> Int {
        guard count > 1 else { return gridColumns }
        switch placement.span {
        case .full: return gridColumns
        case .half, .third: return 1
        }
    }

    @ViewBuilder
    private func panel(for kind: CockpitPanelKind) -> some View {
        switch kind {
        case .hosts: HostsPanel()
        case .ghRunners: GHRunnersPanel()
        case .containers: ContainersPanel()
        case .ghWorkflows: GHWorkflowsPanel()
        case .claudeUsage: ClaudeUsagePanel()
        case .openclawAgents: OpenClawPanel()
        case .azureCost: AzureCostPanel()
        }
    }
}

#Preview {
    CockpitView()
        .frame(width: 1200, height: 800)
}
