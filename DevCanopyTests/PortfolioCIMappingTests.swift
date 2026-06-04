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

    func testRepoShortNameTakesPartAfterSlash() {
        let status = PortfolioCIMapping.map(repo: "Sassy-Dog/velovate", runs: [])
        XCTAssertEqual(status.shortName, "velovate")
    }

    // MARK: - CI selection (latest run picked)

    func testMapPicksLatestCIRunByCreatedAt() {
        let r1 = dto(name: "CI", status: "completed", conclusion: "success")
        let r2 = dto(name: "CI", status: "completed", conclusion: "failure") // newer
        let mapped = PortfolioCIMapping.map(
            repo: "Sassy-Dog/velovate",
            runs: [makeCreated(r1, "2026-05-29T10:00:00Z"), makeCreated(r2, "2026-05-29T11:00:00Z")]
        )
        XCTAssertEqual(mapped.ciConclusion, .failure)
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

    // MARK: - Health derivation

    func testSuccessHealthIsGood() {
        let s = PortfolioCIMapping.map(repo: "o/r", runs: [dto(name: "CI", status: "completed", conclusion: "success")])
        XCTAssertEqual(s.health, .good)
    }

    func testFailureHealthIsBad() {
        let s = PortfolioCIMapping.map(repo: "o/r", runs: [dto(name: "CI", status: "completed", conclusion: "failure")])
        XCTAssertEqual(s.health, .bad)
    }

    func testInProgressHealthIsRunning() {
        let s = PortfolioCIMapping.map(repo: "o/r", runs: [dto(name: "CI", status: "in_progress", conclusion: nil)])
        XCTAssertEqual(s.health, .running)
    }

    func testNoRunsHealthIsUnknown() {
        let s = PortfolioCIMapping.map(repo: "o/r", runs: [])
        XCTAssertEqual(s.health, .unknown)
        XCTAssertNil(s.ciConclusion)
    }

    // MARK: - Release detection

    func testReleaseRunDetectedSeparatelyFromCI() {
        let runs = [
            makeCreated(dto(name: "CI", status: "completed", conclusion: "success"), "2026-05-29T10:00:00Z"),
            makeCreated(dto(name: "Release", status: "completed", conclusion: "failure"), "2026-05-29T11:00:00Z")
        ]
        let s = PortfolioCIMapping.map(repo: "o/r", runs: runs)
        // CI health stays good (release failing shouldn't override CI)
        XCTAssertEqual(s.ciConclusion, .success)
        XCTAssertEqual(s.releaseConclusion, .failure)
    }
}
