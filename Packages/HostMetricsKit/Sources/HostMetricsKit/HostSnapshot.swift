import Foundation

/// A point-in-time snapshot of host machine metrics.
public struct HostSnapshot: Sendable, Codable, Equatable {
    public let timestamp: Date
    public let cpu: CPUMetrics
    public let memory: MemoryMetrics
    public let disk: DiskMetrics
    public let network: NetworkMetrics
    public let gpu: GPUMetrics
    /// `nil` when no battery is present (e.g. desktop Macs).
    public let battery: BatteryMetrics?
    /// Per-mounted-volume usage. Empty when unavailable or from an older agent
    /// that predates this field (decoded tolerantly — see `init(from:)`).
    public let volumes: [VolumeUsage]
    /// Top CPU/RAM-consuming processes, sampled on a slow (~1 min) cadence — the
    /// union of the top-by-CPU and top-by-memory, so the client can render both
    /// lists. Empty from an older agent (decoded tolerantly).
    public let processes: [ProcessSample]

    public init(
        timestamp: Date,
        cpu: CPUMetrics,
        memory: MemoryMetrics,
        disk: DiskMetrics,
        network: NetworkMetrics,
        gpu: GPUMetrics,
        battery: BatteryMetrics?,
        volumes: [VolumeUsage] = [],
        processes: [ProcessSample] = []
    ) {
        self.timestamp = timestamp
        self.cpu = cpu
        self.memory = memory
        self.disk = disk
        self.network = network
        self.gpu = gpu
        self.battery = battery
        self.volumes = volumes
        self.processes = processes
    }

    private enum CodingKeys: String, CodingKey {
        case timestamp, cpu, memory, disk, network, gpu, battery, volumes, processes
    }

    // Custom decode so a payload without `volumes`/`processes` (older agents)
    // still decodes, yielding empty lists rather than failing the whole snapshot.
    // Encoding is synthesized and always includes both.
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        timestamp = try c.decode(Date.self, forKey: .timestamp)
        cpu = try c.decode(CPUMetrics.self, forKey: .cpu)
        memory = try c.decode(MemoryMetrics.self, forKey: .memory)
        disk = try c.decode(DiskMetrics.self, forKey: .disk)
        network = try c.decode(NetworkMetrics.self, forKey: .network)
        gpu = try c.decode(GPUMetrics.self, forKey: .gpu)
        // Tolerant battery decode: a malformed or unexpected battery object must
        // not sink the whole snapshot (which would flip an otherwise-healthy host
        // unreachable). `null`/absent → nil; a present-but-undecodable object →
        // nil rather than a thrown error. Only `level`+`isCharging` are required.
        if let raw = try? c.decodeIfPresent(BatteryMetrics.self, forKey: .battery) {
            battery = raw
        } else {
            battery = nil
        }
        volumes = try c.decodeIfPresent([VolumeUsage].self, forKey: .volumes) ?? []
        processes = try c.decodeIfPresent([ProcessSample].self, forKey: .processes) ?? []
    }
}

/// One process's resource use. `cpuPercent` can exceed 100 on multi-core hosts.
public struct ProcessSample: Sendable, Codable, Equatable, Identifiable {
    public let pid: Int
    public let name: String
    public let cpuPercent: Double
    public let memoryMB: Double

    public var id: Int { pid }

    public init(pid: Int, name: String, cpuPercent: Double, memoryMB: Double) {
        self.pid = pid
        self.name = name
        self.cpuPercent = cpuPercent
        self.memoryMB = memoryMB
    }
}

/// Reduces a full process list to the interesting few: the union of the top-N by
/// CPU and top-N by memory (deduped by pid), so one list backs both a "Top CPU"
/// and a "Top RAM" view. Pure and order-stable for testing.
public enum ProcessRanking {
    public static func top(_ all: [ProcessSample], limit: Int = 5) -> [ProcessSample] {
        let byCPU = all.sorted { $0.cpuPercent > $1.cpuPercent }.prefix(limit)
        let byMem = all.sorted { $0.memoryMB > $1.memoryMB }.prefix(limit)
        var seen = Set<Int>()
        var out: [ProcessSample] = []
        for p in byCPU where seen.insert(p.pid).inserted { out.append(p) }
        for p in byMem where seen.insert(p.pid).inserted { out.append(p) }
        return out.sorted { $0.cpuPercent > $1.cpuPercent }
    }
}

/// Usage of one mounted volume. `percentUsed` is computed client-side (never on
/// the wire) — the agent emits `mount`, `usedGB`, `totalGB`, and (since agent
/// 0.2.0) `fstype`, which is nil from older agents and the local collector.
public struct VolumeUsage: Sendable, Codable, Equatable, Identifiable {
    public let mount: String
    public let usedGB: Double
    public let totalGB: Double
    public let fstype: String?

    public var id: String { mount }
    public var percentUsed: Double { totalGB > 0 ? usedGB / totalGB * 100 : 0 }

    public init(mount: String, usedGB: Double, totalGB: Double, fstype: String? = nil) {
        self.mount = mount
        self.usedGB = usedGB
        self.totalGB = totalGB
        self.fstype = fstype
    }
}

public struct CPUMetrics: Sendable, Codable, Equatable {
    /// Overall CPU usage, 0–100.
    public let totalUsage: Double
    /// Per-core CPU usage, each 0–100.
    public let coreUsages: [Double]
    public let model: String
    public let thermalState: ThermalState

