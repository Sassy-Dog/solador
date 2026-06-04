import Foundation
import HostMetricsKit

/// Connection state for a host's metrics feed.
enum HostConnectionState: Equatable {
    case local            // this machine, in-process
    case connecting       // remote, awaiting first sample
    case connected        // remote, receiving
    case unreachable      // remote, can't reach the agent
}

/// Base class for a host's live metrics + capped history buffers the cockpit's
/// sparklines render from. Concrete sources subclass this and feed it via
/// `ingest(_:)`: `LocalHostMetricsService` (in-process collector) and
/// `RemoteHostMetricsService` (polls a remote agent). Using a base ObservableObject
/// class (not a protocol) keeps `@ObservedObject` in the views simple.
@MainActor
class HostMetricsService: ObservableObject {
    /// Number of recent samples retained for sparklines (~2 min at 1s cadence).
    static let historyCapacity = 120

    @Published private(set) var snapshot: HostSnapshot?
    @Published private(set) var cpuHistory: [Double] = []
    @Published private(set) var coreHistories: [[Double]] = []
    @Published private(set) var memoryHistory: [Double] = []
    @Published private(set) var gpuHistory: [Double] = []
    @Published private(set) var diskReadHistory: [Double] = []
    @Published private(set) var diskWriteHistory: [Double] = []
    @Published private(set) var netDownHistory: [Double] = []
    @Published private(set) var netUpHistory: [Double] = []
    @Published private(set) var connectionState: HostConnectionState

    /// Display name of this host (e.g. "mac-w26h", "ubu-3xdv").
    let hostName: String

    /// Streaming task, managed by subclasses.
    var task: Task<Void, Never>?

    init(hostName: String, connectionState: HostConnectionState) {
        self.hostName = hostName
        self.connectionState = connectionState
    }

    /// Begins streaming. Overridden by subclasses.
    func start(interval: TimeInterval = 1.0) {}

    func stop() {
        task?.cancel()
        task = nil
    }

    /// Records a snapshot and updates the capped history buffers.
    /// Internal so tests and subclasses can drive it.
    func ingest(_ snap: HostSnapshot) {
        snapshot = snap
        append(&cpuHistory, snap.cpu.totalUsage)
        append(&memoryHistory, snap.memory.usagePercentage)
        append(&gpuHistory, snap.gpu.usage)
        append(&diskReadHistory, snap.disk.readMBps)
        append(&diskWriteHistory, snap.disk.writeMBps)
        append(&netDownHistory, snap.network.downloadMBps)
        append(&netUpHistory, snap.network.uploadMBps)
        ingestCores(snap.cpu.coreUsages)
    }

    func setConnection(_ state: HostConnectionState) {
        connectionState = state
    }

    private func ingestCores(_ cores: [Double]) {
        if coreHistories.count != cores.count {
            coreHistories = Array(repeating: [], count: cores.count)
        }
        for (index, value) in cores.enumerated() {
            append(&coreHistories[index], value)
        }
    }

    private func append(_ buffer: inout [Double], _ value: Double) {
        buffer.append(value)
        if buffer.count > Self.historyCapacity {
            buffer.removeFirst(buffer.count - Self.historyCapacity)
        }
    }

    deinit { task?.cancel() }
}
