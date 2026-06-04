import Foundation
import SwiftUI

/// Fetches latest CI/Release health for a configured set of repos using a
/// fine-grained PAT (via `GitHubService`). Network runs off the main actor;
/// results publish on the main actor. Per-repo failures are isolated.
@MainActor
final class PortfolioCIService: ObservableObject {
    /// Phase 1: hardcoded portfolio. Swap to user config later.
    static let configuredRepos = [
        "Sassy-Dog/velovate",
        "Sassy-Dog/qr-ninja",
        "Sassy-Dog/tailored-tip",
        "Sassy-Dog/what2wear"
    ]

    @Published private(set) var statuses: [RepoCIStatus] = []
    @Published private(set) var isAuthenticated = false
    @Published private(set) var isLoading = false

    private let github: GitHubService
    private var task: Task<Void, Never>?

    init(github: GitHubService = .shared) {
        self.github = github
    }

    func refresh() async {
        // Make sure we're using whatever PAT is in the Keychain.
        github.configureFromKeychain()
        let authed = github.hasToken
        self.isAuthenticated = authed

        guard authed else {
            self.statuses = Self.configuredRepos.map { RepoCIStatus.placeholder(repo: $0) }
            return
        }

        isLoading = true
        var results: [RepoCIStatus] = []
        for repo in Self.configuredRepos {
            results.append(await fetchStatus(for: repo))
        }
        self.statuses = results
        self.isLoading = false
    }

    /// Fetches recent runs for a repo and maps them. Any error yields a
    /// placeholder so one repo failing doesn't break the rest.
    private func fetchStatus(for repo: String) async -> RepoCIStatus {
        let endpoint = "/repos/\(repo)/actions/runs"
        let query = [URLQueryItem(name: "per_page", value: "30")]
        do {
            let data = try await github.getRaw(endpoint: endpoint, queryItems: query)
            let response = try JSONDecoder().decode(WorkflowRunsResponse.self, from: data)
            return PortfolioCIMapping.map(repo: repo, runs: response.workflowRuns)
        } catch {
            appLogger.debug("Portfolio CI: \(repo) fetch failed: \(error.localizedDescription)")
            return RepoCIStatus.placeholder(repo: repo)
        }
    }

    func start(interval: TimeInterval = 120) {
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
