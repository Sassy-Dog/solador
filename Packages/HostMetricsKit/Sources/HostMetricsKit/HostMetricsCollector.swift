import Foundation

/// Collects host machine metrics by driving the vendored Lupita monitoring engine.
///
/// The underlying `SystemMonitorV2` holds delta state for disk/network counters
/// across calls, so a single instance is kept actor-isolated and reused between
/// samples. The non-Sendable engine instances never escape the actor; the only
/// thing the stream's producer Task captures is `self` (the actor, which is
/// `Sendable`).
public actor HostMetricsCollector {
    private let monitor = SystemMonitorV2()

    public init() {}

    /// Collects a single point-in-time snapshot of host metrics.
    ///
    /// When `includeBattery` is `true`, battery info is queried; on machines
    /// without a battery (e.g. desktops) `battery` is `nil`. When `includeBattery`
    /// is `false`, `battery` is always `nil`.
    public func collectSnapshot(includeBattery: Bool = true) -> HostSnapshot {
        let cpuData = monitor.getCPUData()
        let memoryData = monitor.getMemoryData()
        let diskData = monitor.getDiskData()
        let networkData = monitor.getNetworkData()
        let gpuData = monitor.getGPUData()

        // CPUData.thermalState is not populated by getCPUData(); read it here.
        let thermalState = Self.mapThermalState(ProcessInfo.processInfo.thermalState)

        let cpu = CPUMetrics(
            totalUsage: cpuData.totalUsage,
            coreUsages: cpuData.coreUsages,
            model: cpuData.model,
            thermalState: thermalState
        )

        let memory = MemoryMetrics(
            usedGB: memoryData.usedMemory,
            totalGB: memoryData.totalMemory,
            swapUsedGB: memoryData.swapUsed,
            pressure: memoryData.pressure
        )

        let disk = DiskMetrics(
            readMBps: diskData.readSpeed,
            writeMBps: diskData.writeSpeed
        )

        let network = NetworkMetrics(
            downloadMBps: networkData.downloadSpeed,
            uploadMBps: networkData.uploadSpeed
        )

        let gpu = GPUMetrics(
            usage: gpuData.usage,
            vramUsedGB: gpuData.memoryUsed,
            vramTotalGB: gpuData.memoryTotal
        )

        var battery: BatteryMetrics?
        if includeBattery {
            if let info = BatteryMonitor().getBatteryInfo() {
                battery = BatteryMetrics(
                    hasBattery: info.hasBattery,
                    currentCapacity: info.currentCapacity,
                    isCharging: info.isCharging,
                    cycleCount: info.cycleCount,
                    health: info.health,
                    powerSource: info.powerSource,
                    timeRemaining: info.timeRemaining,
                    wattage: info.wattage
                )
            } else {
                // No power sources reported — treat as no battery present.
                battery = nil
            }
        }

        return HostSnapshot(
            timestamp: Date(),
            cpu: cpu,
            memory: memory,
            disk: disk,
            network: network,
            gpu: gpu,
            battery: battery
        )
    }

    /// Streams snapshots at the given interval until the consumer breaks or the
    /// task is cancelled.
    ///
    /// Uses `.bufferingNewest(1)`: if the consumer falls behind (e.g. the app is
    /// occluded and the main actor isn't draining the stream), intermediate
    /// snapshots are dropped rather than queued. This prevents a backlog from
    /// replaying as a fast-forward burst when the app becomes visible again.
    public nonisolated func snapshots(
        interval: TimeInterval = 1.0,
        includeBattery: Bool = true
    ) -> AsyncStream<HostSnapshot> {
        AsyncStream(bufferingPolicy: .bufferingNewest(1)) { continuation in
            let task = Task {
                while !Task.isCancelled {
                    let snapshot = await collectSnapshot(includeBattery: includeBattery)
                    continuation.yield(snapshot)
                    try? await Task.sleep(nanoseconds: UInt64(interval * 1_000_000_000))
                }
                continuation.finish()
            }
            continuation.onTermination = { _ in
                task.cancel()
            }
        }
    }

    private static func mapThermalState(_ state: ProcessInfo.ThermalState) -> ThermalState {
        switch state {
        case .nominal: return .nominal
        case .fair: return .fair
        case .serious: return .serious
        case .critical: return .critical
        @unknown default: return .nominal
        }
    }
}
