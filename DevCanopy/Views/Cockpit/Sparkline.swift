import SwiftUI

/// A minimal filled line chart for a series of values, rendered like Lupita: the
/// newest sample is pinned to the right edge and points march left at a fixed
/// spacing keyed to `capacity` (the buffer window). As samples arrive the line
/// slides in from the right; once the buffer is full it scrolls. This keeps point
/// density constant instead of stretching a few early points across the width.
///
/// Pass `range` (e.g. `0...100`) to keep the baseline stable; omit to autoscale.
struct Sparkline: View {
    let values: [Double]
    var capacity: Int
    var color: Color = CockpitTheme.green
    var range: ClosedRange<Double>?

    var body: some View {
        GeometryReader { geo in
            let pts = Self.layout(values: values, capacity: capacity, size: geo.size, range: range)
            if pts.count > 1 {
                ZStack {
                    Path { path in
                        path.move(to: CGPoint(x: pts[0].x, y: geo.size.height))
                        pts.forEach { path.addLine(to: $0) }
                        path.addLine(to: CGPoint(x: pts[pts.count - 1].x, y: geo.size.height))
                        path.closeSubpath()
                    }
                    .fill(LinearGradient(
                        colors: [color.opacity(0.28), color.opacity(0.0)],
                        startPoint: .top, endPoint: .bottom
                    ))

                    Path { path in
                        path.move(to: pts[0])
                        pts.dropFirst().forEach { path.addLine(to: $0) }
                    }
                    .stroke(color, style: StrokeStyle(lineWidth: 1.5, lineJoin: .round))
                }
            }
        }
    }

    /// Pure point mapping (testable). Newest value pinned at `width`; points step
    /// left by `width / (capacity - 1)`. Only the most recent `capacity` values are
    /// plotted. y is normalized within `range` (clamped) or autoscaled over values.
    static func layout(values: [Double], capacity: Int, size: CGSize, range: ClosedRange<Double>?) -> [CGPoint] {
        let window = values.suffix(capacity)
        guard window.count > 1, capacity > 1 else { return [] }
        let pts = Array(window)

        let lo = range?.lowerBound ?? (pts.min() ?? 0)
        let hiRaw = range?.upperBound ?? (pts.max() ?? 1)
        let hi = hiRaw - lo < 0.0001 ? lo + 1 : hiRaw

        let stepX = size.width / CGFloat(capacity - 1)
        let n = pts.count
        return pts.enumerated().map { i, v in
            // offset of this point from the newest (which sits at the right edge)
            let offsetFromNewest = (n - 1) - i
            let x = size.width - CGFloat(offsetFromNewest) * stepX
            let clamped = Swift.min(Swift.max(v, lo), hi)
            let norm = (clamped - lo) / (hi - lo)
            return CGPoint(x: x, y: size.height * (1 - norm))
        }
    }
}
