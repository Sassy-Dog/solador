import Foundation
import SwiftData

/// Owns the editable portfolio repo set. Reads `TrackedRepo` from SwiftData and
/// publishes the enabled `owner/name` slugs that drive the Portfolio CI, CI
/// Runners, and Git/Worktrees panels. Mirrors `RemoteHostsCoordinator`: a thin
/// coordinator over SwiftData that the cockpit services read from.
///
/// Seeds once from `PortfolioRepos.seedSlugs` on first run so an existing user
/// keeps their list and a fresh install starts from the curated set.
@MainActor
final class PortfolioStore: ObservableObject {
    /// Enabled `owner/name` slugs, sorted. Republished whenever the set changes.
    @Published private(set) var slugs: [String] = []

    private let modelContext: ModelContext

    /// Called after the slug set changes so dependent services refresh now rather
    /// than waiting for their next poll. Wired up in `DevCanopyApp`.
    var onChange: (() -> Void)?

    init(modelContext: ModelContext) {
        self.modelContext = modelContext
        seedIfNeeded()
        reload()
    }

    /// Inserts the seed slugs once, only when the store is empty (first run).
    private func seedIfNeeded() {
        let existing = (try? modelContext.fetch(FetchDescriptor<TrackedRepo>())) ?? []
        guard existing.isEmpty else { return }
        for slug in PortfolioRepos.seedSlugs {
            modelContext.insert(TrackedRepo(slug: slug))
        }
        try? modelContext.save()
    }

    /// Refreshes the published `slugs` from the enabled `TrackedRepo` rows.
    func reload() {
        let descriptor = FetchDescriptor<TrackedRepo>(
            predicate: #Predicate { $0.enabled },
            sortBy: [SortDescriptor(\.slug)]
        )
        let tracked = (try? modelContext.fetch(descriptor)) ?? []
        slugs = tracked.map(\.slug)
    }

    /// Adds a repo by `owner/name` slug. No-op on a blank or duplicate slug
    /// (case-insensitive). Returns the inserted repo, or nil if skipped.
    @discardableResult
    func add(slug rawSlug: String) -> TrackedRepo? {
        let slug = rawSlug.trimmingCharacters(in: .whitespacesAndNewlines)
        guard slug.contains("/"), !slug.hasPrefix("/"), !slug.hasSuffix("/") else { return nil }

        let all = (try? modelContext.fetch(FetchDescriptor<TrackedRepo>())) ?? []
        guard !all.contains(where: { $0.slug.caseInsensitiveCompare(slug) == .orderedSame }) else {
            return nil
        }

        let repo = TrackedRepo(slug: slug)
        modelContext.insert(repo)
        try? modelContext.save()
        reload()
        onChange?()
        return repo
    }

    func remove(_ repo: TrackedRepo) {
        modelContext.delete(repo)
        try? modelContext.save()
        reload()
        onChange?()
    }

    func setEnabled(_ enabled: Bool, for repo: TrackedRepo) {
        repo.enabled = enabled
        try? modelContext.save()
        reload()
        onChange?()
    }
}
