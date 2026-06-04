import SwiftUI

/// A minimal filled line chart for a series of values. Autoscales unless an
/// explicit `range` is given (use `0...100` for percentages so the baseline is
/// stable as values move).
struct Sparkline: View {
    let values: [Double]
    var color: Color = CockpitTheme.green
    var range: ClosedRange<Double>? = nil

    var body: some View {
        GeometryReader { geo in
            let pts = points(in: geo.size)
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

    private func points(in size: CGSize) -> [CGPoint] {
        guard values.count > 1 else { return [] }
        let lo = range?.lowerBound ?? (values.min() ?? 0)
        let hiRaw = range?.upperBound ?? (values.max() ?? 1)
        let hi = hiRaw - lo < 0.0001 ? lo + 1 : hiRaw
        let stepX = size.width / CGFloat(values.count - 1)
        return values.enumerated().map { i, v in
            let clamped = Swift.min(Swift.max(v, lo), hi)
            let norm = (clamped - lo) / (hi - lo)
            return CGPoint(x: CGFloat(i) * stepX, y: size.height * (1 - norm))
        }
    }
}
