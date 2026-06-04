import SwiftUI
import HostMetricsKit

/// Hosts panel — live, Lupita-grade utilization. Renders one card per host; today
/// just "this machine" (local, in-process). Phase 2 adds remote hosts as more cards
/// fed by the per-host agent over the same `HostSnapshot` shape.
struct HostsPanel: CockpitPanelView {
    static let kind: CockpitPanelKind = .hosts

    @EnvironmentObject private var local: LocalHostMetricsService
    @State private var showingDetail = false

    var body: some View {
        CockpitPanelContainer(kind: Self.kind, trailing: trailingLabel) {
            HStack(alignment: .top, spacing: 14) {
                Button { showingDetail = true } label: {
                    HostCard(service: local)
                }
                .buttonStyle(.plain)
                .help("Open full metrics for \(local.hostName)")
                // Phase 2: ForEach over connected remote hosts here.
            }
        }
        .sheet(isPresented: $showingDetail) {
            HostDetailView(service: local)
        }
    }

    private var trailingLabel: String {
        local.snapshot == nil ? "connecting…" : "1 host"
    }
}

/// A single machine's live metrics card.
private struct HostCard: View {
    @ObservedObject var service: LocalHostMetricsService

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            header
            if let snap = service.snapshot {
                cpuOverview(snap)
                coreGrid
                metrics(snap)
            } else {
                Text("waiting for first sample…")
                    .font(CockpitTheme.mono(11))
                    .foregroundStyle(CockpitTheme.muted)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(CockpitTheme.panelAlt)
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(CockpitTheme.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 10))
    }

    private var header: some View {
        HStack {
            Circle().fill(CockpitTheme.green).frame(width: 7, height: 7)
            Text(service.hostName)
                .font(CockpitTheme.mono(13, weight: .bold))
                .foregroundStyle(CockpitTheme.ink)
            Spacer()
            if let snap = service.snapshot {
                thermalBadge(snap.cpu.thermalState)
            }
        }
    }

    @ViewBuilder
    private func cpuOverview(_ snap: HostSnapshot) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(snap.cpu.model.isEmpty ? "Processor" : snap.cpu.model)
                .font(CockpitTheme.mono(10))
                .foregroundStyle(CockpitTheme.muted)
            Spacer()
            Text("\(Int(snap.cpu.totalUsage.rounded()))%")
                .font(CockpitTheme.mono(16, weight: .bold))
                .foregroundStyle(usageColor(snap.cpu.totalUsage))
        }
        Sparkline(values: service.cpuHistory, range: 0...100)
            .frame(height: 30)
    }

    private var coreGrid: some View {
        let columns = Array(repeating: GridItem(.flexible(), spacing: 4), count: 5)
        return LazyVGrid(columns: columns, spacing: 4) {
            ForEach(Array(service.coreHistories.enumerated()), id: \.offset) { index, history in
                VStack(spacing: 2) {
                    HStack(spacing: 2) {
                        Text("C\(index)")
                            .font(CockpitTheme.mono(8))
                            .foregroundStyle(CockpitTheme.muted)
                        Spacer()
                        Text("\(Int((history.last ?? 0).rounded()))")
                            .font(CockpitTheme.mono(8))
                            .foregroundStyle(usageColor(history.last ?? 0))
                    }
                    Sparkline(values: history, color: coreColor(index), range: 0...100)
                        .frame(height: 16)
                }
            }
        }
    }

    @ViewBuilder
    private func metrics(_ snap: HostSnapshot) -> some View {
        HStack(spacing: 18) {
            metric("MEM", "\(fmt(snap.memory.usedGB))/\(fmt(snap.memory.totalGB))g",
                   sub: "\(Int(snap.memory.usagePercentage.rounded()))%")
            metric("GPU", "\(Int(snap.gpu.usage.rounded()))%",
                   sub: "\(fmt(snap.gpu.vramUsedGB))g")
            metric("DISK", "r\(fmt(snap.disk.readMBps))",
                   sub: "w\(fmt(snap.disk.writeMBps)) MB/s")
            metric("NET", "↓\(fmt(snap.network.downloadMBps))",
                   sub: "↑\(fmt(snap.network.uploadMBps)) MB/s")
        }
    }

    private func metric(_ label: String, _ value: String, sub: String) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(label).font(CockpitTheme.mono(8)).foregroundStyle(CockpitTheme.muted)
            Text(value).font(CockpitTheme.mono(12, weight: .bold)).foregroundStyle(CockpitTheme.ink)
            Text(sub).font(CockpitTheme.mono(8)).foregroundStyle(CockpitTheme.muted)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func thermalBadge(_ state: ThermalState) -> some View {
        let (text, color): (String, Color) = switch state {
        case .nominal: ("Normal", CockpitTheme.green)
        case .fair: ("Fair", CockpitTheme.green)
        case .serious: ("Hot", CockpitTheme.amber)
        case .critical: ("Critical", CockpitTheme.red)
        }
        return Text(text)
            .font(CockpitTheme.mono(9, weight: .bold))
            .foregroundStyle(color)
            .padding(.horizontal, 6).padding(.vertical, 2)
            .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(color.opacity(0.5), lineWidth: 1))
    }

    private func usageColor(_ v: Double) -> Color {
        switch v {
        case ..<70: CockpitTheme.green
        case ..<90: CockpitTheme.amber
        default: CockpitTheme.red
        }
    }

    private func coreColor(_ index: Int) -> Color {
        // Subtle hue variation per core, kept within the green family.
        let hues: [Color] = [CockpitTheme.green, CockpitTheme.greenDim, CockpitTheme.amber]
        return hues[index % hues.count]
    }

    private func fmt(_ v: Double) -> String {
        v >= 100 ? String(Int(v)) : String(format: "%.1f", v)
    }
}
