import XCTest
@testable import DevCanopy

final class ClaudeUsageAggregatorTests: XCTestCase {

    // A fixed reference "now": 2026-05-29 12:00:00 UTC.
    private let now = Date(timeIntervalSince1970: 1_780_142_400) // 2026-05-29T12:00:00Z

    private func record(
        offsetHours: Double = 0,
        requestId: String = "req_1",
        model: String = "claude-sonnet-4-5",
        project: String = "velovate",
        input: Int = 0,
        output: Int = 0,
        cacheWrite: Int = 0,
        cacheRead: Int = 0
    ) -> UsageRecord {
        UsageRecord(
            timestamp: now.addingTimeInterval(-offsetHours * 3600),
            requestId: requestId,
            model: model,
            project: project,
            inputTokens: input,
            outputTokens: output,
            cacheCreationTokens: cacheWrite,
            cacheReadTokens: cacheRead
        )
    }

    // MARK: - Dedup

    func testDedupDropsDuplicateRequestId() {
        let records = [
            record(requestId: "req_a", input: 100, output: 10),
            record(requestId: "req_a", input: 100, output: 10), // duplicate: must be ignored
            record(requestId: "req_b", input: 50, output: 5)
        ]
        let summary = ClaudeUsageAggregator.summarize(records: records, now: now)
        // last7d should count req_a once + req_b => 150 input, 15 output
        XCTAssertEqual(summary.last7d.inputTokens, 150)
        XCTAssertEqual(summary.last7d.outputTokens, 15)
    }

    // MARK: - 5h rolling window

    func testFiveHourWindowExcludesOldIncludesRecent() {
        let records = [
            record(offsetHours: 6, requestId: "old", input: 1000), // 6h ago -> excluded from 5h
            record(offsetHours: 1, requestId: "recent", input: 200) // 1h ago -> included
        ]
        let summary = ClaudeUsageAggregator.summarize(records: records, now: now)
        XCTAssertEqual(summary.last5h.inputTokens, 200)
        // both are within 7d
        XCTAssertEqual(summary.last7d.inputTokens, 1200)
    }

    // MARK: - 7d window boundary

    func testSevenDayWindowExcludesOlderThanSevenDays() {
        let records = [
            record(offsetHours: 24 * 8, requestId: "ancient", input: 999), // 8 days ago -> excluded
            record(offsetHours: 24 * 3, requestId: "midweek", input: 300)  // 3 days ago -> included
        ]
        let summary = ClaudeUsageAggregator.summarize(records: records, now: now)
        XCTAssertEqual(summary.last7d.inputTokens, 300)
    }

    // MARK: - Cost

    func testCostComputedFromPricingTablePerModel() {
        // Opus: input 15, output 75, cacheWrite 18.75, cacheRead 1.5 (per 1M)
        let records = [
            record(requestId: "opus", model: "claude-opus-4-7",
                   input: 1_000_000, output: 1_000_000,
                   cacheWrite: 1_000_000, cacheRead: 1_000_000)
        ]
        let summary = ClaudeUsageAggregator.summarize(records: records, now: now)
        // 15 + 75 + 18.75 + 1.5 = 110.25
        XCTAssertEqual(summary.last7d.costUSD, 110.25, accuracy: 0.0001)
    }

    func testMixedModelCostSumsPerRecordPricing() {
        let records = [
            record(requestId: "o", model: "claude-opus-4-7", input: 1_000_000), // 15
            record(requestId: "s", model: "claude-sonnet-4-5", input: 1_000_000) // 3
        ]
        let summary = ClaudeUsageAggregator.summarize(records: records, now: now)
        XCTAssertEqual(summary.last7d.costUSD, 18.0, accuracy: 0.0001)
    }

    func testUnknownModelPricedAsSonnet() {
        let records = [
            record(requestId: "u", model: "some-future-model", input: 1_000_000) // sonnet input = 3
        ]
        let summary = ClaudeUsageAggregator.summarize(records: records, now: now)
        XCTAssertEqual(summary.last7d.costUSD, 3.0, accuracy: 0.0001)
    }

    // MARK: - Per-project / per-model breakdowns

    func testPerProjectAttributionSortedByCostDesc() {
        let records = [
            record(requestId: "a", model: "claude-sonnet-4-5", project: "velovate", input: 1_000_000), // $3
            record(requestId: "b", model: "claude-opus-4-7", project: "qr-ninja", input: 1_000_000)     // $15
        ]
        let summary = ClaudeUsageAggregator.summarize(records: records, now: now)
        XCTAssertEqual(summary.projectsLast7d.count, 2)
        // sorted by cost desc => qr-ninja first
        XCTAssertEqual(summary.projectsLast7d[0].name, "qr-ninja")
        XCTAssertEqual(summary.projectsLast7d[0].totals.costUSD, 15.0, accuracy: 0.0001)
        XCTAssertEqual(summary.projectsLast7d[1].name, "velovate")
    }

    func testPerModelBreakdownGroupsByModel() {
        let records = [
            record(requestId: "a", model: "claude-opus-4-7", input: 1_000_000),
            record(requestId: "b", model: "claude-opus-4-7", input: 1_000_000),
            record(requestId: "c", model: "claude-sonnet-4-5", input: 1_000_000)
        ]
        let summary = ClaudeUsageAggregator.summarize(records: records, now: now)
        XCTAssertEqual(summary.modelsLast7d.count, 2)
        XCTAssertEqual(summary.modelsLast7d[0].name, "claude-opus-4-7") // higher cost first
        XCTAssertEqual(summary.modelsLast7d[0].totals.inputTokens, 2_000_000)
    }

    // MARK: - totalTokens

    func testTotalsAggregateAllTokenKinds() {
        let records = [
            record(requestId: "a", input: 10, output: 20, cacheWrite: 30, cacheRead: 40)
        ]
        let summary = ClaudeUsageAggregator.summarize(records: records, now: now)
        XCTAssertEqual(summary.last7d.totalTokens, 100)
    }
}
