import Foundation
import HostMetricsKit

/// Streams live metrics for "this machine" from `HostMetricsKit` (in-process, no
/// agent). Remote hosts use `RemoteHostMetricsService`; both share the history
/// buffers + `ingest` in `HostMetricsService`.
@MainActor
final class LocalHostMetricsService: HostMetricsService {
    /// UserDefaults key for this Mac's hidden volume mounts. Remote hosts persist
    /// theirs on `MonitoredHost` instead — the local host has no SwiftData row.
    static let hiddenMountsDefaultsKey = "localHiddenVolumeMounts"

    private let collector = HostMetricsCollector()

    init(hostName: String? = nil, defaults: UserDefaults = .standard) {
        let name = hostName
            ?? ProcessInfo.processInfo.hostName.replacingOccurrences(of: ".local", with: "")
        super.init(hostName: name, connectionState: .local)
        setHiddenMounts(Set(defaults.stringArray(forKey: Self.hiddenMountsDefaultsKey) ?? []))
        persistHiddenMounts = { mounts in
            defaults.set(mounts.sorted(), forKey: Self.hiddenMountsDefaultsKey)
        }
        installLifecyclePause()
    }

    /// Begins streaming snapshots at the given cadence.
    override func start(interval: TimeInterval = 1.0) {
        startInterval = interval
        guard task == nil else { return }
        task = Task { [weak self] in
            guard let self else { return }
            for await snap in collector.snapshots(interval: interval) {
                ingest(snap)
            }
        }
    }
}
