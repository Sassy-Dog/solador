import Foundation
import SwiftData

/// A repo in the tracked portfolio, persisted so the set can be edited in
/// Settings without recompiling. Drives the Portfolio CI, CI Runners (org), and
/// Git/Worktrees panels. Seeded once on first run from `PortfolioRepos.seedSlugs`;
/// the bare `owner/name` slug is the identity.
@Model
final class TrackedRepo {
    /// `owner/name`, e.g. `Sassy-Dog/velovate`.
    var slug: String = ""
    var enabled: Bool = true
    var createdAt: Date = Date()

    init(slug: String, enabled: Bool = true) {
        self.slug = slug
        self.enabled = enabled
        createdAt = Date()
    }

    /// Repo name after the slash, e.g. `velovate`.
    var name: String {
        String(slug.split(separator: "/").last ?? "")
    }
}
