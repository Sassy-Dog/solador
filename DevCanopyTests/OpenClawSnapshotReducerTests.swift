@testable import DevCanopy
import XCTest

/// Exercises the pure frame→sections reducer with realistic gateway payloads,
/// pinning the status-derivation and shape-flexible decoding ported from periclaw.
final class OpenClawSnapshotReducerTests: XCTestCase {
    private func env(_ json: String) throws -> OCEnvelope {
        try JSONDecoder().decode(OCEnvelope.self, from: Data(json.utf8))
    }

    // MARK: - cron.list

    func testCronListObjectShapePopulatesSummary() throws {
        var r = OpenClawSnapshotReducer()
        let changed = try r.ingest(env("""
        {"type":"res","id":"cron.list","payload":{"jobs":[
          {"name":"nightly","id":"u1","state":{"lastStatus":"ok"}},
          {"name":"sync","state":{"running":true}},
          {"name":"backup","state":{"lastStatus":"error","lastError":"disk full"}}
        ]}}
        """))
        XCTAssertTrue(changed)
        let cron = r.cronSummary
        XCTAssertEqual(cron.ok, 1)
        XCTAssertEqual(cron.running, 1)
        XCTAssertEqual(cron.error, 1)
        XCTAssertEqual(cron.total, 3)
        XCTAssertEqual(cron.lastError, "disk full")
        XCTAssertEqual(cron.jobs.map(\.name), ["backup", "nightly", "sync"]) // sorted
    }

    func testCronListBareArrayShape() throws {
        var r = OpenClawSnapshotReducer()
        try r.ingest(env("""
        {"type":"res","id":"cron.list","payload":[{"name":"a","state":{"lastStatus":"ok"}}]}
        """))
        XCTAssertEqual(r.cronSummary.ok, 1)
    }

    func testCronListCronsKeyShape() throws {
        var r = OpenClawSnapshotReducer()
        try r.ingest(env("""
        {"type":"res","id":"cron.list","payload":{"crons":[{"name":"a","state":{"lastStatus":"timeout"}}]}}
        """))
        XCTAssertEqual(r.cronSummary.error, 1)
    }

    // MARK: - channels.status

    func testChannelsStatusMapsDots() throws {
        var r = OpenClawSnapshotReducer()
        try r.ingest(env("""
        {"type":"res","id":"channels.status","payload":{"channels":[
          {"name":"slack","enabled":true,"connected":true},
          {"name":"telegram","enabled":true,"connected":false},
          {"name":"whatsapp","enabled":false},
          {"name":"discord","enabled":true,"lastError":"oops"}
        ]}}
        """))
        let byName = Dictionary(uniqueKeysWithValues: r.channelStatuses.map { ($0.name, $0.status) })
        XCTAssertEqual(byName["slack"], .ok)
        XCTAssertEqual(byName["telegram"], .unknown)
        XCTAssertEqual(byName["whatsapp"], .disabled)
        XCTAssertEqual(byName["discord"], .error)
        XCTAssertEqual(r.channelStatuses.map(\.name), ["discord", "slack", "telegram", "whatsapp"]) // sorted
    }

    // MARK: - sessions.list (usage)

    func testUsagePicksMostRecentSession() throws {
        var r = OpenClawSnapshotReducer()
        try r.ingest(env("""
        {"type":"res","id":"sessions.list","payload":{"sessions":[
          {"key":"agent:main:a","totalTokens":100,"updatedAt":1000},
          {"key":"agent:main:b","totalTokens":900,"contextTokens":42,"updatedAt":5000}
        ]}}
        """))
        let usage = try XCTUnwrap(r.usageRollup)
        XCTAssertEqual(usage.totalTokens, 900) // the updatedAt=5000 one
        XCTAssertEqual(usage.contextTokens, 42)
    }

    func testUsageNilWhenNoSessions() throws {
        var r = OpenClawSnapshotReducer()
        try r.ingest(env(#"{"type":"res","id":"sessions.list","payload":{"sessions":[]}}"#))
        XCTAssertNil(r.usageRollup)
    }

    // MARK: - agents.list

    func testAgentsListRowsUseIdentityAndModel() throws {
        var r = OpenClawSnapshotReducer()
        try r.ingest(env("""
        {"type":"res","id":"agents.list","payload":{"defaultId":"main","agents":[
          {"id":"main","name":"fallback","identity":{"name":"Sebastian","emoji":"🦀"},
           "model":{"primary":"anthropic/claude-opus-4-8"}}
        ]}}
        """))
        let row = try XCTUnwrap(r.agentRows.first)
        XCTAssertEqual(row.name, "Sebastian")
        XCTAssertEqual(row.emoji, "🦀")
        XCTAssertEqual(row.detail, "anthropic/claude-opus-4-8")
        XCTAssertEqual(row.status, .ok) // resting
    }

    // MARK: - cron push event

    func testCronFinishedEventUpdatesStatus() throws {
        var r = OpenClawSnapshotReducer()
        let changed = try r.ingest(env("""
        {"type":"event","event":"cron","payload":{"jobId":"u","jobName":"nightly","action":"finished","status":"error","error":"boom"}}
        """))
        XCTAssertTrue(changed)
        XCTAssertEqual(r.cronSummary.error, 1)
        XCTAssertEqual(r.cronSummary.lastError, "boom")
    }

    func testCronAddedEventIgnored() throws {
        var r = OpenClawSnapshotReducer()
        let changed = try r.ingest(env("""
        {"type":"event","event":"cron","payload":{"jobId":"u","action":"added"}}
        """))
        XCTAssertFalse(changed)
        XCTAssertEqual(r.cronSummary.total, 0)
    }

    // MARK: - agent activity events

    func testAgentActivityFlipsRunningThenClearsOnLifecycleEnd() throws {
        var r = OpenClawSnapshotReducer()
        try r.ingest(env("""
        {"type":"res","id":"agents.list","payload":{"defaultId":"main","agents":[{"id":"main"}]}}
        """))
        XCTAssertEqual(r.agentRows.first?.status, .ok)

        try r.ingest(env(#"{"type":"event","event":"agent","payload":{"stream":"tool","sessionKey":"agent:main:s"}}"#))
        XCTAssertEqual(r.agentRows.first?.status, .running)

        try r.ingest(env(#"{"type":"event","event":"agent","payload":{"stream":"lifecycle","sessionKey":"agent:main:s","data":{"phase":"end"}}}"#))
        XCTAssertEqual(r.agentRows.first?.status, .ok)
    }

    func testLivenessEventIsNotADataChange() throws {
        var r = OpenClawSnapshotReducer()
        XCTAssertFalse(try r.ingest(env(#"{"type":"event","event":"heartbeat"}"#)))
        XCTAssertFalse(try r.ingest(env(#"{"type":"event","event":"tick"}"#)))
    }

    func testUnknownFrameIgnored() throws {
        var r = OpenClawSnapshotReducer()
        XCTAssertFalse(try r.ingest(env(#"{"type":"req","id":"x"}"#)))
    }
}
