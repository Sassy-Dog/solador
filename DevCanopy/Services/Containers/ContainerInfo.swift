import Foundation

/// A container runtime / VM manager detected on this machine.
enum ContainerRuntime: String, Sendable, CaseIterable {
    case docker
    case podman
    case tart

    /// Tool executable name to resolve on PATH.
    var toolName: String { rawValue }

    var displayName: String {
        switch self {
        case .docker: return "Docker"
        case .podman: return "Podman"
        case .tart: return "Tart"
        }
    }
}

/// One container or VM on this machine.
struct ContainerInfo: Sendable, Identifiable, Equatable {
    /// Stable identity: runtime + name (names are unique within a runtime).
    var id: String { "\(runtime.rawValue):\(name)" }
    let name: String
    let statusText: String
    let isRunning: Bool
    let runtime: ContainerRuntime
    let image: String?
}

/// Pure, tested parsing for container/VM tool output. No I/O here.
enum ContainerParsing {

    /// Parses `docker ps -a --format '{{.Names}}|{{.Status}}|{{.Image}}'`
    /// (and the identical podman invocation): pipe-delimited lines.
    /// `isRunning` is true when the status begins with "Up".
    static func parsePsOutput(_ output: String, runtime: ContainerRuntime) -> [ContainerInfo] {
        output.split(separator: "\n", omittingEmptySubsequences: false).compactMap { rawLine in
            let line = rawLine.trimmingCharacters(in: .whitespaces)
            guard !line.isEmpty else { return nil }

            let fields = line.components(separatedBy: "|")
            guard fields.count >= 2 else { return nil }

            let name = fields[0].trimmingCharacters(in: .whitespaces)
            guard !name.isEmpty else { return nil }

            let statusText = fields[1].trimmingCharacters(in: .whitespaces)
            let imageRaw = fields.count >= 3 ? fields[2].trimmingCharacters(in: .whitespaces) : ""

            return ContainerInfo(
                name: name,
                statusText: statusText,
                isRunning: statusText.hasPrefix("Up"),
                runtime: runtime,
                image: imageRaw.isEmpty ? nil : imageRaw
            )
        }
    }

    /// Parses `tart list` tabular output:
    ///   Source Name Disk Size State
    ///   local  ci-runner-1  50  20  running
    /// name = 2nd column, state = last column, running == "running".
    static func parseTartList(_ output: String) -> [ContainerInfo] {
        output.split(separator: "\n", omittingEmptySubsequences: false).compactMap { rawLine -> ContainerInfo? in
            let line = rawLine.trimmingCharacters(in: .whitespaces)
            guard !line.isEmpty else { return nil }

            let columns = line.split(whereSeparator: { $0 == " " || $0 == "\t" }).map(String.init)
            // Need at least name + state. Skip the header row.
            guard columns.count >= 2 else { return nil }
            if columns.first == "Source" { return nil }

            let name = columns[1]
            let state = columns[columns.count - 1]

            return ContainerInfo(
                name: name,
                statusText: state,
                isRunning: state == "running",
                runtime: .tart,
                image: nil
            )
        }
    }
}
