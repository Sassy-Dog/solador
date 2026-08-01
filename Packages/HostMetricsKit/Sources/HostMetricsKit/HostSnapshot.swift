import Foundation

/// A point-in-time snapshot of host machine metrics.
///
/// # Unknown is representable
///
/// Every field a producer may be unable to measure — ``MemoryMetrics/pressure``,
/// ``CPUMetrics/thermalState``, the ``GPUMetrics`` fields and the
/// ``DiskMetrics``/``NetworkMetrics`` rates — is an Optional, mirroring the
/// `Option` the Rust wire contract carries for it (`crates/wire/src/lib.rs`).
/// An absent key decodes to `nil` rather than failing, and `nil` re-encodes by
/// *omitting* the key rather than emitting `null` — `VolumeUsage.fstype`'s
/// existing tolerance, applied to every measurable figure. `0` therefore means
/// measured zero (an idle disk, a cool CPU) and `nil` means nobody measured it;
/// `HostMetricLabels` renders the two differently — a number and an em dash.
///
/// **The limit:** this only stops the *decoder* fabricating. Agents predating
/// #183 hardcode `pressure: 0.0` and `thermalState: 0` for figures Linux never
/// measures, and those literals decode here as `0` — indistinguishable from a
/// real reading, because on the wire they *are* one. That fabrication can only
/// die at the source (#183's agent rollout); nothing on this side can tell.
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

    /// The measurable figures this snapshot carries no reading for, as dotted
    /// paths in a stable order; empty when everything was measured.
    ///
    /// Every entry here is a cell the cockpit paints as "—". Callers log the
    /// list (once, on change) so an em dash is diagnosable rather than
    /// mysterious — the no-fake-numbers rule's other half.
    ///
    /// The GPU collapses to a single `gpu` entry when the host reports no
    /// adapter at all (see ``GPUMetrics/isPresent``): its three fields are then
    /// one fact, not three, and that is how the card renders it.
    public var unmeasuredFields: [String] {
        var out: [String] = []
        if cpu.thermalState == nil { out.append("cpu.thermalState") }
        if memory.pressure == nil { out.append("memory.pressure") }
        if disk.readMBps == nil { out.append("disk.readMBps") }
        if disk.writeMBps == nil { out.append("disk.writeMBps") }
        if network.downloadMBps == nil { out.append("network.downloadMBps") }
        if network.uploadMBps == nil { out.append("network.uploadMBps") }
        if !gpu.isPresent {
            out.append("gpu")
        } else {
            if gpu.usage == nil { out.append("gpu.usage") }
            if gpu.vramUsedGB == nil { out.append("gpu.vramUsedGB") }
        }
        return out
    }

    private enum CodingKeys: String, CodingKey {
        case timestamp, cpu, memory, disk, network, gpu, battery, volumes, processes
    }

    /// Custom decode so a payload without `volumes`/`processes` (older agents)
    /// still decodes, yielding empty lists rather than failing the whole snapshot.
    /// Encoding is synthesized and always includes both.
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        timestamp = try c.decode(Date.self, forKey: .timestamp)
        cpu = try c.decode(CPUMetrics.self, forKey: .cpu)
        memory = try c.decode(MemoryMetrics.self, forKey: .memory)
        // An agent that measures no rates at all may omit the whole object
        // rather than send one with every key missing, and that must decode as
        // unknown rather than as version skew — the same one-sided tolerance
        // the wire contract gives `Snapshot::{disk,network,gpu}`.
        disk = try c.decodeIfPresent(DiskMetrics.self, forKey: .disk) ?? .unknown
        network = try c.decodeIfPresent(NetworkMetrics.self, forKey: .network) ?? .unknown
        gpu = try c.decodeIfPresent(GPUMetrics.self, forKey: .gpu) ?? .unknown
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

    public var id: Int {
        pid
    }

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
        for p in byCPU where seen.insert(p.pid).inserted {
            out.append(p)
        }
        for p in byMem where seen.insert(p.pid).inserted {
            out.append(p)
        }
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

    public var id: String {
        mount
    }

    public var percentUsed: Double {
        totalGB > 0 ? usedGB / totalGB * 100 : 0
    }

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
    /// Thermal pressure level. `nil` where the platform exposes no thermal
    /// source at all — Linux is that platform, which is why every pre-#183 agent
    /// sends a hardcoded `0` here and every remote card reads "Normal". A badge
    /// must never claim a state nobody measured, so `nil` renders no badge.
    public let thermalState: ThermalState?

    public init(
        totalUsage: Double,
        coreUsages: [Double],
        model: String,
        thermalState: ThermalState?
    ) {
        self.totalUsage = totalUsage
        self.coreUsages = coreUsages
        self.model = model
        self.thermalState = thermalState
    }

    private enum CodingKeys: String, CodingKey {
        case totalUsage, coreUsages, model, thermalState
    }

    /// Custom decode for `thermalState` only: absent key, `null`, an
    /// out-of-range level, or an outright wrong type all yield `nil` rather than
    /// sinking the whole snapshot (which would flip an otherwise-healthy host to
    /// "decode failed"). The wire carries a plain integer, so an unrecognised one
    /// is a producer we don't understand yet — that is unknown, not broken.
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        totalUsage = try c.decode(Double.self, forKey: .totalUsage)
        coreUsages = try c.decode([Double].self, forKey: .coreUsages)
        model = try c.decode(String.self, forKey: .model)
        // `try?` over `decodeIfPresent` nests two optionals — "the key wasn't
        // there" and "it didn't decode" are the same answer here, so flatten
        // before mapping the level.
        let raw: Int?? = try? c.decodeIfPresent(Int.self, forKey: .thermalState)
        thermalState = raw.flatMap(\.self).flatMap(ThermalState.init(rawValue:))
    }

    /// Encode omits an unmeasured thermal state rather than emitting
    /// `"thermalState": null`, so a round-trip re-emits the payload's own shape.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(totalUsage, forKey: .totalUsage)
        try c.encode(coreUsages, forKey: .coreUsages)
        try c.encode(model, forKey: .model)
        try c.encodeIfPresent(thermalState, forKey: .thermalState)
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
    /// Memory pressure, 0–100. `nil` where the platform has no such figure —
    /// Linux again. A defaulted `0` paints a permanently green pressure badge,
    /// which is the fabrication this Optional exists to stop.
    public let pressure: Double?

    public var usagePercentage: Double {
        totalGB > 0 ? usedGB / totalGB * 100 : 0
    }

    public init(
        usedGB: Double,
        totalGB: Double,
        swapUsedGB: Double,
        pressure: Double?
    ) {
        self.usedGB = usedGB
        self.totalGB = totalGB
        self.swapUsedGB = swapUsedGB
        self.pressure = pressure
    }
}

