import Foundation
import HostMetricsKit

private extension Double {
    func clamped(to range: ClosedRange<Double>) -> Double {
        Swift.min(Swift.max(self, range.lowerBound), range.upperBound)
    }
}

/// A synthetic host profile to simulate — hardware shape plus a workload character.
/// Two presets back the local Mac and a synthetic remote Linux box so the demo
/// cockpit shows two visibly different hosts side by side.
struct DemoHostProfile {
    let model: String
    let cpuCores: Int
    let totalMemoryGB: Double
    let vramTotalGB: Double
    /// Mounted volumes to report (mount, usedGB, totalGB, fstype).
    let volumes: [VolumeUsage]
    /// Whether this host reports a battery (laptops do; servers/desktops don't).
    let hasBattery: Bool

    /// The local demo Mac — an Apple-silicon laptop on battery.
    static let localMac = DemoHostProfile(
        model: "Apple M4 Max",
        cpuCores: 16,
        totalMemoryGB: 64,
        vramTotalGB: 48,
        volumes: [
            VolumeUsage(mount: "/", usedGB: 612, totalGB: 1860, fstype: "apfs"),
            VolumeUsage(mount: "/Volumes/Work", usedGB: 1340, totalGB: 4000, fstype: "apfs")
        ],
        hasBattery: true
    )

    /// A synthetic remote Linux build host — desktop-class, no battery.
    static let remoteLinux = DemoHostProfile(
        model: "AMD Ryzen 9 7950X",
        cpuCores: 32,
        totalMemoryGB: 128,
        vramTotalGB: 24,
        volumes: [
            VolumeUsage(mount: "/", usedGB: 88, totalGB: 512, fstype: "ext4"),
            VolumeUsage(mount: "/data", usedGB: 3200, totalGB: 8000, fstype: "xfs")
        ],
        hasBattery: false
    )
}

/// Produces realistic, evolving `HostSnapshot` values for demo mode — no IOKit, no
/// network. A development workload: CPU/GPU compile spikes, a slow memory climb,
/// bursty disk during builds, and front-loaded package downloads. A continuous
/// `phase` (radians) drives the periodic components so the live stream looks like a
/// natural continuation of the pre-seeded history rather than restarting.
struct DemoSnapshotGenerator {
    let profile: DemoHostProfile

    /// Advances `points` snapshots' worth of phase per second-equivalent step.
    /// One step ≈ one sample. Chosen so a ~120-sample window shows several cycles.
    private static let phaseStep = (.pi * 2.0) / 45.0

    init(profile: DemoHostProfile) {
        self.profile = profile
    }

    /// A snapshot for the given phase. `phase` is an ever-increasing angle; callers
    /// step it by `Self.phaseStep` per sample.
    func snapshot(at phase: Double, now: Date = Date()) -> HostSnapshot {
        // CPU: dev workload with compile spikes.
        let cpuBase = 40.0
        let cpu = (cpuBase + sin(phase) * 25.0 + Double.random(in: -5...5)).clamped(to: 12...96)
        let cores = (0..<profile.cpuCores).map { _ in
            (cpu * Double.random(in: 0.55...1.45)).clamped(to: 4...100)
        }

        // GPU: preview/render refreshes, a faster cycle than CPU.
        let gpu = (25.0 + sin(phase * 1.6) * 30.0 + Double.random(in: -8...8)).clamped(to: 8...88)
        let vramUsed = (profile.vramTotalGB * 0.12 + (gpu / 100.0) * profile.vramTotalGB * 0.4)
            .clamped(to: 0...profile.vramTotalGB)

        // Memory: slow climb as caches build, wrapped to a sawtooth via phase.
        let memCycle = (sin(phase * 0.18) + 1) / 2          // 0…1
        let memPct = (50.0 + memCycle * 26.0 + Double.random(in: -2...2)).clamped(to: 44...82)
        let usedGB = profile.totalMemoryGB * memPct / 100.0
        let pressure = memoryPressure(forUsagePercent: memPct)
        let swap = max(0, (memPct - 70) * 0.05)

        // Disk: bursts while "compiling".
        let compiling = sin(phase) > 0.3
        let diskRead = compiling ? Double.random(in: 50...220) : Double.random(in: 0...18)
        let diskWrite = compiling ? Double.random(in: 30...160) : Double.random(in: 0...14)

        // Network: occasional dependency fetches.
        let fetching = sin(phase * 0.5) > 0.6
        let down = fetching ? Double.random(in: 15...85) : Double.random(in: 0...4)
        let up = Double.random(in: 0...3)

        let battery: BatteryMetrics? = profile.hasBattery ? demoBattery(phase: phase, cpu: cpu, gpu: gpu) : nil

        return HostSnapshot(
            timestamp: now,
            cpu: CPUMetrics(
                totalUsage: cpu,
                coreUsages: cores,
                model: profile.model,
                thermalState: thermalState(forCPU: cpu)
            ),
            memory: MemoryMetrics(
                usedGB: usedGB,
                totalGB: profile.totalMemoryGB,
                swapUsedGB: swap,
                pressure: pressure
            ),
            disk: DiskMetrics(readMBps: diskRead, writeMBps: diskWrite),
            network: NetworkMetrics(downloadMBps: down, uploadMBps: up),
            gpu: GPUMetrics(usage: gpu, vramUsedGB: vramUsed, vramTotalGB: profile.vramTotalGB),
            battery: battery,
            volumes: profile.volumes,
            processes: demoProcesses(cpu: cpu, usedGB: usedGB)
        )
    }

