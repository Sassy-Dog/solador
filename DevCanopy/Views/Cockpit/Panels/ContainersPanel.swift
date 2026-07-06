import SwiftUI

/// Containers / VMs panel — local containers/VMs plus those reported by each remote
/// host's agent, grouped by host.
struct ContainersPanel: CockpitPanelView {
    static let kind: CockpitPanelKind = .containers

    @EnvironmentObject private var service: LocalContainerService
    @EnvironmentObject private var remoteHosts: RemoteHostsCoordinator
    @AppStorage("containerGroupRules") private var groupRulesData = Data()

    var body: some View {
        CockpitPanelContainer(kind: Self.kind, trailing: trailingLabel) {
            let rules = ContainerGroupRule.load(from: groupRulesData)
            VStack(alignment: .leading, spacing: 10) {
                if isEmpty {
                    Text("no containers detected")
                        .font(CockpitTheme.mono(11))
                        .foregroundStyle(CockpitTheme.muted)
                        .frame(maxWidth: .infinity, alignment: .leading)
                } else {
                    hostSection(
                        ContainerGroupRule.localHostScope,
                        service.containers,
                        rules: rules,
                        noRuntimes: service.detectedRuntimes.isEmpty
                    )
                    ForEach(remoteHosts.containersByHost.keys.sorted(), id: \.self) { host in
                        hostSection(
                            host,
                            remoteHosts.containersByHost[host] ?? [],
                            rules: rules,
                            noRuntimes: false
                        )
                    }
                }
                PanelStatusFooter(lastUpdated: service.lastUpdated, error: service.lastError, staleAfter: 30)
            }
        }
    }

    private var allRemote: [ContainerInfo] {
        remoteHosts.containersByHost.values.flatMap(\.self)
    }

    private var isEmpty: Bool {
        service.detectedRuntimes.isEmpty && service.containers.isEmpty && allRemote.isEmpty
    }

    /// Counts everything the runtimes report — including containers hidden or
    /// collapsed by rules — so cruft building up (unreaped VMs, lingering exited
    /// containers) stays visible in the numbers even when its rows are not.
    private var trailingLabel: String {
        let all = service.containers + allRemote
        let running = all.filter(\.isRunning).count
        return "\(all.count) total · \(running) up · \(all.count - running) stopped"
    }

    private func hostSection(
        _ host: String,
        _ containers: [ContainerInfo],
        rules: [ContainerGroupRule],
        noRuntimes: Bool
    ) -> some View {
        let parts = ContainerGrouping.partition(containers, rules: rules, host: host)
        return VStack(alignment: .leading, spacing: 3) {
            Text(host.uppercased())
                .font(CockpitTheme.mono(9, weight: .bold))
                .foregroundStyle(CockpitTheme.muted)
            if containers.isEmpty {
                Text(noRuntimes ? "no container runtimes" : "no containers")
                    .font(CockpitTheme.mono(10))
                    .foregroundStyle(CockpitTheme.muted)
            } else {
                ForEach(parts.individual) { container in
                    row(container)
                }
            }
            // Aggregates are rule-driven, not container-driven: a configured collapse
            // rule renders its standing row (×0 when idle) even in an empty section.
            ForEach(parts.aggregates) { aggregate in
                aggregateRow(aggregate)
            }
        }
    }

    /// One collapsed row for all containers a group rule matched on this host — same
    /// visual grammar as `row(_:)`, with the match count in the name and the running
    /// count where the status text goes.
    private func aggregateRow(_ aggregate: ContainerGroupAggregate) -> some View {
        HStack(spacing: 7) {
            Circle()
                .fill(aggregate.runningCount > 0 ? CockpitTheme.green : CockpitTheme.muted)
                .frame(width: 6, height: 6)
            Text("\(aggregate.label) ×\(aggregate.total)")
                .font(CockpitTheme.mono(11, weight: .bold))
                .foregroundStyle(CockpitTheme.ink)
                .lineLimit(1)
            if let runtime = aggregate.dominantRuntime {
                Text(runtime.displayName.lowercased())
                    .font(CockpitTheme.mono(8))
                    .foregroundStyle(CockpitTheme.muted)
            }
            Spacer()
            Text("\(aggregate.runningCount) running")
                .font(CockpitTheme.mono(9))
                .foregroundStyle(aggregate.runningCount > 0 ? CockpitTheme.green : CockpitTheme.muted)
                .lineLimit(1)
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
        .contextMenu {
            Button("Hide \u{201C}\(container.name)\u{201D}") { hide(container.name) }
        }
    }

    /// Right-click shortcut: appends an exact-name hide rule (no `*`, so it matches
    /// literally). Managed — and undoable — in Settings → Hosts → Container Group Rules.
    private func hide(_ name: String) {
        let rules = ContainerGroupRule.load(from: groupRulesData)
        let hideRule = ContainerGroupRule(pattern: name, label: "", action: .hide)
        groupRulesData = ContainerGroupRule.encode(rules + [hideRule])
    }
}
