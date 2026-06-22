import SwiftUI

/// OpenClaw panel — a glanceable, read-only rollup of an OpenClaw agent farm:
/// per-agent status, cron health, channel connectivity, and token usage. Reads
/// the runtime-agnostic `AgentRuntimeCoordinator`, so when a second runtime
/// (hermes) is added it renders as an additional section with zero panel changes.
struct OpenClawPanel: CockpitPanelView {
    static let kind: CockpitPanelKind = .openclawAgents

    @EnvironmentObject private var coordinator: AgentRuntimeCoordinator

    var body: some View {
        CockpitPanelContainer(kind: Self.kind, trailing: trailing) {
            if coordinator.snapshots.isEmpty {
                muted("no agent runtime configured")
            } else {
                VStack(alignment: .leading, spacing: 14) {
                    ForEach(coordinator.snapshots) { runtimeSection($0) }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    private var trailing: String? {
        // One-line health across all runtimes for the card header — must agree
        // with the body, so it reflects the actual aggregate connection state.
        let snaps = coordinator.snapshots
        guard !snaps.isEmpty else { return nil }
        if snaps.contains(where: { $0.pairing != nil }) { return "pairing required" }
        if snaps.allSatisfy({ $0.connection == .connected }) {
            let agents = snaps.reduce(0) { $0 + $1.agents.count }
            return "\(agents) agent\(agents == 1 ? "" : "s")"
        }
        if snaps.contains(where: { $0.connection == .connecting }) { return "connecting…" }
        if snaps.contains(where: {
            if case .disconnected = $0.connection { true } else { false }
        }) { return "disconnected" }
        // All idle → not configured yet; the body carries the Settings hint.
        return nil
    }

    private func runtimeSection(_ snap: AgentRuntimeSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            // Multi-runtime sub-header (hidden when there's only one).
            if coordinator.snapshots.count > 1 {
                Text(snap.displayName.uppercased())
                    .font(CockpitTheme.mono(10, weight: .bold))
                    .foregroundStyle(CockpitTheme.green)
            }

            if let pairing = snap.pairing {
                pairingBanner(pairing)
            } else if snap.connection == .idle {
                muted("add a gateway URL in Settings → OpenClaw")
            } else if case let .disconnected(reason) = snap.connection {
                connectionLine(reason: reason, color: CockpitTheme.red)
            } else if snap.connection == .connecting {
                connectionLine(reason: "connecting…", color: CockpitTheme.amber)
            }

            if !snap.agents.isEmpty {
                sectionHeader("AGENTS", snap.agents.count)
                ForEach(snap.agents) { agentRow($0) }
            }

            if snap.cron.total > 0 {
                sectionHeader("CRON", snap.cron.total)
                cronRow(snap.cron)
            }

            if !snap.channels.isEmpty {
                sectionHeader("CHANNELS", snap.channels.count)
                channelsRow(snap.channels)
            }

            if let usage = snap.usage {
                usageRow(usage)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: - Rows

    private func agentRow(_ item: AgentRollupItem) -> some View {
        HStack(spacing: 7) {
            statusDot(item.status)
            if let emoji = item.emoji {
                Text(emoji).font(CockpitTheme.mono(11))
            }
            Text(item.name)
                .font(CockpitTheme.mono(11, weight: .bold))
                .foregroundStyle(CockpitTheme.ink)
                .lineLimit(1)
            if let detail = item.detail {
                Text(detail)
                    .font(CockpitTheme.mono(9))
                    .foregroundStyle(CockpitTheme.muted)
                    .lineLimit(1)
            }
            Spacer()
            if item.status == .running {
                Text("running")
                    .font(CockpitTheme.mono(9, weight: .bold))
                    .foregroundStyle(CockpitTheme.amber)
            }
        }
    }

    private func cronRow(_ cron: CronSummary) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 7) {
                statusDot(cron.error > 0 ? .error : (cron.running > 0 ? .running : .ok))
                Text(cronSummaryText(cron))
                    .font(CockpitTheme.mono(11))
                    .foregroundStyle(CockpitTheme.ink)
                    .lineLimit(1)
                Spacer()
            }
            if let err = cron.lastError, !err.isEmpty {
                Text(err)
                    .font(CockpitTheme.mono(9))
                    .foregroundStyle(CockpitTheme.red)
                    .lineLimit(1)
            }
        }
    }

    private func cronSummaryText(_ cron: CronSummary) -> String {
        var parts: [String] = []
        if cron.ok > 0 { parts.append("\(cron.ok) ok") }
        if cron.running > 0 { parts.append("\(cron.running) running") }
        if cron.error > 0 { parts.append("\(cron.error) error") }
        if cron.unknown > 0 { parts.append("\(cron.unknown) unknown") }
        if cron.disabled > 0 { parts.append("\(cron.disabled) disabled") }
        return parts.isEmpty ? "no jobs" : parts.joined(separator: " · ")
    }

    private func channelsRow(_ channels: [ChannelStatus]) -> some View {
        // Inline name + dot per channel — Slack ● Telegram ● …
        FlowRow(spacing: 12) {
            ForEach(channels) { channel in
                HStack(spacing: 5) {
                    statusDot(channel.status)
                    Text(channel.name)
                        .font(CockpitTheme.mono(10))
                        .foregroundStyle(CockpitTheme.ink)
                }
            }
        }
    }

    private func usageRow(_ usage: SessionUsageRollup) -> some View {
        HStack(spacing: 7) {
            Image(systemName: "gauge.with.needle")
                .font(.system(size: 9))
                .foregroundStyle(CockpitTheme.muted)
            Text("\(formatTokens(usage.totalTokens)) tokens · ctx \(formatTokens(usage.contextTokens))")
                .font(CockpitTheme.mono(10))
                .foregroundStyle(CockpitTheme.muted)
            Spacer()
        }
    }

    private func pairingBanner(_ pairing: PairingState) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 7) {
                BlinkingDot(color: CockpitTheme.amber)
                Text(pairing.kind == .scopeUpgrade ? "scope approval required" : "device pairing required")
                    .font(CockpitTheme.mono(11, weight: .bold))
                    .foregroundStyle(CockpitTheme.amber)
                Spacer()
            }
            if let request = pairing.requestID {
                Text("openclaw devices approve \(request)")
                    .font(CockpitTheme.mono(9))
                    .foregroundStyle(CockpitTheme.ink)
                    .lineLimit(1)
                    .textSelection(.enabled)
            }
            Text("device \(pairing.deviceID.prefix(16))…")
                .font(CockpitTheme.mono(9))
                .foregroundStyle(CockpitTheme.muted)
        }
    }

