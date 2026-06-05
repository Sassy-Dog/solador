import SwiftUI
import HostMetricsKit

/// Full Lupita-grade view for a single host, rendered inline as a card: host header,
/// big processor chart, per-core colored grid, Memory + Graphics, Disk + Network I/O.
/// This is the default Hosts surface (no drill-in). Contains no scroll view of its own —
/// the cockpit page scrolls.
struct HostLupitaView: View {
    @ObservedObject var service: HostMetricsService

    /// How many "section rows" tall the per-core grid is. Global (UserDefaults), so every host
    /// card uses the same cores-block height and the cards line up. Set in General settings.
    @AppStorage("coreRowSpan") private var coreRowSpan: Int = 2

    private static let cap = HostMetricsService.historyCapacity

    /// Height of one "section row" — the cores block spans `coreRowSpan` of these. Roughly the
    /// height of the processor chart so one row of cores reads like one of the other sections.
    private static let coreRowUnit: CGFloat = 110

    /// Per-core line colors, cycled — echoes Lupita's multi-hue core grid.
    private static let coreColors: [Color] = [
        Color(hex: 0x5b8def), Color(hex: 0x3fb950), Color(hex: 0xe0922a),
        Color(hex: 0xb066f0), Color(hex: 0xe0584f), Color(hex: 0x33c7c7),
        Color(hex: 0xe06ab0), Color(hex: 0x9bd34a), Color(hex: 0x4fb0e0),
        Color(hex: 0xd0c24a)
    ]
    private let cpuColor = Color(hex: 0x5b8def)
    private let memColor = Color(hex: 0xb066f0)
    private let gpuColor = Color(hex: 0x33c7c7)
    private let readColor = Color(hex: 0x3fb950)
    private let writeColor = Color(hex: 0xe0922a)
    private let netColor = Color(hex: 0x5b8def)
    private let netUpColor = Color(hex: 0x9bd34a)

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            hostHeader
            if let snap = service.snapshot {
                processorSection(snap)
                Divider().overlay(CockpitTheme.line)
                HStack(alignment: .top, spacing: 24) {
                    memorySection(snap)
                    graphicsSection(snap)
                }
                Divider().overlay(CockpitTheme.line)
                HStack(alignment: .top, spacing: 24) {
                    diskSection(snap)
                    networkSection(snap)
                }
            } else {
                Text(service.connectionState == .unreachable ? "unreachable" : "waiting for first sample…")
                    .font(CockpitTheme.mono(12))
                    .foregroundStyle(service.connectionState == .unreachable ? CockpitTheme.red : CockpitTheme.muted)
            }
        }
        .padding(18)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(CockpitTheme.panel)
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(CockpitTheme.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 12))
    }

    private var hostHeader: some View {
        HStack(spacing: 8) {
            Circle().fill(connectionColor).frame(width: 8, height: 8)
            Text(service.hostName)
                .font(CockpitTheme.mono(14, weight: .bold))
                .foregroundStyle(CockpitTheme.ink)
            Spacer()
        }
    }

    private var connectionColor: Color {
        switch service.connectionState {
        case .local, .connected: CockpitTheme.green
        case .connecting: CockpitTheme.amber
        case .unreachable: CockpitTheme.red
        }
    }

    // MARK: Processor

    @ViewBuilder
    private func processorSection(_ snap: HostSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            sectionHeader(
                icon: "cpu",
                title: "Processor (\(snap.cpu.model.isEmpty ? "CPU" : snap.cpu.model))",
                badge: thermalBadge(snap.cpu.thermalState),
                value: "\(Int(snap.cpu.totalUsage.rounded()))%",
                valueColor: usageColor(snap.cpu.totalUsage)
            )
            percentChart(service.cpuHistory, color: cpuColor, height: 110)
            coreGrid
        }
    }

    /// Per-core grid: columns chosen so the core count divides evenly (full last row), and the
    /// whole block bounded to a fixed height (`coreRowSpan` section-rows) divided across the
    /// visual rows — Lupita's trick. The fixed block height is identical on every host, so cards
    /// align regardless of core count.
    @ViewBuilder
    private var coreGrid: some View {
        let count = service.coreHistories.count
        if count > 0 {
            let cols = coreColumns(coreCount: count)
            let visualRows = max(1, Int((Double(count) / Double(cols)).rounded(.up)))
            let spacing: CGFloat = 8
            let blockHeight = CGFloat(max(1, coreRowSpan)) * Self.coreRowUnit
            let cellHeight = (blockHeight - spacing * CGFloat(visualRows - 1)) / CGFloat(visualRows)
            let columns = Array(repeating: GridItem(.flexible(), spacing: spacing), count: cols)
            LazyVGrid(columns: columns, spacing: spacing) {
                ForEach(Array(service.coreHistories.enumerated()), id: \.offset) { index, history in
                    coreCell(index: index, history: history, cellHeight: cellHeight)
                }
            }
            .frame(height: blockHeight, alignment: .top)
        }
    }

    private func coreCell(index: Int, history: [Double], cellHeight: CGFloat) -> some View {
        let color = Self.coreColors[index % Self.coreColors.count]
        return VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text("Core \(index)").font(CockpitTheme.mono(10)).foregroundStyle(CockpitTheme.muted)
                Spacer()
                Text("\(Int((history.last ?? 0).rounded()))%")
                    .font(CockpitTheme.mono(10, weight: .bold))
                    .foregroundStyle(usageColor(history.last ?? 0))
            }
            // Sparkline (a GeometryReader) takes whatever height is left after the label.
            Sparkline(values: history, capacity: Self.cap, color: color, range: 0...100)
                .frame(maxHeight: .infinity)
        }
        .padding(8)
        .frame(height: cellHeight, alignment: .top)
        .background(CockpitTheme.panelAlt)
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }

    // MARK: Memory / Graphics

    @ViewBuilder
    private func memorySection(_ snap: HostSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            sectionHeader(
                icon: "memorychip",
                title: "Memory",
                badge: nil,
                value: "\(fmt(snap.memory.usedGB)) / \(Int(snap.memory.totalGB)) GB",
                valueColor: CockpitTheme.ink
            )
            percentChart(service.memoryHistory, color: memColor, height: 90)
            HStack {
                Text("Swap: \(fmt(snap.memory.swapUsedGB)) GB")
                Spacer()
                Text("Pressure: \(Int(snap.memory.pressure.rounded()))%")
                    .foregroundStyle(pressureColor(snap.memory.pressure))
            }
            .font(CockpitTheme.mono(10))
            .foregroundStyle(CockpitTheme.muted)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder
    private func graphicsSection(_ snap: HostSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            sectionHeader(
                icon: "display",
                title: "Graphics",
                badge: nil,
                value: "\(Int(snap.gpu.usage.rounded()))%",
                valueColor: gpuColor
            )
            percentChart(service.gpuHistory, color: gpuColor, height: 90)
            Text("VRAM: \(fmt(snap.gpu.vramUsedGB)) / \(fmt(snap.gpu.vramTotalGB)) GB")
                .font(CockpitTheme.mono(10))
                .foregroundStyle(CockpitTheme.muted)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: Disk / Network

    @ViewBuilder
    private func diskSection(_ snap: HostSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            ioHeader(icon: "internaldrive", title: "Disk I/O",
                     left: ("Read", "\(fmt(snap.disk.readMBps)) MB/s", readColor),
                     right: ("Write", "\(fmt(snap.disk.writeMBps)) MB/s", writeColor))
            ioChart(seriesA: service.diskReadHistory, colorA: readColor,
                    seriesB: service.diskWriteHistory, colorB: writeColor)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder
    private func networkSection(_ snap: HostSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            ioHeader(icon: "network", title: "Network I/O",
                     left: ("Down", "\(fmt(snap.network.downloadMBps)) MB/s", netColor),
                     right: ("Up", "\(fmt(snap.network.uploadMBps)) MB/s", netUpColor))
            ioChart(seriesA: service.netDownHistory, colorA: netColor,
                    seriesB: service.netUpHistory, colorB: netUpColor)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: Chart helpers

    private func percentChart(_ values: [Double], color: Color, height: CGFloat) -> some View {
        HStack(alignment: .top, spacing: 6) {
            VStack {
                Text("100%"); Spacer(); Text("50%"); Spacer(); Text("0%")
            }
            .font(CockpitTheme.mono(8)).foregroundStyle(CockpitTheme.muted)
            .frame(height: height)
            Sparkline(values: values, capacity: Self.cap, color: color, range: 0...100)
                .frame(height: height)
                .background(gridlines)
        }
    }

    private func ioChart(seriesA: [Double], colorA: Color, seriesB: [Double], colorB: Color) -> some View {
        let maxV = max(seriesA.max() ?? 0, seriesB.max() ?? 0, 0.1)
        return HStack(alignment: .top, spacing: 6) {
            VStack {
                Text("\(fmt(maxV))"); Spacer(); Text("0")
            }
            .font(CockpitTheme.mono(8)).foregroundStyle(CockpitTheme.muted)
            .frame(height: 90)
            ZStack {
                Sparkline(values: seriesA, capacity: Self.cap, color: colorA, range: 0...maxV)
                Sparkline(values: seriesB, capacity: Self.cap, color: colorB, range: 0...maxV)
            }
            .frame(height: 90)
            .background(gridlines)
        }
    }

    private var gridlines: some View {
        VStack { Divider().overlay(CockpitTheme.line); Spacer(); Divider().overlay(CockpitTheme.line); Spacer(); Divider().overlay(CockpitTheme.line) }
    }

    // MARK: Header bits

    private func sectionHeader(icon: String, title: String, badge: AnyView?, value: String, valueColor: Color) -> some View {
        HStack {
            Image(systemName: icon).foregroundStyle(CockpitTheme.green)
            Text(title).font(CockpitTheme.mono(13, weight: .bold)).foregroundStyle(CockpitTheme.ink)
            if let badge { badge }
            Spacer()
            Text(value).font(CockpitTheme.mono(18, weight: .bold)).foregroundStyle(valueColor)
        }
    }

    private func ioHeader(icon: String, title: String, left: (String, String, Color), right: (String, String, Color)) -> some View {
        HStack {
            Image(systemName: icon).foregroundStyle(CockpitTheme.green)
            Text(title).font(CockpitTheme.mono(13, weight: .bold)).foregroundStyle(CockpitTheme.ink)
            Spacer()
            VStack(alignment: .trailing, spacing: 1) {
                legend(left.0, left.1, left.2)
                legend(right.0, right.1, right.2)
            }
        }
    }

    private func legend(_ label: String, _ value: String, _ color: Color) -> some View {
        HStack(spacing: 4) {
            Circle().fill(color).frame(width: 6, height: 6)
            Text("\(label):").font(CockpitTheme.mono(9)).foregroundStyle(CockpitTheme.muted)
            Text(value).font(CockpitTheme.mono(9, weight: .bold)).foregroundStyle(CockpitTheme.ink)
        }
    }

    private func thermalBadge(_ state: ThermalState) -> AnyView {
        let (text, color): (String, Color) = switch state {
        case .nominal: ("Normal", CockpitTheme.green)
        case .fair: ("Fair", CockpitTheme.green)
        case .serious: ("Hot", CockpitTheme.amber)
        case .critical: ("Critical", CockpitTheme.red)
        }
        return AnyView(
            HStack(spacing: 4) {
                Image(systemName: "thermometer.medium").font(.system(size: 9))
                Text(text).font(CockpitTheme.mono(10, weight: .bold))
            }
            .foregroundStyle(color)
            .padding(.horizontal, 7).padding(.vertical, 3)
            .background(color.opacity(0.12))
            .clipShape(Capsule())
        )
    }

    private func usageColor(_ v: Double) -> Color {
        switch v { case ..<70: CockpitTheme.green; case ..<90: CockpitTheme.amber; default: CockpitTheme.red }
    }
    private func pressureColor(_ v: Double) -> Color {
        switch v { case ..<60: CockpitTheme.green; case ..<85: CockpitTheme.amber; default: CockpitTheme.red }
    }
    private func fmt(_ v: Double) -> String {
        v >= 100 ? String(Int(v)) : String(format: "%.1f", v)
    }
}
