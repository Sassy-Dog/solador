import AppKit
import Foundation
import HostMetricsKit

/// Connection state for a host's metrics feed.
enum HostConnectionState: Equatable {
    case local // this machine, in-process
    case connecting // remote, awaiting first sample
    case connected // remote, receiving
    case failed(RemoteHostError) // remote, last poll failed (with cause)

    /// True for any failed state, regardless of cause. Lets call sites that only
    /// care "is this host down?" stay terse.
    var isFailed: Bool {
        if case .failed = self { return true }
        return false
    }

    /// Short, human-facing label for a failed state ("auth failed", "decode
    /// failed", "unreachable", …); empty for non-failed states.
    var failureLabel: String {
        if case let .failed(error) = self { return error.shortLabel }
        return ""
    }
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

    /// Debounced volume list — see `VolumeStabilizer`. Transient mounts that flap
    /// between samples never land here, so the Volumes section can't shift.
    @Published private(set) var stableVolumes: [VolumeUsage] = []

    /// Mount paths the user chose to hide on this host. Mutate via
    /// `hideVolume`/`unhideVolume` so the change is persisted.
    @Published private(set) var hiddenMounts: Set<String> = []

    /// Writes the hide list to this host's backing store (SwiftData for remote
    /// hosts, UserDefaults for the local one). Set by whoever builds the service.
    var persistHiddenMounts: ((Set<String>) -> Void)?

    /// Display name of this host (e.g. "mac-w26h", "ubu-01").
    let hostName: String

    private var volumeStabilizer = VolumeStabilizer()

    /// The last sample's unmeasured-field list, so the log line fires on the
    /// transition rather than on every poll. Readable for tests.
    private(set) var lastUnmeasuredFields: [String] = []

    /// The volumes the cockpit renders: debounced and minus the hide list.
    /// `HostsPanel.volumeSlots` and `HostMetricsPanel` must BOTH read this so slot
    /// reservation and rendering can never disagree.
    var visibleVolumes: [VolumeUsage] {
        stableVolumes.filter { !hiddenMounts.contains($0.mount) }
    }

    func hideVolume(_ mount: String) {
        hiddenMounts.insert(mount)
        persistHiddenMounts?(hiddenMounts)
    }

    func unhideVolume(_ mount: String) {
        hiddenMounts.remove(mount)
        persistHiddenMounts?(hiddenMounts)
    }

    /// Replaces the hide list without persisting — for loading the stored value.
    func setHiddenMounts(_ mounts: Set<String>) {
        hiddenMounts = mounts
    }

    /// Streaming task, managed by subclasses.
    var task: Task<Void, Never>?

    /// Whether collection is currently running.
    var isCollecting: Bool {
        task != nil
    }

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
    func start(interval _: TimeInterval = 1.0) {}

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
    ///
    /// A value the host could not measure is never pushed into a series: a chart
    /// is a claim about measurements, and a fabricated zero in the buffer draws a
    /// flat line that looks exactly like an idle disk. The buffers therefore hold
    /// only real readings and stay plottable on the sample where the latest is
    /// unknown — the panel's "—" is what says the newest one is missing.
    func ingest(_ snap: HostSnapshot) {
        snapshot = snap
        stableVolumes = volumeStabilizer.observe(snap.volumes)
        logUnmeasuredIfChanged(snap)
        append(&cpuHistory, snap.cpu.totalUsage)
        append(&memoryHistory, snap.memory.usagePercentage)
        // A pre-#183 agent reports an absent GPU as a literal `usage: 0.0` on
        // every sample, so gating on the reading alone would still fill the
        // buffer. `isPresent` is the only thing that can see through that.
        append(&gpuHistory, snap.gpu.isPresent ? snap.gpu.usage : nil)
        append(&diskReadHistory, snap.disk.readMBps)
        append(&diskWriteHistory, snap.disk.writeMBps)
        append(&netDownHistory, snap.network.downloadMBps)
        append(&netUpHistory, snap.network.uploadMBps)
        ingestCores(snap.cpu.coreUsages)
    }

    /// Logs which figures this host stopped (or started) reporting, so every "—"
    /// the card paints is diagnosable rather than mysterious. Logged only when
    /// the set changes — a Linux host omits the same fields on every poll, and
    /// one line per second would bury the transition that matters.
    private func logUnmeasuredIfChanged(_ snap: HostSnapshot) {
        let unmeasured = snap.unmeasuredFields
        guard unmeasured != lastUnmeasuredFields else { return }
        lastUnmeasuredFields = unmeasured
        if unmeasured.isEmpty {
            serviceLogger.info("Host \(hostName) now reports every metric")
        } else {
            serviceLogger.info(
                "Host \(hostName) reports no measurement for \(unmeasured.joined(separator: ", ")) — those cells render \u{2014}"
            )
        }
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

    /// Appends one sample, or nothing at all when the host didn't measure it.
    private func append(_ buffer: inout [Double], _ value: Double?) {
        guard let value else { return }
        buffer.append(value)
        if buffer.count > Self.historyCapacity {
            buffer.removeFirst(buffer.count - Self.historyCapacity)
        }
    }

    deinit {
        task?.cancel()
        for (center, token) in lifecycleObservers {
            center.removeObserver(token)
        }
    }
}
