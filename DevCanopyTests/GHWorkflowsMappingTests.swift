@testable import DevCanopy
import XCTest

final class GHWorkflowsMappingTests: XCTestCase {
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

    func testRepoWorkflowHealthShortName() {
        let h = GHWorkflowsMapping.health(repo: "Sassy-Dog/velovate", runs: [])
        XCTAssertEqual(h.shortName, "velovate")
        XCTAssertTrue(h.isHealthy, "no runs = not failed, reachable")
        XCTAssertTrue(h.reachable, "the mapping path means the repo was fetched")
    }

    func testRunningRepoStillCountsHealthy() {
        // A repo with a build in flight (but no failure) must remain healthy —
        // running is activity, not a problem.
        let h = GHWorkflowsMapping.health(repo: "o/r", runs: [
            dto(name: "CI", status: "completed", conclusion: "success", branch: "main", event: "push"),
            dto(name: "CI", status: "in_progress", conclusion: nil, branch: "main", event: "push")
        ])
        XCTAssertFalse(h.running.isEmpty, "precondition: a run is in progress")
        XCTAssertTrue(h.isHealthy, "running does not make a repo unhealthy")
    }

    func testUnreachableRepoIsNeverHealthy() {
        let h = RepoWorkflowHealth.unreachable(repo: "Sassy-Dog/platform")
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
        let h = GHWorkflowsMapping.health(repo: "Sassy-Dog/velovate", runs: runs)
        XCTAssertEqual(h.main?.conclusion, .failure, "main = newest push-on-main run")
        XCTAssertEqual(h.main?.context, "main")
    }

    func testHealthLastPRPicksLatestPullRequestRun() {
        let runs = [
            makeCreated(dto(name: "CI", status: "completed", conclusion: "failure", branch: "feat/old", event: "pull_request"), "2026-05-29T10:00:00Z"),
            makeCreated(dto(name: "CI", status: "completed", conclusion: "success", branch: "feat/new", event: "pull_request"), "2026-05-29T12:00:00Z") // newer
        ]
        let h = GHWorkflowsMapping.health(repo: "o/r", runs: runs)
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
        let h = GHWorkflowsMapping.health(repo: "o/r", runs: runs, now: Self.now)
        XCTAssertEqual(h.running.count, 2, "both in_progress and a fresh queued run are 'running'")
        XCTAssertTrue(h.running.allSatisfy { $0.isRunning(now: Self.now) })
    }

    // MARK: - needs approval / stuck classification (issue #47)

