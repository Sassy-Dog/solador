import SwiftUI

/// CI Runners panel — all self-hosted runners registered with the GitHub org and
/// their idle/busy/offline state, across every host (mirrors Mission Control).
struct CIRunnersPanel: CockpitPanelView {
    static let kind: CockpitPanelKind = .ciRunners

    @EnvironmentObject private var service: CIRunnersService

    var body: some View {
        CockpitPanelContainer(kind: Self.kind, trailing: trailingLabel) {
            if !service.isAuthenticated {
                empty("connect a GitHub token in Settings")
            } else if let error = service.loadError {
                empty(error)
            } else if let summary = service.summary {
                VStack(alignment: .leading, spacing: 8) {
                    summaryRow(summary)
                    osChips(summary)
                    Divider().overlay(CockpitTheme.line)
                    ForEach(service.runners) { runner in
                        row(runner)
                    }
                }
            } else {
                empty("loading runners…")
            }
        }
    }

    private var trailingLabel: String? {
        guard let s = service.summary else { return nil }
        return "\(s.online)/\(s.total)"
    }

    private func summaryRow(_ s: RunnerSummary) -> some View {
        HStack(spacing: 16) {
            stat("ONLINE", "\(s.online)/\(s.total)", CockpitTheme.green)
            stat("BUSY", "\(s.busy)", s.busy > 0 ? CockpitTheme.amber : CockpitTheme.muted)
            stat("IDLE", "\(s.idle)", CockpitTheme.green)
        }
    }

    private func stat(_ label: String, _ value: String, _ color: Color) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(label).font(CockpitTheme.mono(8)).foregroundStyle(CockpitTheme.muted)
            Text(value).font(CockpitTheme.mono(15, weight: .bold)).foregroundStyle(color)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func osChips(_ s: RunnerSummary) -> some View {
        HStack(spacing: 8) {
            if s.macOSTotal > 0 { chip("macOS \(s.macOSOnline)/\(s.macOSTotal)") }
            if s.linuxTotal > 0 { chip("Linux \(s.linuxOnline)/\(s.linuxTotal)") }
        }
    }

    private func chip(_ text: String) -> some View {
        Text(text)
            .font(CockpitTheme.mono(9))
            .foregroundStyle(CockpitTheme.muted)
            .padding(.horizontal, 7).padding(.vertical, 3)
            .overlay(Capsule().strokeBorder(CockpitTheme.line, lineWidth: 1))
    }

    private func row(_ runner: CIRunner) -> some View {
        HStack(spacing: 7) {
            Circle().fill(dotColor(runner.state)).frame(width: 6, height: 6)
            Text(runner.name)
                .font(CockpitTheme.mono(11, weight: .bold))
                .foregroundStyle(CockpitTheme.ink)
                .lineLimit(1)
            Spacer()
            Text(runner.os.label.uppercased())
                .font(CockpitTheme.mono(8))
                .foregroundStyle(CockpitTheme.muted)
            Text(runner.state.label)
                .font(CockpitTheme.mono(9, weight: .bold))
                .foregroundStyle(dotColor(runner.state))
                .frame(width: 48, alignment: .trailing)
        }
    }

    private func dotColor(_ state: RunnerState) -> Color {
        switch state {
        case .idle: CockpitTheme.green
        case .busy: CockpitTheme.amber
        case .offline: CockpitTheme.muted
        }
    }

    private func empty(_ text: String) -> some View {
        Text(text)
            .font(CockpitTheme.mono(11))
            .foregroundStyle(CockpitTheme.muted)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}
