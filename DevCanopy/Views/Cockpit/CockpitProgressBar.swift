import SwiftUI

/// A thin cockpit progress bar: a full-width track with a fill whose width tracks
/// `fraction` and whose color steps green → amber → red at the given thresholds.
/// Shared by the Claude Usage window bars and the Azure Cost projected-vs-budget bar.
///
/// The fill width is clamped to the track (so an over-budget `fraction > 1` pins the
/// bar full rather than overflowing), but the color uses the raw `fraction` — so an
/// over-budget bar still reads red.
struct CockpitProgressBar: View {
    let fraction: Double
    var amberAt: Double = 0.6
    var redAt: Double = 0.9

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                RoundedRectangle(cornerRadius: 2)
                    .fill(CockpitTheme.panelAlt)
                RoundedRectangle(cornerRadius: 2)
                    .fill(fillColor)
                    .frame(width: max(2, geo.size.width * min(1, max(0, fraction))))
            }
        }
        .frame(height: 4)
    }

    private var fillColor: Color {
        if fraction >= redAt { return CockpitTheme.red }
        if fraction >= amberAt { return CockpitTheme.amber }
        return CockpitTheme.green
    }
}
