import Foundation

/// Pure helpers + first-run seed for the tracked portfolio. The live, editable
/// set now lives in SwiftData (`TrackedRepo`) and is owned by `PortfolioStore`;
/// this enum only provides the one-time seed and the slug/name normalization used
/// to match an on-disk repo directory against a tracked slug.
enum PortfolioRepos {
    static let org = "Sassy-Dog"

    /// The list used to seed `TrackedRepo` once, on first run. After that the set
    /// is user-editable in Settings; changing this array does not retro-edit an
    /// already-seeded store.
    static let seedSlugs = [
        "Sassy-Dog/velovate",
        "Sassy-Dog/qr-ninja",
        "Sassy-Dog/tailoredtip",
        "Sassy-Dog/what2wear",
        "Sassy-Dog/devcanopy",
        "Sassy-Dog/platform"
    ]

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
