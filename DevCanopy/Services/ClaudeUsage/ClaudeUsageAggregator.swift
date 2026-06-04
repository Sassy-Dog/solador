import Foundation

/// Pure aggregation of `[UsageRecord]` into a `UsageSummary`. No file I/O, no
/// clock access — `now` is injected so results are deterministic and testable.
///
/// Pricing is applied **per record by its own model** before summing, so windows
/// that mix models (e.g. opus + sonnet in the same hour) cost out correctly.
enum ClaudeUsageAggregator {

    static func summarize(records: [UsageRecord], now: Date, calendar: Calendar = .current) -> UsageSummary {
        // Dedup by requestId, keeping the first occurrence.
        var seen = Set<String>()
        let deduped = records.filter { seen.insert($0.requestId).inserted }

        let midnight = calendar.startOfDay(for: now)
        let fiveHoursAgo = now.addingTimeInterval(-5 * 3600)
        let sevenDaysAgo = now.addingTimeInterval(-7 * 24 * 3600)

        var today = UsageTotals()
        var last5h = UsageTotals()
        var last7d = UsageTotals()

        var projectTotals: [String: UsageTotals] = [:]
        var modelTotals: [String: UsageTotals] = [:]

        for record in deduped {
            let cost = ModelPricing.pricing(for: record.model).cost(
                input: record.inputTokens,
                output: record.outputTokens,
                cacheWrite: record.cacheCreationTokens,
                cacheRead: record.cacheReadTokens
            )

            if record.timestamp >= midnight {
                today.add(record, cost: cost)
            }
            if record.timestamp >= fiveHoursAgo {
                last5h.add(record, cost: cost)
            }
            if record.timestamp >= sevenDaysAgo {
                last7d.add(record, cost: cost)
                projectTotals[record.project, default: UsageTotals()].add(record, cost: cost)
                modelTotals[record.model, default: UsageTotals()].add(record, cost: cost)
            }
        }

        return UsageSummary(
            today: today,
            last5h: last5h,
            last7d: last7d,
            projectsLast7d: sortedBreakdowns(projectTotals),
            modelsLast7d: sortedBreakdowns(modelTotals)
        )
    }

    /// Breakdowns sorted by cost descending, with name as a stable tiebreaker.
    private static func sortedBreakdowns(_ map: [String: UsageTotals]) -> [UsageBreakdown] {
        map.map { UsageBreakdown(name: $0.key, totals: $0.value) }
            .sorted {
                if $0.totals.costUSD != $1.totals.costUSD {
                    return $0.totals.costUSD > $1.totals.costUSD
                }
                return $0.name < $1.name
            }
    }
}
