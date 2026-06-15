import Foundation
import HostMetricsKit

/// A `HostMetricsService` driven by `DemoSnapshotGenerator` instead of a live
/// collector or agent. Used only in demo mode (see `DemoMode`). Pre-seeds the
/// history buffers with a warmup so sparklines render populated immediately, then
/// streams fresh synthetic snapshots on a timer — no IOKit, no network.
@MainActor
final class DemoHostMetricsService: HostMetricsService {
    private let generator: DemoSnapshotGenerator
    private var phase: Double

    /// - Parameters:
    ///   - hostName: display name for the card.
    ///   - profile: the synthetic hardware/workload to simulate.
    ///   - connectionState: `.local` for "this machine", `.connected` for a
    ///     synthetic remote host (so it renders like a healthy agent).
    init(hostName: String, profile: DemoHostProfile, connectionState: HostConnectionState) {
        self.generator = DemoSnapshotGenerator(profile: profile)
        let warmup = generator.warmup(samples: HostMetricsService.historyCapacity)
        self.phase = warmup.nextPhase
        super.init(hostName: hostName, connectionState: connectionState)
        for snap in warmup.snapshots { ingest(snap) }
        installLifecyclePause()
    }

    override func start(interval: TimeInterval = 1.0) {
        startInterval = interval
        guard task == nil else { return }
        task = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                self.phase += DemoSnapshotGenerator.step
                self.ingest(self.generator.snapshot(at: self.phase))
                try? await Task.sleep(nanoseconds: UInt64(interval * 1_000_000_000))
            }
        }
    }
}
