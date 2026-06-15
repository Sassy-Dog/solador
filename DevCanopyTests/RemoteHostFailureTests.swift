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
}
