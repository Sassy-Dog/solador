import Foundation

/// Decodes GitHub's `GET /orgs/{org}/actions/runners` response.
struct RunnersResponse: Decodable {
    let totalCount: Int
    let runners: [RunnerDTO]

    enum CodingKeys: String, CodingKey {
        case totalCount = "total_count"
        case runners
    }
}

struct RunnerDTO: Decodable {
    let id: Int
    let name: String
    let os: String
    let status: String      // "online" | "offline"
    let busy: Bool
    let labels: [Label]?

    struct Label: Decodable { let name: String }
}

enum RunnerOS: Sendable, Equatable {
    case macOS, linux, other
    var label: String {
        switch self { case .macOS: "macOS"; case .linux: "Linux"; case .other: "Other" }
    }
}

/// Idle/busy/offline — the thing Chris wants to see at a glance (mirrors Mission Control).
enum RunnerState: Sendable, Equatable {
    case idle, busy, offline
    var label: String {
        switch self { case .idle: "idle"; case .busy: "busy"; case .offline: "offline" }
    }
}

struct CIRunner: Sendable, Identifiable, Equatable {
    let id: Int
    let name: String
    let os: RunnerOS
    let state: RunnerState
}

struct RunnerSummary: Sendable, Equatable {
    let total: Int
    let online: Int
    let busy: Int
    let idle: Int
    let macOSOnline: Int
    let macOSTotal: Int
    let linuxOnline: Int
    let linuxTotal: Int
}

enum CIRunnerMapping {
    static func map(dtos: [RunnerDTO]) -> [CIRunner] {
        dtos.map { dto in
            CIRunner(id: dto.id, name: dto.name, os: os(from: dto), state: state(from: dto))
        }
    }

    private static func state(from dto: RunnerDTO) -> RunnerState {
        guard dto.status.lowercased() == "online" else { return .offline }
        return dto.busy ? .busy : .idle
    }

    private static func os(from dto: RunnerDTO) -> RunnerOS {
        let o = dto.os.lowercased()
        if o.contains("mac") || o.contains("darwin") { return .macOS }
        if o.contains("linux") { return .linux }
        return .other
    }

    static func summarize(_ runners: [CIRunner]) -> RunnerSummary {
        let online = runners.filter { $0.state != .offline }
        let mac = runners.filter { $0.os == .macOS }
        let linux = runners.filter { $0.os == .linux }
        return RunnerSummary(
            total: runners.count,
            online: online.count,
            busy: runners.filter { $0.state == .busy }.count,
            idle: runners.filter { $0.state == .idle }.count,
            macOSOnline: mac.filter { $0.state != .offline }.count,
            macOSTotal: mac.count,
            linuxOnline: linux.filter { $0.state != .offline }.count,
            linuxTotal: linux.count
        )
    }
}
