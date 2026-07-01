import Foundation

/// Pure mapping from a repo's workflow-runs DTOs to workflow health models.
enum GHWorkflowsMapping {
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

/// One workflow run distilled to what the GitHub Workflows panel renders.
struct RunRef: Equatable, Identifiable {
    let runID: Int64
    let title: String // workflow name, e.g. "CI"
    let context: String // "main" or the PR/branch name
    let conclusion: RunConclusion?
    let status: RunStatus?
    let startedAt: Date?
    let htmlURL: String

    var id: Int64 {
        runID
    }

    /// A `.pending`/`.queued` run older than this with no jobs dispatched reads as
    /// STUCK/BLOCKED rather than a healthy long-running job (the 17h51m incident).
    static let stuckThreshold: TimeInterval = 3600 // 1 hour

    /// Parked at a deployment-protection gate (manual approval or wait timer).
    /// GitHub's `status == "waiting"` is the precise signal a human must act.
    var needsApproval: Bool {
        status == .waiting
    }

    /// Queued/pending past the staleness threshold — concurrency-blocked or
    /// stuck-queued, not actually executing. `now` is injectable for testing.
    func isStuck(now: Date = Date()) -> Bool {
        switch status {
        case .some(.queued), .some(.pending), .some(.requested):
            guard let startedAt else { return false }
            return now.timeIntervalSince(startedAt) > Self.stuckThreshold
        default:
            return false
        }
    }

    /// Actively executing (or freshly queued) — excludes the `.waiting` approval
    /// gate and excludes pending/queued runs that have gone stale (`isStuck`).
    /// Those render in their own NEEDS APPROVAL / STUCK sections.
    func isRunning(now: Date = Date()) -> Bool {
        switch status {
        case .some(.inProgress):
            true
        case .some(.queued), .some(.requested), .some(.pending):
            !isStuck(now: now)
        default:
            // .waiting → needs approval (not running); .completed/.none → not running
            false
        }
    }

    /// Default-clock convenience for callers that don't inject a clock.
    var isRunning: Bool {
        isRunning(now: Date())
    }

    var isFailed: Bool {
        switch conclusion {
        case .failure, .timedOut, .cancelled, .startupFailure:
            true
        default:
            false
        }
    }
}

/// Per-repo workflow health: latest main run, latest PR run, and any running runs.
struct RepoWorkflowHealth: Equatable, Identifiable {
    let repo: String // "owner/name"
    let main: RunRef?
    let lastPR: RunRef?
    let running: [RunRef] // actively executing (excludes waiting + stuck)
    let needsApproval: [RunRef] // parked at a deployment-protection gate (.waiting)
    let stuck: [RunRef] // queued/pending past the staleness threshold
    let reachable: Bool // false when the repo's runs couldn't be fetched
    let remoteBranches: Int? // branch count from the GitHub API; nil if unknown/fetch failed
    let openIssues: Int? // open issues (PRs excluded); nil if unknown/fetch failed
    let openPRs: Int? // open pull requests; nil if unknown/fetch failed

    var id: String {
        repo
    }

    var shortName: String {
        repo.split(separator: "/").last.map(String.init) ?? repo
    }

    /// Healthy = reachable and neither main nor lastPR failed. A *running* workflow
    /// does NOT make a repo unhealthy (running is activity, not a problem, and is
    /// shown separately). A nil slot (no run of that type) counts as not-failed.
    /// An UNREACHABLE repo is never healthy — a fetch failure must surface.
    var isHealthy: Bool {
        reachable && !(main?.isFailed ?? false) && !(lastPR?.isFailed ?? false)
    }

    /// A repo whose runs couldn't be fetched (auth/network error).
    static func unreachable(repo: String) -> RepoWorkflowHealth {
        RepoWorkflowHealth(
            repo: repo,
            main: nil,
            lastPR: nil,
            running: [],
            needsApproval: [],
            stuck: [],
            reachable: false,
            remoteBranches: nil,
            openIssues: nil,
            openPRs: nil
        )
    }
}

extension GHWorkflowsMapping {
    /// Pure open-issue count. GitHub's issues endpoint counts pull requests as
    /// issues, so the caller passes the inclusive total and the open-PR count and
    /// this subtracts them. Returns nil when either input is unknown (a failed
    /// fetch), and clamps at 0 so a transient issues/PRs sampling skew never shows
    /// a negative.
    static func pureOpenIssueCount(openIssuesIncludingPRs: Int?, openPullRequests: Int?) -> Int? {
        guard let inclusive = openIssuesIncludingPRs, let prs = openPullRequests else { return nil }
        return max(0, inclusive - prs)
    }

