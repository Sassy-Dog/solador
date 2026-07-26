import SwiftUI

/// The distinct panels the cockpit can show. Each panel view declares its own
/// `kind`; the *arrangement* of panels lives separately in `CockpitLayout`, so
/// panels never know where they sit. Adding a panel = adding a case here and a
/// concrete view; adding a layout = adding a `CockpitLayout` value.
enum CockpitPanelKind: String, CaseIterable, Identifiable {
    case hosts
    case ghRunners
    case containers
    case ghWorkflows
    case claudeUsage
    case openclawAgents
    case azureCost

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .hosts: "Hosts"
        case .ghRunners: "GitHub Runners"
        case .containers: "Containers / VMs"
        case .ghWorkflows: "Repos"
        case .claudeUsage: "Claude Usage"
        case .openclawAgents: "OpenClaw"
        case .azureCost: "Azure Cost"
        }
    }

    var systemImage: String {
        switch self {
        case .hosts: "cpu"
        case .ghRunners: "server.rack"
        case .containers: "shippingbox"
        case .ghWorkflows: "checkmark.seal"
        case .claudeUsage: "gauge.with.needle"
        case .openclawAgents: "brain.head.profile"
        case .azureCost: "dollarsign.circle"
        }
    }

    /// The narrowest width at which this panel still reads well — its personal
    /// breakpoint. `CockpitBreakpoints.reflow` breaks a row apart only when *its*
    /// panels stop fitting, so a lean panel pair stays side-by-side at a width that
    /// forces a hungrier pair to stack.
    ///
    /// Each figure is the panel's widest fixed content plus its 28pt of card padding
    /// — e.g. Repos sums seven fixed numeric columns (312pt), their gaps, the status
    /// dot and a legible repo name. Widen a panel's content, widen this number.
    var minWidth: CGFloat {
        switch self {
        case .hosts: CockpitBreakpoints.hostCardMinWidth
        case .ghWorkflows: 560
        case .openclawAgents: 440
        case .ghRunners: 400
        case .containers: 400
        case .azureCost: 400
        case .claudeUsage: 360
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
                CockpitPlacement(kind: .ghWorkflows, span: .half),
                CockpitPlacement(kind: .ghRunners, span: .half)
            ],
            [
                CockpitPlacement(kind: .containers, span: .half),
                CockpitPlacement(kind: .openclawAgents, span: .half)
            ],
            [
                CockpitPlacement(kind: .claudeUsage, span: .half),
                CockpitPlacement(kind: .azureCost, span: .half)
            ]
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
