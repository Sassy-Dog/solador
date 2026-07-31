@testable import DevCanopy
import XCTest

final class NeonUsageMappingTests: XCTestCase {
    // MARK: - Fixtures

    /// A miniature `consumption_history/v2/projects` response in the documented wire
    /// shape (projects → periods → consumption → metrics[{metric_name, value}]).
    private func fixture(_ json: String) throws -> NeonConsumptionResponse {
        try JSONDecoder().decode(NeonConsumptionResponse.self, from: Data(json.utf8))
    }

    private let twoProjects = """
    {
      "projects": [
        {
          "project_id": "qr-ninja",
          "periods": [
            {
              "period_id": "p1",
              "period_plan": "scale",
              "period_start": "2026-07-01T00:00:00Z",
              "consumption": [
                {
                  "timeframe_start": "2026-07-01T00:00:00Z",
                  "timeframe_end": "2026-08-01T00:00:00Z",
                  "metrics": [
                    { "metric_name": "compute_unit_seconds", "value": 7200 },
                    { "metric_name": "root_branch_bytes_month", "value": 1073741824 },
                    { "metric_name": "child_branch_bytes_month", "value": 536870912 }
                  ]
                }
              ]
            }
          ]
        },
        {
          "project_id": "velovate",
          "periods": [
            {
              "consumption": [
                {
                  "timeframe_start": "2026-07-01T00:00:00Z",
                  "timeframe_end": "2026-08-01T00:00:00Z",
                  "metrics": [
                    { "metric_name": "compute_unit_seconds", "value": 3600 },
                    { "metric_name": "root_branch_bytes_month", "value": 536870912 },
                    { "metric_name": "public_network_transfer_bytes", "value": 999 }
                  ]
                }
              ]
            }
          ]
        }
      ],
      "pagination": { "cursor": "" }
    }
    """

    // MARK: - summarize

    func testSummarizeSumsMetricsAcrossProjectsAndConvertsUnits() throws {
        let summary = try NeonUsageMapping.summarize(fixture(twoProjects))

        // 7200 + 3600 compute-unit seconds = 3 CU-hours.
        XCTAssertEqual(try XCTUnwrap(summary.computeUnitHours), 3.0, accuracy: 1e-9)
        // 1 GiB + 0.5 GiB root/child on the first project, 0.5 GiB root on the second.
        XCTAssertEqual(try XCTUnwrap(summary.storageGiB), 2.0, accuracy: 1e-9)
        XCTAssertEqual(summary.projectCount, 2)
    }

    func testSummarizeIgnoresMetricsThePanelDidNotRequest() throws {
        // `public_network_transfer_bytes` (999) appears in the fixture and must not
        // land in either bucket — guessing which one it belongs to would fabricate
        // a storage figure.
        let summary = try NeonUsageMapping.summarize(fixture(twoProjects))
        XCTAssertEqual(try XCTUnwrap(summary.storageGiB), 2.0, accuracy: 1e-9)
    }

    func testSummarizeTreatsAnOmittedMetricInAReturnedTimeframeAsZero() throws {
        // Neon documents omitting a metric whose value is zero for a timeframe it
        // *did* return — that is a real zero, not an unknown.
        let json = """
        {
          "projects": [
            {
              "project_id": "idle",
              "periods": [
                {
                  "consumption": [
                    {
                      "timeframe_start": "2026-07-01T00:00:00Z",
                      "timeframe_end": "2026-08-01T00:00:00Z",
                      "metrics": [
                        { "metric_name": "compute_unit_seconds", "value": 1800 }
                      ]
                    }
                  ]
                }
              ]
            }
          ]
        }
        """
        let summary = try NeonUsageMapping.summarize(fixture(json))
        XCTAssertEqual(try XCTUnwrap(summary.computeUnitHours), 0.5, accuracy: 1e-9)
        XCTAssertEqual(try XCTUnwrap(summary.storageGiB), 0.0, accuracy: 1e-9)
        XCTAssertEqual(summary.projectCount, 1)
    }

    func testSummarizeReportsUnknownWhenNoTimeframesComeBack() throws {
        // The plan-gated / empty-org shape: projects with no consumption timeframes.
        // Nothing was measured, so both figures must be nil (the panel's "—"), never 0.
        let json = """
        { "projects": [ { "project_id": "gated", "periods": [] } ] }
        """
        let summary = try NeonUsageMapping.summarize(fixture(json))
        XCTAssertNil(summary.computeUnitHours)
        XCTAssertNil(summary.storageGiB)
        XCTAssertEqual(summary.projectCount, 0)
    }

