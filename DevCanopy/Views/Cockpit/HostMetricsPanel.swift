import HostMetricsKit
import SwiftUI

/// Full metrics view for a single host, rendered inline as a card: host header,
/// big processor chart, per-core colored grid, Memory + Graphics, Disk + Network I/O.
/// This is the default Hosts surface (no drill-in). Contains no scroll view of its own —
/// the cockpit page scrolls.
struct HostMetricsPanel: View {
    @ObservedObject var service: HostMetricsService

    /// How many "section rows" tall the per-core grid is. Global (UserDefaults), so every host
    /// card uses the same cores-block height and the cards line up. Set in General settings.
    @AppStorage("coreRowSpan") private var coreRowSpan: Int = 2

    /// Reserve the Volumes section for this many volumes — the max across the
    /// cards in this row — laid out in `volumeColumns`, so the sections below
    /// (Top CPU/RAM) line up across side-by-side host cards. Set by `HostsPanel`.
    var volumeSlots: Int = 0
    var volumeColumns: Int = 1

    private static let cap = HostMetricsService.historyCapacity
    private static let volumeCellHeight: CGFloat = 28

    /// Height of one "section row" — the cores block spans `coreRowSpan` of these. Roughly the
    /// height of the processor chart so one row of cores reads like one of the other sections.
    private static let coreRowUnit: CGFloat = 110

