import Foundation
import HostMetricsKit

/// Every string a host card paints for a metric, and the one place a value the
/// agent could not measure is lowered to an em dash.
///
/// Pure values, no SwiftUI — unit-tested like `CockpitBreakpoints` and
/// `CPUModelFormatter`. The rule it enforces: `nil` (nobody measured it) renders
/// ``unknown``, while a measured `0` renders "0". A cockpit that shows a green
/// `Pressure: 0%` for a figure Linux never reports is lying, and the two cases
/// are indistinguishable once either one has been turned into a number.
enum HostMetricLabels {
    /// What an unmeasured value renders as, everywhere. Matches the em dash the
    /// Rust view-model paints (`viewmodel::card::UNKNOWN`) so the two cockpits
    /// admit ignorance the same way.
    static let unknown = "—"

    /// A gigabyte/megabyte magnitude: one decimal below 100, whole above, so the
    /// label's width stays bounded as the number grows.
    static func number(_ value: Double?) -> String {
        guard let value else { return unknown }
        return value >= 100 ? String(Int(value)) : String(format: "%.1f", value)
    }

    /// A whole-number percentage ("41%"), or "—" when unmeasured.
    static func percent(_ value: Double?) -> String {
        guard let value else { return unknown }
        return "\(Int(value.rounded()))%"
    }

    /// A throughput rate with bounded width and unit: stays in MB/s up to 1000,
    /// then switches to GB/s. Caps the label so bursts (e.g. 17 GB/s disk reads)
    /// don't blow out the anchored legend column.
    static func rate(_ mbps: Double?) -> String {
        guard let mbps else { return unknown }
        return mbps >= 1000 ? String(format: "%.1f GB/s", mbps / 1024) : "\(number(mbps)) MB/s"
    }

    /// A unitless axis tick with bounded width: collapses thousands to a `k`
    /// suffix ("17151" → "17k") so the chart's auto-scaled max never widens the
    /// y-axis column and shifts the plot. Always a real reading — the axis is
    /// derived from the plotted series, which never holds an unmeasured sample.
    static func axis(_ value: Double) -> String {
        value >= 1000 ? String(format: "%.0fk", value / 1000) : number(value)
    }

    /// Process memory: GB once past a gigabyte, MB below.
    static func processMemory(_ megabytes: Double) -> String {
        megabytes >= 1024 ? "\(number(megabytes / 1024)) GB" : "\(Int(megabytes.rounded())) MB"
    }

    /// The Memory header value. Capacity is always measured, so an unreadable
    /// `used` blanks only its own half — "— / 32 GB" still says how much RAM the
    /// machine has, which is a fact the failed read didn't take away.
    static func memoryUsage(used: Double?, total: Double) -> String {
        "\(number(used)) / \(Int(total)) GB"
    }

    /// The swap footer. Collapses to a bare "—" when unread rather than "— GB",
    /// which would attach a unit to a number nobody has — the same call
    /// ``vram(_:)`` makes for an absent adapter.
    static func swap(_ gb: Double?) -> String {
        guard let gb else { return "Swap: \(unknown)" }
        return "Swap: \(number(gb)) GB"
    }

    /// The Graphics header value. A host with no adapter is unknown outright —
    /// asking `isPresent` rather than re-deriving it keeps this in step with the
    /// producer — and a present-but-unreadable usage blanks only itself.
    static func gpuUsage(_ gpu: GPUMetrics) -> String {
        gpu.isPresent ? percent(gpu.usage) : unknown
    }

    /// The VRAM footer. Collapses to a single "—" for an absent adapter rather
    /// than the "— / — GB" that would imply two missing readings.
    static func vram(_ gpu: GPUMetrics) -> String {
        guard gpu.isPresent else { return "VRAM: \(unknown)" }
        return "VRAM: \(number(gpu.vramUsedGB)) / \(number(gpu.vramTotalGB)) GB"
    }
}
