@testable import DevCanopy
import HostMetricsKit
import XCTest

/// Locks the remote-host failure taxonomy (issue #36): a wrong token, a broken
/// wire contract, and a dead VM must surface as distinct causes rather than one
/// undifferentiated "unreachable", and the `/v1/health` payload must decode so
/// the app can show the agent version without SSHing in.
final class RemoteHostFailureTests: XCTestCase {
    // MARK: Health payload decoding

    func testDecodesHealthPayloadWithVersionAndHostname() throws {
        let json = """
        {
          "status": "ok",
          "hostname": "ubu-3xdv",
          "version": "0.3.1",
          "sampleAgeSeconds": 2,
          "samplerStale": false
        }
        """
        let info = try RemoteHostMetricsService.snapshotDecoder.decode(
            HealthInfo.self, from: Data(json.utf8)
        )
        XCTAssertEqual(info.hostname, "ubu-3xdv")
        XCTAssertEqual(info.version, "0.3.1")
        XCTAssertEqual(info.status, "ok")
        XCTAssertEqual(info.sampleAgeSeconds, 2)
        XCTAssertEqual(info.samplerStale, false)
    }

    func testDecodesDegradedHealthPayload() throws {
        let json = """
        { "status": "degraded", "hostname": "h", "version": "9.9.9", "sampleAgeSeconds": 3600, "samplerStale": true }
        """
        let info = try RemoteHostMetricsService.snapshotDecoder.decode(
            HealthInfo.self, from: Data(json.utf8)
        )
        XCTAssertEqual(info.status, "degraded")
        XCTAssertEqual(info.samplerStale, true)
    }

    // MARK: Error labels are distinct per cause

    func testEachFailureCauseHasADistinctLabel() {
        XCTAssertEqual(RemoteHostError.authFailed.shortLabel, "auth failed")
        XCTAssertEqual(RemoteHostError.decodeFailed.shortLabel, "decode failed")
        XCTAssertEqual(RemoteHostError.unreachable.shortLabel, "unreachable")
        XCTAssertEqual(RemoteHostError.httpStatus(503).shortLabel, "HTTP 503")

        let labels = Set([
            RemoteHostError.authFailed.shortLabel,
            RemoteHostError.decodeFailed.shortLabel,
            RemoteHostError.unreachable.shortLabel,
            RemoteHostError.httpStatus(503).shortLabel
        ])
        XCTAssertEqual(labels.count, 4, "failure causes must be distinguishable")
    }

    // MARK: Connection-state surfacing

    func testFailedConnectionStateCarriesAndExposesCause() {
        let authState = HostConnectionState.failed(.authFailed)
        XCTAssertTrue(authState.isFailed)
        XCTAssertEqual(authState.failureLabel, "auth failed")

        let decodeState = HostConnectionState.failed(.decodeFailed)
        XCTAssertEqual(decodeState.failureLabel, "decode failed")

        XCTAssertNotEqual(authState, decodeState)
    }

    func testNonFailedStatesAreNotFlaggedAsFailed() {
        for state in [HostConnectionState.local, .connecting, .connected] {
            XCTAssertFalse(state.isFailed)
            XCTAssertEqual(state.failureLabel, "")
        }
    }

    // MARK: Failure debounce (issue: a high-latency link must not flap the card)

    /// Builds a service without touching the network — `init` only configures a
    /// `URLSession`; nothing connects until `start()`, so the debounce state
    /// machine can be driven directly via `recordFailure`/`recordSuccess`.
    @MainActor
    private func makeService() -> RemoteHostMetricsService {
        RemoteHostMetricsService(hostName: "h", address: "h", port: 7878, token: "t")
    }

    @MainActor
    func testSingleFailureDoesNotMarkHostDown() {
        let service = makeService()
        service.recordFailure(.unreachable)
        XCTAssertFalse(service.connectionState.isFailed, "one missed poll must be absorbed")
        XCTAssertEqual(service.consecutiveFailures, 1)
    }

    @MainActor
    func testSecondConsecutiveFailureMarksHostDown() {
        let service = makeService()
        service.recordFailure(.unreachable)
        service.recordFailure(.unreachable)
        XCTAssertEqual(service.connectionState, .failed(.unreachable))
        XCTAssertEqual(service.consecutiveFailures, 2)
    }

    @MainActor
    func testSuccessAfterOutageRecoversAndResetsStreak() {
        let service = makeService()
        service.recordFailure(.unreachable)
        service.recordFailure(.unreachable)
        service.recordSuccess()
        XCTAssertEqual(service.connectionState, .connected)
        XCTAssertEqual(service.consecutiveFailures, 0)
    }

    @MainActor
    func testSuccessBetweenBlipsPreventsFalseDown() {
        let service = makeService()
        service.recordFailure(.unreachable) // blip 1
        service.recordSuccess() // recovered, streak reset
        service.recordFailure(.unreachable) // blip 2 — must start fresh
        XCTAssertFalse(service.connectionState.isFailed, "non-consecutive blips must not trip the threshold")
        XCTAssertEqual(service.consecutiveFailures, 1)
    }

    @MainActor
    func testCauseChangeWhileDownUpdatesState() {
        let service = makeService()
        service.recordFailure(.unreachable)
        service.recordFailure(.unreachable) // down on unreachable
        service.recordFailure(.authFailed) // cause changes mid-outage
        XCTAssertEqual(service.connectionState, .failed(.authFailed))
    }
}
