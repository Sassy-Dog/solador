import SwiftUI

/// A panel's refresh-health footer. Surfaces a warning when the last refresh
/// failed, or when the data has gone stale (no successful refresh within
/// `staleAfter`) — so a panel never silently presents stale data as current.
/// Renders nothing when the panel is healthy and fresh, keeping the cockpit
/// glanceable.
struct PanelStatusFooter: View {
    let lastUpdated: Date?
    let error: String?
    /// Warn "stale" if there's been no successful refresh within this window.
    var staleAfter: TimeInterval

    var body: some View {
        if let error {
            warn("⚠ \(error)\(lastOKSuffix)")
        } else if let lastUpdated, Date().timeIntervalSince(lastUpdated) > staleAfter {
            warn("⚠ stale · updated \(Self.relative(lastUpdated))")
        }
    }

    private var lastOKSuffix: String {
        guard let lastUpdated else { return "" }
        return " · last ok \(Self.relative(lastUpdated))"
    }

    private func warn(_ text: String) -> some View {
        Text(text)
            .font(CockpitTheme.mono(9))
            .foregroundStyle(CockpitTheme.amber)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    static func relative(_ date: Date) -> String {
        let s = Int(max(0, Date().timeIntervalSince(date)))
        if s < 60 { return "\(s)s ago" }
        if s < 3600 { return "\(s / 60)m ago" }
        if s < 86400 { return "\(s / 3600)h ago" }
        return "\(s / 86400)d ago"
    }
}