    private func connectionLine(reason: String, color: Color) -> some View {
        HStack(spacing: 7) {
            Circle().fill(color).frame(width: 6, height: 6)
            Text(reason).font(CockpitTheme.mono(10)).foregroundStyle(CockpitTheme.muted).lineLimit(1)
            Spacer()
        }
    }

    // MARK: - Helpers

    private func sectionHeader(_ title: String, _ count: Int) -> some View {
        Text("\(title) (\(count))")
            .font(CockpitTheme.mono(10, weight: .bold))
            .foregroundStyle(CockpitTheme.muted)
    }

    private func statusDot(_ status: AgentStatus) -> some View {
        Circle()
            .fill(Self.color(for: status))
            .frame(width: 6, height: 6)
            .opacity(status == .disabled ? 0.4 : 1)
    }

    static func color(for status: AgentStatus) -> Color {
        switch status {
        case .running: CockpitTheme.amber
        case .ok: CockpitTheme.green
        case .error: CockpitTheme.red
        case .unknown, .disabled: CockpitTheme.muted
        }
    }

    private func muted(_ text: String) -> some View {
        Text(text).font(CockpitTheme.mono(11)).foregroundStyle(CockpitTheme.muted)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func formatTokens(_ n: Int) -> String {
        if n >= 1_000_000 { return String(format: "%.1fM", Double(n) / 1_000_000) }
        if n >= 1000 { return String(format: "%.1fk", Double(n) / 1000) }
        return "\(n)"
    }
}

/// A status dot that pulses to draw the eye — used for the pairing banner, where
/// a human must act. Mirrors the idiom in `GHWorkflowsPanel` (private there).
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

/// Minimal wrapping HStack for the channel dots — wraps to the next line when the
/// row runs out of width. Keeps the glance compact when there are many channels.
private struct FlowRow: Layout {
    var spacing: CGFloat = 8

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache _: inout ()) -> CGSize {
        let maxWidth = proposal.width ?? .infinity
        var x: CGFloat = 0
        var y: CGFloat = 0
        var rowHeight: CGFloat = 0
        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if x + size.width > maxWidth, x > 0 {
                x = 0
                y += rowHeight + spacing
                rowHeight = 0
            }
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }
        return CGSize(width: maxWidth == .infinity ? x : maxWidth, height: y + rowHeight)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache _: inout ()) {
        let maxWidth = proposal.width ?? bounds.width
        var x: CGFloat = bounds.minX
        var y: CGFloat = bounds.minY
        var rowHeight: CGFloat = 0
        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if x + size.width > bounds.minX + maxWidth, x > bounds.minX {
                x = bounds.minX
                y += rowHeight + spacing
                rowHeight = 0
            }
            subview.place(at: CGPoint(x: x, y: y), proposal: .unspecified)
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }
    }
}
