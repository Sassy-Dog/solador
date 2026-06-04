import SwiftUI

/// Containers / VMs panel — local docker/podman/tart detection (Phase 2: remote).
struct ContainersPanel: CockpitPanelView {
    static let kind: CockpitPanelKind = .containers

    @EnvironmentObject private var service: LocalContainerService

    var body: some View {
        CockpitPanelContainer(kind: Self.kind, trailing: trailingLabel) {
            if service.detectedRuntimes.isEmpty {
                Text("no container runtimes detected")
                    .font(CockpitTheme.mono(11))
                    .foregroundStyle(CockpitTheme.muted)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(ContainerRuntime.allCases.filter { service.detectedRuntimes.contains($0) }, id: \.self) { runtime in
                        runtimeGroup(runtime)
                    }
                }
            }
        }
    }

    private var trailingLabel: String {
        let running = service.containers.filter(\.isRunning).count
        return "\(running) running"
    }

    @ViewBuilder
    private func runtimeGroup(_ runtime: ContainerRuntime) -> some View {
        let items = service.containers.filter { $0.runtime == runtime }
        VStack(alignment: .leading, spacing: 3) {
            Text(runtime.displayName.uppercased())
                .font(CockpitTheme.mono(9, weight: .bold))
                .foregroundStyle(CockpitTheme.muted)
            if items.isEmpty {
                Text("—")
                    .font(CockpitTheme.mono(11))
                    .foregroundStyle(CockpitTheme.muted)
            } else {
                ForEach(items) { container in
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
            Spacer()
            Text(container.statusText)
                .font(CockpitTheme.mono(9))
                .foregroundStyle(container.isRunning ? CockpitTheme.green : CockpitTheme.muted)
                .lineLimit(1)
        }
    }
}