    func testWaitingRunIsNeedsApprovalNotRunning() {
        let runs = [
            dto(name: "Release", status: "waiting", conclusion: nil, branch: "main", event: "push", id: 10),
            dto(name: "CI", status: "in_progress", conclusion: nil, branch: "main", event: "push", id: 11)
        ]
        let h = GHWorkflowsMapping.health(repo: "Sassy-Dog/velovate", runs: runs, now: Self.now)
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
            dto(
                name: "Release",
                status: "pending",
                conclusion: nil,
                branch: "main",
                event: "push",
                id: 20,
                createdAt: "2026-05-28T12:00:00Z"
            ) // ~24h before now
        ]
        let h = GHWorkflowsMapping.health(repo: "o/r", runs: runs, now: Self.now)
        XCTAssertEqual(h.stuck.count, 1, "a long-pending run reads as stuck")
        XCTAssertTrue(h.running.isEmpty, "a stuck run is not counted as running")
        XCTAssertTrue(h.stuck.first?.isStuck(now: Self.now) == true)
    }

    func testFreshPendingRunIsRunningNotStuck() {
        let runs = [
            dto(
                name: "Release",
                status: "pending",
                conclusion: nil,
                branch: "main",
                event: "push",
                id: 21,
                createdAt: "2026-05-29T12:00:00Z"
            ) // 5m before now
        ]
        let h = GHWorkflowsMapping.health(repo: "o/r", runs: runs, now: Self.now)
        XCTAssertEqual(h.running.count, 1, "a freshly pending run is still 'running'")
        XCTAssertTrue(h.stuck.isEmpty, "not stuck until past the threshold")
    }

    func testRunRefIsFailedAcrossFailingConclusions() {
        for c in ["failure", "timed_out", "cancelled", "startup_failure"] {
            let h = GHWorkflowsMapping.health(repo: "o/r", runs: [dto(name: "CI", status: "completed", conclusion: c, branch: "main", event: "push")])
            XCTAssertEqual(h.main?.isFailed, true, "\(c) should be a failure")
        }
        let ok = GHWorkflowsMapping.health(repo: "o/r", runs: [dto(name: "CI", status: "completed", conclusion: "success", branch: "main", event: "push")])
        XCTAssertEqual(ok.main?.isFailed, false)
    }

    func testRepoIsHealthyWhenNotFailed() {
        let clean = GHWorkflowsMapping.health(repo: "o/r", runs: [
            dto(name: "CI", status: "completed", conclusion: "success", branch: "main", event: "push"),
            dto(name: "CI", status: "completed", conclusion: "success", branch: "feat/x", event: "pull_request")
        ])
        XCTAssertTrue(clean.isHealthy)

        let failing = GHWorkflowsMapping.health(repo: "o/r", runs: [
            dto(name: "CI", status: "completed", conclusion: "failure", branch: "main", event: "push")
        ])
        XCTAssertFalse(failing.isHealthy)
    }

    // MARK: - watched workflows + event-gate relaxation (issue #76)

    /// The silent-failure incident: a `release.yml` run fires on `workflow_run`
    /// after `ci.yml`. With the legacy `event == "push"` gate it's dropped — green
    /// panel, failed release. Without a watch list, that's the (preserved) default.
    func testWorkflowRunFailureInvisibleWithoutWatchList() {
        let runs = [
            dto(name: "CI", status: "completed", conclusion: "success", branch: "main", event: "push", id: 1),
            dto(name: "Release", status: "completed", conclusion: "failure", branch: "main", event: "workflow_run", id: 2)
        ]
        let h = GHWorkflowsMapping.health(repo: "Sassy-Dog/velovate", runs: runs)
        XCTAssertTrue(h.isHealthy, "default view ignores the workflow_run failure (legacy behavior preserved)")
        XCTAssertEqual(h.main?.title, "CI")
    }

    /// Adding `release.yml` to the watch list relaxes the event gate so the
    /// `workflow_run` failure on main reddens the repo and names the workflow.
    func testWatchedReleaseWorkflowRunFailureRedensRepo() {
        let runs = [
            dto(name: "CI", status: "completed", conclusion: "success", branch: "main", event: "push", id: 1),
            dto(name: "Release", status: "completed", conclusion: "failure", branch: "main", event: "workflow_run", id: 2)
        ]
        let h = GHWorkflowsMapping.health(
            repo: "Sassy-Dog/velovate",
            runs: runs,
            watchedWorkflows: ["release.yml"]
        )
        XCTAssertFalse(h.isHealthy, "watched release failure must redden the repo")
        XCTAssertEqual(h.main?.conclusion, .failure)
        XCTAssertEqual(h.main?.title, "Release", "the failing workflow is named")
    }

    /// `schedule`-triggered runs on the default branch also count when watched.
    func testWatchedScheduleFailureRedensRepo() {
        let runs = [
            dto(name: "Nightly", status: "completed", conclusion: "failure", branch: "main", event: "schedule", id: 1)
        ]
        let h = GHWorkflowsMapping.health(repo: "o/r", runs: runs, watchedWorkflows: ["Nightly"])
        XCTAssertFalse(h.isHealthy)
        XCTAssertEqual(h.main?.title, "Nightly")
    }

    /// Watch-list match is case-insensitive and extension-agnostic: `release.yml`,
    /// `Release.yaml`, and `Release` all match a run named "Release".
    func testWatchedMatchIsCaseAndExtensionInsensitive() {
        let runs = [
            dto(name: "Release", status: "completed", conclusion: "failure", branch: "main", event: "workflow_run", id: 1)
        ]
        for key in ["release.yml", "Release.yaml", "RELEASE", "release"] {
            let h = GHWorkflowsMapping.health(repo: "o/r", runs: runs, watchedWorkflows: [key])
            XCTAssertFalse(h.isHealthy, "key \(key) should match the 'Release' run")
        }
    }

    /// When a watch list is set, non-watched workflows are filtered out — a
    /// passing `ci.yml` doesn't mask a failing watched `release.yml`, and a
    /// failing non-watched workflow doesn't redden a repo that opted into only
    /// specific workflows.
    func testWatchListFiltersOutNonWatchedWorkflows() {
        let runs = [
            dto(name: "CI", status: "completed", conclusion: "failure", branch: "main", event: "push", id: 1),
            dto(name: "Release", status: "completed", conclusion: "success", branch: "main", event: "workflow_run", id: 2)
        ]
        // Only "Release" is watched; the failing "CI" run is out of scope.
        let h = GHWorkflowsMapping.health(repo: "o/r", runs: runs, watchedWorkflows: ["release.yml"])
        XCTAssertTrue(h.isHealthy, "non-watched CI failure is filtered out when a watch list is set")
        XCTAssertEqual(h.main?.title, "Release")
    }

    /// An empty watch list is identical to no watch list (default push+PR view).
    func testEmptyWatchListPreservesDefaultBehavior() {
        let runs = [
            dto(name: "Release", status: "completed", conclusion: "failure", branch: "main", event: "workflow_run", id: 1),
            dto(name: "CI", status: "completed", conclusion: "success", branch: "main", event: "push", id: 2)
        ]
        let h = GHWorkflowsMapping.health(repo: "o/r", runs: runs, watchedWorkflows: [])
        XCTAssertTrue(h.isHealthy, "empty list == legacy behavior; workflow_run failure ignored")
        XCTAssertEqual(h.main?.title, "CI")
    }

    /// A watched workflow_run failure on a non-default branch must not feed the
    /// main slot — only default-branch runs count toward repo health.
    func testWatchedRunOffDefaultBranchDoesNotFeedMain() {
        let runs = [
            dto(name: "Release", status: "completed", conclusion: "failure", branch: "release/1.2", event: "workflow_run", id: 1)
        ]
        let h = GHWorkflowsMapping.health(repo: "o/r", runs: runs, watchedWorkflows: ["release.yml"])
        XCTAssertNil(h.main, "a non-main watched run must not populate the main slot")
        XCTAssertTrue(h.isHealthy)
    }

    // MARK: - open issue / PR counts

    /// GitHub's issues endpoint counts PRs, so the pure open-issue count is the
    /// inclusive total minus the open-PR count.
    func testPureOpenIssueCountSubtractsPullRequests() {
        XCTAssertEqual(GHWorkflowsMapping.pureOpenIssueCount(openIssuesIncludingPRs: 10, openPullRequests: 3), 7)
    }

    /// The issues and PRs totals are sampled from two separate requests, so a
    /// transient skew could make PRs momentarily exceed the inclusive total —
    /// clamp at 0 rather than show a negative.
    func testPureOpenIssueCountClampsAtZero() {
        XCTAssertEqual(GHWorkflowsMapping.pureOpenIssueCount(openIssuesIncludingPRs: 2, openPullRequests: 5), 0)
    }

    /// A failed fetch on either input yields nil (renders "—"), never a wrong number.
    func testPureOpenIssueCountNilWhenEitherInputMissing() {
        XCTAssertNil(GHWorkflowsMapping.pureOpenIssueCount(openIssuesIncludingPRs: nil, openPullRequests: 3))
        XCTAssertNil(GHWorkflowsMapping.pureOpenIssueCount(openIssuesIncludingPRs: 10, openPullRequests: nil))
        XCTAssertNil(GHWorkflowsMapping.pureOpenIssueCount(openIssuesIncludingPRs: nil, openPullRequests: nil))
    }

    /// health() threads the pure issue count and the raw PR count onto the model.
    func testHealthThreadsOpenIssueAndPRCounts() {
        let h = GHWorkflowsMapping.health(
            repo: "o/r",
            runs: [],
            openIssuesIncludingPRs: 12,
            openPullRequests: 4
        )
        XCTAssertEqual(h.openIssues, 8, "12 inclusive − 4 PRs = 8 pure issues")
        XCTAssertEqual(h.openPRs, 4)
    }

    /// When the side-count fetches fail (nil), the counts are nil, not zero.
    func testHealthLeavesOpenCountsNilWhenNotFetched() {
        let h = GHWorkflowsMapping.health(repo: "o/r", runs: [])
        XCTAssertNil(h.openIssues)
        XCTAssertNil(h.openPRs)
    }

    /// An unreachable repo carries nil issue/PR counts (no data to show).
    func testUnreachableRepoHasNilOpenCounts() {
        let h = RepoWorkflowHealth.unreachable(repo: "o/r")
        XCTAssertNil(h.openIssues)
        XCTAssertNil(h.openPRs)
    }
}
