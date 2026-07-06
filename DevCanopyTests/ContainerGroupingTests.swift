@testable import DevCanopy
import XCTest

final class ContainerGroupingTests: XCTestCase {
    // MARK: - Helpers

    private func container(
        _ name: String,
        running: Bool = true,
        runtime: ContainerRuntime = .podman
    ) -> ContainerInfo {
        ContainerInfo(
            name: name,
            statusText: running ? "Up 5 minutes" : "Exited (0) 2 hours ago",
            isRunning: running,
            runtime: runtime,
            image: nil
        )
    }

    private func rule(_ pattern: String, _ label: String) -> ContainerGroupRule {
        ContainerGroupRule(pattern: pattern, label: label)
    }

    /// Partition tests inject a plain prefix matcher so they are independent of the
    /// glob implementation.
    private let prefixMatcher: (String, String) -> Bool = { name, pattern in
        name.hasPrefix(pattern)
    }

    // MARK: - Persistence

    func testLoadFromEmptyDataYieldsSeededDefaults() {
        XCTAssertEqual(ContainerGroupRule.load(from: Data()), ContainerGroupRule.seededDefaults)
    }

    func testLoadFromGarbageDataYieldsSeededDefaults() {
        let garbage = Data("not json".utf8)
        XCTAssertEqual(ContainerGroupRule.load(from: garbage), ContainerGroupRule.seededDefaults)
    }

    func testClearedRulesRoundTripAsEmptyNotSeeded() {
        let data = ContainerGroupRule.encode([])
        XCTAssertFalse(data.isEmpty, "encoded empty list must be distinguishable from unset")
        XCTAssertEqual(ContainerGroupRule.load(from: data), [])
    }

    func testRuleIdentitySurvivesRoundTrip() {
        let original = [rule("api-*", "workflow jobs"), rule("ghr-*", "runners")]
        let decoded = ContainerGroupRule.load(from: ContainerGroupRule.encode(original))
        XCTAssertEqual(decoded, original)
        XCTAssertEqual(decoded.map(\.id), original.map(\.id))
    }

    // MARK: - Partition (injected prefix matcher)

    func testIndividualsSortByNameRegardlessOfArrivalOrder() {
        let input = [container("charlie"), container("alpha"), container("bravo")]
        let result = ContainerGrouping.partition(input, rules: [], host: "h", matcher: prefixMatcher)
        XCTAssertEqual(result.individual.map(\.name), ["alpha", "bravo", "charlie"])
        XCTAssertTrue(result.aggregates.isEmpty)
    }

    func testSameNameDifferentRuntimeSortsDeterministically() {
        let input = [container("x", runtime: .tart), container("x", runtime: .docker)]
        let result = ContainerGrouping.partition(input, rules: [], host: "h", matcher: prefixMatcher)
        XCTAssertEqual(result.individual.map(\.runtime), [.docker, .tart])
    }

    func testAggregateCountsTotalAndRunning() {
        let input = [
            container("ghr-1", running: true),
            container("ghr-2", running: false),
            container("postgres")
        ]
        let result = ContainerGrouping.partition(
            input, rules: [rule("ghr-", "runners")], host: "h", matcher: prefixMatcher
        )
        XCTAssertEqual(result.individual.map(\.name), ["postgres"])
        XCTAssertEqual(result.aggregates.count, 1)
        XCTAssertEqual(result.aggregates[0].total, 2)
        XCTAssertEqual(result.aggregates[0].runningCount, 1)
    }

    func testAllStoppedAggregateHasZeroRunning() {
        let input = [container("ghr-1", running: false), container("ghr-2", running: false)]
        let result = ContainerGrouping.partition(
            input, rules: [rule("ghr-", "runners")], host: "h", matcher: prefixMatcher
        )
        XCTAssertEqual(result.aggregates[0].runningCount, 0)
        XCTAssertEqual(result.aggregates[0].total, 2)
    }

    func testZeroMatchCollapseRuleRendersZeroAggregate() {
        let input = [container("postgres")]
        let standing = rule("ghr-", "runners")
        let result = ContainerGrouping.partition(
            input, rules: [standing], host: "h", matcher: prefixMatcher
        )
        XCTAssertEqual(result.individual.map(\.name), ["postgres"])
        XCTAssertEqual(result.aggregates.count, 1)
        XCTAssertEqual(result.aggregates[0].id, standing.id)
        XCTAssertEqual(result.aggregates[0].label, "runners")
        XCTAssertEqual(result.aggregates[0].total, 0)
        XCTAssertEqual(result.aggregates[0].runningCount, 0)
        XCTAssertNil(result.aggregates[0].dominantRuntime, "an empty group has no runtime to show")
    }