/// Disk I/O rates. Both `nil` before a producer has two cumulative readings to
/// diff — a rate is a delta, and the first sample has nothing to subtract from.
public struct DiskMetrics: Sendable, Codable, Equatable {
    public let readMBps: Double?
    public let writeMBps: Double?

    /// Rates nobody measured. What an omitted `disk` object decodes to.
    public static let unknown = DiskMetrics(readMBps: nil, writeMBps: nil)

    public init(readMBps: Double?, writeMBps: Double?) {
        self.readMBps = readMBps
        self.writeMBps = writeMBps
    }
}

/// Network rates. `nil` for the same reason as ``DiskMetrics``'.
public struct NetworkMetrics: Sendable, Codable, Equatable {
    public let downloadMBps: Double?
    public let uploadMBps: Double?

    /// Rates nobody measured. What an omitted `network` object decodes to.
    public static let unknown = NetworkMetrics(downloadMBps: nil, uploadMBps: nil)

    public init(downloadMBps: Double?, uploadMBps: Double?) {
        self.downloadMBps = downloadMBps
        self.uploadMBps = uploadMBps
    }
}

public struct GPUMetrics: Sendable, Codable, Equatable {
    /// GPU usage, 0–100.
    public let usage: Double?
    public let vramUsedGB: Double?
    public let vramTotalGB: Double?

    /// A GPU nobody measured: every field absent. What an omitted `gpu` object
    /// decodes to, and what a producer with no portable GPU read should send —
    /// ``isPresent`` reads it as "no GPU" and the card renders "—".
    public static let unknown = GPUMetrics(usage: nil, vramUsedGB: nil, vramTotalGB: nil)

    public init(usage: Double?, vramUsedGB: Double?, vramTotalGB: Double?) {
        self.usage = usage
        self.vramUsedGB = vramUsedGB
        self.vramTotalGB = vramTotalGB
    }

    /// Whether this host actually reports a GPU.
    ///
    /// VRAM capacity is the discriminator, not `usage`: a real GPU sitting idle
    /// reports `usage == 0` and must still render as `0%`, while a host with no
    /// adapter reports zeros (or, post-#183, nothing) and must render as unknown.
    /// Callers deciding "number or em dash" ask this, never re-derive it — the
    /// same rule, and the same `vramTotalGB > 0` test, as `wire::Gpu::is_present`.
    ///
    /// Absent capacity and a capacity of zero are the same answer here, which is
    /// what keeps a pre-#183 agent's all-zero GPU reading as absent too.
    public var isPresent: Bool {
        (vramTotalGB ?? 0) > 0
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

    /// Custom decode: the two contract keys are required; the macOS-rich keys are
    /// optional, so an agent payload with only `level`/`isCharging` decodes. An
    /// unexpected/incompatible battery shape throws (caught upstream → nil).
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

    /// Encode omits nil enrichment so the round-trip of an agent-shaped value
    /// (only `level`/`isCharging`) re-emits exactly the contract floor.
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
