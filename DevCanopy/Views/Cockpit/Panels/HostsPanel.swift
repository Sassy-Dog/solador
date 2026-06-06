import SwiftUI

/// Hosts panel — the cockpit's default top surface. Renders the full Lupita view
/// per host (no drill-in). Responsive: as many hosts side-by-side as the width
/// allows (each ≥ `minHostWidth`), else one tab per host. Today just "this machine";
/// Phase 2 adds remote hosts to the `hosts` array fed by the per-host agent.
struct HostsPanel: CockpitPanelView {
    static let kind: CockpitPanelKind = .hosts

    @EnvironmentObject private var local: LocalHostMetricsService
    @EnvironmentObject private var remoteHosts: RemoteHostsCoordinator
    @State private var availableWidth: CGFloat = 0

    private let minHostWidth: CGFloat = 760

    private var hosts: [HostMetricsService] { [local] + remoteHosts.hosts }

    var body: some View {
        content
            .background(
                GeometryReader { geo in
                    Color.clear.preference(key: WidthKey.self, value: geo.size.width)
                }
            )
            .onPreferenceChange(WidthKey.self) { availableWidth = $0 }
    }

    @ViewBuilder
    private var content: some View {
        // Initial render (width unknown) falls through to side-by-side, which is
        // correct for the common single-host case.
        let fits = availableWidth == 0 || availableWidth >= CGFloat(hosts.count) * minHostWidth
        if hosts.count <= 1 || fits {
            HStack(alignment: .top, spacing: 16) {
                ForEach(hosts, id: \.hostName) { service in
                    HostLupitaView(service: service,
                                   volumeSlots: volumeSlots,
                                   volumeColumns: volumeColumns(forCardCount: hosts.count))
                }
            }
        } else {
            TabView {
                ForEach(hosts, id: \.hostName) { service in
                    HostLupitaView(service: service,
                                   volumeSlots: volumeSlots,
                                   volumeColumns: volumeColumns(forCardCount: 1))
                        .tabItem { Text(service.hostName) }
                }
            }
            .frame(minHeight: 780)
        }
    }

    /// Most volumes any visible host reports — all cards reserve this many so the
    /// Volumes section (and the Top CPU/RAM below it) line up across cards.
    private var volumeSlots: Int {
        hosts.map { $0.snapshot?.volumes.count ?? 0 }.max() ?? 0
    }

    /// Two volume columns once each card is wide enough; one otherwise.
    private func volumeColumns(forCardCount n: Int) -> Int {
        guard availableWidth > 0, n > 0 else { return 2 }   // first render: assume wide
        let perCard = (availableWidth - 16 * CGFloat(n - 1)) / CGFloat(n)
        return perCard >= 560 ? 2 : 1
    }
}

private struct WidthKey: PreferenceKey {
    static var defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = nextValue()
    }
}
