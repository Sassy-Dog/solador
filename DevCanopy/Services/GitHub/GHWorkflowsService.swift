import Foundation
import SwiftUI

/// Fetches per-repo workflow health (main / last-PR / running) for the curated repo set
/// using a fine-grained PAT (via `GitHubService`). Network off the main actor;
/// results publish on the main actor. Per-repo failures are isolated.
@MainActor
final class GHWorkflowsService: ObservableObject {
    @Published private(set) var health: [RepoWorkflowHealth] = []
    @Published private(set) var isAuthenticated = false
    @Published private(set) var isLoading = false

    /// The tracked portfolio slugs to fetch. Supplied by `PortfolioStore` so the
    /// set stays user-editable; defaults to the first-run seed for previews/tests.
    var slugsProvider: () -> [String] = { PortfolioRepos.seedSlugs }

    /// Per-repo watched workflows keyed by slug (issue #76). Supplied by
    /// `PortfolioStore`; a missing/empty entry preserves the legacy push+PR view
    /// for that repo. Defaults to none for previews/tests.
    var watchedProvider: () -> [String: [String]] = { [:] }

    private let github: GitHubService
    private var task: Task<Void, Never>?

    /// Notification delivery for CI events (needs-approval). Injectable for tests.
    private let notifier: WorkflowNotificationService?
    /// Display preferences gating which notifications fire. There is no per-repo
    /// UI for these yet, so the defaults apply (notifyOnApprovalNeeded == true).
    private let displayOptions: WorkflowDisplayOptions
    /// Run IDs already seen in `.waiting` — used to fire the approval notification
    /// once on the transition *into* waiting, not on every poll while it stays.
    private var knownApprovalRunIDs: Set<Int64> = []
    /// Suppress notifications on the very first refresh so we don't alert for runs
    /// that were already parked before the app launched.
    private var didInitialRefresh = false

    /// `nil` defaults (not `.shared`) because default arguments are evaluated in a
    /// nonisolated context, where the `@MainActor` `shared` singletons can't be
    /// referenced; resolved here in the main-actor init body. (Disabling
    /// notifications is done via `displayOptions`, not a nil notifier.)
    init(
        github: GitHubService? = nil,
        notifier: WorkflowNotificationService? = nil,
        displayOptions: WorkflowDisplayOptions = WorkflowDisplayOptions()
    ) {
        self.github = github ?? .shared
        self.notifier = notifier ?? .shared
        self.displayOptions = displayOptions
    }

    func refresh() async {
        github.configureFromKeychain()
        let authed = github.hasToken
        isAuthenticated = authed

        guard authed else {
            health = []
            return
        }

        isLoading = true
        var results: [RepoWorkflowHealth] = []
        for repo in slugsProvider() {
            await results.append(fetchHealth(for: repo))
        }
        health = results
        isLoading = false
        notifyApprovalTransitions(in: results)
    }

    /// Fire a needs-approval notification once per run, on its transition *into*
    /// the `.waiting` gate. Diffs the current waiting set against what we've seen
    /// so re-polling a still-waiting run doesn't re-notify. The first refresh only
    /// seeds the baseline (no alert for runs already parked before launch).
    private func notifyApprovalTransitions(in results: [RepoWorkflowHealth]) {
        let currentWaiting = results.flatMap { h in h.needsApproval.map { ($0, h.shortName) } }
        let currentIDs = Set(currentWaiting.map(\.0.runID))

        defer {
            knownApprovalRunIDs = currentIDs
            didInitialRefresh = true
        }

        guard didInitialRefresh, displayOptions.notifyOnApprovalNeeded else { return }

        for (ref, shortName) in currentWaiting where !knownApprovalRunIDs.contains(ref.runID) {
            notifier?.notifyNeedsApproval(
                repo: shortName,
                title: ref.title,
                context: ref.context,
                htmlURL: ref.htmlURL
            )
        }
    }

    /// Fetches recent runs for a repo and categorizes them. Any error yields an
    /// empty (clean) health so one repo failing doesn't break the rest.
    private func fetchHealth(for repo: String) async -> RepoWorkflowHealth {
        let endpoint = "/repos/\(repo)/actions/runs"
        let query = [URLQueryItem(name: "per_page", value: "30")]
        let watched = watchedProvider()[repo]
        do {
            let data = try await github.getRaw(endpoint: endpoint, queryItems: query)
            let response = try JSONDecoder().decode(WorkflowRunsResponse.self, from: data)
            // Best-effort: a branch-count failure shouldn't mark the repo unreachable
            // (its runs decoded fine). nil renders as "—" in the panel.
            let branches = try? await github.branchCount(for: repo)
            return GHWorkflowsMapping.health(
                repo: repo,
                runs: response.workflowRuns,
                watchedWorkflows: watched,
                remoteBranches: branches
            )
        } catch {
            appLogger.debug("GitHub Workflows: \(repo) fetch failed: \(error.localizedDescription)")
            return .unreachable(repo: repo)
        }
    }

    func start(interval: TimeInterval = 60) {
        guard task == nil else { return }
        task = Task { [weak self] in
            guard let self else { return }
            await refresh()
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: UInt64(interval * 1_000_000_000))
                if Task.isCancelled { break }
                await refresh()
            }
        }
    }

    func stop() {
        task?.cancel()
        task = nil
    }

    /// Stops the current polling loop and restarts it on a new cadence. Used
    /// when the user changes the Refresh Interval setting.
    func restart(interval: TimeInterval) {
        stop()
        start(interval: interval)
    }

    deinit { task?.cancel() }
}