    /// A back-filled history ending at `endPhase`, oldest first, so sparklines render
    /// populated from the first frame instead of drawing in over two minutes.
    /// Returns the snapshots and the phase to continue the live stream from.
    func warmup(samples: Int, endingAt now: Date = Date()) -> (snapshots: [HostSnapshot], nextPhase: Double) {
        var out: [HostSnapshot] = []
        let startPhase = -Double(samples) * Self.phaseStep
        for i in 0..<samples {
            let phase = startPhase + Double(i) * Self.phaseStep
            let t = now.addingTimeInterval(Double(i - samples))
            out.append(snapshot(at: phase, now: t))
        }
        return (out, 0)
    }

    /// Phase increment to apply between live samples.
    static var step: Double { phaseStep }

    private func memoryPressure(forUsagePercent pct: Double) -> Double {
        let p: Double
        switch pct {
        case ..<50: p = pct * 0.2
        case ..<70: p = 10 + (pct - 50) * 1.0
        case ..<85: p = 30 + (pct - 70) * 2.0
        case ..<95: p = 60 + (pct - 85) * 3.0
        default: p = 90 + (pct - 95) * 2.0
        }
        return p.clamped(to: 0...100)
    }

    private func thermalState(forCPU cpu: Double) -> ThermalState {
        switch cpu {
        case ..<40: return .nominal
        case ..<70: return .fair
        case ..<85: return .serious
        default: return .critical
        }
    }

    private func demoBattery(phase: Double, cpu: Double, gpu: Double) -> BatteryMetrics {
        let level = (78.0 - (sin(phase * 0.05) + 1) * 9.0).clamped(to: 60...85)
        let wattage = (20 + (cpu / 100) * 30 + (gpu / 100) * 40 + Double.random(in: -2...2)).clamped(to: 15...90)
        return BatteryMetrics(
            level: level,
            isCharging: false,
            hasBattery: true,
            cycleCount: 234,
            health: 91.0,
            powerSource: "Battery Power",
            timeRemaining: "3:45",
            wattage: wattage
        )
    }

    private func demoProcesses(cpu: Double, usedGB: Double) -> [ProcessSample] {
        [
            ProcessSample(pid: 412, name: "swift-frontend", cpuPercent: (cpu * 1.4).clamped(to: 5...780), memoryMB: 2_300),
            ProcessSample(pid: 88, name: "Xcode", cpuPercent: (cpu * 0.5).clamped(to: 2...160), memoryMB: 4_100),
            ProcessSample(pid: 901, name: "node", cpuPercent: Double.random(in: 5...60), memoryMB: 1_250),
            ProcessSample(pid: 233, name: "WindowServer", cpuPercent: Double.random(in: 3...22), memoryMB: 980),
            ProcessSample(pid: 77, name: "kernel_task", cpuPercent: Double.random(in: 1...12), memoryMB: 640)
        ]
    }
}
