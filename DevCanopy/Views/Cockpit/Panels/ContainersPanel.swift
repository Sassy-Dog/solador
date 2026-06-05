import SwiftUI

/// Containers / VMs panel — local containers/VMs plus those reported by each remote
/// host's agent, grouped by host.
struct ContainersPanel: CockpitPanelView {
    static let kind: CockpitPanelKind = .containers

    @EnvironmentObject private var service: LocalContainerService
    @EnvironmentObject private var remoteHosts: RemoteHostsCoordinator

    var body: some View {
        CockpitPanelContainer(kind: Self.kind, trailing: trailingLabel) {
            if isEmpty {
                Text("no containers detected")
                    .font(CockpitTheme.mono(11))
                    .foregroundStyle(CockpitTheme.muted)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                VStack(alignment: .leading, spacing: 10) {
                    hostSection("this machine", service.containers,
                                noRuntimes: service.detectedRuntimes.isEmpty)
                    ForEach(remoteHosts.containersByHost.keys.sorted(), id: \.self) { host in
                        hostSection(host, remoteHosts.containersByHost[host] ?? [], noRuntimes: false)
                    }
                }
            }
        }
    }

    private var allRemote: [ContainerInfo] { remoteHosts.containersByHost.values.flatMap { $0 } }

    private var isEmpty: Bool {
        service.detectedRuntimes.isEmpty && service.containers.isEmpty && allRemote.isEmpty
    }

    private var trailingLabel: String {
        let running = (service.containers + allRemote).filter(\.isRunning).count
        return "\(running) running"
    }

    @ViewBuilder
    private func hostSection(_ host: String, _ containers: [ContainerInfo], noRuntimes: Bool) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(host.uppercased())
                .font(CockpitTheme.mono(9, weight: .bold))
                .foregroundStyle(CockpitTheme.muted)
            if containers.isEmpty {
                Text(noRuntimes ? "no container runtimes" : "no containers")
                    .font(CockpitTheme.mono(10))
                    .foregroundStyle(CockpitTheme.muted)
            } else {
                ForEach(containers) { container in
                    row(container)
                }
            }
        }
    }

    private func row(_ container: ContainerInfo) -> some View {
        HStack(spacing: 7) {
            Circle()
                .fill(container.isRunning ? CockpitTheme.green : CockpitTheme.muted)
                .frame(width: 6, height: 6)
            Text(container.name)
                .font(CockpitTheme.mono(11, weight: .bold))
                .foregroundStyle(CockpitTheme.ink)
                .lineLimit(1)
            Text(container.runtime.displayName.lowercased())
                .font(CockpitTheme.mono(8))
                .foregroundStyle(CockpitTheme.muted)
            Spacer()
            Text(container.statusText)
                .font(CockpitTheme.mono(9))
                .foregroundStyle(container.isRunning ? CockpitTheme.green : CockpitTheme.muted)
                .lineLimit(1)
        }
    }
}
