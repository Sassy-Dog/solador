import Foundation

/// Pure mapping from a repo's workflow-runs DTOs to CI health models.
enum PortfolioCIMapping {

    private static let isoStandard = ISO8601DateFormatter()

    private static let isoFractional: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()

    private static func createdDate(_ run: WorkflowRunDTO) -> Date {
        isoStandard.date(from: run.createdAt)
            ?? isoFractional.date(from: run.createdAt)
            ?? Date.distantPast
    }
}

/// One workflow run distilled to what the CI Health panel renders.
struct RunRef: Equatable, Identifiable {
    let runID: Int64
    let title: String          // workflow name, e.g. "CI"
    let context: String        // "main" or the PR/branch name
    let conclusion: RunConclusion?
    let status: RunStatus?
    let startedAt: Date?
    let htmlURL: String

    var id: Int64 { runID }

    var isRunning: Bool {
        switch status {
        case .some(.inProgress), .some(.queued), .some(.requested), .some(.waiting), .some(.pending):
            return true
        default:
            return false
        }
    }

    var isFailed: Bool {
        switch conclusion {
        case .failure, .timedOut, .cancelled, .startupFailure:
            return true
        default:
            return false
        }
    }
}

/// Per-repo CI health: latest main run, latest PR run, and any running runs.
struct RepoCIHealth: Equatable, Identifiable {
    let repo: String           // "owner/name"
    let main: RunRef?
    let lastPR: RunRef?
    let running: [RunRef]
    let reachable: Bool        // false when the repo's runs couldn't be fetched

    var id: String { repo }
    var shortName: String { repo.split(separator: "/").last.map(String.init) ?? repo }

    /// Healthy = reachable and neither main nor lastPR failed. A *running* workflow
    /// does NOT make a repo unhealthy (running is activity, not a problem, and is
    /// shown separately). A nil slot (no run of that type) counts as not-failed.
    /// An UNREACHABLE repo is never healthy — a fetch failure must surface.
    var isHealthy: Bool {
        reachable && !(main?.isFailed ?? false) && !(lastPR?.isFailed ?? false)
    }

    /// A repo whose runs couldn't be fetched (auth/network error).
    static func unreachable(repo: String) -> RepoCIHealth {
        RepoCIHealth(repo: repo, main: nil, lastPR: nil, running: [], reachable: false)
    }
}

extension PortfolioCIMapping {
    /// Categorize a repo's runs into main / lastPR / running. Pure; "newest" is by
    /// createdAt descending. Assumes the default branch is `main`.
    static func health(repo: String, runs: [WorkflowRunDTO]) -> RepoCIHealth {
        let sorted = runs.sorted { createdDate($0) > createdDate($1) }
        let main = sorted.first { $0.event == "push" && $0.headBranch == "main" }.map(ref)
        let lastPR = sorted.first { $0.event == "pull_request" }.map(ref)
        let running = sorted.map(ref).filter { $0.isRunning }
        return RepoCIHealth(repo: repo, main: main, lastPR: lastPR, running: running, reachable: true)
    }

    private static func ref(_ run: WorkflowRunDTO) -> RunRef {
        RunRef(
            runID: run.id,
            title: run.name,
            context: contextLabel(run),
            conclusion: run.conclusion.flatMap { RunConclusion(rawValue: $0) },
            status: RunStatus(rawValue: run.status),
            startedAt: startedDate(run),
            htmlURL: run.htmlUrl
        )
    }

    private static func contextLabel(_ run: WorkflowRunDTO) -> String {
        if run.event == "push" && run.headBranch == "main" { return "main" }
        if run.event == "pull_request" { return run.headBranch ?? "PR" }
        return run.headBranch ?? run.event
    }

    private static func startedDate(_ run: WorkflowRunDTO) -> Date? {
        let s = run.runStartedAt ?? run.createdAt
        return isoStandard.date(from: s) ?? isoFractional.date(from: s)
    }
}