    func testFirstMatchingRuleWinsOnOverlap() {
        let input = [container("api-123")]
        let broad = rule("api-", "jobs")
        let alsoMatches = rule("api-1", "specific")
        let result = ContainerGrouping.partition(
            input, rules: [broad, alsoMatches], host: "h", matcher: prefixMatcher
        )
        XCTAssertEqual(result.aggregates.map(\.id), [broad.id, alsoMatches.id])
        XCTAssertEqual(
            result.aggregates.map(\.total), [1, 0],
            "the loser of first-match-wins keeps its standing row at zero"
        )
    }

    func testDominantRuntimeIsMostFrequent() {
        let input = [
            container("ghr-1", runtime: .docker),
            container("ghr-2", runtime: .docker),
            container("ghr-3", runtime: .podman)
        ]
        let result = ContainerGrouping.partition(
            input, rules: [rule("ghr-", "runners")], host: "h", matcher: prefixMatcher
        )
        XCTAssertEqual(result.aggregates[0].dominantRuntime, .docker)
    }

    func testEmptyRulesLeaveEverythingIndividual() {
        let input = [container("api-123"), container("ghr-1")]
        let result = ContainerGrouping.partition(input, rules: [], host: "h", matcher: prefixMatcher)
        XCTAssertEqual(result.individual.count, 2)
        XCTAssertTrue(result.aggregates.isEmpty)
    }

    func testAggregatesFollowRuleOrderNotMatchOrder() {
        let input = [container("api-1"), container("ghr-1")]
        let rules = [rule("ghr-", "runners"), rule("api-", "jobs")]
        let result = ContainerGrouping.partition(input, rules: rules, host: "h", matcher: prefixMatcher)
        XCTAssertEqual(result.aggregates.map(\.label), ["runners", "jobs"])
    }

    // MARK: - Host scoping

    func testScopedRuleAppliesOnlyOnItsHost() {
        let scoped = ContainerGroupRule(pattern: "ghr-", label: "runners", host: "ubu")
        let input = [container("ghr-1")]

        let onHost = ContainerGrouping.partition(
            input, rules: [scoped], host: "ubu", matcher: prefixMatcher
        )
        XCTAssertTrue(onHost.individual.isEmpty)
        XCTAssertEqual(onHost.aggregates.map(\.total), [1])

        let elsewhere = ContainerGrouping.partition(
            input, rules: [scoped], host: "mac", matcher: prefixMatcher
        )
        XCTAssertTrue(elsewhere.aggregates.isEmpty, "no standing row off the scoped host")
        XCTAssertEqual(
            elsewhere.individual.map(\.name), ["ghr-1"],
            "off-host, would-be matches render individually"
        )
    }

    func testScopedHideHidesOnlyOnItsHost() {
        let hide = ContainerGroupRule(pattern: "ghcr.io/", label: "", action: .hide, host: "mac")
        let input = [container("ghcr.io/base:1")]

        let onHost = ContainerGrouping.partition(
            input, rules: [hide], host: "mac", matcher: prefixMatcher
        )
        XCTAssertTrue(onHost.individual.isEmpty)

        let elsewhere = ContainerGrouping.partition(
            input, rules: [hide], host: "ubu", matcher: prefixMatcher
        )
        XCTAssertEqual(elsewhere.individual.map(\.name), ["ghcr.io/base:1"])
    }

    func testHostScopeSurvivesRoundTrip() {
        let original = [ContainerGroupRule(pattern: "api-*", label: "jobs", host: "ubu-3xdv")]
        let decoded = ContainerGroupRule.load(from: ContainerGroupRule.encode(original))
        XCTAssertEqual(decoded, original)
        XCTAssertEqual(decoded[0].host, "ubu-3xdv")
    }

    // MARK: - Hide rules & migration

    func testLegacyRulesWithoutActionOrHostDecodeAsCollapseAllHosts() {
        let legacy = Data("""
        [{"id":"11111111-2222-3333-4444-555555555555","pattern":"api-*","label":"workflow jobs"}]
        """.utf8)
        let rules = ContainerGroupRule.load(from: legacy)
        XCTAssertEqual(rules.count, 1)
        XCTAssertEqual(rules[0].action, .collapse)
        XCTAssertNil(rules[0].host)
        XCTAssertEqual(rules[0].pattern, "api-*")
    }