    public init(
        totalUsage: Double,
        coreUsages: [Double],
        model: String,
        thermalState: ThermalState
    ) {
        self.totalUsage = totalUsage
        self.coreUsages = coreUsages
        self.model = model
        self.thermalState = thermalState
    }
}

public enum ThermalState: Int, Sendable, Codable {
    case nominal
    case fair
    case serious
    case critical
}

public struct MemoryMetrics: Sendable, Codable, Equatable {
    public let usedGB: Double
    public let totalGB: Double
    public let swapUsedGB: Double
    /// Memory pressure, 0–100.
    public let pressure: Double

    public var usagePercentage: Double { totalGB > 0 ? usedGB / totalGB * 100 : 0 }

    public init(
        usedGB: Double,
        totalGB: Double,
        swapUsedGB: Double,
        pressure: Double
    ) {
        self.usedGB = usedGB
        self.totalGB = totalGB
        self.swapUsedGB = swapUsedGB
        self.pressure = pressure
    }
}

public struct DiskMetrics: Sendable, Codable, Equatable {
    public let readMBps: Double
    public let writeMBps: Double

    public init(readMBps: Double, writeMBps: Double) {
        self.readMBps = readMBps
        self.writeMBps = writeMBps
    }
}

public struct NetworkMetrics: Sendable, Codable, Equatable {
    public let downloadMBps: Double
    public let uploadMBps: Double

    public init(downloadMBps: Double, uploadMBps: Double) {
        self.downloadMBps = downloadMBps
        self.uploadMBps = uploadMBps
    }
}

public struct GPUMetrics: Sendable, Codable, Equatable {
    /// GPU usage, 0–100.
    public let usage: Double
    public let vramUsedGB: Double
    public let vramTotalGB: Double

    public init(usage: Double, vramUsedGB: Double, vramTotalGB: Double) {
        self.usage = usage
        self.vramUsedGB = vramUsedGB
        self.vramTotalGB = vramTotalGB
    }
}

/// Battery state. The wire contract (locked by the shared fixture
/// `TestFixtures/battery_contract.json`) is the minimal cross-platform shape the
/// Rust agent emits: `level` (0–100) and `isCharging` — the only two fields a
/// generic host agent can produce. Everything else is macOS-local enrichment from
/// the IOKit collector and is decode-optional, so an agent payload carrying only
/// the two contract keys still decodes.
///
/// Decoding is tolerant: only `level` and `isCharging` are required. A battery
/// object missing either (or with an incompatible type) throws here; the caller
/// (`HostSnapshot.init(from:)`) catches that and degrades `battery` to `nil`
/// rather than failing the entire snapshot.
public struct BatteryMetrics: Sendable, Codable, Equatable {
    /// Charge level, 0–100. Canonical wire key; the agent's `level`.
    public let level: Double
    public let isCharging: Bool

    // macOS-local IOKit enrichment — absent from the agent wire contract.
    public let hasBattery: Bool?
    public let cycleCount: Int?
    public let health: Double?
    public let powerSource: String?
    public let timeRemaining: String?
    public let wattage: Double?

    public init(
        level: Double,
        isCharging: Bool,
        hasBattery: Bool? = nil,
        cycleCount: Int? = nil,
        health: Double? = nil,
        powerSource: String? = nil,
        timeRemaining: String? = nil,
        wattage: Double? = nil
    ) {
        self.level = level
        self.isCharging = isCharging
        self.hasBattery = hasBattery
        self.cycleCount = cycleCount
        self.health = health
        self.powerSource = powerSource
        self.timeRemaining = timeRemaining
        self.wattage = wattage
    }

    private enum CodingKeys: String, CodingKey {
        case level, isCharging, hasBattery, cycleCount, health
        case powerSource, timeRemaining, wattage
    }

    // Custom decode: the two contract keys are required; the macOS-rich keys are
    // optional, so an agent payload with only `level`/`isCharging` decodes. An
    // unexpected/incompatible battery shape throws (caught upstream → nil).
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        level = try c.decode(Double.self, forKey: .level)
        isCharging = try c.decode(Bool.self, forKey: .isCharging)
        hasBattery = try c.decodeIfPresent(Bool.self, forKey: .hasBattery)
        cycleCount = try c.decodeIfPresent(Int.self, forKey: .cycleCount)
        health = try c.decodeIfPresent(Double.self, forKey: .health)
        powerSource = try c.decodeIfPresent(String.self, forKey: .powerSource)
        timeRemaining = try c.decodeIfPresent(String.self, forKey: .timeRemaining)
        wattage = try c.decodeIfPresent(Double.self, forKey: .wattage)
    }

    // Encode omits nil enrichment so the round-trip of an agent-shaped value
    // (only `level`/`isCharging`) re-emits exactly the contract floor.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(level, forKey: .level)
        try c.encode(isCharging, forKey: .isCharging)
        try c.encodeIfPresent(hasBattery, forKey: .hasBattery)
        try c.encodeIfPresent(cycleCount, forKey: .cycleCount)
        try c.encodeIfPresent(health, forKey: .health)
        try c.encodeIfPresent(powerSource, forKey: .powerSource)
        try c.encodeIfPresent(timeRemaining, forKey: .timeRemaining)
        try c.encodeIfPresent(wattage, forKey: .wattage)
    }
}