    /// Per-core line colors, cycled — echoes Lupita's multi-hue core grid.
    private static let coreColors: [Color] = [
        Color(hex: 0x5B8DEF), Color(hex: 0x3FB950), Color(hex: 0xE0922A),
        Color(hex: 0xB066F0), Color(hex: 0xE0584F), Color(hex: 0x33C7C7),
        Color(hex: 0xE06AB0), Color(hex: 0x9BD34A), Color(hex: 0x4FB0E0),
        Color(hex: 0xD0C24A)
    ]
    private let cpuColor = Color(hex: 0x5B8DEF)
    private let memColor = Color(hex: 0xB066F0)
    private let gpuColor = Color(hex: 0x33C7C7)
    private let readColor = Color(hex: 0x3FB950)
    private let writeColor = Color(hex: 0xE0922A)
    private let netColor = Color(hex: 0x5B8DEF)
    private let netUpColor = Color(hex: 0x9BD34A)

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
                let volumeReserve = max(volumeSlots, service.visibleVolumes.count)
                if volumeReserve > 0 {
                    Divider().overlay(CockpitTheme.line)
                    volumesSection(service.visibleVolumes, slots: volumeReserve, columns: max(1, volumeColumns))
                }
                if !snap.processes.isEmpty {
                    Divider().overlay(CockpitTheme.line)
                    processesSection(snap.processes)
                }
            } else {
                let failed = service.connectionState.isFailed
                Text(failed ? service.connectionState.failureLabel : "waiting for first sample…")
                    .font(CockpitTheme.mono(12))
                    .foregroundStyle(failed ? CockpitTheme.red : CockpitTheme.muted)
                    .help(failed ? failureTooltip : "")
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
        case .failed: CockpitTheme.red
        }
    }

    /// Cause-specific guidance shown on hover over a failed host, so the user
    /// chases the right layer instead of SSHing into a healthy VM.
    private var failureTooltip: String {
        guard case let .failed(error) = service.connectionState else { return "" }
        switch error {
        case .authFailed:
            return "Agent rejected the bearer token (401). Check the host's token in Settings."
        case .decodeFailed:
            return "Agent responded but the payload didn't decode — likely an agent/app version skew after a redeploy."
        case let .httpStatus(code):
            return "Agent returned HTTP \(code)."
        case .unreachable:
            return "Couldn't reach the agent. Check the host is up and the agent is running."
        }
    }

    // MARK: Processor

    private func processorSection(_ snap: HostSnapshot) -> some View {
        let model = CPUModelFormatter.clean(snap.cpu.model)
        return VStack(alignment: .leading, spacing: 12) {
            sectionHeader(
                icon: "cpu",
                title: model.isEmpty ? "Processor" : model,
                // An unread thermal state renders no badge at all, deliberately
                // rather than via the encoding coincidence that `0` means
                // Nominal. The badge must never claim a state nobody measured.
                badge: snap.cpu.thermalState.map { AnyView(ThermalBadge(state: $0)) },
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
            Sparkline(values: history, capacity: Self.cap, color: color, range: 0 ... 100)
                .frame(maxHeight: .infinity)
        }
        .padding(8)
        .frame(height: cellHeight, alignment: .top)
        .background(CockpitTheme.panelAlt)
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }

    // MARK: Memory / Graphics

    private func memorySection(_ snap: HostSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            sectionHeader(
                icon: "memorychip",
                title: "Memory",
                badge: nil,
                value: HostMetricLabels.memoryUsage(used: snap.memory.usedGB, total: snap.memory.totalGB),
                valueColor: snap.memory.usedGB == nil ? CockpitTheme.muted : CockpitTheme.ink
            )
            percentChart(service.memoryHistory, color: memColor, height: 90)
            HStack {
                // A failed VM-statistics read loses used, swap and pressure
                // together, so all three go muted rather than one inventing a 0.
                Text(HostMetricLabels.swap(snap.memory.swapUsedGB))
                Spacer()
                // Unmeasured pressure reads "—" in the muted tint: a green 0%
                // is the exact fabrication this panel must not paint.
                Text("Pressure: \(HostMetricLabels.percent(snap.memory.pressure))")
                    .foregroundStyle(snap.memory.pressure.map(pressureColor) ?? CockpitTheme.muted)
            }
            .font(CockpitTheme.mono(10))
            .foregroundStyle(CockpitTheme.muted)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func graphicsSection(_ snap: HostSnapshot) -> some View {
        // No adapter means no reading, not a reading of zero — and no series
        // either. `HostMetricsService` never records a sample for an absent GPU,
        // so the chart draws its grid and nothing else rather than a flat 0%
        // line underneath the em dash above.
        let present = snap.gpu.isPresent
        return VStack(alignment: .leading, spacing: 8) {
            sectionHeader(
                icon: "display",
                title: "Graphics",
                badge: nil,
                value: HostMetricLabels.gpuUsage(snap.gpu),
                valueColor: present ? gpuColor : CockpitTheme.muted
            )
            percentChart(service.gpuHistory, color: gpuColor, height: 90)
            Text(HostMetricLabels.vram(snap.gpu))
                .font(CockpitTheme.mono(10))
                .foregroundStyle(CockpitTheme.muted)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: Disk / Network

    private func diskSection(_ snap: HostSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            ioHeader(
                icon: "internaldrive",
                title: "Disk I/O",
                left: ("Read", HostMetricLabels.rate(snap.disk.readMBps), readColor),
                right: ("Write", HostMetricLabels.rate(snap.disk.writeMBps), writeColor)
            )
            ioChart(
                seriesA: service.diskReadHistory,
                colorA: readColor,
                seriesB: service.diskWriteHistory,
                colorB: writeColor
            )
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func networkSection(_ snap: HostSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            ioHeader(
                icon: "network",
                title: "Network I/O",
                left: ("Down", HostMetricLabels.rate(snap.network.downloadMBps), netColor),
                right: ("Up", HostMetricLabels.rate(snap.network.uploadMBps), netUpColor)
            )
            ioChart(
                seriesA: service.netDownHistory,
                colorA: netColor,
                seriesB: service.netUpHistory,
                colorB: netUpColor
            )
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: Volumes

    /// Renders volumes, reserving `slots` (the row's max volume count) worth of
    /// rows so every card reserves the same Volumes height and the sections below
    /// align. Most-important (fullest) volumes lead; layout in `VolumeGridLayout`.
    @ViewBuilder
    private func volumesSection(_ volumes: [VolumeUsage], slots: Int, columns: Int) -> some View {
        let sorted = volumes.sorted { $0.percentUsed > $1.percentUsed }
        let reservedRows = VolumeGridLayout.rowCount(slots, columns: columns)
        let rows = VolumeGridLayout.rows(sorted, columns: columns)
        let pad = max(0, reservedRows - rows.count)
        VStack(alignment: .leading, spacing: 8) {
            sectionHeader(
                icon: "externaldrive",
                title: "Volumes",
                badge: nil,
                value: "\(volumes.count)",
                valueColor: CockpitTheme.muted
            )
            VStack(alignment: .leading, spacing: 6) {
                ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                    HStack(spacing: 16) {
                        ForEach(row) { volumeCell($0) }
                    }
                }
                ForEach(Array(0 ..< pad), id: \.self) { _ in
                    Color.clear.frame(height: Self.volumeCellHeight)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func volumeCell(_ v: VolumeUsage) -> some View {
        volumeRow(v).frame(maxWidth: .infinity, alignment: .leading)
            .frame(height: Self.volumeCellHeight, alignment: .top)
            .contentShape(Rectangle())
            .contextMenu {
                if let fstype = v.fstype {
                    Text("\(v.mount) — \(fstype)")
                }
                Button("Hide \u{201C}\(v.mount)\u{201D}") { service.hideVolume(v.mount) }
            }
            .help(v.fstype.map { "\(v.mount) (\($0))" } ?? v.mount)
    }

    private func volumeRow(_ v: VolumeUsage) -> some View {
        let pct = v.percentUsed
        let color = volumeColor(pct)
        return VStack(alignment: .leading, spacing: 3) {
            HStack {
                Text(v.mount).font(CockpitTheme.mono(10, weight: .bold))
                    .foregroundStyle(CockpitTheme.ink).lineLimit(1)
                Spacer()
                Text("\(HostMetricLabels.number(v.usedGB)) / \(HostMetricLabels.number(v.totalGB)) GB · \(Int(pct.rounded()))%")
                    .font(CockpitTheme.mono(9)).foregroundStyle(color)
            }
            GeometryReader { geo in
                ZStack(alignment: .leading) {
                    RoundedRectangle(cornerRadius: 2).fill(CockpitTheme.panelAlt)
                    RoundedRectangle(cornerRadius: 2).fill(color)
                        .frame(width: geo.size.width * CGFloat(min(max(pct, 0), 100) / 100))
                }
            }
            .frame(height: 4)
        }
    }

    /// Volumes fill up gradually; warn earlier than CPU since a full volume fails.
    private func volumeColor(_ pct: Double) -> Color {
        switch pct { case ..<85: CockpitTheme.green
        case ..<95: CockpitTheme.amber
        default: CockpitTheme.red }
    }

    // MARK: Top processes

    private func processesSection(_ procs: [ProcessSample]) -> some View {
        HStack(alignment: .top, spacing: 24) {
            processList(
                "Top CPU",
                procs.sorted { $0.cpuPercent > $1.cpuPercent }.prefix(5).map(\.self),
                value: { "\(Int($0.cpuPercent.rounded()))%" }
            )
            processList(
                "Top RAM",
                procs.sorted { $0.memoryMB > $1.memoryMB }.prefix(5).map(\.self),
                value: { memoryLabel($0.memoryMB) }
            )
        }
    }

    private func processList(_ title: String, _ items: [ProcessSample], value: @escaping (ProcessSample) -> String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title.uppercased())
                .font(CockpitTheme.mono(10, weight: .bold))
                .foregroundStyle(CockpitTheme.muted)
            ForEach(items) { p in
                HStack(spacing: 6) {
                    Text(p.name).font(CockpitTheme.mono(10)).foregroundStyle(CockpitTheme.ink).lineLimit(1)
                    Spacer(minLength: 8)
                    Text(value(p)).font(CockpitTheme.mono(10, weight: .bold)).foregroundStyle(CockpitTheme.muted)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func memoryLabel(_ mb: Double) -> String {
        HostMetricLabels.processMemory(mb)
    }

    // MARK: Chart helpers

    private func percentChart(_ values: [Double], color: Color, height: CGFloat) -> some View {
        HStack(alignment: .top, spacing: 6) {
            VStack {
                Text("100%")
                Spacer()
                Text("50%")
                Spacer()
                Text("0%")
            }
            .font(CockpitTheme.mono(8)).foregroundStyle(CockpitTheme.muted)
            .frame(height: height)
            Sparkline(values: values, capacity: Self.cap, color: color, range: 0 ... 100)
                .frame(height: height)
                .background(gridlines)
        }
    }

    private func ioChart(seriesA: [Double], colorA: Color, seriesB: [Double], colorB: Color) -> some View {
        let maxV = max(seriesA.max() ?? 0, seriesB.max() ?? 0, 0.1)
        return HStack(alignment: .top, spacing: 6) {
            // Reserve the axis width to the widest label so the plot stays
            // anchored when the auto-scaled max changes its digit-count.
            VStack(alignment: .trailing) {
                anchored(
                    fmtAxis(maxV),
                    reserve: "9999",
                    size: 8,
                    color: CockpitTheme.muted,
                    alignment: .trailing
                )
                Spacer()
                anchored(
                    "0",
                    reserve: "9999",
                    size: 8,
                    color: CockpitTheme.muted,
                    alignment: .trailing
                )
            }
            .frame(height: 90)
            ZStack {
                Sparkline(values: seriesA, capacity: Self.cap, color: colorA, range: 0 ... maxV)
                Sparkline(values: seriesB, capacity: Self.cap, color: colorB, range: 0 ... maxV)
            }
            .frame(height: 90)
            .background(gridlines)
        }
    }

    private var gridlines: some View {
        VStack { Divider().overlay(CockpitTheme.line)
            Spacer()
            Divider().overlay(CockpitTheme.line)
            Spacer()
            Divider().overlay(CockpitTheme.line)
        }
    }

    // MARK: Header bits

    /// The title is the only compressible element: it truncates on one line while the
    /// badge and value are fixed-size, so a long CPU model can never wrap the header
    /// to two lines (which would change the card height and shift the cockpit grid).
    private func sectionHeader(icon: String, title: String, badge: AnyView?, value: String, valueColor: Color) -> some View {
        HStack {
            Image(systemName: icon).foregroundStyle(CockpitTheme.green)
            Text(title)
                .font(CockpitTheme.mono(13, weight: .bold))
                .foregroundStyle(CockpitTheme.ink)
                .lineLimit(1)
                .truncationMode(.tail)
                .layoutPriority(1)
            if let badge { badge.fixedSize() }
            Spacer()
            Text(value)
                .font(CockpitTheme.mono(18, weight: .bold))
                .foregroundStyle(valueColor)
                .fixedSize()
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
            // Anchor both columns so the dot and label stay put as the value's
            // digit-count changes — otherwise the live numbers jitter the labels.
            anchored(
                "\(label):",
                reserve: "Write:",
                size: 9,
                color: CockpitTheme.muted,
                alignment: .leading
            )
            anchored(
                value,
                reserve: "9999 MB/s",
                size: 9,
                weight: .bold,
                color: CockpitTheme.ink,
                alignment: .trailing
            )
        }
    }

    /// Renders `text` inside a footprint reserved for `placeholder`, aligned as
    /// requested. Because the cockpit font is monospaced, sizing to a worst-case
    /// placeholder keeps value labels (and their neighbors) from reflowing as the
    /// underlying number's width changes — anchoring them to kill the distraction.
    private func anchored(
        _ text: String,
        reserve placeholder: String,
        size: CGFloat,
        weight: Font.Weight = .regular,
        color: Color,
        alignment: Alignment
    ) -> some View {
        Text(placeholder)
            .font(CockpitTheme.mono(size, weight: weight))
            .hidden()
            .overlay(alignment: alignment) {
                Text(text)
                    .font(CockpitTheme.mono(size, weight: weight))
                    .monospacedDigit()
                    .foregroundStyle(color)
            }
    }

    private func usageColor(_ v: Double) -> Color {
        switch v { case ..<70: CockpitTheme.green
        case ..<90: CockpitTheme.amber
        default: CockpitTheme.red }
    }

    private func pressureColor(_ v: Double) -> Color {
        switch v { case ..<60: CockpitTheme.green
        case ..<85: CockpitTheme.amber
        default: CockpitTheme.red }
    }

    /// Formatting lives in `HostMetricLabels` — the same code that decides what
    /// an unmeasured value looks like — so a number's shape can't drift between
    /// the cells that can be unknown and the cells that can't.
    private func fmtAxis(_ v: Double) -> String {
        HostMetricLabels.axis(v)
    }
}
