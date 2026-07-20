@testable import DevCanopy
import XCTest

final class LocalContainerMergeTests: XCTestCase {
    private func container(_ name: String, runtime: ContainerRuntime) -> ContainerInfo {
        ContainerInfo(name: name, statusText: "running", isRunning: true, runtime: runtime, image: nil)
    }

    func testSuccessContributesFreshAndUpdatesLastKnown() {
        let fresh = [container("s1", runtime: .tart)]
        let result = LocalContainerMerge.merge(results: [(.tart, fresh)], lastKnown: [:])
        XCTAssertEqual(result.merged, fresh)
        XCTAssertEqual(result.updatedLastKnown[.tart], fresh)
        XCTAssertEqual(result.succeeded, [.tart])
        XCTAssertTrue(result.errored.isEmpty)
    }

    func testFailureContributesLastKnownForThatRuntime() {
        let known = [container("s1", runtime: .tart), container("s2", runtime: .tart)]
        let result = LocalContainerMerge.merge(results: [(.tart, nil)], lastKnown: [.tart: known])
        XCTAssertEqual(result.merged, known, "a transient tart failure must not blank the VM rows")
        XCTAssertEqual(result.updatedLastKnown[.tart], known)
        XCTAssertEqual(result.errored, ["tart"])
        XCTAssertTrue(result.succeeded.isEmpty)
    }

    func testFailureWithNoHistoryContributesNothing() {
        let result = LocalContainerMerge.merge(results: [(.docker, nil)], lastKnown: [:])
        XCTAssertTrue(result.merged.isEmpty)
        XCTAssertEqual(result.errored, ["docker"])
    }

    func testSuccessReplacesStaleLastKnownWholesale() {
        let stale = [container("old", runtime: .tart)]
        let fresh = [container("new", runtime: .tart)]
        let result = LocalContainerMerge.merge(results: [(.tart, fresh)], lastKnown: [.tart: stale])
        XCTAssertEqual(result.merged, fresh)
        XCTAssertEqual(result.updatedLastKnown[.tart], fresh, "success replaces, never unions")
    }

    func testMixedResultsMergeFreshWithRetained() {
        let freshPodman = [container("pg", runtime: .podman)]
        let knownTart = [container("s1", runtime: .tart)]
        let result = LocalContainerMerge.merge(
            results: [(.podman, freshPodman), (.tart, nil)],
            lastKnown: [.tart: knownTart]
        )
        XCTAssertEqual(result.merged, freshPodman + knownTart)
        XCTAssertEqual(result.succeeded, [.podman])
        XCTAssertEqual(result.errored, ["tart"])
    }

    func testNotAttemptedRuntimeKeepsCacheButContributesNothing() {
        let knownDocker = [container("web", runtime: .docker)]
        let result = LocalContainerMerge.merge(results: [(.tart, nil)], lastKnown: [.docker: knownDocker])
        XCTAssertTrue(result.merged.isEmpty, "a runtime that vanished from PATH contributes nothing")
        XCTAssertEqual(result.updatedLastKnown[.docker], knownDocker, "but its cache survives a comeback")
    }
}
