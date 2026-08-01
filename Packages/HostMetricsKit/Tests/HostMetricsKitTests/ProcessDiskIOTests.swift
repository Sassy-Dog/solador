import Darwin
import Foundation
@testable import HostMetricsKit
import XCTest

/// Per-process disk I/O: the cumulative-counter → per-second-rate conversion and
/// the unknown handling that keeps a failed `proc_pid_rusage` read from
/// fabricating a zero.
final class ProcessDiskIOTests: XCTestCase {
    // MARK: - Rate computation

    func testBytesPerSecondNormalRate() {
        // 4 MiB of new I/O over 2 seconds = 2 MiB/s.
        let rate = ProcessDiskIOSampler.bytesPerSecond(
            currentBytes: 5 * 1_048_576,
            previousBytes: 1 * 1_048_576,
            elapsedSeconds: 2.0
        )
        XCTAssertEqual(rate, 2 * 1_048_576)
    }

    func testBytesPerSecondRoundsToNearestByte() {
        // 10 bytes over 3s = 3.33… → 3.
        XCTAssertEqual(
            ProcessDiskIOSampler.bytesPerSecond(currentBytes: 110, previousBytes: 100, elapsedSeconds: 3.0),
            3
        )
    }

    func testBytesPerSecondUnchangedCounterIsMeasuredZero() {
        // The counter did not move: the process really did no I/O this tick. That
        // is a measured zero, not an unknown — it must NOT come back as nil.
        let rate = ProcessDiskIOSampler.bytesPerSecond(
            currentBytes: 4096,
            previousBytes: 4096,
            elapsedSeconds: 1.0
        )
        XCTAssertEqual(rate, 0, "A stationary counter is a measured zero, not unknown")
    }

    // MARK: - Unknown handling

    func testBytesPerSecondFirstSampleIsUnknown() {
        // Acceptance: the first sample for a pid has no baseline, so it reports
        // unknown rather than treating the lifetime total as one second of I/O.
        XCTAssertNil(
            ProcessDiskIOSampler.bytesPerSecond(
                currentBytes: 900_000_000,
                previousBytes: nil,
                elapsedSeconds: 1.0
            ),
            "No previous counter means the rate is unknown, not a bogus lifetime-total rate"
        )
    }

    func testBytesPerSecondBackwardsCounterIsUnknown() {
        // PID reuse: the cached counter belongs to a dead process, so the diff is
        // meaningless (and would underflow UInt64 — the crash class fixed for CPU).
        XCTAssertNil(
            ProcessDiskIOSampler.bytesPerSecond(
                currentBytes: 1000,
                previousBytes: 5_000_000_000,
                elapsedSeconds: 1.0
            ),
            "A backwards counter (reused PID) is unknown, not 0"
        )
    }

    func testBytesPerSecondZeroElapsedIsUnknown() {
        XCTAssertNil(
            ProcessDiskIOSampler.bytesPerSecond(
                currentBytes: 2000,
                previousBytes: 1000,
                elapsedSeconds: 0
            )
        )
    }

    func testBytesPerSecondNegativeElapsedIsUnknown() {
        // Wall-clock can jump backwards (NTP step); no usable window.
        XCTAssertNil(
            ProcessDiskIOSampler.bytesPerSecond(
                currentBytes: 2000,
                previousBytes: 1000,
                elapsedSeconds: -5
            )
        )
    }

    func testBytesPerSecondSaturatesInsteadOfTrapping() {
        // A huge delta over a sub-millisecond window exceeds UInt64 once scaled to
        // a per-second rate. Converting a Double >= 2^64 to UInt64 traps, so the
        // value must saturate.
        let rate = ProcessDiskIOSampler.bytesPerSecond(
            currentBytes: UInt64.max,
            previousBytes: 0,
            elapsedSeconds: 0.000_000_001
        )
        XCTAssertEqual(rate, UInt64.max, "An out-of-range rate must saturate, not trap")
    }

    // MARK: - Aggregate totals

    private func process(pid: Int32, read: UInt64?, written: UInt64?) -> ProcessItem {
        ProcessItem(pid: pid, name: "App Helper", diskReadBytes: read, diskWriteBytes: written)
    }