    func testSummarizeReportsUnknownForAnEmptyProjectList() throws {
        let summary = try NeonUsageMapping.summarize(fixture(#"{ "projects": [] }"#))
        XCTAssertNil(summary.computeUnitHours)
        XCTAssertNil(summary.storageGiB)
        XCTAssertEqual(summary.projectCount, 0)
    }

    func testDecodeToleratesAMissingProjectsKey() throws {
        let summary = try NeonUsageMapping.summarize(fixture("{}"))
        XCTAssertEqual(summary.projectCount, 0)
    }

    // MARK: - Window + RFC 3339

    func testMonthToDateWindowStartsAtTheFirstInstantOfTheUTCMonth() {
        let now = utc(2026, 7, 31, 18, 45)
        let window = NeonUsageMapping.monthToDateWindow(now: now)
        XCTAssertEqual(NeonUsageMapping.rfc3339(window.from), "2026-07-01T00:00:00Z")
        XCTAssertEqual(NeonUsageMapping.rfc3339(window.to), "2026-07-31T18:45:00Z")
    }

    func testMonthToDateWindowUsesUTCNotLocalTime() {
        // 00:30 UTC on the 1st is still the 1st in UTC; a local-time month boundary
        // would slide this back into the previous month for negative offsets.
        let window = NeonUsageMapping.monthToDateWindow(now: utc(2026, 1, 1, 0, 30))
        XCTAssertEqual(NeonUsageMapping.rfc3339(window.from), "2026-01-01T00:00:00Z")
    }

    // MARK: - Errors

    func testAuthFailuresAreDetectedFor401And403() {
        XCTAssertTrue(NeonUsageError.http(status: 401, body: nil).isAuthFailure)
        XCTAssertTrue(NeonUsageError.http(status: 403, body: nil).isAuthFailure)
        XCTAssertFalse(NeonUsageError.http(status: 500, body: nil).isAuthFailure)
        XCTAssertFalse(NeonUsageError.invalidResponse.isAuthFailure)
    }

    @MainActor
    func testFriendlyMessageAsksForANewKeyOnAuthFailure() {
        let message = NeonUsageService.friendlyMessage(for: NeonUsageError.http(status: 403, body: nil))
        XCTAssertEqual(message, "API key invalid — paste a new one in Settings")
    }

    @MainActor
    func testFriendlyMessagePointsAtTheOrgIDOn404() {
        let message = NeonUsageService.friendlyMessage(for: NeonUsageError.http(status: 404, body: nil))
        XCTAssertEqual(message, "Neon org not found — check the org ID in Settings")
    }

    // MARK: - Service (driven through the injected transport seam, no network)

    @MainActor
    func testServiceSurfacesTheSummaryFromTheStubTransport() async throws {
        let stub = try StubNeonFetcher(response: fixture(twoProjects))
        let service = NeonUsageService(
            makeFetcher: { _ in stub },
            apiKeyProvider: { "neon_api_key_value" },
            orgIDProvider: { "org-abc" }
        )

        await service.refresh(now: utc(2026, 7, 31, 12))

        XCTAssertTrue(service.isConfigured)
        XCTAssertNil(service.lastError)
        XCTAssertEqual(try XCTUnwrap(service.summary?.computeUnitHours), 3.0, accuracy: 1e-9)
        XCTAssertNotNil(service.lastUpdated)
    }

    @MainActor
    func testServiceRequestsTheMonthToDateWindowForTheConfiguredOrg() async throws {
        let stub = try StubNeonFetcher(response: fixture(twoProjects))
        let service = NeonUsageService(
            makeFetcher: { _ in stub },
            apiKeyProvider: { "key" },
            orgIDProvider: { "org-abc" }
        )

        await service.refresh(now: utc(2026, 7, 20, 9, 15))

        let recorded = await stub.lastCall
        let call = try XCTUnwrap(recorded)
        XCTAssertEqual(call.orgID, "org-abc")
        XCTAssertEqual(NeonUsageMapping.rfc3339(call.from), "2026-07-01T00:00:00Z")
        XCTAssertEqual(NeonUsageMapping.rfc3339(call.to), "2026-07-20T09:15:00Z")
    }

    @MainActor
    func testTransportFailureLeavesTheValueUnknownAndSurfacesTheError() async {
        let service = NeonUsageService(
            makeFetcher: { _ in StubNeonFetcher(error: NeonUsageError.http(status: 401, body: nil)) },
            apiKeyProvider: { "key" },
            orgIDProvider: { "org-abc" }
        )

        await service.refresh(now: utc(2026, 7, 31, 12))

        // The "—" path: no summary at all, so the panel renders a dash rather than a
        // defaulted 0, and the footer explains why.
        XCTAssertNil(service.summary)
        XCTAssertEqual(service.lastError, "API key invalid — paste a new one in Settings")
        XCTAssertFalse(service.isLoading)
    }

    @MainActor
    func testASuccessfulButEmptyResponseKeepsTheValuesUnknown() async throws {
        let stub = try StubNeonFetcher(response: fixture(#"{ "projects": [] }"#))
        let service = NeonUsageService(
            makeFetcher: { _ in stub },
            apiKeyProvider: { "key" },
            orgIDProvider: { "org-abc" }
        )

        await service.refresh(now: utc(2026, 7, 31, 12))

        XCTAssertNil(service.summary?.computeUnitHours)
        XCTAssertNil(service.summary?.storageGiB)
        XCTAssertEqual(service.lastError, NeonUsageService.noConsumptionMessage)
    }

    @MainActor
    func testAMissingOrgIDIsReportedWithoutCallingTheAPI() async throws {
        let stub = try StubNeonFetcher(response: fixture(twoProjects))
        let service = NeonUsageService(
            makeFetcher: { _ in stub },
            apiKeyProvider: { "key" },
            orgIDProvider: { "   " }
        )

        await service.refresh(now: utc(2026, 7, 31, 12))

        XCTAssertNil(service.summary)
        XCTAssertEqual(service.lastError, "Add your Neon org ID in Settings")
        let call = await stub.lastCall
        XCTAssertNil(call)
    }

    @MainActor
    func testNoAPIKeyLeavesTheServiceUnconfiguredSoThePanelHidesTheSection() async {
        let service = NeonUsageService(
            makeFetcher: { _ in StubNeonFetcher(error: NeonUsageError.invalidResponse) },
            apiKeyProvider: { nil },
            orgIDProvider: { "org-abc" }
        )

        await service.refresh(now: utc(2026, 7, 31, 12))

        XCTAssertFalse(service.isConfigured)
        XCTAssertNil(service.summary)
        XCTAssertNil(service.lastError)
    }

    // MARK: - Panel formatting

    func testPanelRendersADashForUnknownFigures() {
        XCTAssertNil(ClaudeUsagePanel.cuHours(nil))
        XCTAssertNil(ClaudeUsagePanel.gibibytes(nil))
        XCTAssertEqual(ClaudeUsagePanel.cuHours(3.0), "3.0 CU-h")
        XCTAssertEqual(ClaudeUsagePanel.gibibytes(2.25), "2.2 GiB")
    }

    // MARK: - Helpers

    private func utc(_ year: Int, _ month: Int, _ day: Int, _ hour: Int = 0, _ minute: Int = 0) -> Date {
        var components = DateComponents()
        components.year = year
        components.month = month
        components.day = day
        components.hour = hour
        components.minute = minute
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        return calendar.date(from: components)!
    }
}

/// Stub transport: hands back a canned response (or error) and records the call, so
/// the service's fold + error paths run entirely off the network. An actor because
/// `NeonConsumptionFetching` is `Sendable` and this stub is mutable.
private actor StubNeonFetcher: NeonConsumptionFetching {
    /// The arguments of the one call the service is expected to make.
    struct Call: Equatable {
        let orgID: String
        let from: Date
        let to: Date
    }

    private let response: NeonConsumptionResponse
    private let error: NeonUsageError?
    private(set) var lastCall: Call?

    init(response: NeonConsumptionResponse) {
        self.response = response
        error = nil
    }

    init(error: NeonUsageError) {
        response = NeonConsumptionResponse(projects: nil)
        self.error = error
    }

    func consumptionHistory(orgID: String, from: Date, to: Date) async throws -> NeonConsumptionResponse {
        lastCall = Call(orgID: orgID, from: from, to: to)
        if let error { throw error }
        return response
    }
}
