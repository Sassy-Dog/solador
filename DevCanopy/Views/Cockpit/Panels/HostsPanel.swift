import SwiftUI

/// Hosts panel — the cockpit's default top surface. Renders the full Lupita view
/// per host (no drill-in).
///
/// Responsive: as many cards across as the width allows, each at least
/// `CockpitBreakpoints.hostCardMinWidth`, wrapping to further rows below that.
/// One column is the stacked case — every host stays visible, which is the point of
/// an always-on cockpit. Users who'd rather keep the panel short can opt into the
/// tab fallback in General settings.
struct HostsPanel: CockpitPanelView {
    static let kind: CockpitPanelKind = .hosts

    @EnvironmentObject private var local: HostMetricsService
    @EnvironmentObject private var remoteHosts: RemoteHostsCoordinator

    /// Width the cockpit allotted this panel. 0 outside the cockpit (previews).
    @Environment(\.cockpitPanelWidth) private var availableWidth

    /// What to do when the cards can't sit side-by-side. Global (UserDefaults), set
    /// in General settings; stored raw because `@AppStorage` can't hold the enum.
    @AppStorage("hostOverflowMode") private var hostOverflowRaw: String = HostOverflowMode.stack.rawValue

    /// Per-card width at which the Volumes section splits into two columns.
    private let twoColumnVolumeWidth: CGFloat = 560

    private var hosts: [HostMetricsService] {
        [local] + remoteHosts.hosts
    }

    private var overflowMode: HostOverflowMode {
        HostOverflowMode(rawValue: hostOverflowRaw) ?? .stack
    }

    var body: some View {
        content
    }

    @ViewBuilder
    private var content: some View {
        let columns = CockpitBreakpoints.hostColumns(available: availableWidth, hostCount: hosts.count)
        if columns <= 1, hosts.count > 1, overflowMode == .tabs {
            tabbedHosts
        } else {
            cardGrid(columns: columns)
        }
    }

    /// Cards laid out `columns` per row. `Grid` (not `HStack`) so cards sharing a row
    /// also share a height, the same way the cockpit equalizes its own panel rows.
    private func cardGrid(columns: Int) -> some View {
        let rows = CockpitBreakpoints.rows(hosts, columns: columns)
        return Grid(
            horizontalSpacing: CockpitBreakpoints.spacing,
            verticalSpacing: CockpitBreakpoints.spacing
        ) {
            ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                GridRow {
                    ForEach(row, id: \.hostName) { service in
                        card(for: service, cardsPerRow: columns)
                    }
                }
            }
        }
    }

    private var tabbedHosts: some View {
        TabView {
            ForEach(hosts, id: \.hostName) { service in
                card(for: service, cardsPerRow: 1)
                    .tabItem { Text(service.hostName) }
            }
        }
        .frame(minHeight: 780)
    }

    private func card(for service: HostMetricsService, cardsPerRow: Int) -> some View {
        HostMetricsPanel(
            service: service,
            volumeSlots: volumeSlots,
            volumeColumns: volumeColumns(forCardsPerRow: cardsPerRow)
        )
    }

    /// Most volumes any visible host reports — all cards reserve this many so the
    /// Volumes section (and the Top CPU/RAM below it) line up across cards.
    /// Reads `visibleVolumes` (debounced + hide-list-filtered), so a transient
    /// mount on one host can't reflow every card.
    private var volumeSlots: Int {
        hosts.map(\.visibleVolumes.count).max() ?? 0
    }

    /// Two volume columns once each card is wide enough; one otherwise. Stacked cards
    /// get the panel's whole width, so stacking usually buys the second column back.
    ///
    /// Unknown width falls back to a single column for the same reason `hostColumns`
    /// falls back to stacking: one column always reads, two can truncate.
    private func volumeColumns(forCardsPerRow n: Int) -> Int {
        guard availableWidth > 0, n > 0 else { return 1 }
        let perCard = (availableWidth - CockpitBreakpoints.spacing * CGFloat(n - 1)) / CGFloat(n)
        return perCard >= twoColumnVolumeWidth ? 2 : 1
    }
}
