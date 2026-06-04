import SwiftUI

/// Hosts panel — the cockpit's default top surface. Renders the full Lupita view
/// per host (no drill-in). Responsive: as many hosts side-by-side as the width
/// allows (each ≥ `minHostWidth`), else one tab per host. Today just "this machine";
/// Phase 2 adds remote hosts to the `hosts` array fed by the per-host agent.
struct HostsPanel: CockpitPanelView {
    static let kind: CockpitPanelKind = .hosts

    @EnvironmentObject private var local: LocalHostMetricsService
    @State private var availableWidth: CGFloat = 0

    private let minHostWidth: CGFloat = 760

    private var hosts: [LocalHostMetricsService] { [local] }   // Phase 2: + remote hosts

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
                    HostLupitaView(service: service)
                }
            }
        } else {
            TabView {
                ForEach(hosts, id: \.hostName) { service in
                    HostLupitaView(service: service)
                        .tabItem { Text(service.hostName) }
                }
            }
            .frame(minHeight: 780)
        }
    }
}

private struct WidthKey: PreferenceKey {
    static var defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = nextValue()
    }
}
