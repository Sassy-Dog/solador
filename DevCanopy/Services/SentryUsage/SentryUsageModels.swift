import Foundation

// MARK: - Query vocabulary

/// The exact `stats_v2` query vocabulary this panel sends, and the one outcome it
/// renders. Sentry's outcome axis splits an org's ingest into `accepted`, `filtered`,
/// `rate_limited`, `invalid`, `dropped`… — only `accepted` counts against quota, so
/// that's the single figure the cockpit shows.
enum SentryStatsQuery {
    /// The aggregate field: event *quantity*, not times-seen.
    static let field = "sum(quantity)"
    /// The data category. Errors only — transactions/attachments bill separately and
    /// folding them together would produce a number that matches nothing in Sentry.
    static let category = "error"
    /// The axis we split on, so `accepted` can be isolated from the other outcomes.
    static let groupBy = "outcome"
    /// The outcome whose total is the headline figure.
    static let acceptedOutcome = "accepted"
    /// Rolling window. `30d` is Sentry's own statsPeriod grammar.
    static let statsPeriod = "30d"
    /// Days in `statsPeriod`, for the panel's label.
    static let windowDays = 30
}

// MARK: - Summary

/// Accepted error events for one Sentry organization over the rolling window.
///
/// `acceptedErrorEvents` is optional on purpose. Sentry returns an outcome group only
/// when that outcome had data in the window, so a *missing* `accepted` group among
/// other groups is a real zero — the org ingested errors, none were accepted. But a
/// response with no groups at all measured nothing (wrong slug, brand-new org, no
/// ingest), which is genuinely unknown: the panel must render `—` there rather than a
/// fabricated 0.
struct SentryUsageSummary: Equatable {
    /// Accepted error events over the window. nil when unknown.
    let acceptedErrorEvents: Int?
    /// How many outcome groups the response carried. 0 means nothing was measured, in
    /// which case `acceptedErrorEvents` is nil.
    let outcomeGroupCount: Int
}

// MARK: - Wire format

/// `GET /api/0/organizations/{org}/stats_v2/` — the org event-count response.
/// Every nested container is optional because this is an external contract we don't
/// control; a response without groups must decode cleanly and fold to "unknown".
struct SentryStatsResponse: Decodable, Equatable {
    let start: String?
    let end: String?
    let groups: [SentryStatsGroup]?
}

/// One `groupBy` bucket. `by` carries the axis values (here `{"outcome": "accepted"}`)
/// and `totals` the aggregates keyed by the requested field name.
///
/// Both dictionaries are typed narrowly for the one query this app sends: `outcome`
/// values are strings, and `sum(quantity)` is numeric. Decoding `totals` as `Double`
/// (rather than `Int`) tolerates a JSON number written with a fractional part; the
/// fold rounds it back to a whole event count.
struct SentryStatsGroup: Decodable, Equatable {
    let by: [String: String]?
    let totals: [String: Double]?
}

// MARK: - Errors

/// Failures from reading Sentry org stats. `isAuthFailure` is the signal the panel
/// turns into a "paste a new token" prompt — a revoked or under-scoped token answers
/// with 401 / 403.
enum SentryUsageError: LocalizedError, Equatable {
    case missingOrgSlug
    case invalidURL
    case invalidResponse
    case http(status: Int, body: String?)
    case decodingFailed(String)

    var errorDescription: String? {
        switch self {
        case .missingOrgSlug:
            "Add your Sentry org slug in Settings"
        case .invalidURL:
            "Invalid Sentry API URL — check the org slug in Settings"
        case .invalidResponse:
            "Invalid response from the Sentry API"
        case let .http(status, _):
            "Sentry API request failed (HTTP \(status))"
        case let .decodingFailed(detail):
            "Couldn't read the Sentry response (\(detail))"
        }
    }

    /// True when the failure looks like a bad, revoked, or under-scoped token.
    var isAuthFailure: Bool {
        guard case let .http(status, _) = self else { return false }
        return status == 401 || status == 403
    }
}

// MARK: - Folding

/// Pure, network-free fold from the `stats_v2` wire response to the panel's summary.
/// Kept free of I/O so it is fully unit-testable.
enum SentryUsageMapping {
    /// Pick the `accepted` outcome's `sum(quantity)` total out of the grouped response.
    ///
    /// A response with no groups yields nil — the `—` path, because nothing was
    /// measured. A response *with* groups but no `accepted` bucket folds to 0: Sentry
    /// measured the window and accepted nothing, which is a real zero rather than a
    /// guess.
    static func summarize(_ response: SentryStatsResponse) -> SentryUsageSummary {
        let groups = response.groups ?? []
        guard !groups.isEmpty else {
            return SentryUsageSummary(acceptedErrorEvents: nil, outcomeGroupCount: 0)
        }

        var accepted = 0.0
        for group in groups where group.by?[SentryStatsQuery.groupBy] == SentryStatsQuery.acceptedOutcome {
            accepted += group.totals?[SentryStatsQuery.field] ?? 0
        }
        return SentryUsageSummary(
            acceptedErrorEvents: Int(accepted.rounded()),
            outcomeGroupCount: groups.count
        )
    }
}
