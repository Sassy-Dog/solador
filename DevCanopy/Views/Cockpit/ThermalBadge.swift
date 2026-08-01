import HostMetricsKit
import SwiftUI

/// The thermal-pressure capsule beside a host's processor header.
///
/// Only ever constructed for a level the host actually reported: an unmeasured
/// thermal state renders no badge at all — `HostMetricsPanel` maps the Optional
/// — rather than the "Normal" a defaulted `0` used to paint on every Linux host.
struct ThermalBadge: View {
    let state: ThermalState

    var body: some View {
        let (text, color) = Self.label(for: state)
        return HStack(spacing: 4) {
            Image(systemName: "thermometer.medium").font(.system(size: 9))
            Text(text).font(CockpitTheme.mono(10, weight: .bold))
        }
        .foregroundStyle(color)
        .padding(.horizontal, 7).padding(.vertical, 3)
        .background(color.opacity(0.12))
        .clipShape(Capsule())
    }

    private static func label(for state: ThermalState) -> (String, Color) {
        switch state {
        case .nominal: ("Normal", CockpitTheme.green)
        case .fair: ("Fair", CockpitTheme.green)
        case .serious: ("Hot", CockpitTheme.amber)
        case .critical: ("Critical", CockpitTheme.red)
        }
    }
}
