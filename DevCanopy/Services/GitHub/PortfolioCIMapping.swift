import Foundation

/// Overall glanceable health of a repo's CI.
enum RepoHealth: Equatable {
    case good       // latest CI succeeded
    case bad        // latest CI failed/cancelled/timed-out
    case running    // latest CI in progress / queued
    case unknown    // no runs, or no token
}

/// Distilled CI/Release status for one configured repo. Pure value type produced
/// by `PortfolioCIMapping` from the GitHub runs DTOs.
struct RepoCIStatus: Equatable, Identifiable {
    let repo: String                    // "owner/name"
    let ciConclusion: RunConclusion?
    let ciStatus: RunStatus?
    let releaseConclusion: RunConclusion?
    let lastRunTitle: String?
    let htmlURL: String?
    let health: RepoHealth

    var id: String { repo }

    /// Last path component of "owner/name".
    var shortName: String {
        repo.split(separator: "/").last.map(String.init) ?? repo
    }

    /// An unauthenticated / unfetched placeholder for a repo.
    static func placeholder(repo: String) -> RepoCIStatus {
        RepoCIStatus(
            repo: repo, ciConclusion: nil, ciStatus: nil, releaseConclusion: nil,
            lastRunTitle: nil, htmlURL: nil, health: .unknown
        )
    }
}

/// Pure mapping from a repo's workflow-runs DTOs to a `RepoCIStatus`.
///
/// "CI" = the latest run that is NOT a release workflow; "Release" = the latest
/// run whose workflow name/event looks release-related. Health is derived from
/// the CI run only (a failing release shouldn't redden the CI dot).
enum PortfolioCIMapping {

    static func map(repo: String, runs: [WorkflowRunDTO]) -> RepoCIStatus {
        guard !runs.isEmpty else { return .placeholder(repo: repo) }

        let sorted = runs.sorted { createdDate($0) > createdDate($1) }

        let latestCI = sorted.first { !isRelease($0) }
        let latestRelease = sorted.first { isRelease($0) }

        let ciConclusion = latestCI?.conclusion.flatMap { RunConclusion(rawValue: $0) }
        let ciStatus = latestCI.flatMap { RunStatus(rawValue: $0.status) }
        let releaseConclusion = latestRelease?.conclusion.flatMap { RunConclusion(rawValue: $0) }

        let health = deriveHealth(status: ciStatus, conclusion: ciConclusion, hasCI: latestCI != nil)

        return RepoCIStatus(
            repo: repo,
            ciConclusion: ciConclusion,
            ciStatus: ciStatus,
            releaseConclusion: releaseConclusion,
            lastRunTitle: latestCI?.displayTitle,
            htmlURL: latestCI?.htmlUrl,
            health: health
        )
    }

    private static func deriveHealth(status: RunStatus?, conclusion: RunConclusion?, hasCI: Bool) -> RepoHealth {
        guard hasCI else { return .unknown }

        switch status {
        case .some(.inProgress), .some(.queued), .some(.requested), .some(.waiting), .some(.pending):
            return .running
        default:
            break
        }

        switch conclusion {
        case .success:
            return .good
        case .failure, .timedOut, .cancelled, .startupFailure:
            return .bad
        case .none:
            // Completed-ish status but no conclusion yet, or still running.
            return .running
        default:
            return .unknown
        }
    }

    /// A run is "release-related" if its workflow/run name or trigger event
    /// looks like a release/publish/deploy.
    private static func isRelease(_ run: WorkflowRunDTO) -> Bool {
        let name = run.name.lowercased()
        if name.contains("release") || name.contains("publish") || name.contains("deploy") {
            return true
        }
        return run.event.lowercased() == "release"
    }

    private static let isoFractional: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()

    private static func createdDate(_ run: WorkflowRunDTO) -> Date {
        ISO8601DateFormatter().date(from: run.createdAt)
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

    var id: String { repo }
    var shortName: String { repo.split(separator: "/").last.map(String.init) ?? repo }

    /// Clean = nothing running and neither main nor lastPR failed.
    var isClean: Bool {
        running.isEmpty && !(main?.isFailed ?? false) && !(lastPR?.isFailed ?? false)
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
        return RepoCIHealth(repo: repo, main: main, lastPR: lastPR, running: running)
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
        return ISO8601DateFormatter().date(from: s) ?? isoFractional.date(from: s)
    }
}