    func testApplicationTotalsSumKnownRates() {
        let app = ApplicationItem(name: "App", processes: [
            process(pid: 1, read: 100, written: 10),
            process(pid: 2, read: 250, written: 40)
        ])
        XCTAssertEqual(app.diskReadBytes, 350)
        XCTAssertEqual(app.diskWriteBytes, 50)
    }

    func testApplicationTotalsExcludeUnknownProcesses() {
        // The unknown process must not contribute a fabricated 0 to the total —
        // the total is the sum of what was actually measured.
        let app = ApplicationItem(name: "App", processes: [
            process(pid: 1, read: 100, written: 10),
            process(pid: 2, read: nil, written: nil)
        ])
        XCTAssertEqual(app.diskReadBytes, 100)
        XCTAssertEqual(app.diskWriteBytes, 10)
    }

    func testApplicationTotalsAreUnknownWhenEveryProcessIsUnknown() {
        let app = ApplicationItem(name: "App", processes: [
            process(pid: 1, read: nil, written: nil),
            process(pid: 2, read: nil, written: nil)
        ])
        XCTAssertNil(app.diskReadBytes, "Nothing was measured, so the total is unknown — not 0")
        XCTAssertNil(app.diskWriteBytes, "Nothing was measured, so the total is unknown — not 0")
    }

    func testApplicationTotalsAreUnknownWithNoProcesses() {
        let app = ApplicationItem(name: "App")
        XCTAssertNil(app.diskReadBytes)
        XCTAssertNil(app.diskWriteBytes)
    }

    func testApplicationTotalsKeepMeasuredZeros() {
        // A measured 0 is data: mixed with a known rate it stays in the sum, and
        // on its own it yields 0 rather than unknown.
        let mixed = ApplicationItem(name: "App", processes: [
            process(pid: 1, read: 0, written: 0),
            process(pid: 2, read: 7, written: 3)
        ])
        XCTAssertEqual(mixed.diskReadBytes, 7)
        XCTAssertEqual(mixed.diskWriteBytes, 3)

        let allZero = ApplicationItem(name: "App", processes: [
            process(pid: 1, read: 0, written: 0)
        ])
        XCTAssertEqual(allZero.diskReadBytes, 0, "A measured zero is not unknown")
        XCTAssertEqual(allZero.diskWriteBytes, 0, "A measured zero is not unknown")
    }

    func testSumKnownRatesSaturatesInsteadOfTrapping() {
        XCTAssertEqual(ApplicationItem.sumKnownRates([.max, 1]), .max, "Overflow must saturate, not trap")
    }

    // MARK: - Sampler state

    func testSamplerFirstTickIsUnknownAndSecondTickIsKnown() {
        var sampler = ProcessDiskIOSampler()
        let me = getpid()

        let first = sampler.rates(pid: me, elapsedSeconds: 1.0)
        XCTAssertNil(first.read, "No baseline on the first tick: unknown, not 0")
        XCTAssertNil(first.written, "No baseline on the first tick: unknown, not 0")

        let second = sampler.rates(pid: me, elapsedSeconds: 1.0)
        XCTAssertNotNil(second.read, "The first tick establishes the baseline the second one diffs against")
        XCTAssertNotNil(second.written)
    }

    func testSamplerForgettingAPIDDropsItsBaseline() {
        var sampler = ProcessDiskIOSampler()
        let me = getpid()

        _ = sampler.rates(pid: me, elapsedSeconds: 1.0)
        XCTAssertNotNil(sampler.rates(pid: me, elapsedSeconds: 1.0).written)

        // A pid that vanished must not leave its counter behind: if the kernel
        // hands the number to a new process, the stale baseline would produce a
        // garbage rate for it.
        sampler.forgetPIDs(notIn: [])
        XCTAssertNil(
            sampler.rates(pid: me, elapsedSeconds: 1.0).written,
            "After its baseline is forgotten, a pid is unknown again until it is re-sampled"
        )
    }

    func testSamplerUnreadableProcessIsUnknown() {
        var sampler = ProcessDiskIOSampler()
        let rates = sampler.rates(pid: pid_t.max, elapsedSeconds: 1.0)
        XCTAssertNil(rates.read, "An unreadable process contributes no fabricated zero")
        XCTAssertNil(rates.written, "An unreadable process contributes no fabricated zero")
    }

