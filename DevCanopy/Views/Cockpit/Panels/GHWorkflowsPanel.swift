import AppKit
import SwiftUI

/// GitHub Workflows panel — what's running now and what's failing across the curated
/// repos, so it's clear what needs a look. Authenticates with a fine-grained PAT
/// from the Keychain (set in Settings).
struct GHWorkflowsPanel: CockpitPanelView {
    static let kind: CockpitPanelKind = .ghWorkflows

    @EnvironmentObject private var service: GHWorkflowsService

    private struct RunningItem: Identifiable {
        let repo: String
        let ref: RunRef
        var id: Int64 {
            ref.runID
        }
    }

    private struct ApprovalItem: Identifiable {
        let repo: String
        let ref: RunRef
        var id: Int64 {
            ref.runID
        }
    }

    private struct StuckItem: Identifiable {
        let repo: String
        let ref: RunRef
        var id: Int64 {
            ref.runID
        }
    }

    private struct AttentionItem: Identifiable {
        let repo: String
        let which: String
        let ref: RunRef
        var id: String {
            "\(repo):\(ref.runID):\(which)"
        }
    }

    var body: some View {
        // Compute the lists once; the header summary and the rows must agree.
        let running = runningItems
        let approval = approvalItems
        let stuck = stuckItems
        let attention = attentionItems
        let unreadable = unreadableRepos
        let loading = service.isLoading && service.health.isEmpty

        return CockpitPanelContainer(
            kind: Self.kind,
            trailing: trailing(
                running: running.count,
                approval: approval.count,
                stuck: stuck.count,
                attention: attention.count,
                unreadable: unreadable.count,
                loading: loading
            )
        ) {
            if !service.isAuthenticated {
                muted("connect a GitHub token in Settings")
            } else if loading {
                muted("loading…")
            } else {
                VStack(alignment: .leading, spacing: 12) {
                    if !approval.isEmpty {
                        sectionHeader("NEEDS APPROVAL", approval.count)
                        ForEach(approval) { approvalRow($0) }
                    }
                    if !stuck.isEmpty {
                        sectionHeader("STUCK", stuck.count)
                        ForEach(stuck) { stuckRow($0) }
                    }
                    if !running.isEmpty {
                        sectionHeader("RUNNING", running.count)
                        ForEach(running) { runningRow($0) }
                    }
                    if !attention.isEmpty {
                        sectionHeader("NEEDS ATTENTION", attention.count)
                        ForEach(attention) { attentionRow($0) }
                    }
                    if !unreadable.isEmpty {
                        sectionHeader("CAN'T READ", unreadable.count)
                        unreadableRow(unreadable)
                    }
                    healthLine(attention: attention.count, unreadable: unreadable.count)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    private var runningItems: [RunningItem] {
        service.health
            .flatMap { h in h.running.map { RunningItem(repo: h.shortName, ref: $0) } }
            .sorted { ($0.ref.startedAt ?? .distantFuture) < ($1.ref.startedAt ?? .distantFuture) }
    }

    private var approvalItems: [ApprovalItem] {
        service.health
            .flatMap { h in h.needsApproval.map { ApprovalItem(repo: h.shortName, ref: $0) } }
            .sorted { ($0.ref.startedAt ?? .distantFuture) < ($1.ref.startedAt ?? .distantFuture) }
    }

    private var stuckItems: [StuckItem] {
        service.health
            .flatMap { h in h.stuck.map { StuckItem(repo: h.shortName, ref: $0) } }
            .sorted { ($0.ref.startedAt ?? .distantFuture) < ($1.ref.startedAt ?? .distantFuture) }
    }

    private var attentionItems: [AttentionItem] {
        service.health.flatMap { h -> [AttentionItem] in
            var items: [AttentionItem] = []
            if let m = h.main, m.isFailed {
                items.append(AttentionItem(repo: h.shortName, which: "main", ref: m))
            }
            if let p = h.lastPR, p.isFailed {
                items.append(AttentionItem(repo: h.shortName, which: "PR " + p.context, ref: p))
            }
            return items
        }
    }

    /// Repos whose runs couldn't be fetched (auth/network) — surfaced so a broken
    /// token never masquerades as "all green".
    private var unreadableRepos: [String] {
        service.health.filter { !$0.reachable }.map(\.shortName)
    }

    private func trailing(
        running: Int,
        approval: Int,
        stuck: Int,
        attention: Int,
        unreadable: Int,
        loading: Bool
    ) -> String? {
        guard service.isAuthenticated, !loading else { return nil }
        var parts: [String] = []
        if approval > 0 { parts.append("\(approval) needs approval") }
        if stuck > 0 { parts.append("\(stuck) stuck") }
        if running > 0 { parts.append("\(running) running") }
        if attention > 0 { parts.append("\(attention) failed") }
        if unreadable > 0 { parts.append("\(unreadable) unreadable") }
        return parts.isEmpty ? "all green" : parts.joined(separator: " · ")
    }

    private func sectionHeader(_ title: String, _ count: Int) -> some View {
        Text("\(title) (\(count))")
            .font(CockpitTheme.mono(10, weight: .bold))
            .foregroundStyle(CockpitTheme.muted)
    }

    private func runningRow(_ item: RunningItem) -> some View {
        rowChrome(url: item.ref.htmlURL) {
            HStack(spacing: 7) {
                Circle().fill(CockpitTheme.amber).frame(width: 6, height: 6)
                Text(item.repo).font(CockpitTheme.mono(11, weight: .bold)).foregroundStyle(CockpitTheme.ink).lineLimit(1)
                Text("\(item.ref.title) · \(item.ref.context)")
                    .font(CockpitTheme.mono(9)).foregroundStyle(CockpitTheme.muted).lineLimit(1)
                Spacer()
                Text(elapsed(item.ref.startedAt)).font(CockpitTheme.mono(9)).foregroundStyle(CockpitTheme.amber)
            }
        }
    }

    /// A run parked at a deployment-protection gate. Blinking amber dot (the user
    /// explicitly asked for blinking) so it reads as "act now", not "still cooking".
    private func approvalRow(_ item: ApprovalItem) -> some View {
        rowChrome(url: item.ref.htmlURL) {
            HStack(spacing: 7) {
                BlinkingDot(color: CockpitTheme.amber)
                Text(item.repo).font(CockpitTheme.mono(11, weight: .bold)).foregroundStyle(CockpitTheme.ink).lineLimit(1)
                Text("\(item.ref.title) · \(item.ref.context)")
                    .font(CockpitTheme.mono(9)).foregroundStyle(CockpitTheme.muted).lineLimit(1)
                Spacer()
                Text("needs approval · \(elapsed(item.ref.startedAt))")
                    .font(CockpitTheme.mono(9, weight: .bold)).foregroundStyle(CockpitTheme.amber)
            }
        }
    }

    /// A queued/pending run that has gone stale (concurrency-blocked / stuck-queued)
    /// — surfaced distinctly from a healthy long-running job (the 17h51m incident).
    private func stuckRow(_ item: StuckItem) -> some View {
        rowChrome(url: item.ref.htmlURL) {
            HStack(spacing: 7) {
                Circle().fill(CockpitTheme.red).frame(width: 6, height: 6)
                Text(item.repo).font(CockpitTheme.mono(11, weight: .bold)).foregroundStyle(CockpitTheme.ink).lineLimit(1)
                Text("\(item.ref.title) · \(item.ref.context)")
                    .font(CockpitTheme.mono(9)).foregroundStyle(CockpitTheme.muted).lineLimit(1)
                Spacer()
                Text("stuck · \(elapsed(item.ref.startedAt))")
                    .font(CockpitTheme.mono(9, weight: .bold)).foregroundStyle(CockpitTheme.red)
            }
        }
    }

    private func attentionRow(_ item: AttentionItem) -> some View {
        rowChrome(url: item.ref.htmlURL) {
            HStack(spacing: 7) {
                Circle().fill(CockpitTheme.red).frame(width: 6, height: 6)
                Text(item.repo).font(CockpitTheme.mono(11, weight: .bold)).foregroundStyle(CockpitTheme.ink).lineLimit(1)
                Text(item.which).font(CockpitTheme.mono(9)).foregroundStyle(CockpitTheme.muted).lineLimit(1)
                Spacer()
                Text("failed · \(relative(item.ref.startedAt))").font(CockpitTheme.mono(9)).foregroundStyle(CockpitTheme.red)
            }
        }
    }

    private func unreadableRow(_ repos: [String]) -> some View {
        HStack(spacing: 7) {
            Circle().fill(CockpitTheme.amber).frame(width: 6, height: 6)
            Text(repos.joined(separator: ", "))
                .font(CockpitTheme.mono(11, weight: .bold)).foregroundStyle(CockpitTheme.ink).lineLimit(2)
            Text("(check token access)")
                .font(CockpitTheme.mono(9)).foregroundStyle(CockpitTheme.muted)
            Spacer()
        }
    }

    /// Reassurance line. "Healthy" excludes only failed/unreachable repos — a
    /// repo that is merely running still counts as healthy, so the fraction never
    /// implies a problem just because a build is in flight.
    @ViewBuilder
    private func healthLine(attention: Int, unreadable: Int) -> some View {
        let total = service.health.count
        let healthy = service.health.count(where: { $0.isHealthy })
        if attention == 0, unreadable == 0 {
            label("✓ all \(total) healthy", CockpitTheme.green)
        } else {
            label("✓ \(healthy)/\(total) healthy", CockpitTheme.green)
        }
    }

    private func rowChrome(url: String, @ViewBuilder _ content: () -> some View) -> some View {
        content()
            .contentShape(Rectangle())
            .onTapGesture {
                if let u = URL(string: url) { NSWorkspace.shared.open(u) }
            }
    }

    private func muted(_ text: String) -> some View {
        Text(text).font(CockpitTheme.mono(11)).foregroundStyle(CockpitTheme.muted)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func label(_ text: String, _ color: Color) -> some View {
        Text(text).font(CockpitTheme.mono(10)).foregroundStyle(color)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func elapsed(_ date: Date?) -> String {
        guard let date else { return "" }
        let s = Int(max(0, Date().timeIntervalSince(date)))
        if s < 60 { return "\(s)s" }
        if s < 3600 { return "\(s / 60)m" }
        return "\(s / 3600)h\((s % 3600) / 60)m"
    }

    private func relative(_ date: Date?) -> String {
        guard let date else { return "recently" }
        let s = Int(max(0, Date().timeIntervalSince(date)))
        if s < 3600 { return "\(max(1, s / 60))m ago" }
        if s < 86400 { return "\(s / 3600)h ago" }
        return "\(s / 86400)d ago"
    }
}

/// A status dot that pulses its opacity to draw the eye — used for NEEDS APPROVAL,
/// where a human needs to act, vs the steady dot used for plain RUNNING.
private struct BlinkingDot: View {
    let color: Color
    @State private var dim = false

    var body: some View {
        Circle()
            .fill(color)
            .frame(width: 6, height: 6)
            .opacity(dim ? 0.25 : 1.0)
            .animation(.easeInOut(duration: 0.7).repeatForever(autoreverses: true), value: dim)
            .onAppear { dim = true }
    }
}
