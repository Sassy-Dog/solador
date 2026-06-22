@testable import DevCanopy
import XCTest

/// Covers the loose wire decoding (`OCJSON`), the status-derivation funcs, and the
/// `key`/`sessionKey` aliasing on session info — all ported from periclaw.
final class OpenClawWireModelsTests: XCTestCase {
    private func json(_ s: String) throws -> OCJSON {
        try JSONDecoder().decode(OCJSON.self, from: Data(s.utf8))
    }

    // MARK: - OCJSON

    func testOCJSONDecodesAllScalarsAndContainers() throws {
        let v = try json(#"{"s":"x","n":3,"b":true,"z":null,"arr":[1,2],"obj":{"k":"v"}}"#)
        XCTAssertEqual(v["s"]?.stringValue, "x")
        XCTAssertNil(v["missing"])
        if case let .number(n) = try XCTUnwrap(v["n"]) { XCTAssertEqual(n, 3) } else { XCTFail("n not number") }
        if case let .bool(b) = try XCTUnwrap(v["b"]) { XCTAssertTrue(b) } else { XCTFail("b not bool") }
        if case .null = try XCTUnwrap(v["z"]) {} else { XCTFail("z not null") }
        if case let .array(a) = try XCTUnwrap(v["arr"]) { XCTAssertEqual(a.count, 2) } else { XCTFail("arr") }
        XCTAssertEqual(v["obj"]?["k"]?.stringValue, "v")
    }

    func testOCJSONSubscriptOnNonObjectIsNil() throws {
        let v = try json(#"[1,2,3]"#)
        XCTAssertNil(v["anything"])
        XCTAssertNil(v.stringValue)
    }

    func testOCJSONRoundTripsThroughDecode() throws {
        // Re-decoding payload into a concrete type is how the reducer routes.
        let v = try json(#"{"name":"slack","enabled":true,"connected":false}"#)
        let channel = try XCTUnwrap(v.decode(OCChannel.self))
        XCTAssertEqual(channel.name, "slack")
        XCTAssertEqual(channel.enabled, true)
        XCTAssertEqual(channel.connected, false)
    }

    // MARK: - Status mapping

    func testCronStatus() {
        XCTAssertEqual(OpenClawStatusMapping.cronStatus(nil), .unknown)
        XCTAssertEqual(OpenClawStatusMapping.cronStatus(state(running: true, last: "error")), .running)
        XCTAssertEqual(OpenClawStatusMapping.cronStatus(state(last: "ok")), .ok)
        for s in ["error", "failed", "timeout"] {
            XCTAssertEqual(OpenClawStatusMapping.cronStatus(state(last: s)), .error)
        }
        XCTAssertEqual(OpenClawStatusMapping.cronStatus(state(last: "weird-future")), .unknown)
        XCTAssertEqual(OpenClawStatusMapping.cronStatus(state(last: nil)), .unknown)
    }

    func testChannelStatus() {
        XCTAssertEqual(OpenClawStatusMapping.channelStatus(chan(enabled: false)), .disabled)
        XCTAssertEqual(OpenClawStatusMapping.channelStatus(chan(enabled: true, error: "x")), .error)
        XCTAssertEqual(OpenClawStatusMapping.channelStatus(chan(enabled: true, connected: true)), .ok)
        XCTAssertEqual(OpenClawStatusMapping.channelStatus(chan(enabled: true, connected: false)), .unknown)
    }

    func testAgentActivityMapping() {
        XCTAssertEqual(OpenClawStatusMapping.agentActivity(stream: "tool"), .running)
        XCTAssertEqual(OpenClawStatusMapping.agentActivity(stream: "item"), .running)
        XCTAssertEqual(OpenClawStatusMapping.agentActivity(stream: "assistant"), .running)
        XCTAssertEqual(OpenClawStatusMapping.agentActivity(stream: "error"), .error)
        XCTAssertNil(OpenClawStatusMapping.agentActivity(stream: "lifecycle"))
        XCTAssertNil(OpenClawStatusMapping.agentActivity(stream: nil))
    }

    func testAgentIDFromSessionKey() {
        XCTAssertEqual(OpenClawStatusMapping.agentID(fromSessionKey: "agent:main:sess-1"), "main")
        XCTAssertEqual(OpenClawStatusMapping.agentID(fromSessionKey: "agent:sebastian:x"), "sebastian")
        XCTAssertNil(OpenClawStatusMapping.agentID(fromSessionKey: "notagent:x"))
        XCTAssertNil(OpenClawStatusMapping.agentID(fromSessionKey: nil))
    }

    // MARK: - OCSessionInfo key aliasing

    func testSessionInfoAcceptsKeyOrSessionKey() throws {
        let a = try JSONDecoder().decode(OCSessionInfo.self, from: Data(#"{"key":"k1","totalTokens":5}"#.utf8))
        XCTAssertEqual(a.key, "k1")
        XCTAssertEqual(a.totalTokens, 5)
        let b = try JSONDecoder().decode(OCSessionInfo.self, from: Data(#"{"sessionKey":"k2"}"#.utf8))
        XCTAssertEqual(b.key, "k2")
    }

    // MARK: - Helpers

    private func state(running: Bool? = nil, last: String?) -> OCCronState {
        OCCronState(
            nextRunAtMs: nil,
            lastRunAtMs: nil,
            lastStatus: last,
            lastDurationMs: nil,
            lastError: nil,
            running: running
        )
    }

    private func chan(enabled: Bool?, connected: Bool? = nil, error: String? = nil) -> OCChannel {
        OCChannel(name: "c", enabled: enabled, connected: connected, lastError: error)
    }
}
