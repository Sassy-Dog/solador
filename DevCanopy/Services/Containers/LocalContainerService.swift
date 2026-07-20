import Foundation

/// Result of one per-runtime merge pass.
struct LocalMergeOutcome: Equatable {
    let merged: [ContainerInfo]
    let updatedLastKnown: [ContainerRuntime: [ContainerInfo]]
    let errored: [String]
    let succeeded: Set<ContainerRuntime>
}

/// Pure per-runtime merge with last-known retention: a runtime whose poll failed
/// contributes its previous list instead of nothing, so one transient `tart list`
/// failure can't blank every VM row until the next poll. No I/O here.
enum LocalContainerMerge {
    static func merge(
        results: [(runtime: ContainerRuntime, containers: [ContainerInfo]?)],
        lastKnown: [ContainerRuntime: [ContainerInfo]]
    ) -> LocalMergeOutcome {
        var merged: [ContainerInfo] = []
        var updated = lastKnown
        var errored: [String] = []
        var succeeded: Set<ContainerRuntime> = []

        for (runtime, containers) in results {
            if let containers {
                merged += containers
                updated[runtime] = containers
                succeeded.insert(runtime)
            } else {
                merged += lastKnown[runtime] ?? []
                errored.append(runtime.rawValue)
            }
        }
        return LocalMergeOutcome(
            merged: merged,
            updatedLastKnown: updated,
            errored: errored,
            succeeded: succeeded
        )
    }
}

/// Detects and polls local container runtimes / VM managers (docker, podman,
/// tart) on "this machine". GUI apps inherit a minimal PATH, so tools are
/// resolved by absolute path. Resilient: a missing or erroring tool is treated
/// as contributing nothing new — its last-known list is retained.
@MainActor
final class LocalContainerService: ObservableObject {
    @Published private(set) var containers: [ContainerInfo] = []
    @Published private(set) var detectedRuntimes: [ContainerRuntime] = []
    @Published private(set) var lastUpdated: Date?
    @Published private(set) var lastError: String?

    /// Directories searched (in order) for tool executables.
    private static let searchPaths = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"]

    /// Runtimes whose most recent poll actually succeeded — presence clocks must
    /// only advance for these (a failed source proves nothing about absence).
    private(set) var lastSucceededRuntimes: Set<ContainerRuntime> = []

    /// Fed after every poll so expected-container absence can be measured.
    /// Assigned once at app startup; nil in previews/tests that don't care.
    var presenceStore: ContainerPresenceStore?

    private var lastKnownByRuntime: [ContainerRuntime: [ContainerInfo]] = [:]
    private var task: Task<Void, Never>?

    /// Resolves the absolute path of a tool by probing the known bin dirs.
    func toolPath(_ name: String) -> String? {
        let fm = FileManager.default
        for dir in Self.searchPaths {
            let candidate = "\(dir)/\(name)"
            if fm.isExecutableFile(atPath: candidate) {
                return candidate
            }
        }
        return nil
    }

    /// Runs whichever tools exist, parses their output, and publishes the merged
    /// result. Never throws; a failed tool retains its last-known list so one
    /// transient failure can't blank rows for containers that still exist.
    func refresh() async {
        // Resolve tools on the main actor (cheap FS probes), then do the
        // process spawning + parsing off-actor.
        let docker = toolPath(ContainerRuntime.docker.toolName)
        let podman = toolPath(ContainerRuntime.podman.toolName)
        let tart = toolPath(ContainerRuntime.tart.toolName)

        let (results, runtimes) = await Task.detached {
            () -> ([(runtime: ContainerRuntime, containers: [ContainerInfo]?)], [ContainerRuntime]) in
            var results: [(runtime: ContainerRuntime, containers: [ContainerInfo]?)] = []
            var runtimes: [ContainerRuntime] = []

            let psArgs = ["ps", "-a", "--format", "{{.Names}}|{{.Status}}|{{.Image}}"]

            if let docker {
                runtimes.append(.docker)
                let out = Self.run(docker, psArgs)
                results.append((.docker, out.map { ContainerParsing.parsePsOutput($0, runtime: .docker) }))
            }
            if let podman {
                runtimes.append(.podman)
                let out = Self.run(podman, psArgs)
                results.append((.podman, out.map { ContainerParsing.parsePsOutput($0, runtime: .podman) }))
            }
            if let tart {
                runtimes.append(.tart)
                let out = Self.run(tart, ["list"])
                results.append((.tart, out.map(ContainerParsing.parseTartList)))
            }

            return (results, runtimes)
        }.value

        let outcome = LocalContainerMerge.merge(results: results, lastKnown: lastKnownByRuntime)
        lastKnownByRuntime = outcome.updatedLastKnown
        containers = outcome.merged
        detectedRuntimes = runtimes
        lastSucceededRuntimes = outcome.succeeded
        lastError = outcome.errored.isEmpty ? nil : "couldn't read \(outcome.errored.joined(separator: ", "))"
        lastUpdated = Date()

        presenceStore?.noteLocalPoll(
            containers: outcome.merged,
            succeeded: outcome.succeeded,
            rules: ContainerGroupRule.loadFromDefaults()
        )
    }

    /// Initial refresh plus a repeating refresh every 10s.
    func start(interval: TimeInterval = 10) {
        guard task == nil else { return }
        task = Task { [weak self] in
            guard let self else { return }
            await refresh()
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: UInt64(interval * 1_000_000_000))
                if Task.isCancelled {
                    break
                }
                await refresh()
            }
        }
    }

    func stop() {
        task?.cancel()
        task = nil
    }

    deinit { task?.cancel() }

    /// Runs a tool and captures stdout. Returns nil on any failure. Off-actor.
    private nonisolated static func run(_ executable: String, _ arguments: [String]) -> String? {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments

        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = Pipe() // discard stderr

        do {
            try process.run()
            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            guard process.terminationStatus == 0 else { return nil }
            return String(data: data, encoding: .utf8)
        } catch {
            return nil
        }
    }
}
