import SwiftUI

/// Containers / VMs panel — local containers/VMs plus those reported by each remote
/// host's agent, grouped by host. Expected entities (ephemeral runner VMs) keep a
/// standing row while absent — amber while recycling, red once missing — instead of
/// vanishing with every churn of `tart list`.
struct ContainersPanel: CockpitPanelView {
    static let kind: CockpitPanelKind = .containers

    @EnvironmentObject private var service: LocalContainerService
    @EnvironmentObject private var remoteHosts: RemoteHostsCoordinator
    @EnvironmentObject private var presence: ContainerPresenceStore
    @AppStorage(ContainerGroupRule.rulesDefaultsKey) private var groupRulesData = Data()

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
    /// containers) stays visible in the numbers. Missing-expected entities get
    /// their own count: they are exactly what the totals can no longer see.
    private var trailingLabel: String {
        let all = service.containers + allRemote
        let running = all.filter(\.isRunning).count
        var label = "\(all.count) total · \(running) up · \(all.count - running) stopped"
        if missingCount > 0 {
            label += " · \(missingCount) missing"
        }
        return label
    }

    private var missingCount: Int {
        let rules = ContainerGroupRule.load(from: groupRulesData)
        var sections: [(host: String, containers: [ContainerInfo])] =
            [(ContainerGroupRule.localHostScope, service.containers)]
        sections += remoteHosts.containersByHost.map { ($0.key, $0.value) }
        return sections.reduce(0) { count, section in
            let absent = ContainerGrouping.partition(
                section.containers,
                rules: rules,
                host: section.host,
                presence: presence.recordsByName(host: section.host),
                now: presence.lastSuccess[section.host]
            ).expectedAbsent
            return count + absent.count { row in
                if case .missing = row.state {
                    return true
                }
                return false
            }
        }
    }

    private func hostSection(
        _ host: String,
        _ containers: [ContainerInfo],
        rules: [ContainerGroupRule],
        noRuntimes: Bool
    ) -> some View {
        let parts = ContainerGrouping.partition(
            containers,
            rules: rules,
            host: host,
            presence: presence.recordsByName(host: host),
            now: presence.lastSuccess[host]
        )
        let rows = ContainerGrouping.displayRows(individual: parts.individual, absent: parts.expectedAbsent)
        return VStack(alignment: .leading, spacing: 3) {
            Text(host.uppercased())
                .font(CockpitTheme.mono(9, weight: .bold))
                .foregroundStyle(CockpitTheme.muted)
            if rows.isEmpty {
                Text(noRuntimes ? "no container runtimes" : "no containers")
                    .font(CockpitTheme.mono(10))
                    .foregroundStyle(CockpitTheme.muted)
            } else {
                ForEach(rows) { displayRow in
                    switch displayRow {
                    case let .present(container): row(container, host: host)
                    case let .absent(absent): absentRow(absent, host: host)
                    }
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
    /// count where the status text goes. A rule with an expected count renders
    /// ×matched/expected and warns amber when short.
    private func aggregateRow(_ aggregate: ContainerGroupAggregate) -> some View {
        let short = aggregate.expectedCount.map { aggregate.total < $0 } ?? false
        let countText = aggregate.expectedCount.map { "×\(aggregate.total)/\($0)" } ?? "×\(aggregate.total)"
        return HStack(spacing: 7) {
            Circle()
                .fill(short ? CockpitTheme.amber : (aggregate.runningCount > 0 ? CockpitTheme.green : CockpitTheme.muted))
                .frame(width: 6, height: 6)
            Text("\(aggregate.label) \(countText)")
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

    private func row(_ container: ContainerInfo, host: String) -> some View {
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
            if hasExactExpectRule(container.name, host: host) {
                Button("Unexpect \u{201C}\(container.name)\u{201D}") { unexpect(container.name, host: host) }
            } else {
                Button("Expect \u{201C}\(container.name)\u{201D}") { expect(container.name, host: host) }
            }
            Button("Hide \u{201C}\(container.name)\u{201D}") { hide(container.name) }
        }
    }

    /// A standing row for an expected entity that is absent from the current poll:
    /// amber dot + "recycling Xs" during normal ephemeral churn, red + "missing Xm"
    /// once absence exceeds grace.
    private func absentRow(_ absent: ExpectedAbsentContainer, host: String) -> some View {
        HStack(spacing: 7) {
            Circle()
                .fill(presenceColor(absent.state))
                .frame(width: 6, height: 6)
            Text(absent.name)
                .font(CockpitTheme.mono(11, weight: .bold))
                .foregroundStyle(CockpitTheme.ink)
                .lineLimit(1)
            if let runtime = absent.runtime {
                Text(runtime.displayName.lowercased())
                    .font(CockpitTheme.mono(8))
                    .foregroundStyle(CockpitTheme.muted)
            }
            Spacer()
            Text(Presence.label(absent.state) ?? "—")
                .font(CockpitTheme.mono(9, weight: .bold))
                .foregroundStyle(presenceColor(absent.state))
                .lineLimit(1)
        }
        .contextMenu {
            Button("Forget \u{201C}\(absent.name)\u{201D}") { forget(absent.name, host: host) }
        }
    }

    private func presenceColor(_ state: PresenceState) -> Color {
        switch state {
        case .present: CockpitTheme.green
        case .recycling: CockpitTheme.amber
        case .missing: CockpitTheme.red
        }
    }

    // MARK: - Rule shortcuts (managed — and undoable — in Settings → Hosts)

    /// Right-click shortcut: appends an exact-name hide rule (no `*`, so it matches
    /// literally).
    private func hide(_ name: String) {
        let rules = ContainerGroupRule.load(from: groupRulesData)
        let hideRule = ContainerGroupRule(pattern: name, label: "", action: .hide)
        groupRulesData = ContainerGroupRule.encode(rules + [hideRule])
    }

    /// Right-click shortcut: appends an exact-name, host-scoped expect rule so this
    /// entity keeps a standing presence row when it drops out of discovery.
    private func expect(_ name: String, host: String) {
        let rules = ContainerGroupRule.load(from: groupRulesData)
        let expectRule = ContainerGroupRule(pattern: name, label: "", action: .expect, host: host)
        groupRulesData = ContainerGroupRule.encode(rules + [expectRule])
    }

    /// Removes the exact-name expect rule(s) covering this entity on this host.
    private func unexpect(_ name: String, host: String) {
        let rules = ContainerGroupRule.load(from: groupRulesData)
        groupRulesData = ContainerGroupRule.encode(rules.filter { rule in
            !(rule.action == .expect && rule.pattern == name && (rule.host == host || rule.host == nil))
        })
        presence.forget(host: host, name: name)
    }

    /// "Forget" on an absent row: drop the exact-name rule (if any) and the memory.
    /// A glob-learned entry relearns if the entity ever returns — which is correct.
    private func forget(_ name: String, host: String) {
        unexpect(name, host: host)
    }

    private func hasExactExpectRule(_ name: String, host: String) -> Bool {
        ContainerGroupRule.load(from: groupRulesData).contains { rule in
            rule.action == .expect && rule.pattern == name && (rule.host == host || rule.host == nil)
        }
    }
}
