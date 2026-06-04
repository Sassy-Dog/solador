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
