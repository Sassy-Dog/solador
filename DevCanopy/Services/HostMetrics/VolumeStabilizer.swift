import Foundation
import HostMetricsKit

/// Hysteresis filter for a host's volume list, so transient mounts (autofs/NFS
/// automounts, brief container mounts) can't make the Volumes section flap: a
/// volume must be observed for `threshold` consecutive polls before it appears,
/// and missing for `threshold` consecutive polls before it disappears. Values of
/// visible volumes always reflect the latest observation. The first observation
/// seeds the visible set directly so the UI isn't empty right after launch.
/// Pure state machine — no timers, no I/O — driven by `observe(_:)`.
struct VolumeStabilizer {
    static let defaultThreshold = 5

    private let threshold: Int
    private var seeded = false
    /// Consecutive polls a not-yet-visible mount has been present.
    private var presentStreaks: [String: Int] = [:]
    /// Consecutive polls a visible mount has been missing.
    private var absentStreaks: [String: Int] = [:]
    /// Latest data for each visible mount.
    private var visible: [String: VolumeUsage] = [:]

    init(threshold: Int = VolumeStabilizer.defaultThreshold) {
        self.threshold = max(1, threshold)
    }

    /// Feed one poll's observed volumes; returns the stable list, sorted by mount.
    mutating func observe(_ volumes: [VolumeUsage]) -> [VolumeUsage] {
        let observed = Dictionary(volumes.map { ($0.mount, $0) }, uniquingKeysWith: { first, _ in first })

        guard seeded else {
            seeded = true
            visible = observed
            return stableList
        }

        for (mount, volume) in observed {
            if visible[mount] != nil {
                visible[mount] = volume
                absentStreaks[mount] = nil
            } else {
                let streak = (presentStreaks[mount] ?? 0) + 1
                if streak >= threshold {
                    visible[mount] = volume
                    presentStreaks[mount] = nil
                } else {
                    presentStreaks[mount] = streak
                }
            }
        }

        // "Consecutive" means consecutive: a pending mount that skips a poll
        // starts over, and a visible mount that misses `threshold` polls is gone.
        presentStreaks = presentStreaks.filter { observed[$0.key] != nil }
        for mount in visible.keys where observed[mount] == nil {
            let streak = (absentStreaks[mount] ?? 0) + 1
            if streak >= threshold {
                visible[mount] = nil
                absentStreaks[mount] = nil
            } else {
                absentStreaks[mount] = streak
            }
        }

        return stableList
    }

    private var stableList: [VolumeUsage] {
        visible.values.sorted { $0.mount < $1.mount }
    }
}
