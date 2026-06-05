import XCTest
@testable import DevCanopy

final class PortfolioCIMappingTests: XCTestCase {

    private func dto(
        name: String,
        status: String,
        conclusion: String?,
        branch: String? = "main",
        event: String = "push",
        title: String = "a commit"
    ) -> WorkflowRunDTO {
        WorkflowRunDTO(
            id: 1,
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
            createdAt: "2026-05-29T12:00:00Z",
            updatedAt: "2026-05-29T12:05:00Z",
            runStartedAt: nil,
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
        let runs = [
            dto(name: "CI", status: "in_progress", conclusion: nil, branch: "main", event: "push"),
            dto(name: "Deploy", status: "queued", conclusion: nil, branch: "main", event: "workflow_dispatch"),
            dto(name: "CI", status: "completed", conclusion: "success", branch: "main", event: "push")
        ]
        let h = PortfolioCIMapping.health(repo: "o/r", runs: runs)
        XCTAssertEqual(h.running.count, 2, "both in_progress and queued runs are 'running'")
        XCTAssertTrue(h.running.allSatisfy { $0.isRunning })
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
