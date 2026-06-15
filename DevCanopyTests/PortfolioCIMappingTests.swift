import XCTest
@testable import DevCanopy

final class PortfolioCIMappingTests: XCTestCase {

    /// A fixed "now" close to the default DTO timestamps so that, unless a test
    /// deliberately ages a run, queued/pending runs read as running (not stuck).
    private static let now = ISO8601DateFormatter().date(from: "2026-05-29T12:05:00Z")!

    private func dto(
        name: String,
        status: String,
        conclusion: String?,
        branch: String? = "main",
        event: String = "push",
        title: String = "a commit",
        id: Int64 = 1,
        createdAt: String = "2026-05-29T12:00:00Z",
        runStartedAt: String? = nil
    ) -> WorkflowRunDTO {
        WorkflowRunDTO(
            id: id,
            name: name,
            nodeId: "n",
            headBranch: branch,
            headSha: "sha",
            runNumber: 1,
            workflowId: 1,
            event: event,
            status: status,
            conclusion: conclusion,
            workflowUrl: "wf",
            htmlUrl: "https://github.com/Sassy-Dog/velovate/actions/runs/1",
            createdAt: createdAt,
            updatedAt: "2026-05-29T12:05:00Z",
            runStartedAt: runStartedAt,
            runAttemptedAt: nil,
            actor: nil,
            triggeringActor: nil,
            displayTitle: title
        )
    }

    // MARK: - Short name

    func testRepoCIHealthShortName() {
        let h = PortfolioCIMapping.health(repo: "Sassy-Dog/velovate", runs: [])
        XCTAssertEqual(h.shortName, "velovate")
        XCTAssertTrue(h.isHealthy, "no runs = not failed, reachable")
        XCTAssertTrue(h.reachable, "the mapping path means the repo was fetched")
    }

    func testRunningRepoStillCountsHealthy() {
        // A repo with a build in flight (but no failure) must remain healthy —
        // running is activity, not a problem.
        let h = PortfolioCIMapping.health(repo: "o/r", runs: [
            dto(name: "CI", status: "completed", conclusion: "success", branch: "main", event: "push"),
            dto(name: "CI", status: "in_progress", conclusion: nil, branch: "main", event: "push")
        ])
        XCTAssertFalse(h.running.isEmpty, "precondition: a run is in progress")
        XCTAssertTrue(h.isHealthy, "running does not make a repo unhealthy")
    }

    func testUnreachableRepoIsNeverHealthy() {
        let h = RepoCIHealth.unreachable(repo: "Sassy-Dog/platform")
        XCTAssertFalse(h.reachable)
        XCTAssertFalse(h.isHealthy, "an unreachable repo must not count as healthy")
        XCTAssertEqual(h.shortName, "platform")
    }

    private func makeCreated(_ d: WorkflowRunDTO, _ created: String) -> WorkflowRunDTO {
        WorkflowRunDTO(
            id: d.id, name: d.name, nodeId: d.nodeId, headBranch: d.headBranch,
            headSha: d.headSha, runNumber: d.runNumber, workflowId: d.workflowId,
            event: d.event, status: d.status, conclusion: d.conclusion,
            workflowUrl: d.workflowUrl, htmlUrl: d.htmlUrl, createdAt: created,
            updatedAt: d.updatedAt, runStartedAt: d.runStartedAt,
            runAttemptedAt: d.runAttemptedAt, actor: d.actor,
            triggeringActor: d.triggeringActor, displayTitle: d.displayTitle
        )
    }

    // MARK: - health() categorization

    func testHealthMainPicksLatestPushOnMain() {
        let runs = [
            makeCreated(dto(name: "CI", status: "completed", conclusion: "success", branch: "main", event: "push"), "2026-05-29T10:00:00Z"),
            makeCreated(dto(name: "CI", status: "completed", conclusion: "failure", branch: "main", event: "push"), "2026-05-29T11:00:00Z"), // newer
            makeCreated(dto(name: "CI", status: "completed", conclusion: "success", branch: "feat/x", event: "pull_request"), "2026-05-29T12:00:00Z")
        ]
        let h = PortfolioCIMapping.health(repo: "Sassy-Dog/velovate", runs: runs)
        XCTAssertEqual(h.main?.conclusion, .failure, "main = newest push-on-main run")
        XCTAssertEqual(h.main?.context, "main")
    }

    func testHealthLastPRPicksLatestPullRequestRun() {
        let runs = [
            makeCreated(dto(name: "CI", status: "completed", conclusion: "failure", branch: "feat/old", event: "pull_request"), "2026-05-29T10:00:00Z"),
            makeCreated(dto(name: "CI", status: "completed", conclusion: "success", branch: "feat/new", event: "pull_request"), "2026-05-29T12:00:00Z") // newer
        ]
        let h = PortfolioCIMapping.health(repo: "o/r", runs: runs)
        XCTAssertEqual(h.lastPR?.conclusion, .success)
        XCTAssertEqual(h.lastPR?.context, "feat/new")
    }

