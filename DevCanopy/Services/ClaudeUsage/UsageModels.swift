import Foundation

/// One billable API call distilled from a Claude Code log line. Pure value type;
/// no file I/O. Dedup is keyed on `requestId`.
struct UsageRecord: Equatable {
    let timestamp: Date
    let requestId: String
    let model: String
    let project: String
    let inputTokens: Int
    let outputTokens: Int
    let cacheCreationTokens: Int
    let cacheReadTokens: Int
}

/// Summed token counts plus computed cost for a window or breakdown bucket.
struct UsageTotals: Equatable {
    var inputTokens: Int = 0
    var outputTokens: Int = 0
    var cacheCreationTokens: Int = 0
    var cacheReadTokens: Int = 0
    var costUSD: Double = 0

    /// All token kinds combined (input + output + cache write + cache read).
    var totalTokens: Int {
        inputTokens + outputTokens + cacheCreationTokens + cacheReadTokens
    }

    /// Folds one already-priced record into the running totals.
    mutating func add(_ record: UsageRecord, cost: Double) {
        inputTokens += record.inputTokens
        outputTokens += record.outputTokens
        cacheCreationTokens += record.cacheCreationTokens
        cacheReadTokens += record.cacheReadTokens
        costUSD += cost
    }
}

/// A named breakdown bucket (project or model) with its totals.
struct UsageBreakdown: Equatable, Identifiable {
    let name: String
    let totals: UsageTotals
    var id: String {
        name
    }
}

/// The aggregate view the panel renders.
struct UsageSummary: Equatable {
    var today: UsageTotals = .init()
    var last5h: UsageTotals = .init()
    var last7d: UsageTotals = .init()
    var projectsLast7d: [UsageBreakdown] = []
    var modelsLast7d: [UsageBreakdown] = []
}
