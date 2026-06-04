import Foundation
import HostMetricsKit

/// Streams live metrics for "this machine" from `HostMetricsKit` and keeps capped
/// history buffers the cockpit's sparklines render from.
///
/// "This machine" is collected in-process (no agent). Remote hosts (Phase 2) will
/// feed the same `HostSnapshot` shape from the per-host agent.
@MainActor
final class LocalHostMetricsService: ObservableObject {
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

    /// Display name of this machine (e.g. "mac-w26h").
    let hostName: String

    private let collector = HostMetricsCollector()
    private var task: Task<Void, Never>?

    init(hostName: String? = nil) {
        self.hostName = hostName
            ?? ProcessInfo.processInfo.hostName.replacingOccurrences(of: ".local", with: "")
    }

    /// Begins streaming snapshots at the given cadence.
    func start(interval: TimeInterval = 1.0) {
        guard task == nil else { return }
        task = Task { [weak self] in
            guard let self else { return }
            for await snap in collector.snapshots(interval: interval) {
                self.ingest(snap)
            }
        }
    }

    func stop() {
        task?.cancel()
        task = nil
    }

    /// Records a snapshot and updates the capped history buffers.
    /// Internal so tests can drive it with synthetic snapshots.
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

    private func ingestCores(_ cores: [Double]) {
        // Reset if the core count changes (shouldn't on a stable host).
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
