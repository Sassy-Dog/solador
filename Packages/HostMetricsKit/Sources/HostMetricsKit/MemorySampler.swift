import Darwin
import Foundation

/// One reading of the host's memory counters.
///
/// A struct rather than a tuple because all four figures come from the same
/// `host_statistics64` call and travel together: they succeed together, and when
/// that call fails they are unknown together. Nothing here is Optional — this
/// type *is* the successful case, and `MonitoringResult` carries the other one.
struct MemoryReading {
    /// Used memory in GB.
    let usedGB: Double
    /// Total physical memory in GB.
    let totalGB: Double
    /// Swap in use in GB.
    let swapUsedGB: Double
    /// Memory pressure, 0–100.
    let pressure: Double
}

/// Samples the host's memory, and tells the truth when it can't.
///
/// Split out of `SystemMonitorV2` for the same reasons `ProcessDiskIOSampler`
/// was: the failure-state machine is testable without standing up the whole
/// monitor, and the already-oversized monitor doesn't grow further.
///
/// The rule it enforces (#204): a failed kernel read publishes **unknown**, not a
/// plausible number. The old failure path invented `usedMemory = totalMemory *
/// 0.5` with swap and pressure defaulted to 0 — an entire memory panel derived
/// from a constant and painted with the confidence of a real one.
struct MemorySampler {
    /// Reads the kernel counters. Injectable so the failure path — otherwise
    /// unreachable on a healthy machine — can be exercised in a test.
    private let read: () -> MonitoringResult<MemoryReading>

    /// Whether the last read failed, so the failure is logged on the
    /// *transition* rather than once per poll. Collection runs at 1 Hz and a
    /// broken mach call fails every tick; a line per second buries the moment it
    /// broke. Same rule as `HostMetricsService.logUnmeasuredIfChanged`.
    private(set) var isFailing = false

    init(read: @escaping () -> MonitoringResult<MemoryReading> = MemorySampler.readKernelCounters) {
        self.read = read
    }

    /// Takes one sample. Every figure derived from the kernel read is `nil` when
    /// that read failed; `totalGB` survives, because `ProcessInfo.physicalMemory`
    /// is a separate and infallible source — the failure doesn't take away how
    /// much RAM the machine has.
    mutating func sample() -> MemoryData {
        switch read() {
        case let .success(reading):
            noteRead(failure: nil)
            return MemoryData(
                usedMemory: reading.usedGB,
                totalMemory: reading.totalGB,
                swapUsed: reading.swapUsedGB,
                pressure: reading.pressure
            )
        case let .failure(error):
            noteRead(failure: error)
            return MemoryData(
                usedMemory: nil,
                totalMemory: Self.physicalMemoryGB,
                swapUsed: nil,
                pressure: nil
            )
        }
    }

    /// Logs a failure (and the later recovery) once per transition — never once
    /// per poll. See ``isFailing``.
    private mutating func noteRead(failure: SystemMonitorError?) {
        guard let failure else {
            if isFailing {
                isFailing = false
                Logger.monitor.info("Memory monitoring recovered; readings are live again")
            }
            return
        }
        guard !isFailing else { return }
        isFailing = true
        Logger.monitor.error(
            "Memory monitoring failed: \(failure.localizedDescription) — used/swap/pressure render \u{2014} until it recovers"
        )
    }

    /// Total physical memory in GB. Not part of the kernel read below: this one
    /// cannot fail, which is why capacity stays a measurement across a failure.
    static var physicalMemoryGB: Double {
        Double(ProcessInfo.processInfo.physicalMemory) / bytesPerGiB
    }

    private static let bytesPerGiB = 1_073_741_824.0

    /// Reads the kernel's memory counters, or fails.
    ///
    /// Stateless — it holds no delta across calls — so it doubles as the default
    /// for the injectable reader.
    static func readKernelCounters() -> MonitoringResult<MemoryReading> {
        var vmInfo = vm_statistics64()
        var vmInfoSize = mach_msg_type_number_t(MemoryLayout<vm_statistics64>.size / MemoryLayout<natural_t>.size)
        let hostPort = mach_host_self()
        defer { mach_port_deallocate(machTaskSelf, hostPort) }

        let vmResult = withUnsafeMutablePointer(to: &vmInfo) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(vmInfoSize)) {
                host_statistics64(hostPort, HOST_VM_INFO64, $0, &vmInfoSize)
            }
        }
        guard vmResult == KERN_SUCCESS else {
            return .failure(.ioKitError(vmResult, "host_statistics64"))
        }

        // Page counts are only bytes once multiplied by the page size, so a
        // failed `host_page_size` makes the whole reading unknown. It used to be
        // called on a second, never-deallocated host port with its return code
        // dropped on the floor — leaving the size at 0, which silently turned
        // every page count into 0 GB. Same fabrication as `total * 0.5`, quieter.
        var pageSizeBytes = vm_size_t()
        let pageSizeResult = host_page_size(hostPort, &pageSizeBytes)
        guard pageSizeResult == KERN_SUCCESS else {
            return .failure(.ioKitError(pageSizeResult, "host_page_size"))
        }
        guard pageSizeBytes > 0 else {
            return .failure(.invalidData("host_page_size reported a page size of 0"))
        }
        let pageSize = Double(pageSizeBytes)

        let usedPages = Double(vmInfo.active_count + vmInfo.wire_count)
        let usedGB = (usedPages * pageSize) / bytesPerGiB
        let swapUsedGB = Double(vmInfo.swapouts) * pageSize / bytesPerGiB

        // Pressure is the share of compressed pages the compressor has not handed
        // back. No compressions at all is a *measured* zero, not a missing
        // reading: the compressor ran and did nothing, which is what an unloaded
        // machine looks like. Only a failed read above is unknown.
        let compressions = Double(vmInfo.compressions)
        let decompressions = Double(vmInfo.decompressions)
        let pressure: Double = compressions > 0
            ? ((compressions - decompressions) / compressions) * 100
            : 0

        return .success(MemoryReading(
            usedGB: usedGB,
            totalGB: physicalMemoryGB,
            swapUsedGB: swapUsedGB,
            pressure: pressure.clamped(to: 0 ... 100)
        ))
    }
}
