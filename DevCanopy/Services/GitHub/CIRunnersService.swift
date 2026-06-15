import Foundation
import SwiftUI

/// Fetches all self-hosted runners registered with the GitHub org and their
/// idle/busy/offline state — the authoritative cross-host view (a runner on any
/// machine shows here), mirroring Mission Control's CI Runners card.
@MainActor
final class CIRunnersService: ObservableObject {
    @Published private(set) var runners: [CIRunner] = []
    @Published private(set) var summary: RunnerSummary?
    @Published private(set) var isAuthenticated = false
    @Published private(set) var loadError: String?
    @Published private(set) var lastUpdated: Date?

    private let github: GitHubService
    private var task: Task<Void, Never>?

    init(github: GitHubService = .shared) {
        self.github = github
    }

    func refresh() async {
        github.configureFromKeychain()
        isAuthenticated = github.hasToken
        guard isAuthenticated else {
            runners = []
            summary = nil
            loadError = nil
            return
        }
        do {
            let data = try await github.getRaw(
                endpoint: "/orgs/\(PortfolioRepos.org)/actions/runners",
                queryItems: [URLQueryItem(name: "per_page", value: "100")]
            )
            let resp = try JSONDecoder().decode(RunnersResponse.self, from: data)
            let mapped = CIRunnerMapping.map(dtos: resp.runners).sorted(by: Self.order)
            runners = mapped
            summary = CIRunnerMapping.summarize(mapped)
            loadError = nil
            lastUpdated = Date()
        } catch {
            // Most likely the PAT lacks org self-hosted-runners (read) permission.
            loadError = "couldn't read runners — token needs org self-hosted runners (read)"
            appLogger.debug("CI runners fetch failed: \(error.localizedDescription)")
        }
    }

    /// macOS first, then Linux, then by name.
    private static func order(_ a: CIRunner, _ b: CIRunner) -> Bool {
        func rank(_ os: RunnerOS) -> Int {
            switch os
            { case .macOS: 0
            case .linux: 1
            case .other: 2 }
        }
        if rank(a.os) != rank(b.os) { return rank(a.os) < rank(b.os) }
        return a.name.localizedStandardCompare(b.name) == .orderedAscending
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
