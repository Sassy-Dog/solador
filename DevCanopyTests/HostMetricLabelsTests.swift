@testable import DevCanopy
import HostMetricsKit
import XCTest

/// The render half of the wire-unknown contract (#192): every cell a host card
/// paints from an Optional must show an em dash when nobody measured it, and a
/// number when someone did — including when that number is zero.
final class HostMetricLabelsTests: XCTestCase {
    private let dash = HostMetricLabels.unknown

    func testUnknownIsAnEmDash() {
        XCTAssertEqual(HostMetricLabels.unknown, "\u{2014}", "matches viewmodel::card::UNKNOWN")
    }

    // MARK: - Absent / present / zero, for every lowered cell

    func testPercentDistinguishesUnknownFromZero() {
        XCTAssertEqual(HostMetricLabels.percent(nil), dash)
        XCTAssertEqual(HostMetricLabels.percent(0), "0%", "a measured zero is a real reading")
        XCTAssertEqual(HostMetricLabels.percent(41.4), "41%")
        XCTAssertEqual(HostMetricLabels.percent(41.6), "42%")
    }

    func testRateDistinguishesUnknownFromIdle() {
        XCTAssertEqual(HostMetricLabels.rate(nil), dash)
        XCTAssertEqual(HostMetricLabels.rate(0), "0.0 MB/s", "an idle disk still reports a rate")
        XCTAssertEqual(HostMetricLabels.rate(12.4), "12.4 MB/s")
        XCTAssertEqual(HostMetricLabels.rate(1024), "1.0 GB/s", "bounded width past 1000 MB/s")
    }

    func testNumberDistinguishesUnknownFromZero() {
        XCTAssertEqual(HostMetricLabels.number(nil), dash)
        XCTAssertEqual(HostMetricLabels.number(0), "0.0")
        XCTAssertEqual(HostMetricLabels.number(9.54), "9.5", "one decimal below 100")
        XCTAssertEqual(HostMetricLabels.number(412), "412", "whole numbers past 100 keep the label narrow")
    }

    func testAxisTicksStayNarrow() {
        XCTAssertEqual(HostMetricLabels.axis(88.1), "88.1")
        XCTAssertEqual(HostMetricLabels.axis(17151), "17k")
    }

    func testProcessMemorySwitchesUnitAtAGigabyte() {
        XCTAssertEqual(HostMetricLabels.processMemory(256), "256 MB")
        XCTAssertEqual(HostMetricLabels.processMemory(2150), "2.1 GB")
    }

    // MARK: - GPU: presence decides, usage doesn't

    func testAbsentGPURendersUnknownNeverZero() {
        XCTAssertEqual(HostMetricLabels.gpuUsage(.unknown), dash)
        XCTAssertEqual(HostMetricLabels.vram(.unknown), "VRAM: \(dash)", "one missing fact, not two")
    }

    /// The pre-#183 payload: all three fields present and all zero. Capacity is
    /// the discriminator, so this still reads as "no GPU" rather than an idle one.
    func testPre183AllZeroGPUAlsoRendersUnknown() {
        let zeros = GPUMetrics(usage: 0, vramUsedGB: 0, vramTotalGB: 0)
        XCTAssertFalse(zeros.isPresent)
        XCTAssertEqual(HostMetricLabels.gpuUsage(zeros), dash)
        XCTAssertEqual(HostMetricLabels.vram(zeros), "VRAM: \(dash)")
    }

    func testIdleButPresentGPURendersZeroPercent() {
        let idle = GPUMetrics(usage: 0, vramUsedGB: 0.5, vramTotalGB: 24)
        XCTAssertTrue(idle.isPresent)
        XCTAssertEqual(HostMetricLabels.gpuUsage(idle), "0%", "an idle adapter is a measurement")
        XCTAssertEqual(HostMetricLabels.vram(idle), "VRAM: 0.5 / 24.0 GB")
    }

    func testBusyGPURendersItsReadings() {
        let busy = GPUMetrics(usage: 41, vramUsedGB: 3.5, vramTotalGB: 24)
        XCTAssertEqual(HostMetricLabels.gpuUsage(busy), "41%")
        XCTAssertEqual(HostMetricLabels.vram(busy), "VRAM: 3.5 / 24.0 GB")
    }

    /// A present adapter with an unreadable usage blanks only that field — the
    /// VRAM it did report keeps rendering.
    func testPresentGPUWithUnreadableUsageBlanksOnlyThatField() {
        let partial = GPUMetrics(usage: nil, vramUsedGB: 3.5, vramTotalGB: 24)
        XCTAssertEqual(HostMetricLabels.gpuUsage(partial), dash)
        XCTAssertEqual(HostMetricLabels.vram(partial), "VRAM: 3.5 / 24.0 GB")
    }

    // MARK: - A whole card's worth, end to end

    /// The regression this issue exists for: the Linux host that reports no
    /// pressure, no thermal state, no rates and no GPU renders dashes — while a
    /// genuinely idle host, reporting the same figures as measured zeros, does
    /// not. Same card, opposite claims.
    func testAnUnmeasuringHostDashesWhereAnIdleOneShowsZeros() {
        let blind = HostSnapshot(
            timestamp: Date(),
            cpu: CPUMetrics(totalUsage: 11.5, coreUsages: [9], model: "x", thermalState: nil),
            memory: MemoryMetrics(usedGB: 5.5, totalGB: 16, swapUsedGB: 0, pressure: nil),
            disk: .unknown,
            network: .unknown,
            gpu: .unknown,
            battery: nil
        )
        XCTAssertNil(blind.cpu.thermalState, "no badge is rendered for an unread thermal state")
        XCTAssertEqual(HostMetricLabels.percent(blind.memory.pressure), dash)
        XCTAssertEqual(HostMetricLabels.rate(blind.disk.readMBps), dash)
        XCTAssertEqual(HostMetricLabels.rate(blind.disk.writeMBps), dash)
        XCTAssertEqual(HostMetricLabels.rate(blind.network.downloadMBps), dash)
        XCTAssertEqual(HostMetricLabels.rate(blind.network.uploadMBps), dash)
        XCTAssertEqual(HostMetricLabels.gpuUsage(blind.gpu), dash)

        let idle = HostSnapshot(
            timestamp: Date(),
            cpu: CPUMetrics(totalUsage: 0, coreUsages: [0], model: "x", thermalState: .nominal),
            memory: MemoryMetrics(usedGB: 5.5, totalGB: 16, swapUsedGB: 0, pressure: 0),
            disk: DiskMetrics(readMBps: 0, writeMBps: 0),
            network: NetworkMetrics(downloadMBps: 0, uploadMBps: 0),
            gpu: GPUMetrics(usage: 0, vramUsedGB: 0, vramTotalGB: 24),
            battery: nil
        )
        XCTAssertEqual(idle.cpu.thermalState, .nominal)
        XCTAssertEqual(HostMetricLabels.percent(idle.memory.pressure), "0%")
        XCTAssertEqual(HostMetricLabels.rate(idle.disk.readMBps), "0.0 MB/s")
        XCTAssertEqual(HostMetricLabels.rate(idle.network.uploadMBps), "0.0 MB/s")
        XCTAssertEqual(HostMetricLabels.gpuUsage(idle.gpu), "0%")
    }
}