    func testHideActionSurvivesRoundTrip() {
        let original = [ContainerGroupRule(pattern: "ghcr.io/*", label: "", action: .hide)]
        let decoded = ContainerGroupRule.load(from: ContainerGroupRule.encode(original))
        XCTAssertEqual(decoded, original)
        XCTAssertEqual(decoded[0].action, .hide)
    }

    func testHideRuleDropsMatchesFromRowsAndAggregates() {
        let input = [container("ghcr.io/base:1", running: false), container("postgres")]
        let hide = ContainerGroupRule(pattern: "ghcr.io/", label: "", action: .hide)
        let result = ContainerGrouping.partition(input, rules: [hide], host: "h", matcher: prefixMatcher)
        XCTAssertEqual(result.individual.map(\.name), ["postgres"])
        XCTAssertTrue(result.aggregates.isEmpty, "hide rules never emit a row")
    }

    func testRuleOrderArbitratesCollapseVersusHide() {
        let input = [container("api-1")]
        let collapse = rule("api-", "jobs")
        let hide = ContainerGroupRule(pattern: "api-", label: "", action: .hide)

        let collapsed = ContainerGrouping.partition(
            input, rules: [collapse, hide], host: "h", matcher: prefixMatcher
        )
        XCTAssertEqual(collapsed.aggregates.map(\.label), ["jobs"])
        XCTAssertEqual(collapsed.aggregates.map(\.total), [1])

        let hidden = ContainerGrouping.partition(
            input, rules: [hide, collapse], host: "h", matcher: prefixMatcher
        )
        XCTAssertTrue(hidden.individual.isEmpty)
        XCTAssertEqual(
            hidden.aggregates.map(\.total), [0],
            "the collapse rule keeps its standing row; its would-be member was hidden first"
        )
    }

    // MARK: - Glob matching (executable spec for ContainerGrouping.matches)

    func testGlobExactLiteralMatch() {
        XCTAssertTrue(ContainerGrouping.matches(name: "api", pattern: "api"))
        XCTAssertFalse(ContainerGrouping.matches(name: "apix", pattern: "api"))
        XCTAssertFalse(ContainerGrouping.matches(name: "xapi", pattern: "api"))
    }

    func testGlobTrailingStar() {
        XCTAssertTrue(ContainerGrouping.matches(name: "api-123", pattern: "api-*"))
        XCTAssertTrue(ContainerGrouping.matches(name: "api-", pattern: "api-*"))
        XCTAssertFalse(ContainerGrouping.matches(name: "api", pattern: "api-*"))
        XCTAssertFalse(ContainerGrouping.matches(name: "capi-1", pattern: "api-*"))
    }

    func testGlobSeededRunnerPattern() {
        XCTAssertTrue(ContainerGrouping.matches(
            name: "sassydog-ghr-ubu-492d", pattern: "sassydog-ghr-ubu-*"
        ))
        XCTAssertFalse(ContainerGrouping.matches(
            name: "sassydog-ghr-mac-s1", pattern: "sassydog-ghr-ubu-*"
        ))
    }

    func testGlobMultipleStars() {
        XCTAssertTrue(ContainerGrouping.matches(name: "sassydog-ghr-ubu-1", pattern: "*-ghr-*"))
        XCTAssertFalse(ContainerGrouping.matches(name: "sassydog-runner", pattern: "*-ghr-*"))
    }

    func testGlobEscapesRegexMetacharacters() {
        XCTAssertTrue(ContainerGrouping.matches(name: "web.123", pattern: "web.1*"))
        XCTAssertFalse(ContainerGrouping.matches(name: "webX1x", pattern: "web.1*"))
    }

    func testGlobIsCaseSensitive() {
        XCTAssertFalse(ContainerGrouping.matches(name: "api-1", pattern: "API-*"))
    }

    func testGlobBareStarMatchesEverything() {
        XCTAssertTrue(ContainerGrouping.matches(name: "anything", pattern: "*"))
        XCTAssertTrue(ContainerGrouping.matches(name: "", pattern: "*"))
    }

    func testGlobEmptyPatternMatchesOnlyEmptyName() {
        XCTAssertTrue(ContainerGrouping.matches(name: "", pattern: ""))
        XCTAssertFalse(ContainerGrouping.matches(name: "x", pattern: ""))
    }
}
