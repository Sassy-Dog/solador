import Darwin
import Foundation

/// Per-process disk I/O sampling.
///
/// `proc_pid_rusage` reports **cumulative** byte counters, while `ProcessItem`
/// publishes per-second rates, so every tick is diffed against the previous one —
/// the same shape as `SystemMonitorV2.getProcessCPUUsage`. Keeping that state in
/// its own value (rather than another dictionary on the monitor) lets the delta
/// logic be tested without standing up the whole monitor, and keeps the already
/// oversized `SystemMonitorV2.swift` from growing further.
///
/// Everything here is unknown-first: a rate the sampler could not measure comes
/// back as `nil`, never as 0. Most processes on the system are unreadable to an
/// unprivileged app, and a fabricated 0 for them would be indistinguishable from
/// a process that genuinely did no I/O.
struct ProcessDiskIOSampler {
    /// Cumulative counters from the previous tick, keyed by pid.
    private var previousCounters: [pid_t: DiskStats] = [:]

    /// Samples `pid`'s cumulative counters and converts them into per-second
    /// rates by diffing against the previous tick. Either component is `nil` when
    /// unknown — see `bytesPerSecond` for exactly when.
    ///
    /// - Parameter elapsedSeconds: wall-clock time since the previous tick.
    mutating func rates(pid: pid_t, elapsedSeconds: TimeInterval) -> (read: UInt64?, written: UInt64?) {
        guard let current = Self.readCounters(pid: pid) else {
            // Drop any stale baseline: if this pid becomes readable again later,
            // the unmeasured gap makes its cached counter useless as a rate.
            previousCounters[pid] = nil
            return (nil, nil)
        }

        let previous = previousCounters[pid]
        let read = Self.bytesPerSecond(
            currentBytes: current.bytesRead,
            previousBytes: previous?.bytesRead,
            elapsedSeconds: elapsedSeconds
        )
        let written = Self.bytesPerSecond(
            currentBytes: current.bytesWritten,
            previousBytes: previous?.bytesWritten,
            elapsedSeconds: elapsedSeconds
        )

        // Always re-baseline so the next sample diffs against this one, matching
        // the CPU path's recovery from PID reuse.
        previousCounters[pid] = current
        return (read, written)
    }

    /// Drops baselines for processes that no longer exist, so the map does not
    /// grow without bound over a long uptime. Mirrors the CPU map's cleanup.
    mutating func forgetPIDs(notIn livePIDs: Set<pid_t>) {
        previousCounters = previousCounters.filter { livePIDs.contains($0.key) }
    }

    /// Reads a process's *cumulative* disk I/O byte counters via `proc_pid_rusage`.
    ///
    /// Returns `nil` when the read fails. That is the common case, not an edge
    /// case: rusage is denied (EPERM) for processes this app does not own — most
    /// of the system when running unprivileged — and ESRCH for a pid that exited
    /// between enumeration and this call. A failed read must stay unknown; it is
    /// the whole reason the published rates are optional.
    static func readCounters(pid: pid_t) -> DiskStats? {
        var usage = rusage_info_v4()
        let result = withUnsafeMutablePointer(to: &usage) { pointer -> Int32 in
            pointer.withMemoryRebound(to: rusage_info_t?.self, capacity: 1) { rebound in
                proc_pid_rusage(pid, RUSAGE_INFO_V4, rebound)
            }
        }

        guard result == 0 else { return nil }

        return DiskStats(
            bytesRead: usage.ri_diskio_bytesread,
            bytesWritten: usage.ri_diskio_byteswritten
        )
    }

    /// Converts two cumulative disk-I/O byte counters into a bytes-per-second rate.
    ///
    /// Returns `nil` — *unknown*, never a fabricated 0 — when there is no prior
    /// sample for the pid (the first tick after the process appears), when no
    /// wall-clock time has elapsed, or when the counter has gone backwards. A
    /// backwards counter means the kernel recycled the pid: the same trap that
    /// made the CPU delta underflow and trap (see `SystemMonitorV2.cpuPercent`).
    /// The cached number belongs to a dead process and yields no meaningful rate.
    ///
    /// A counter that simply did not move returns 0. That is a *measured* zero —
    /// the process really did no I/O this tick — and is honest to publish.
    static func bytesPerSecond(
        currentBytes: UInt64,
        previousBytes: UInt64?,
        elapsedSeconds: TimeInterval
    ) -> UInt64? {
        guard let previousBytes, currentBytes >= previousBytes, elapsedSeconds > 0 else {
            return nil
        }

        let rate = (Double(currentBytes - previousBytes) / elapsedSeconds).rounded()
        guard rate.isFinite else { return nil }

        // Saturate rather than trap: converting a Double >= 2^64 to UInt64 crashes.
        return rate >= Double(UInt64.max) ? .max : UInt64(rate)
    }
}
