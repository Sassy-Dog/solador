import Foundation
import SwiftUI

/// Fetches per-repo CI health (main / last-PR / running) for the curated repo set
/// using a fine-grained PAT (via `GitHubService`). Network off the main actor;
/// results publish on the main actor. Per-repo failures are isolated.
@MainActor
final class PortfolioCIService: ObservableObject {
    /// The tracked portfolio (shared source of truth with runners + worktrees).
    static let configuredRepos = PortfolioRepos.slugs

    @Published private(set) var health: [RepoCIHealth] = []
    @Published private(set) var isAuthenticated = false
    @Published private(set) var isLoading = false

    private let github: GitHubService
    private var task: Task<Void, Never>?

    init(github: GitHubService = .shared) {
        self.github = github
    }

    func refresh() async {
        github.configureFromKeychain()
        let authed = github.hasToken
        self.isAuthenticated = authed

        guard authed else {
            self.health = []
            return
        }

        isLoading = true
        var results: [RepoCIHealth] = []
        for repo in Self.configuredRepos {
            results.append(await fetchHealth(for: repo))
        }
        self.health = results
        self.isLoading = false
    }

    /// Fetches recent runs for a repo and categorizes them. Any error yields an
    /// empty (clean) health so one repo failing doesn't break the rest.
    private func fetchHealth(for repo: String) async -> RepoCIHealth {
        let endpoint = "/repos/\(repo)/actions/runs"
        let query = [URLQueryItem(name: "per_page", value: "30")]
        do {
            let data = try await github.getRaw(endpoint: endpoint, queryItems: query)
            let response = try JSONDecoder().decode(WorkflowRunsResponse.self, from: data)
            return PortfolioCIMapping.health(repo: repo, runs: response.workflowRuns)
        } catch {
            appLogger.debug("CI Health: \(repo) fetch failed: \(error.localizedDescription)")
            return RepoCIHealth(repo: repo, main: nil, lastPR: nil, running: [])
        }
    }

    func start(interval: TimeInterval = 60) {
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
}
