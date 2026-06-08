import AppKit
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

    /// Whether collection is currently running.
    var isCollecting: Bool { task != nil }

    /// Interval the subclass was last started with, so a lifecycle-resume can
    /// restart at the same cadence. Subclasses set this in their `start` override.
    var startInterval: TimeInterval = 1.0

    private var lifecycleObservers: [(NotificationCenter, NSObjectProtocol)] = []
    private var wasRunningBeforePause = false

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

    // MARK: App-lifecycle pause

    /// Pauses collection across system sleep and resumes (fresh) on wake. Call once
    /// from a subclass `init`.
    ///
    /// We deliberately do NOT pause on app focus changes (`resignActive`/`becomeActive`):
    /// the cockpit is built to be a glanceable, persistent display on a second monitor,
    /// so it must keep updating while it's visible but not the frontmost app (e.g. in
    /// fullscreen while you work elsewhere). The "fast replay burst" that a paused-then-
    /// resumed collector used to cause is already prevented at the source by the
    /// collector's `.bufferingNewest(1)` policy (a lagging consumer only ever sees the
    /// newest sample), so a focus-based pause buys nothing and only freezes the dashboard.
    func installLifecyclePause() {
        let ws = NSWorkspace.shared.notificationCenter
        func observe(_ name: Notification.Name, on center: NotificationCenter, _ handler: @escaping () -> Void) {
            let token = center.addObserver(forName: name, object: nil, queue: .main) { _ in
                MainActor.assumeIsolated { handler() }
            }
            lifecycleObservers.append((center, token))
        }
        observe(NSWorkspace.willSleepNotification, on: ws) { [weak self] in self?.pauseForLifecycle() }
        observe(NSWorkspace.didWakeNotification, on: ws) { [weak self] in self?.resumeForLifecycle() }
    }

    /// Stops collection but remembers it was running, so `resumeForLifecycle`
    /// restarts it. No-op if not currently collecting. Internal for testing.
    func pauseForLifecycle() {
        guard isCollecting else { return }
        wasRunningBeforePause = true
        stop()
    }

    /// Restarts collection (fresh — no pending samples) if it was paused by the
    /// lifecycle. Internal for testing.
    func resumeForLifecycle() {
        guard wasRunningBeforePause else { return }
        wasRunningBeforePause = false
        start(interval: startInterval)
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

    deinit {
        task?.cancel()
        for (center, token) in lifecycleObservers { center.removeObserver(token) }
    }
}
