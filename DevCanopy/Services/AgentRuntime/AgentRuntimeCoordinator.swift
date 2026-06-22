import Combine
import Foundation

/// Owns the set of monitored agent runtimes and re-publishes their snapshots as
/// one array for the cockpit panel. v1 holds exactly one runtime (OpenClaw);
/// adding hermes is a one-line append here plus a new conformer — the panel
/// renders one section per snapshot and never changes.
@MainActor
final class AgentRuntimeCoordinator: ObservableObject {
    @Published private(set) var snapshots: [AgentRuntimeSnapshot] = []

    private let runtimes: [any AgentRuntime]
    private var cancellables: Set<AnyCancellable> = []

    init(runtimes: [any AgentRuntime]) {
        self.runtimes = runtimes
        snapshots = runtimes.map(\.snapshot)

        // Re-collect the whole array whenever any runtime publishes. The set is
        // tiny (one, soon two), so a full rebuild is simpler than diffing and
        // keeps render order stable (declaration order in `runtimes`).
        for runtime in runtimes {
            runtime.snapshotPublisher
                .receive(on: RunLoop.main)
                .sink { [weak self] _ in
                    guard let self else { return }
                    snapshots = self.runtimes.map(\.snapshot)
                }
                .store(in: &cancellables)
        }
    }

    func start() {
        runtimes.forEach { $0.start() }
    }

    func stop() {
        runtimes.forEach { $0.stop() }
    }
}