    /// Default branch assumed for the repo "main" health slot.
    private static let defaultBranch = "main"

    /// Categorize a repo's runs into main / lastPR / running. Pure; "newest" is by
    /// createdAt descending. Assumes the default branch is `main`.
    ///
    /// `watchedWorkflows` is an optional per-repo opt-in list of workflows that
    /// should count toward repo health beyond the default `push`/`pull_request`
    /// view (issue #76). nil/empty preserves the legacy behavior:
    ///   - `main` = newest `push` run on the default branch.
    ///   - `lastPR` = newest `pull_request` run.
    ///
    /// When non-empty, runs are first filtered to the watched workflows (matched
    /// against `WorkflowRunDTO.name` — the workflow's display name, e.g. "Release";
    /// entries may be given as a filename like `release.yml`/`release.yaml` or the
    /// display name, matched case-insensitively). The event gate is then *relaxed*
    /// so any watched run on the default branch — `workflow_run`, `schedule`,
    /// `workflow_dispatch`, or `push` — counts toward the `main` slot. This is the
    /// silent-failure fix: a `release.yml` run fires on `workflow_run`, which the
    /// legacy `event == "push"` gate dropped on the floor.
    static func health(
        repo: String,
        runs: [WorkflowRunDTO],
        watchedWorkflows: [String]? = nil,
        remoteBranches: Int? = nil,
        openIssuesIncludingPRs: Int? = nil,
        openPullRequests: Int? = nil,
        now: Date = Date()
    ) -> RepoWorkflowHealth {
        let watched = normalizedWatched(watchedWorkflows)
        let scoped = watched.isEmpty ? runs : runs.filter { matchesWatched($0, watched) }
        let sorted = scoped.sorted { createdDate($0) > createdDate($1) }

        let main = sorted.first { isDefaultBranchHealth($0, watchScoped: !watched.isEmpty) }.map(ref)
        let lastPR = sorted.first { $0.event == "pull_request" }.map(ref)
        let refs = sorted.map(ref)
        let running = refs.filter { $0.isRunning(now: now) }
        let needsApproval = refs.filter(\.needsApproval)
        let stuck = refs.filter { $0.isStuck(now: now) }
        return RepoWorkflowHealth(
            repo: repo,
            main: main,
            lastPR: lastPR,
            running: running,
            needsApproval: needsApproval,
            stuck: stuck,
            reachable: true,
            remoteBranches: remoteBranches,
            openIssues: pureOpenIssueCount(
                openIssuesIncludingPRs: openIssuesIncludingPRs,
                openPullRequests: openPullRequests
            ),
            openPRs: openPullRequests
        )
    }

    /// Whether a run feeds the `main` health slot. Default behavior is strictly
    /// `push` on the default branch. When the caller has scoped to watched
    /// workflows, the event gate is relaxed to any non-PR run on the default
    /// branch (`workflow_run`/`schedule`/`workflow_dispatch`/`push`) so a release
    /// pipeline's failure surfaces.
    private static func isDefaultBranchHealth(_ run: WorkflowRunDTO, watchScoped: Bool) -> Bool {
        guard run.headBranch == defaultBranch else { return false }
        if watchScoped { return run.event != "pull_request" }
        return run.event == "push"
    }

    /// Lowercased, extension-stripped, de-blanked watched-workflow keys.
    private static func normalizedWatched(_ watched: [String]?) -> Set<String> {
        guard let watched else { return [] }
        return Set(watched.map(normalizeWorkflowKey).filter { !$0.isEmpty })
    }

    /// Normalize a workflow identifier for matching: trim, drop a trailing
    /// `.yml`/`.yaml`, lowercase. So `release.yml`, `Release.yaml`, and `Release`
    /// all collapse to `release`.
    private static func normalizeWorkflowKey(_ s: String) -> String {
        var key = s.trimmingCharacters(in: .whitespacesAndNewlines)
        for ext in [".yml", ".yaml"] where key.lowercased().hasSuffix(ext) {
            key = String(key.dropLast(ext.count))
            break
        }
        return key.lowercased()
    }

    /// True when a run's display name matches any watched key.
    private static func matchesWatched(_ run: WorkflowRunDTO, _ watched: Set<String>) -> Bool {
        watched.contains(normalizeWorkflowKey(run.name))
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
        if run.event == "push", run.headBranch == "main" { return "main" }
        if run.event == "pull_request" { return run.headBranch ?? "PR" }
        return run.headBranch ?? run.event
    }

    private static func startedDate(_ run: WorkflowRunDTO) -> Date? {
        let s = run.runStartedAt ?? run.createdAt
        return isoStandard.date(from: s) ?? isoFractional.date(from: s)
    }
}
