import Foundation

/// Detects and polls local container runtimes / VM managers (docker, podman,
/// tart) on "this machine". GUI apps inherit a minimal PATH, so tools are
/// resolved by absolute path. Resilient: a missing or erroring tool is treated
/// as contributing nothing.
@MainActor
final class LocalContainerService: ObservableObject {
    @Published private(set) var containers: [ContainerInfo] = []
    @Published private(set) var detectedRuntimes: [ContainerRuntime] = []

    /// Directories searched (in order) for tool executables.
    private static let searchPaths = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"]

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
    /// result. Never throws; failures yield empty contributions.
    func refresh() async {
        // Resolve tools on the main actor (cheap FS probes), then do the
        // process spawning + parsing off-actor.
        let docker = toolPath(ContainerRuntime.docker.toolName)
        let podman = toolPath(ContainerRuntime.podman.toolName)
        let tart = toolPath(ContainerRuntime.tart.toolName)

        let (merged, runtimes) = await Task.detached { () -> ([ContainerInfo], [ContainerRuntime]) in
            var merged: [ContainerInfo] = []
            var runtimes: [ContainerRuntime] = []

            let psArgs = ["ps", "-a", "--format", "{{.Names}}|{{.Status}}|{{.Image}}"]

            if let docker {
                runtimes.append(.docker)
                if let out = Self.run(docker, psArgs) {
                    merged += ContainerParsing.parsePsOutput(out, runtime: .docker)
                }
            }
            if let podman {
                runtimes.append(.podman)
                if let out = Self.run(podman, psArgs) {
                    merged += ContainerParsing.parsePsOutput(out, runtime: .podman)
                }
            }
            if let tart {
                runtimes.append(.tart)
                if let out = Self.run(tart, ["list"]) {
                    merged += ContainerParsing.parseTartList(out)
                }
            }

            return (merged, runtimes)
        }.value

        self.containers = merged
        self.detectedRuntimes = runtimes
    }

    /// Initial refresh plus a repeating refresh every 10s.
    func start(interval: TimeInterval = 10) {
        guard task == nil else { return }
        task = Task { [weak self] in
            guard let self else { return }
            await self.refresh()
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: UInt64(interval * 1_000_000_000))
                if Task.isCancelled { break }
                await self.refresh()
            }
        }
    }

    func stop() {
        task?.cancel()
        task = nil
    }

    deinit { task?.cancel() }

    /// Runs a tool and captures stdout. Returns nil on any failure. Off-actor.
    nonisolated private static func run(_ executable: String, _ arguments: [String]) -> String? {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments

        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = Pipe()  // discard stderr

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
