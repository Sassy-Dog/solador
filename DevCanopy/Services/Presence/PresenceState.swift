import Foundation

/// Presence of an entity the user expects to exist (an ephemeral runner VM, a
/// registered GitHub runner): present in the source's current data, briefly
/// absent (a normal recycle window, amber), or absent beyond grace (alarm, red).
enum PresenceState: Equatable {
    case present
    case recycling(absence: TimeInterval)
    case missing(absence: TimeInterval)
}

/// Pure presence evaluation. No I/O, no wall clock — callers pass `now` as the
/// owning source's last SUCCESSFUL poll time, never render time, so absence
/// clocks freeze while a source is failing instead of aging toward a false alarm.
enum Presence {
    /// Absence grace before "recycling" escalates to "missing". The mac runner
    /// slots recycle in 1–4 minutes, so 5 minutes of absence means wedged.
    static let defaultGrace: TimeInterval = 300

    static func state(
        isPresent: Bool,
        lastSeen: Date,
        now: Date,
        grace: TimeInterval = defaultGrace
    ) -> PresenceState {
        guard !isPresent else { return .present }
        let absence = max(0, now.timeIntervalSince(lastSeen))
        return absence < grace ? .recycling(absence: absence) : .missing(absence: absence)
    }

    /// "recycling 40s" / "missing 12m" — nil for present.
    static func label(_ state: PresenceState) -> String? {
        switch state {
        case .present: nil
        case let .recycling(absence): "recycling \(duration(absence))"
        case let .missing(absence): "missing \(duration(absence))"
        }
    }

    /// Same unit ladder as `PanelStatusFooter.relative`, without the "ago".
    private static func duration(_ interval: TimeInterval) -> String {
        let s = Int(interval)
        if s < 60 {
            return "\(s)s"
        }
        if s < 3600 {
            return "\(s / 60)m"
        }
        if s < 86400 {
            return "\(s / 3600)h"
        }
        return "\(s / 86400)d"
    }
}