    // MARK: - Live counter reads

    func testDiskIOCountersAreReadableForOwnProcess() {
        // The app always owns itself, so rusage must succeed here. If this ever
        // returns nil, every process would report unknown and the feature is dead.
        XCTAssertNotNil(
            ProcessDiskIOSampler.readCounters(pid: getpid()),
            "rusage must be readable for the current process"
        )
    }

    func testDiskIOCountersAreUnknownForNonexistentProcess() {
        // ESRCH: a pid that cannot exist must read as unknown, not as zeroes.
        XCTAssertNil(
            ProcessDiskIOSampler.readCounters(pid: pid_t.max),
            "A nonexistent pid must be unknown, not a zeroed counter"
        )
    }

    /// Acceptance check for "under real disk load the app's own process reports a
    /// nonzero rate", reduced to the part that can be asserted deterministically:
    /// the cumulative write counter must actually move when this process writes
    /// and flushes to disk. The rate math on top of it is covered above.
    func testWriteCounterAdvancesUnderRealDiskLoad() throws {
        let before = try XCTUnwrap(ProcessDiskIOSampler.readCounters(pid: getpid()))
        try writeAndFlushTestFile(megabytes: 16)
        let after = try XCTUnwrap(ProcessDiskIOSampler.readCounters(pid: getpid()))

        XCTAssertGreaterThan(
            after.bytesWritten,
            before.bytesWritten,
            "Writing 16 MiB must advance this process's cumulative disk-write counter"
        )
        XCTAssertNotNil(
            ProcessDiskIOSampler.bytesPerSecond(
                currentBytes: after.bytesWritten,
                previousBytes: before.bytesWritten,
                elapsedSeconds: 1.0
            ),
            "A real write between two samples must produce a known rate"
        )
    }

    // MARK: - End-to-end collection

    /// Locks the wiring the unit tests above cannot see: that `getProcessData`
    /// actually samples rusage per process rather than publishing a constant.
    ///
    /// Both halves are deterministic. On the first tick *no* pid has a baseline,
    /// so every rate is unknown; on the second tick this process — which the app
    /// always owns — has one, so its rate is known and reflects the disk load
    /// written in between.
    func testCollectedRatesAreUnknownOnFirstTickAndMeasuredOnSecond() throws {
        let monitor = SystemMonitorV2()

        let firstTick = monitor.getProcessData().processes
        let selfOnFirstTick = try XCTUnwrap(
            firstTick.first { $0.id == getpid() },
            "The test process must appear in its own process enumeration"
        )
        XCTAssertNil(
            selfOnFirstTick.diskReadBytes,
            "First sample for a pid has no baseline: the rate is unknown, not 0"
        )
        XCTAssertNil(selfOnFirstTick.diskWriteBytes, "First sample for a pid is unknown, not 0")

        try writeAndFlushTestFile(megabytes: 16)

        let secondTick = monitor.getProcessData().processes
        let selfOnSecondTick = try XCTUnwrap(secondTick.first { $0.id == getpid() })
        XCTAssertNotNil(
            selfOnSecondTick.diskReadBytes,
            "With a baseline in hand, an owned process must report a known read rate"
        )
        XCTAssertGreaterThan(
            try XCTUnwrap(selfOnSecondTick.diskWriteBytes),
            0,
            "16 MiB flushed between ticks must surface as a nonzero write rate"
        )
    }

    /// Writes and fully flushes a scratch file, generating disk I/O the kernel
    /// charges to this process. `F_FULLFSYNC` (not plain `fsync`) is what forces
    /// the bytes past the buffer cache so they land in the rusage counters.
    private func writeAndFlushTestFile(megabytes: Int) throws {
        let url = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("hostmetricskit-diskio-\(UUID().uuidString).bin")
        defer { try? FileManager.default.removeItem(at: url) }

        XCTAssertTrue(FileManager.default.createFile(atPath: url.path, contents: nil))
        let handle = try FileHandle(forWritingTo: url)
        try handle.write(contentsOf: Data(repeating: 0xAB, count: megabytes * 1_048_576))
        XCTAssertNotEqual(fcntl(handle.fileDescriptor, F_FULLFSYNC), -1, "F_FULLFSYNC must succeed")
        try handle.close()
    }
}
