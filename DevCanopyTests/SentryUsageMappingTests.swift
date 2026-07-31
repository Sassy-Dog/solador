@testable import DevCanopy
import XCTest

final class SentryUsageMappingTests: XCTestCase {
    // MARK: - Fixtures

    /// A miniature `stats_v2` response in the documented wire shape
    /// (start/end/intervals/groups → {by, totals, series}).
    private func fixture(_ json: String) throws -> SentryStatsResponse {
        try JSONDecoder().decode(SentryStatsResponse.self, from: Data(json.utf8))
    }

    private let outcomes = """
    {
      "start": "2026-07-01T00:00:00Z",
      "end": "2026-07-31T00:00:00Z",
      "intervals": ["2026-07-01T00:00:00Z"],
      "groups": [
        {
          "by": { "outcome": "accepted" },
          "totals": { "sum(quantity)": 12345 },
          "series": { "sum(quantity)": [12345] }
        },
        {
          "by": { "outcome": "rate_limited" },
          "totals": { "sum(quantity)": 500 },
          "series": { "sum(quantity)": [500] }
        },
        {
          "by": { "outcome": "invalid" },
          "totals": { "sum(quantity)": 165665 },
          "series": { "sum(quantity)": [165665] }
        }
      ]
    }
    """

    // MARK: - summarize

    func testSummarizePicksOnlyTheAcceptedOutcome() throws {
        // rate_limited (500) and invalid (165665) must not land in the headline — they
        // don't count against quota, and folding them in would inflate the figure.
        let summary = try SentryUsageMapping.summarize(fixture(outcomes))
        XCTAssertEqual(summary.acceptedErrorEvents, 12345)
        XCTAssertEqual(summary.outcomeGroupCount, 3)
    }

    func testSummarizeTreatsAMissingAcceptedGroupAsARealZero() throws {
        // Sentry returns an outcome group only when that outcome had data, so a
        // response that measured *something* but accepted nothing is a real zero.
        let json = """
        {
          "groups": [
            { "by": { "outcome": "rate_limited" }, "totals": { "sum(quantity)": 42 } }
          ]
        }
        """
        let summary = try SentryUsageMapping.summarize(fixture(json))
        XCTAssertEqual(summary.acceptedErrorEvents, 0)
        XCTAssertEqual(summary.outcomeGroupCount, 1)
    }

