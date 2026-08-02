import Foundation

// MARK: - Metric names

/// The exact `metrics` names the Neon consumption endpoint understands. Only the
/// three this panel renders are listed; the endpoint exposes more (instant restore,
/// snapshots, network transfer, extra branches) that we deliberately don't request,
/// keeping the response small and the fold unambiguous.
enum NeonMetric {
    /// Compute burn, in compute-unit seconds. Rendered as CU-hours (`/3600`).
    static let computeUnitSeconds = "compute_unit_seconds"
    /// Storage held by root branches, in byte-months over the queried window.
    static let rootBranchBytesMonth = "root_branch_bytes_month"
    /// Storage held by child branches, in byte-months over the queried window.
    static let childBranchBytesMonth = "child_branch_bytes_month"

    /// The comma-separated `metrics` query parameter this app sends.
    static let requested = [computeUnitSeconds, rootBranchBytesMonth, childBranchBytesMonth]
}

// MARK: - Summary

/// Month-to-date Neon consumption for one organization, folded across every project
/// the API key can see.
///
/// Both figures are optional on purpose. Neon omits a metric from the response when
/// its value is zero *for a timeframe it did return* — that's a real zero. But it
/// returns no timeframes at all when the org has no projects, or when the account's
/// plan doesn't include consumption history. That second case is genuinely unknown,
/// and the panel must render `—` for it rather than a fabricated 0 (the project's
/// no-fake-numbers rule).
struct NeonUsageSummary: Equatable {
    /// Compute burn over the window, in compute-unit hours. nil when unknown.
    let computeUnitHours: Double?
    /// Branch storage over the window, in GiB (root + child branch bytes). nil when
    /// unknown. Neon reports these as byte-months; with the one-month window this
    /// service queries, that reads as the month's storage footprint.
    let storageGiB: Double?
    /// How many projects contributed consumption timeframes. 0 means nothing was
    /// measured, in which case both figures above are nil.
    let projectCount: Int
}

// MARK: - Wire format

/// `GET /api/v2/consumption_history/v2/projects` — the org-wide consumption history
/// response. Nested arrays are decoded leniently (optional, coalesced to empty when
/// folded) because this is an external contract we don't control; a project with no
/// billing period yet must not fail the whole decode.
struct NeonConsumptionResponse: Decodable, Equatable {
    let projects: [NeonConsumptionProject]?
}

struct NeonConsumptionProject: Decodable, Equatable {
    let projectID: String?
    let periods: [NeonConsumptionPeriod]?

    enum CodingKeys: String, CodingKey {
        case projectID = "project_id"
        case periods
    }
}

struct NeonConsumptionPeriod: Decodable, Equatable {
    let consumption: [NeonConsumptionEntry]?
}

struct NeonConsumptionEntry: Decodable, Equatable {
    let timeframeStart: String?
    let timeframeEnd: String?
    let metrics: [NeonConsumptionMetric]?

    enum CodingKeys: String, CodingKey {
        case timeframeStart = "timeframe_start"
        case timeframeEnd = "timeframe_end"
        case metrics
    }
}

struct NeonConsumptionMetric: Decodable, Equatable {
    let metricName: String
    let value: Double

    enum CodingKeys: String, CodingKey {
        case metricName = "metric_name"
        case value
    }
}

// MARK: - Errors

/// Failures from reading Neon consumption. `isAuthFailure` is the signal the panel
/// turns into a "paste a new key" prompt — a revoked or mistyped org API key answers
/// with 401 / 403.
enum NeonUsageError: LocalizedError, Equatable {
    case missingOrgID
    case invalidURL
    case invalidResponse
    case http(status: Int, body: String?)
    case decodingFailed(String)

    var errorDescription: String? {
        switch self {
        case .missingOrgID:
            "Add your Neon org ID in Settings"
        case .invalidURL:
            "Invalid Neon API URL — check the org ID in Settings"
        case .invalidResponse:
            "Invalid response from the Neon API"
        case let .http(status, _):
            "Neon API request failed (HTTP \(status))"
        case let .decodingFailed(detail):
            "Couldn't read the Neon response (\(detail))"
        }
    }

    /// True when the failure looks like a bad/revoked API key.
    var isAuthFailure: Bool {
        guard case let .http(status, _) = self else { return false }
        return status == 401 || status == 403
    }
}

// MARK: - Window + folding

/// Pure, network-free helpers for the Neon consumption read: the month-to-date
/// window, RFC 3339 formatting, and the fold from the wire response to the summary.
/// Kept free of I/O so it is fully unit-testable.
enum NeonUsageMapping {
    static let secondsPerHour = 3600.0
    static let bytesPerGiB = 1024.0 * 1024.0 * 1024.0

    /// The month-to-date window: first instant of the UTC calendar month containing
    /// `now`, through the first instant of the following month. UTC because Neon
    /// bills and buckets in UTC — using local time would slide the boundary by the
    /// timezone offset.
    ///
    /// `to` is the month's *exclusive end*, never `now`: under monthly granularity
    /// the endpoint floors both bounds to the bucket boundary, so a mid-month `to`
    /// comes back equal to `from` and the whole read fails with an HTTP 400 — on
    /// every day of the month. A future `to` is fine; the returned timeframe still
    /// only covers what was consumed.
    static func monthToDateWindow(now: Date) -> (from: Date, to: Date) {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        let start = calendar.date(from: calendar.dateComponents([.year, .month], from: now)) ?? now
        let end = calendar.date(byAdding: .month, value: 1, to: start) ?? now
        return (from: start, to: end)
    }

    /// RFC 3339 in UTC, e.g. `2026-07-01T00:00:00Z` — the format the consumption
    /// endpoint's `from`/`to` parameters require.
    static func rfc3339(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        formatter.timeZone = TimeZone(identifier: "UTC")
        return formatter.string(from: date)
    }

    /// Fold a consumption_history response into the panel's summary: sum
    /// `compute_unit_seconds` (→ CU-hours) and root + child branch bytes (→ GiB)
    /// across every project and timeframe returned.
    ///
    /// A response carrying no consumption timeframes at all yields nil figures — the
    /// `—` path. That's the plan-gated / no-projects case, and folding it to 0 would
    /// be a fabricated number. A metric *missing from a timeframe that was returned*
    /// is a documented real zero and simply contributes nothing to the sum.
    static func summarize(_ response: NeonConsumptionResponse) -> NeonUsageSummary {
        var computeUnitSeconds = 0.0
        var storageBytes = 0.0
        var projectCount = 0

        for project in response.projects ?? [] {
            var sawTimeframe = false
            for period in project.periods ?? [] {
                for entry in period.consumption ?? [] {
                    sawTimeframe = true
                    for metric in entry.metrics ?? [] {
                        switch metric.metricName {
                        case NeonMetric.computeUnitSeconds:
                            computeUnitSeconds += metric.value
                        case NeonMetric.rootBranchBytesMonth, NeonMetric.childBranchBytesMonth:
                            storageBytes += metric.value
                        default:
                            // A metric we didn't ask for — ignore rather than guess
                            // which bucket it belongs in.
                            break
                        }
                    }
                }
            }
            if sawTimeframe { projectCount += 1 }
        }

        guard projectCount > 0 else {
            return NeonUsageSummary(computeUnitHours: nil, storageGiB: nil, projectCount: 0)
        }
        return NeonUsageSummary(
            computeUnitHours: computeUnitSeconds / secondsPerHour,
            storageGiB: storageBytes / bytesPerGiB,
            projectCount: projectCount
        )
    }
}
