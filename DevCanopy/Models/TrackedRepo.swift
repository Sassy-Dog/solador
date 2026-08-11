import Foundation
import SwiftData

/// A repo in the tracked portfolio, persisted so the set can be edited in
/// Settings without recompiling. Drives the GitHub Workflows, GitHub Runners (org), and
/// Git/Worktrees panels. Seeded once on first run from `PortfolioRepos.seedSlugs`;
/// the bare `owner/name` slug is the identity.
@Model
final class TrackedRepo {
    /// `owner/name`, e.g. `acme/gadget`.
    var slug: String = ""
    var enabled: Bool = true
    var createdAt: Date = Date()

    /// Workflows (beyond the default `ci.yml` / push+PR view) whose failures
    /// should turn this repo's health panel red (issue #76). Each entry is a
    /// workflow display name (e.g. `Release`) or filename (e.g. `release.yml`);
    /// matching is case-insensitive and ignores the `.yml`/`.yaml` extension.
    /// nil/empty preserves the legacy behavior (push-on-main + PR runs only).
    /// Optional with no default so existing SwiftData stores open without a
    /// migration.
    var watchedWorkflows: [String]?

    init(slug: String, enabled: Bool = true, watchedWorkflows: [String]? = nil) {
        self.slug = slug
        self.enabled = enabled
        self.watchedWorkflows = watchedWorkflows
        createdAt = Date()
    }

    /// Repo name after the slash, e.g. `gadget`.
    var name: String {
        String(slug.split(separator: "/").last ?? "")
    }
}
