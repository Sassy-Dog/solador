@testable import DevCanopy
import XCTest

@MainActor
final class ContainerPresenceStoreTests: XCTestCase {
    private var suiteName = ""
    private var defaults: UserDefaults!

    override func setUp() {
        super.setUp()
        suiteName = "ContainerPresenceStoreTests-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    private let now = Date(timeIntervalSince1970: 3_000_000)
    private let local = ContainerGroupRule.localHostScope

    private func store() -> ContainerPresenceStore {
        ContainerPresenceStore(defaults: defaults)
    }

    private func tartVM(_ name: String) -> ContainerInfo {
        ContainerInfo(name: name, statusText: "running", isRunning: true, runtime: .tart, image: nil)
    }

    private func expectRule(_ pattern: String, host: String? = nil) -> ContainerGroupRule {
        ContainerGroupRule(pattern: pattern, label: "", action: .expect, host: host)
    }

    // MARK: - Upsert

    func testUpsertRecordsExpectMatchedContainersAndAdvancesLastSuccess() {
        let sut = store()
        sut.noteLocalPoll(
            containers: [tartVM("mac-s1")], succeeded: [.tart],
            rules: [expectRule("mac-*")], now: now
        )
        let records = sut.recordsByName(host: local)
        XCTAssertEqual(records["mac-s1"]?.lastSeen, now)
        XCTAssertEqual(records["mac-s1"]?.runtime, .tart)
        XCTAssertEqual(sut.lastSuccess[local], now)
    }

    func testContainersWithoutExpectRuleAreNotRecorded() {
        let sut = store()
        sut.noteLocalPoll(
            containers: [tartVM("mac-s1")], succeeded: [.tart],
            rules: [ContainerGroupRule(pattern: "mac-*", label: "macs")], now: now
        )
        XCTAssertTrue(sut.recordsByName(host: local).isEmpty, "collapse rules do not create expectations")
    }

    // MARK: - Per-runtime freeze

    func testFailedRuntimeNeitherUpsertsNorAdvancesLastSuccess() {
        let sut = store()
        let earlier = now.addingTimeInterval(-100)
        sut.noteLocalPoll(
            containers: [tartVM("mac-s1")], succeeded: [.tart],
            rules: [expectRule("mac-*")], now: earlier
        )
        // tart fails this cycle: the retained row is still handed in, but nothing
        // about it is fresh knowledge.
        sut.noteLocalPoll(
            containers: [tartVM("mac-s1")], succeeded: [],
            rules: [expectRule("mac-*")], now: now
        )
        XCTAssertEqual(sut.recordsByName(host: local)["mac-s1"]?.lastSeen, earlier)
        XCTAssertEqual(sut.lastSuccess[local], earlier, "an all-failed poll must not advance the clock")
    }

    func testFailedRuntimeRecordSurvivesRuleRemovalUntilRuntimeRecovers() {
        let sut = store()
        sut.noteLocalPoll(
            containers: [tartVM("mac-s1")], succeeded: [.tart],
            rules: [expectRule("mac-*")], now: now.addingTimeInterval(-100)
        )
        sut.noteLocalPoll(containers: [], succeeded: [.docker], rules: [], now: now)
        XCTAssertNotNil(
            sut.recordsByName(host: local)["mac-s1"],
            "a record is frozen — not even prunable — while its runtime is failing"
        )
        sut.noteLocalPoll(containers: [], succeeded: [.tart], rules: [], now: now)
        XCTAssertNil(sut.recordsByName(host: local)["mac-s1"], "prune applies once its runtime reports again")
    }

    // MARK: - Seeding

    func testExactNameExpectRuleSeedsUnobservedRecord() {
        let sut = store()
        sut.noteLocalPoll(containers: [], succeeded: [.tart], rules: [expectRule("mac-s3")], now: now)
        let record = sut.recordsByName(host: local)["mac-s3"]
        XCTAssertEqual(record?.lastSeen, now)
        XCTAssertEqual(record?.runtime, ContainerRuntime?.none, "never observed → no runtime to claim")
    }

    func testGlobExpectRuleDoesNotSeed() {
        let sut = store()
        sut.noteLocalPoll(containers: [], succeeded: [.tart], rules: [expectRule("mac-*")], now: now)
        XCTAssertTrue(sut.recordsByName(host: local).isEmpty, "globs learn observed names only — never invent them")
    }

    func testSeedDoesNotResetAnExistingRecord() {
        let sut = store()
        let earlier = now.addingTimeInterval(-100)
        sut.noteLocalPoll(
            containers: [tartVM("mac-s3")], succeeded: [.tart],
            rules: [expectRule("mac-s3")], now: earlier
        )
        sut.noteLocalPoll(containers: [], succeeded: [.docker], rules: [expectRule("mac-s3")], now: now)
        XCTAssertEqual(
            sut.recordsByName(host: local)["mac-s3"]?.lastSeen, earlier,
            "seeding must never overwrite a real sighting"
        )
    }

    // MARK: - Prune & scoping

    func testPruneDropsRecordsWhoseRuleIsGone() {
        let sut = store()
        sut.noteLocalPoll(
            containers: [tartVM("mac-s1")], succeeded: [.tart],
            rules: [expectRule("mac-*")], now: now.addingTimeInterval(-10)
        )
        sut.noteLocalPoll(containers: [], succeeded: [.tart], rules: [], now: now)
        XCTAssertTrue(sut.recordsByName(host: local).isEmpty)
    }

    func testLocalPruneSparesRemoteHostRecords() {
        let sut = store()
        sut.noteRemotePoll(host: "ubu", containers: [tartVM("mac-s1")], rules: [expectRule("mac-*")], now: now)
        sut.noteLocalPoll(containers: [], succeeded: [.tart], rules: [], now: now)
        XCTAssertEqual(sut.recordsByName(host: "ubu")["mac-s1"]?.lastSeen, now)
    }

    func testHostScopedRuleOnlyAppliesOnItsHost() {
        let sut = store()
        sut.noteLocalPoll(
            containers: [tartVM("mac-s1")], succeeded: [.tart],
            rules: [expectRule("mac-*", host: "ubu")], now: now
        )
        XCTAssertTrue(sut.recordsByName(host: local).isEmpty)
    }

    // MARK: - Remote, forget, persistence

    func testRemotePollUpsertsAndAdvancesItsHostClock() {
        let sut = store()
        sut.noteRemotePoll(host: "ubu", containers: [tartVM("vm-a")], rules: [expectRule("vm-*")], now: now)
        XCTAssertEqual(sut.recordsByName(host: "ubu")["vm-a"]?.lastSeen, now)
        XCTAssertEqual(sut.lastSuccess["ubu"], now)
        XCTAssertNil(sut.lastSuccess[local])
    }

    func testForgetRemovesOneRecord() {
        let sut = store()
        sut.noteLocalPoll(
            containers: [tartVM("mac-s1"), tartVM("mac-s2")], succeeded: [.tart],
            rules: [expectRule("mac-*")], now: now
        )
        sut.forget(host: local, name: "mac-s1")
        XCTAssertNil(sut.recordsByName(host: local)["mac-s1"])
        XCTAssertNotNil(sut.recordsByName(host: local)["mac-s2"])
    }

    func testRecordsPersistAcrossInstancesButClocksDoNot() {
        let first = store()
        first.noteLocalPoll(
            containers: [tartVM("mac-s1")], succeeded: [.tart],
            rules: [expectRule("mac-*")], now: now
        )
        let second = store()
        XCTAssertEqual(
            second.recordsByName(host: local)["mac-s1"]?.lastSeen, now,
            "lastSeen survives relaunch so absence spans restarts"
        )
        XCTAssertTrue(
            second.lastSuccess.isEmpty,
            "poll clocks are in-memory only — never alarm before the first fresh look"
        )
    }
}
