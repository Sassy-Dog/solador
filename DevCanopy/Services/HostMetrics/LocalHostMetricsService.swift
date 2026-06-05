import Foundation
import HostMetricsKit

/// Streams live metrics for "this machine" from `HostMetricsKit` (in-process, no
/// agent). Remote hosts use `RemoteHostMetricsService`; both share the history
/// buffers + `ingest` in `HostMetricsService`.
@MainActor
final class LocalHostMetricsService: HostMetricsService {
    private let collector = HostMetricsCollector()

    init(hostName: String? = nil) {
        let name = hostName
            ?? ProcessInfo.processInfo.hostName.replacingOccurrences(of: ".local", with: "")
        super.init(hostName: name, connectionState: .local)
        installLifecyclePause()
    }

    /// Begins streaming snapshots at the given cadence.
    override func start(interval: TimeInterval = 1.0) {
        startInterval = interval
        guard task == nil else { return }
        task = Task { [weak self] in
            guard let self else { return }
            for await snap in collector.snapshots(interval: interval) {
                self.ingest(snap)
            }
        }
    }
}
