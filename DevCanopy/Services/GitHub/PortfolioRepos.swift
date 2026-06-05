import Foundation

/// Single source of truth for the portfolio Chris actively tracks. Used by the
/// Portfolio CI panel (GitHub slugs), the CI Runners panel (org), and the
/// Git/Worktrees panel (local repo-dir matching). Phase 1: hardcoded; swap to
/// user config later.
enum PortfolioRepos {
    static let org = "Sassy-Dog"

    /// owner/name slugs.
    static let slugs = [
        "Sassy-Dog/velovate",
        "Sassy-Dog/qr-ninja",
        "Sassy-Dog/tailored-tip",
        "Sassy-Dog/what2wear",
        "Sassy-Dog/devcanopy",
        "Sassy-Dog/platform"
    ]

    /// Repo names (after the slash).
    static let names = slugs.map { String($0.split(separator: "/").last ?? "") }

    /// Normalizes a repo/dir name for matching: lowercase, letters+digits only,
    /// so slug `tailored-tip` matches an on-disk dir `tailoredtip`.
    static func normalize(_ s: String) -> String {
        s.lowercased().filter { $0.isLetter || $0.isNumber }
    }

    private static let normalizedNames = Set(names.map(normalize))

    /// True if a local repo directory name belongs to the tracked portfolio.
    static func matches(repoDirName: String) -> Bool {
        normalizedNames.contains(normalize(repoDirName))
    }
}
