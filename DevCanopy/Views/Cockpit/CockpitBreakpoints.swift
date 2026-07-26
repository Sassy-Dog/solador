import Foundation

/// Responsive layout math for the cockpit — the "breakpoints" layer.
///
/// Modelled on CSS `repeat(auto-fit, minmax(<min>, 1fr))` rather than on global
/// `sm`/`md`/`lg` tiers: **each panel declares its own `minWidth`** (see
/// `CockpitPanelKind.minWidth`) and a row breaks only when *its* panels stop
/// fitting. Named tiers can't express the case that actually matters here —
/// Claude Usage + Azure Cost still sit comfortably side-by-side at a width where
/// the much hungrier host cards must stack.
///
/// Pure value math with no SwiftUI, like `CoreGridLayout` and `VolumeGridLayout`,
/// so the reflow decisions are unit-testable without rendering anything.
enum CockpitBreakpoints {
    /// Gap between cockpit cards, matching `CockpitView`'s grid spacing.
    static let spacing: CGFloat = 16

    /// Minimum comfortable width for one host card. Below this the per-core grid
    /// loses its `Core N  xx%` labels and volume mounts truncate, so cards stack
    /// instead of squeezing. Two cards therefore need ≥ 1816pt, three ≥ 2732pt.
    static let hostCardMinWidth: CGFloat = 900

    /// Repacks layout rows so no row asks for more width than it has.
    ///
    /// Greedy and order-preserving: panels stay in their authored sequence and are
    /// never merged across authored rows — a row only ever splits into more rows.
    /// Every placement survives exactly once.
    ///
    /// `available == 0` means the width is unknown (a panel rendered outside the
    /// cockpit, e.g. a preview); the authored rows pass through untouched. Unlike
    /// `hostColumns`, "as authored" is the safe answer here — panel minimums are
    /// small, so an authored pair is never the unreadable option.
    static func reflow(
        _ rows: [[CockpitPlacement]],
        available: CGFloat,
        spacing: CGFloat = spacing
    ) -> [[CockpitPlacement]] {
        guard available > 0 else { return rows }
        return rows.flatMap { pack($0, available: available, spacing: spacing) }
    }

    /// One authored row → one or more rendered rows.
    private static func pack(
        _ row: [CockpitPlacement],
        available: CGFloat,
        spacing: CGFloat
    ) -> [[CockpitPlacement]] {
        var packed: [[CockpitPlacement]] = []
        var current: [CockpitPlacement] = []
        var currentWidth: CGFloat = 0

        for placement in row {
            let needed = placement.kind.minWidth
            // A lone panel always stays on its row even if it's wider than the
            // window — better an overflowing card than an empty layout.
            if current.isEmpty {
                current = [placement]
                currentWidth = needed
            } else if currentWidth + spacing + needed <= available {
                current.append(placement)
                currentWidth += spacing + needed
            } else {
                packed.append(current)
                current = [placement]
                currentWidth = needed
            }
        }
        if !current.isEmpty { packed.append(current) }
        return packed
    }

    /// How many host cards fit across `available` points, capped at `hostCount`.
    ///
    /// The `auto-fit` formula: each card occupies a slot of `minCardWidth + spacing`,
    /// and the trailing card needs no gap after it — hence the `+ spacing` on the
    /// numerator. Returns 1 when nothing fits side-by-side, which is the caller's
    /// cue to stack (or tab).
    ///
    /// **Unknown width (`available == 0`) stacks.** An earlier version assumed wide
    /// here, which quietly turned a dead width reading into a cockpit of crushed
    /// cards that looked deliberate. Stacking can never be unreadable, and it's
    /// visibly wrong on a big display, so a broken measurement gets reported instead
    /// of tolerated.
    static func hostColumns(
        available: CGFloat,
        hostCount: Int,
        minCardWidth: CGFloat = hostCardMinWidth,
        spacing: CGFloat = spacing
    ) -> Int {
        guard hostCount > 0 else { return 1 }
        guard available > 0 else { return 1 }

        let slot = minCardWidth + spacing
        guard slot > 0 else { return 1 }

        let fits = Int((available + spacing) / slot)
        return min(max(fits, 1), hostCount)
    }

    /// Splits items into rows of at most `columns`. `columns <= 1` gives one item
    /// per row — the stacked case.
    static func rows<T>(_ items: [T], columns: Int) -> [[T]] {
        guard columns > 1 else { return items.map { [$0] } }
        return stride(from: 0, to: items.count, by: columns).map { start in
            Array(items[start ..< min(start + columns, items.count)])
        }
    }
}

/// What the Hosts panel does when its cards can't sit side-by-side.
///
/// Stacking is the default: an always-on cockpit shouldn't hide a host behind a
/// click. Tabs stay available for anyone running the window short as well as narrow.
enum HostOverflowMode: String, CaseIterable, Identifiable {
    case stack
    case tabs

    var id: String {
        rawValue
    }

    var displayName: String {
        switch self {
        case .stack: "Stack vertically"
        case .tabs: "Show as tabs"
        }
    }
}