    func testHealthRunningCollectsInProgressRuns() {
        // Use a "now" right after the runs were created so the fresh queued run
        // reads as running, not stuck.
        let runs = [
            dto(name: "CI", status: "in_progress", conclusion: nil, branch: "main", event: "push", id: 1),
            dto(name: "Deploy", status: "queued", conclusion: nil, branch: "main", event: "workflow_dispatch", id: 2),
            dto(name: "CI", status: "completed", conclusion: "success", branch: "main", event: "push", id: 3)
        ]
        let h = PortfolioCIMapping.health(repo: "o/r", runs: runs, now: Self.now)
        XCTAssertEqual(h.running.count, 2, "both in_progress and a fresh queued run are 'running'")
        XCTAssertTrue(h.running.allSatisfy { $0.isRunning(now: Self.now) })
    }

    // MARK: - needs approval / stuck classification (issue #47)

    func testWaitingRunIsNeedsApprovalNotRunning() {
        let runs = [
            dto(name: "Release", status: "waiting", conclusion: nil, branch: "main", event: "push", id: 10),
            dto(name: "CI", status: "in_progress", conclusion: nil, branch: "main", event: "push", id: 11)
        ]
        let h = PortfolioCIMapping.health(repo: "Sassy-Dog/velovate", runs: runs, now: Self.now)
        XCTAssertEqual(h.needsApproval.count, 1, "waiting run is a needs-approval item")
        XCTAssertEqual(h.needsApproval.first?.title, "Release")
        XCTAssertTrue(h.needsApproval.first?.needsApproval == true)
        XCTAssertEqual(h.running.count, 1, "only the in_progress run is running")
        XCTAssertFalse(h.running.contains { $0.status == .waiting }, "waiting must not be 'running'")
        XCTAssertTrue(h.isHealthy, "a parked-for-approval repo isn't a failure")
    }

    func testStalePendingRunIsStuckNotRunning() {
        // A pending run created well over an hour before "now" with no jobs.
        let runs = [
            dto(name: "Release", status: "pending", conclusion: nil, branch: "main",
                event: "push", id: 20, createdAt: "2026-05-28T12:00:00Z") // ~24h before now
        ]
        let h = PortfolioCIMapping.health(repo: "o/r", runs: runs, now: Self.now)
        XCTAssertEqual(h.stuck.count, 1, "a long-pending run reads as stuck")
        XCTAssertTrue(h.running.isEmpty, "a stuck run is not counted as running")
        XCTAssertTrue(h.stuck.first?.isStuck(now: Self.now) == true)
    }

    func testFreshPendingRunIsRunningNotStuck() {
        let runs = [
            dto(name: "Release", status: "pending", conclusion: nil, branch: "main",
                event: "push", id: 21, createdAt: "2026-05-29T12:00:00Z") // 5m before now
        ]
        let h = PortfolioCIMapping.health(repo: "o/r", runs: runs, now: Self.now)
        XCTAssertEqual(h.running.count, 1, "a freshly pending run is still 'running'")
        XCTAssertTrue(h.stuck.isEmpty, "not stuck until past the threshold")
    }

    func testRunRefIsFailedAcrossFailingConclusions() {
        for c in ["failure", "timed_out", "cancelled", "startup_failure"] {
            let h = PortfolioCIMapping.health(repo: "o/r", runs: [dto(name: "CI", status: "completed", conclusion: c, branch: "main", event: "push")])
            XCTAssertEqual(h.main?.isFailed, true, "\(c) should be a failure")
        }
        let ok = PortfolioCIMapping.health(repo: "o/r", runs: [dto(name: "CI", status: "completed", conclusion: "success", branch: "main", event: "push")])
        XCTAssertEqual(ok.main?.isFailed, false)
    }

    func testRepoIsHealthyWhenNotFailed() {
        let clean = PortfolioCIMapping.health(repo: "o/r", runs: [
            dto(name: "CI", status: "completed", conclusion: "success", branch: "main", event: "push"),
            dto(name: "CI", status: "completed", conclusion: "success", branch: "feat/x", event: "pull_request")
        ])
        XCTAssertTrue(clean.isHealthy)

        let failing = PortfolioCIMapping.health(repo: "o/r", runs: [
            dto(name: "CI", status: "completed", conclusion: "failure", branch: "main", event: "push")
        ])
        XCTAssertFalse(failing.isHealthy)
    }
}
