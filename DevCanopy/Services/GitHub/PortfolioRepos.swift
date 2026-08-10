import Foundation

/// Pure helpers + first-run seed for the tracked portfolio. The live, editable
/// set now lives in SwiftData (`TrackedRepo`) and is owned by `PortfolioStore`;
/// this enum only provides the one-time seed and the slug/name normalization used
/// to match an on-disk repo directory against a tracked slug.
enum PortfolioRepos {
    /// The organization `GHRunnersService` queries for self-hosted runners.
    ///
    /// Retained only because this app is frozen and still has to compile. The
    /// cross-platform app replaced this constant with a `github_org` **setting**
    /// (`Settings.github_org`): hardcoding it meant every install queried one
    /// particular organization's runners, so the Runners panel could only ever
    /// work for its author. Do not copy this pattern forward.
    static let org = "Sassy-Dog"

    /// Empty on purpose: a portfolio is per-operator, so there is no defensible
    /// default. Shipping one author's repositories meant every other user's
    /// first launch opened on rows of 404s against repos they cannot read.
    ///
    /// The seed-once contract still holds — a store that exists is never
    /// re-seeded — which is what would stop a future non-empty seed being
    /// retro-applied to somebody's saved portfolio.
    static let seedSlugs: [String] = []

    /// Normalizes a repo/dir name for matching: lowercase, letters+digits only,
    /// so slug `tailored-tip` matches an on-disk dir `tailoredtip`.
    static func normalize(_ s: String) -> String {
        s.lowercased().filter { $0.isLetter || $0.isNumber }
    }

    /// Repo names (after the slash) for a set of `owner/name` slugs.
    static func names(from slugs: [String]) -> [String] {
        slugs.map { String($0.split(separator: "/").last ?? "") }
    }

    /// True if `repoDirName` matches one of the tracked `slugs` after normalization.
    static func matches(repoDirName: String, in slugs: [String]) -> Bool {
        let normalized = Set(names(from: slugs).map(normalize))
        return normalized.contains(normalize(repoDirName))
    }
}
