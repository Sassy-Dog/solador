import SwiftUI
import AppKit

/// CI Health panel — what's running now and what's failing across the curated
/// repos, so it's clear what needs a look. Authenticates with a fine-grained PAT
/// from the Keychain (set in Settings).
struct CIHealthPanel: CockpitPanelView {
    static let kind: CockpitPanelKind = .ciHealth

    @EnvironmentObject private var service: PortfolioCIService

    private struct RunningItem: Identifiable {
        let repo: String
        let ref: RunRef
        var id: Int64 { ref.runID }
    }
    private struct AttentionItem: Identifiable {
        let repo: String
        let which: String
        let ref: RunRef
        var id: String { "\(repo):\(ref.runID):\(which)" }
    }

    var body: some View {
        // Compute the item lists once; the header summary and the rows must agree.
        let running = runningItems
        let attention = attentionItems
        let loading = service.isLoading && service.health.isEmpty
        let trailing: String? = (!service.isAuthenticated || loading)
            ? nil
            : (running.isEmpty && attention.isEmpty
                ? "all green"
                : "\(running.count) running · \(attention.count) failed")

        return CockpitPanelContainer(kind: Self.kind, trailing: trailing) {
            if !service.isAuthenticated {
                muted("connect a GitHub token in Settings")
            } else if loading {
                muted("loading…")
            } else {
                VStack(alignment: .leading, spacing: 12) {
                    if !running.isEmpty {
                        sectionHeader("RUNNING", running.count)
                        ForEach(running) { runningRow($0) }
                    }
                    if !attention.isEmpty {
                        sectionHeader("NEEDS ATTENTION", attention.count)
                        ForEach(attention) { attentionRow($0) }
                    }
                    greenLine(running: running.count, attention: attention.count)
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

    @ViewBuilder
    private func greenLine(running: Int, attention: Int) -> some View {
        let total = service.health.count
        let green = service.health.filter { $0.isClean }.count
        if running == 0 && attention == 0 {
            label("✓ All \(total) repos green", CockpitTheme.green)
        } else {
            label("✓ \(green)/\(total) repos green", CockpitTheme.green)
        }
    }

    private func rowChrome<Content: View>(url: String, @ViewBuilder _ content: () -> Content) -> some View {
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
