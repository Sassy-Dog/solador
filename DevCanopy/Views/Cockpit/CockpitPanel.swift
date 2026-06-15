import SwiftUI

/// The distinct panels the cockpit can show. Each panel view declares its own
/// `kind`; the *arrangement* of panels lives separately in `CockpitLayout`, so
/// panels never know where they sit. Adding a panel = adding a case here and a
/// concrete view; adding a layout = adding a `CockpitLayout` value.
enum CockpitPanelKind: String, CaseIterable, Identifiable {
    case hosts
    case ciRunners
    case containers
    case ciHealth
    case gitWorktrees
    case claudeUsage

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .hosts: "Hosts"
        case .ciRunners: "CI Runners"
        case .containers: "Containers / VMs"
        case .ciHealth: "CI Health"
        case .gitWorktrees: "Git / Worktrees"
        case .claudeUsage: "Claude Usage"
        }
    }

    var systemImage: String {
        switch self {
        case .hosts: "cpu"
        case .ciRunners: "server.rack"
        case .containers: "shippingbox"
        case .ciHealth: "checkmark.seal"
        case .gitWorktrees: "arrow.triangle.branch"
        case .claudeUsage: "gauge.with.needle"
        }
    }
}

/// How wide a panel sits within its cockpit row.
enum PanelSpan {
    case full
    case half
    case third
}

/// One panel placed in the layout.
struct CockpitPlacement: Identifiable {
    let kind: CockpitPanelKind
    let span: PanelSpan
    var id: String {
        kind.id
    }
}

/// A data-driven cockpit arrangement: ordered rows of placements. This is the
/// seam that makes layouts swappable without touching panel views.
struct CockpitLayout {
    let name: String
    let rows: [[CockpitPlacement]]

    /// Every panel kind present in this layout, flattened in render order.
    var panelKinds: [CockpitPanelKind] {
        rows.flatMap { $0.map(\.kind) }
    }
}

extension CockpitLayout {
    /// Layout B — "hosts-forward": big host cards on top, work surfaces below.
    static let hostsForward = CockpitLayout(
        name: "Hosts-forward",
        rows: [
            [CockpitPlacement(kind: .hosts, span: .full)],
            [
                CockpitPlacement(kind: .ciRunners, span: .half),
                CockpitPlacement(kind: .containers, span: .half)
            ],
            [
                CockpitPlacement(kind: .ciHealth, span: .half),
                CockpitPlacement(kind: .gitWorktrees, span: .half)
            ],
            [CockpitPlacement(kind: .claudeUsage, span: .full)]
        ]
    )
}

/// A panel view that knows its own kind. Concrete panels conform so the cockpit
/// can title/route them without hard-coding the mapping in two places.
protocol CockpitPanelView: View {
    static var kind: CockpitPanelKind { get }
}

/// Shared chrome for every panel: titled, bordered card in the terminal palette.
struct CockpitPanelContainer<Content: View>: View {
    let kind: CockpitPanelKind
    var trailing: String?
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: kind.systemImage)
                    .foregroundStyle(CockpitTheme.green)
                Text(kind.title.uppercased())
                    .font(CockpitTheme.mono(13, weight: .bold))
                    .foregroundStyle(CockpitTheme.ink)
                Spacer()
                if let trailing {
                    Text(trailing)
                        .font(CockpitTheme.mono(11))
                        .foregroundStyle(CockpitTheme.muted)
                }
            }
            content
        }
        .padding(14)
        // Fill the height offered by the cockpit Grid so cards in the same row
        // share a height (the card chrome stretches; content stays top-aligned).
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(CockpitTheme.panel)
        .overlay(
            RoundedRectangle(cornerRadius: 12)
                .strokeBorder(CockpitTheme.line, lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: 12))
    }
}