    func testSummarizeReportsUnknownWhenNoGroupsComeBack() throws {
        // Nothing was measured — the wrong slug or an org with no ingest. That is
        // genuinely unknown, so the figure must be nil (the panel's "—"), never 0.
        let summary = try SentryUsageMapping.summarize(fixture(#"{ "groups": [] }"#))
        XCTAssertNil(summary.acceptedErrorEvents)
        XCTAssertEqual(summary.outcomeGroupCount, 0)
    }

    func testDecodeToleratesAMissingGroupsKey() throws {
        let summary = try SentryUsageMapping.summarize(fixture("{}"))
        XCTAssertNil(summary.acceptedErrorEvents)
        XCTAssertEqual(summary.outcomeGroupCount, 0)
    }

    func testSummarizeToleratesAnAcceptedGroupWithoutTheRequestedField() throws {
        // The group exists but carries no `sum(quantity)` total: contribute nothing
        // rather than guessing a value from the series.
        let json = """
        { "groups": [ { "by": { "outcome": "accepted" }, "totals": {} } ] }
        """
        let summary = try SentryUsageMapping.summarize(fixture(json))
        XCTAssertEqual(summary.acceptedErrorEvents, 0)
    }

    // MARK: - Query vocabulary

    func testQueryVocabularyMatchesTheDocumentedStatsV2Contract() {
        XCTAssertEqual(SentryStatsQuery.field, "sum(quantity)")
        XCTAssertEqual(SentryStatsQuery.category, "error")
        XCTAssertEqual(SentryStatsQuery.groupBy, "outcome")
        XCTAssertEqual(SentryStatsQuery.statsPeriod, "30d")
        XCTAssertEqual(SentryStatsQuery.windowDays, 30)
    }

    // MARK: - Errors

    func testAuthFailuresAreDetectedFor401And403() {
        XCTAssertTrue(SentryUsageError.http(status: 401, body: nil).isAuthFailure)
        XCTAssertTrue(SentryUsageError.http(status: 403, body: nil).isAuthFailure)
        XCTAssertFalse(SentryUsageError.http(status: 500, body: nil).isAuthFailure)
        XCTAssertFalse(SentryUsageError.invalidResponse.isAuthFailure)
    }

    @MainActor
    func testFriendlyMessageAsksForANewTokenOnAuthFailure() {
        let message = SentryUsageService.friendlyMessage(for: SentryUsageError.http(status: 403, body: nil))
        XCTAssertEqual(message, "token invalid — paste a new one in Settings")
    }

    @MainActor
    func testFriendlyMessagePointsAtTheOrgSlugOn404() {
        let message = SentryUsageService.friendlyMessage(for: SentryUsageError.http(status: 404, body: nil))
        XCTAssertEqual(message, "Sentry org not found — check the org slug in Settings")
    }

    // MARK: - Service (driven through the injected transport seam, no network)

    @MainActor
    func testServiceSurfacesTheSummaryFromTheStubTransport() async throws {
        let stub = try StubSentryFetcher(response: fixture(outcomes))
        let service = SentryUsageService(
            makeFetcher: { _ in stub },
            tokenProvider: { "sentry_token_value" },
            orgSlugProvider: { "sassy-dog" }
        )

        await service.refresh()

        XCTAssertTrue(service.isConfigured)
        XCTAssertNil(service.lastError)
        XCTAssertEqual(service.summary?.acceptedErrorEvents, 12345)
        XCTAssertNotNil(service.lastUpdated)
        XCTAssertFalse(service.isLoading)
    }

    @MainActor
    func testServiceRequestsTheRollingWindowForTheConfiguredOrg() async throws {
        let stub = try StubSentryFetcher(response: fixture(outcomes))
        let service = SentryUsageService(
            makeFetcher: { _ in stub },
            tokenProvider: { "token" },
            orgSlugProvider: { "sassy-dog" }
        )

        await service.refresh()

        // Hop off the actor first: unwrapping inline would hoist the `await` onto
        // `XCTUnwrap` and read the isolated property from the main actor.
        let recorded = await stub.lastCall
        let call = try XCTUnwrap(recorded)
        XCTAssertEqual(call.orgSlug, "sassy-dog")
        XCTAssertEqual(call.statsPeriod, "30d")
    }

    @MainActor
    func testTransportFailureLeavesTheValueUnknownAndSurfacesTheError() async {
        let service = SentryUsageService(
            makeFetcher: { _ in StubSentryFetcher(error: SentryUsageError.http(status: 401, body: nil)) },
            tokenProvider: { "token" },
            orgSlugProvider: { "sassy-dog" }
        )

        await service.refresh()

        // The "—" path: no summary at all, so the panel renders a dash rather than a
        // defaulted 0, and the footer explains why.
        XCTAssertNil(service.summary)
        XCTAssertEqual(service.lastError, "token invalid — paste a new one in Settings")
        XCTAssertFalse(service.isLoading)
    }

    @MainActor
    func testATransientFailureKeepsTheLastGoodFigure() async throws {
        let stub = try StubSentryFetcher(response: fixture(outcomes))
        let service = SentryUsageService(
            makeFetcher: { _ in stub },
            tokenProvider: { "token" },
            orgSlugProvider: { "sassy-dog" }
        )
        await service.refresh()
        XCTAssertEqual(service.summary?.acceptedErrorEvents, 12345)

        await stub.fail(with: .http(status: 503, body: nil))
        await service.refresh()

        // Last-good survives one bad poll; the footer carries the failure.
        XCTAssertEqual(service.summary?.acceptedErrorEvents, 12345)
        XCTAssertEqual(service.lastError, "Sentry API request failed (HTTP 503)")
    }

    @MainActor
    func testASuccessfulButEmptyResponseKeepsTheValueUnknown() async throws {
        let stub = try StubSentryFetcher(response: fixture(#"{ "groups": [] }"#))
        let service = SentryUsageService(
            makeFetcher: { _ in stub },
            tokenProvider: { "token" },
            orgSlugProvider: { "sassy-dog" }
        )

        await service.refresh()

        XCTAssertNil(service.summary?.acceptedErrorEvents)
        XCTAssertEqual(service.lastError, SentryUsageService.noStatsMessage)
    }

    @MainActor
    func testAMissingOrgSlugIsReportedWithoutCallingTheAPI() async throws {
        let stub = try StubSentryFetcher(response: fixture(outcomes))
        let service = SentryUsageService(
            makeFetcher: { _ in stub },
            tokenProvider: { "token" },
            orgSlugProvider: { "   " }
        )

        await service.refresh()

        XCTAssertNil(service.summary)
        XCTAssertEqual(service.lastError, "Add your Sentry org slug in Settings")
        let call = await stub.lastCall
        XCTAssertNil(call)
    }

    @MainActor
    func testNoTokenLeavesTheServiceUnconfiguredSoThePanelHidesTheSection() async {
        let service = SentryUsageService(
            makeFetcher: { _ in StubSentryFetcher(error: SentryUsageError.invalidResponse) },
            tokenProvider: { nil },
            orgSlugProvider: { "sassy-dog" }
        )

        await service.refresh()

        XCTAssertFalse(service.isConfigured)
        XCTAssertNil(service.summary)
        XCTAssertNil(service.lastError)
    }

    // MARK: - Panel formatting

    func testPanelRendersADashForAnUnknownEventCount() {
        XCTAssertNil(ClaudeUsagePanel.events(nil))
        XCTAssertEqual(ClaudeUsagePanel.events(0), "0")
        XCTAssertEqual(ClaudeUsagePanel.events(12345), "12k")
        XCTAssertEqual(ClaudeUsagePanel.events(1_200_000), "1.2M")
    }
}

/// Stub transport: hands back a canned response (or error) and records the call, so the
/// service's fold + error paths run entirely off the network. An actor because
/// `SentryStatsFetching` is `Sendable` and this stub is mutable.
private actor StubSentryFetcher: SentryStatsFetching {
    /// The arguments of the one call the service is expected to make.
    struct Call: Equatable {
        let orgSlug: String
        let statsPeriod: String
    }

    private let response: SentryStatsResponse
    private var error: SentryUsageError?
    private(set) var lastCall: Call?

    init(response: SentryStatsResponse) {
        self.response = response
        error = nil
    }

    init(error: SentryUsageError) {
        response = SentryStatsResponse(start: nil, end: nil, groups: nil)
        self.error = error
    }

    /// Flip an already-succeeding stub into failing, to exercise keep-last-good.
    func fail(with error: SentryUsageError) {
        self.error = error
    }

    func errorOutcomes(orgSlug: String, statsPeriod: String) async throws -> SentryStatsResponse {
        lastCall = Call(orgSlug: orgSlug, statsPeriod: statsPeriod)
        if let error { throw error }
        return response
    }
}
