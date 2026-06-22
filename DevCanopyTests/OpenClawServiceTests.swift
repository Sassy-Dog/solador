@testable import DevCanopy
import XCTest

/// Lifecycle/wiring coverage for the service + coordinator that doesn't require a
/// live socket. The frame-reduction logic itself is covered by
/// `OpenClawSnapshotReducerTests`.
@MainActor
final class OpenClawServiceTests: XCTestCase {
    func testStartsIdleWhenNoURLConfigured() {
        let service = OpenClawService(urlProvider: { nil }, tokenProvider: { nil })
        XCTAssertEqual(service.snapshot.connection, .idle)
        XCTAssertEqual(service.snapshot.displayName, "OpenClaw")
        XCTAssertEqual(service.snapshot.id, "openclaw")

        // No URL → start() must not attempt a connection; it stays idle.
        service.start()
        XCTAssertEqual(service.snapshot.connection, .idle)
        service.stop()
        XCTAssertEqual(service.snapshot.connection, .idle)
    }

    func testEmptyURLTreatedAsUnconfigured() {
        let service = OpenClawService(urlProvider: { "" }, tokenProvider: { nil })
        service.start()
        XCTAssertEqual(service.snapshot.connection, .idle)
    }

    func testCoordinatorSeedsSnapshotsFromRuntimes() {
        let service = OpenClawService(urlProvider: { nil }, tokenProvider: { nil })
        let coordinator = AgentRuntimeCoordinator(runtimes: [service])
        XCTAssertEqual(coordinator.snapshots.count, 1)
        XCTAssertEqual(coordinator.snapshots.first?.displayName, "OpenClaw")
    }

    func testSnapshotPublisherEmitsCurrentValue() {
        let service = OpenClawService(urlProvider: { nil }, tokenProvider: { nil })
        var received: AgentRuntimeSnapshot?
        let cancellable = service.snapshotPublisher.sink { received = $0 }
        XCTAssertEqual(received?.id, "openclaw")
        cancellable.cancel()
    }
}
