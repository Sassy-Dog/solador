//! Workflow-run DTOs and the pure mapping to per-repo health. Port of
//! `DevCanopy/Models/WorkflowRunModels.swift` (the status/conclusion enums) and
//! `DevCanopy/Services/GitHub/GHWorkflowsMapping.swift`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The branch a repo's `main` health slot is read from.
pub const DEFAULT_BRANCH: &str = "main";

/// A `pending`/`queued` run older than this with no jobs dispatched reads as
/// STUCK/BLOCKED rather than a healthy long-running job (the 17h51m incident).
pub const STUCK_THRESHOLD_SECS: i64 = 3_600;

/// `GET /repos/{slug}/actions/runs`.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowRunsResponse {
    pub total_count: u32,
    pub workflow_runs: Vec<WorkflowRun>,
}

/// One workflow run as GitHub reports it.
///
/// Only the fields the mapping actually reads appear, and only the ones GitHub
/// always populates are required — everything else it sends is ignored. A
/// payload that grew a field must not break the panel, and the fields GitHub
/// legitimately leaves null (a run has no conclusion until it finishes) are
/// `Option` with a default rather than a decode failure waiting to happen.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WorkflowRun {
    pub id: i64,
    /// The workflow's display name, e.g. "CI" — what a watch list matches on.
    pub name: String,
    pub event: String,
    pub status: String,
    pub html_url: String,
    pub created_at: String,
    #[serde(default)]
    pub head_branch: Option<String>,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub run_started_at: Option<String>,
    #[serde(default)]
    pub display_title: Option<String>,
}

/// Status of a workflow run (the `status` field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Requested,
    Queued,
    Pending,
    Waiting,
    InProgress,
    Completed,
}

impl RunStatus {
    /// `None` for a status GitHub has that this build doesn't know — an
    /// unrecognised status is not silently coerced into a known one.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "requested" => RunStatus::Requested,
            "queued" => RunStatus::Queued,
            "pending" => RunStatus::Pending,
            "waiting" => RunStatus::Waiting,
            "in_progress" => RunStatus::InProgress,
            "completed" => RunStatus::Completed,
            _ => return None,
        })
    }
}

/// Conclusion of a completed workflow run (the `conclusion` field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunConclusion {
    Success,
    Failure,
    Neutral,
    Cancelled,
    Skipped,
    TimedOut,
    ActionRequired,
    Stale,
    StartupFailure,
}

impl RunConclusion {
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "success" => RunConclusion::Success,
            "failure" => RunConclusion::Failure,
            "neutral" => RunConclusion::Neutral,
            "cancelled" => RunConclusion::Cancelled,
            "skipped" => RunConclusion::Skipped,
            "timed_out" => RunConclusion::TimedOut,
            "action_required" => RunConclusion::ActionRequired,
            "stale" => RunConclusion::Stale,
            "startup_failure" => RunConclusion::StartupFailure,
            _ => return None,
        })
    }
}

/// One workflow run distilled to what the Repos panel renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRef {
    pub run_id: i64,
    /// Workflow name, e.g. "CI".
    pub title: String,
    /// "main" or the PR/branch name.
    pub context: String,
    pub conclusion: Option<RunConclusion>,
    pub status: Option<RunStatus>,
    pub started_at: Option<DateTime<Utc>>,
    pub html_url: String,
}

impl RunRef {
    /// Parked at a deployment-protection gate (manual approval or wait timer).
    /// GitHub's `status == "waiting"` is the precise signal a human must act.
    #[must_use]
    pub fn needs_approval(&self) -> bool {
        self.status == Some(RunStatus::Waiting)
    }

    /// Queued/pending past the staleness threshold — concurrency-blocked or
    /// stuck-queued, not actually executing.
    #[must_use]
    pub fn is_stuck(&self, now: DateTime<Utc>) -> bool {
        match self.status {
            Some(RunStatus::Queued | RunStatus::Pending | RunStatus::Requested) => {
                let Some(started_at) = self.started_at else {
                    return false;
                };
                (now - started_at).num_milliseconds() > STUCK_THRESHOLD_SECS * 1_000
            }
            _ => false,
        }
    }

    /// Actively executing (or freshly queued) — excludes the `waiting` approval
    /// gate and excludes pending/queued runs that have gone stale
    /// ([`RunRef::is_stuck`]). Those render in their own NEEDS APPROVAL / STUCK
    /// sections.
    #[must_use]
    pub fn is_running(&self, now: DateTime<Utc>) -> bool {
        match self.status {
            Some(RunStatus::InProgress) => true,
            Some(RunStatus::Queued | RunStatus::Requested | RunStatus::Pending) => {
                !self.is_stuck(now)
            }
            // `waiting` → needs approval (not running); completed/unknown → not
            // running.
            _ => false,
        }
    }

    #[must_use]
    pub fn is_failed(&self) -> bool {
        matches!(
            self.conclusion,
            Some(
                RunConclusion::Failure
                    | RunConclusion::TimedOut
                    | RunConclusion::Cancelled
                    | RunConclusion::StartupFailure
            )
        )
    }
}

/// The side counts a repo's health carries alongside its runs.
///
/// Every field is optional and `None` means *unknown*, never zero — a PAT
/// missing the Issues scope, or a count the `Link`-header trick cannot compute,
/// must render as "—". A zero here is a claim that the repo genuinely has none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RepoCounts {
    pub remote_branches: Option<u32>,
    /// GitHub counts every pull request as an issue, so this is
    /// `open issues + open PRs`.
    pub open_issues_including_prs: Option<u32>,
    pub open_pull_requests: Option<u32>,
}

/// Per-repo workflow health: latest main run, latest PR run, and any running
/// runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoWorkflowHealth {
    /// `owner/name`.
    pub repo: String,
    pub main: Option<RunRef>,
    pub last_pr: Option<RunRef>,
    /// Actively executing (excludes waiting + stuck).
    pub running: Vec<RunRef>,
    /// Parked at a deployment-protection gate (`waiting`).
    pub needs_approval: Vec<RunRef>,
    /// Queued/pending past the staleness threshold.
    pub stuck: Vec<RunRef>,
    /// False when the repo's runs couldn't be fetched.
    pub reachable: bool,
    /// Branch count from the GitHub API; `None` if unknown/fetch failed.
    pub remote_branches: Option<u32>,
    /// Open issues (PRs excluded); `None` if unknown/fetch failed.
    pub open_issues: Option<u32>,
    /// Open pull requests; `None` if unknown/fetch failed.
    pub open_prs: Option<u32>,
}

impl RepoWorkflowHealth {
    /// A repo whose runs couldn't be fetched (auth/network error). Every count
    /// is `None` — there is no data to show, and showing zero would be a lie.
    #[must_use]
    pub fn unreachable(repo: impl Into<String>) -> Self {
        RepoWorkflowHealth {
            repo: repo.into(),
            main: None,
            last_pr: None,
            running: Vec::new(),
            needs_approval: Vec::new(),
            stuck: Vec::new(),
            reachable: false,
            remote_branches: None,
            open_issues: None,
            open_prs: None,
        }
    }

    #[must_use]
    pub fn short_name(&self) -> &str {
        self.repo.rsplit('/').next().unwrap_or(&self.repo)
    }

    /// Healthy = reachable and neither main nor lastPR failed.
    ///
    /// A *running* workflow does NOT make a repo unhealthy (running is
    /// activity, not a problem, and is shown separately). A `None` slot (no run
    /// of that type) counts as not-failed. An UNREACHABLE repo is never healthy
    /// — a fetch failure must surface.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.reachable
            && !self.main.as_ref().is_some_and(RunRef::is_failed)
            && !self.last_pr.as_ref().is_some_and(RunRef::is_failed)
    }
}

/// Pure open-issue count.
///
/// GitHub's issues count includes pull requests, so the caller passes the
/// inclusive total and the open-PR count and this subtracts them. Returns
/// `None` when **either** input is unknown (a failed fetch) — half an answer is
/// not an answer — and saturates at 0 so a transient issues/PRs sampling skew
/// never shows a negative.
#[must_use]
pub fn pure_open_issue_count(
    open_issues_including_prs: Option<u32>,
    open_pull_requests: Option<u32>,
) -> Option<u32> {
    Some(open_issues_including_prs?.saturating_sub(open_pull_requests?))
}

/// Categorize a repo's runs into main / lastPR / running. "Newest" is by
/// `created_at` descending. Assumes the default branch is `main`.
///
/// `watched` is an optional per-repo opt-in list of workflows that should count
/// toward repo health beyond the default `push`/`pull_request` view. `None` or
/// empty preserves the legacy behaviour:
///   - `main` = newest `push` run on the default branch;
///   - `last_pr` = newest `pull_request` run.
///
/// When non-empty, runs are first filtered to the watched workflows (matched
/// against [`WorkflowRun::name`] — the workflow's display name, e.g. "Release";
/// entries may be a filename like `release.yml`/`release.yaml` or the display
/// name, matched case-insensitively). The event gate is then *relaxed* so any
/// watched non-PR run on the default branch — `workflow_run`, `schedule`,
/// `workflow_dispatch`, or `push` — counts toward the `main` slot. That is the
/// silent-failure fix: a `release.yml` run fires on `workflow_run`, which the
/// legacy `event == "push"` gate dropped on the floor.
#[must_use]
pub fn health(
    repo: &str,
    runs: &[WorkflowRun],
    watched: Option<&[String]>,
    counts: RepoCounts,
    now: DateTime<Utc>,
) -> RepoWorkflowHealth {
    let watched = normalized_watched(watched);
    let mut sorted: Vec<&WorkflowRun> = if watched.is_empty() {
        runs.iter().collect()
    } else {
        runs.iter()
            .filter(|run| watched.contains(&normalize_workflow_key(&run.name)))
            .collect()
    };
    // Newest first. Cached because the key is a timestamp *parse*, and a plain
    // comparator would re-parse both sides on every comparison.
    sorted.sort_by_cached_key(|run| std::cmp::Reverse(created_date(run)));

    let watch_scoped = !watched.is_empty();
    let main = sorted
        .iter()
        .find(|run| is_default_branch_health(run, watch_scoped))
        .map(|run| run_ref(run));
    let last_pr = sorted
        .iter()
        .find(|run| run.event == "pull_request")
        .map(|run| run_ref(run));

    let refs: Vec<RunRef> = sorted.iter().map(|run| run_ref(run)).collect();
    RepoWorkflowHealth {
        repo: repo.to_string(),
        main,
        last_pr,
        running: refs.iter().filter(|r| r.is_running(now)).cloned().collect(),
        needs_approval: refs
            .iter()
            .filter(|r| r.needs_approval())
            .cloned()
            .collect(),
        stuck: refs.iter().filter(|r| r.is_stuck(now)).cloned().collect(),
        reachable: true,
        remote_branches: counts.remote_branches,
        open_issues: pure_open_issue_count(
            counts.open_issues_including_prs,
            counts.open_pull_requests,
        ),
        open_prs: counts.open_pull_requests,
    }
}

/// Whether a run feeds the `main` health slot. Default behaviour is strictly
/// `push` on the default branch, plus successful merge-queue runs targeting it.
/// When the caller has scoped to watched workflows, the event gate relaxes to
/// any non-PR run on the default branch.
fn is_default_branch_health(run: &WorkflowRun, watch_scoped: bool) -> bool {
    if is_merge_queue_success(run) {
        return true;
    }
    if run.head_branch.as_deref() != Some(DEFAULT_BRANCH) {
        return false;
    }
    if watch_scoped {
        run.event != "pull_request"
    } else {
        run.event == "push"
    }
}

/// A successful merge-queue run is main-health evidence: the queue validated
/// exactly the tree it then landed on the default branch, and merge-queue repos
/// may never run push CI on main again (path filters), leaving a stale push
/// failure as "latest" forever.
///
/// Failed or cancelled queue runs are NOT evidence against main — a rejected
/// candidate never lands (the queue doing its job), and queue batch
/// recalculations cancel runs routinely.
fn is_merge_queue_success(run: &WorkflowRun) -> bool {
    run.event == "merge_group"
        && run
            .head_branch
            .as_deref()
            .is_some_and(targets_default_queue)
        && run.conclusion.as_deref() == Some("success")
}

/// Whether a ref is in the default branch's merge-queue namespace,
/// `gh-readonly-queue/<default branch>/…`. The trailing separator is checked
/// explicitly so a `main-experiment` queue can never pass as `main`'s.
fn targets_default_queue(head_branch: &str) -> bool {
    head_branch
        .strip_prefix("gh-readonly-queue/")
        .and_then(|rest| rest.strip_prefix(DEFAULT_BRANCH))
        .is_some_and(|rest| rest.starts_with('/'))
}

/// Lowercased, extension-stripped, de-blanked watched-workflow keys.
fn normalized_watched(watched: Option<&[String]>) -> Vec<String> {
    watched
        .unwrap_or(&[])
        .iter()
        .map(|s| normalize_workflow_key(s))
        .filter(|key| !key.is_empty())
        .collect()
}

/// Normalize a workflow identifier for matching: trim, drop a trailing
/// `.yml`/`.yaml`, lowercase. So `release.yml`, `Release.yaml`, and `Release`
/// all collapse to `release`.
fn normalize_workflow_key(raw: &str) -> String {
    let key = raw.trim();
    let lowered = key.to_lowercase();
    for ext in [".yml", ".yaml"] {
        if let Some(stripped) = lowered.strip_suffix(ext) {
            return stripped.to_string();
        }
    }
    lowered
}

fn run_ref(run: &WorkflowRun) -> RunRef {
    RunRef {
        run_id: run.id,
        title: run.name.clone(),
        context: context_label(run),
        conclusion: run.conclusion.as_deref().and_then(RunConclusion::parse),
        status: RunStatus::parse(&run.status),
        started_at: started_date(run),
        html_url: run.html_url.clone(),
    }
}

fn context_label(run: &WorkflowRun) -> String {
    let branch = run.head_branch.as_deref();
    if run.event == "push" && branch == Some(DEFAULT_BRANCH) {
        return DEFAULT_BRANCH.to_string();
    }
    if run.event == "pull_request" {
        return branch.unwrap_or("PR").to_string();
    }
    branch.unwrap_or(&run.event).to_string()
}

/// Ordering key. An unparseable timestamp sorts oldest rather than newest: a
/// run whose age we cannot read must never claim the `main` slot from one whose
/// age we can.
fn created_date(run: &WorkflowRun) -> DateTime<Utc> {
    parse_timestamp(&run.created_at).unwrap_or(DateTime::<Utc>::MIN_UTC)
}

fn started_date(run: &WorkflowRun) -> Option<DateTime<Utc>> {
    parse_timestamp(run.run_started_at.as_deref().unwrap_or(&run.created_at))
}

/// GitHub stamps RFC 3339, with or without fractional seconds. `parse_rfc3339`
/// covers both, so there is no second format to fall back to.
fn parse_timestamp(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const RUNS_FIXTURE: &str = include_str!("../tests/fixtures/workflow_runs.json");

    /// A fixed "now" close to the default run timestamps so that, unless a test
    /// deliberately ages a run, queued/pending runs read as running (not stuck).
    fn now() -> DateTime<Utc> {
        parse_timestamp("2026-05-29T12:05:00Z").expect("valid timestamp")
    }

    /// Every synthetic run is built by decoding JSON, so the decoder is on the
    /// path of every classification assertion — a DTO that stopped matching
    /// GitHub's payload would fail these tests, not just the fixture one.
    struct Run {
        value: serde_json::Value,
    }

    impl Run {
        fn new(name: &str, status: &str, conclusion: Option<&str>) -> Self {
            Run {
                value: json!({
                    "id": 1,
                    "name": name,
                    "head_branch": "main",
                    "head_sha": "sha",
                    "run_number": 1,
                    "workflow_id": 1,
                    "event": "push",
                    "status": status,
                    "conclusion": conclusion,
                    "html_url": "https://github.com/Sassy-Dog/velovate/actions/runs/1",
                    "created_at": "2026-05-29T12:00:00Z",
                    "updated_at": "2026-05-29T12:05:00Z",
                    "run_started_at": null,
                    "display_title": "a commit",
                }),
            }
        }

        fn set(mut self, key: &str, value: serde_json::Value) -> Self {
            self.value[key] = value;
            self
        }

        fn branch(self, branch: &str) -> Self {
            self.set("head_branch", json!(branch))
        }

        fn event(self, event: &str) -> Self {
            self.set("event", json!(event))
        }

        fn id(self, id: i64) -> Self {
            self.set("id", json!(id))
        }

        fn created(self, created_at: &str) -> Self {
            self.set("created_at", json!(created_at))
        }

        fn build(self) -> WorkflowRun {
            serde_json::from_value(self.value).expect("fixture-shaped run")
        }
    }

    fn run(name: &str, status: &str, conclusion: Option<&str>) -> WorkflowRun {
        Run::new(name, status, conclusion).build()
    }

    fn health_of(repo: &str, runs: &[WorkflowRun]) -> RepoWorkflowHealth {
        health(repo, runs, None, RepoCounts::default(), now())
    }

    fn watched(repo: &str, runs: &[WorkflowRun], keys: &[&str]) -> RepoWorkflowHealth {
        let keys: Vec<String> = keys.iter().map(|k| (*k).to_string()).collect();
        health(repo, runs, Some(&keys), RepoCounts::default(), now())
    }

    // MARK: - Short name

    #[test]
    fn repo_workflow_health_short_name() {
        let h = health_of("Sassy-Dog/velovate", &[]);
        assert_eq!(h.short_name(), "velovate");
        assert!(h.is_healthy(), "no runs = not failed, reachable");
        assert!(h.reachable, "the mapping path means the repo was fetched");
    }

    /// A repo with a build in flight (but no failure) must remain healthy —
    /// running is activity, not a problem.
    #[test]
    fn a_running_repo_still_counts_healthy() {
        let h = health_of(
            "o/r",
            &[
                run("CI", "completed", Some("success")),
                run("CI", "in_progress", None),
            ],
        );
        assert!(!h.running.is_empty(), "precondition: a run is in progress");
        assert!(h.is_healthy(), "running does not make a repo unhealthy");
    }

    #[test]
    fn an_unreachable_repo_is_never_healthy() {
        let h = RepoWorkflowHealth::unreachable("Sassy-Dog/platform");
        assert!(!h.reachable);
        assert!(
            !h.is_healthy(),
            "an unreachable repo must not count as healthy"
        );
        assert_eq!(h.short_name(), "platform");
    }

    // MARK: - health() categorization

    #[test]
    fn main_picks_the_latest_push_on_main() {
        let runs = [
            Run::new("CI", "completed", Some("success"))
                .created("2026-05-29T10:00:00Z")
                .build(),
            // Newer.
            Run::new("CI", "completed", Some("failure"))
                .created("2026-05-29T11:00:00Z")
                .build(),
            Run::new("CI", "completed", Some("success"))
                .branch("feat/x")
                .event("pull_request")
                .created("2026-05-29T12:00:00Z")
                .build(),
        ];
        let h = health_of("Sassy-Dog/velovate", &runs);
        assert_eq!(
            h.main.as_ref().and_then(|r| r.conclusion),
            Some(RunConclusion::Failure),
            "main = newest push-on-main run"
        );
        assert_eq!(h.main.as_ref().map(|r| r.context.as_str()), Some("main"));
    }

    #[test]
    fn last_pr_picks_the_latest_pull_request_run() {
        let runs = [
            Run::new("CI", "completed", Some("failure"))
                .branch("feat/old")
                .event("pull_request")
                .created("2026-05-29T10:00:00Z")
                .build(),
            // Newer.
            Run::new("CI", "completed", Some("success"))
                .branch("feat/new")
                .event("pull_request")
                .created("2026-05-29T12:00:00Z")
                .build(),
        ];
        let h = health_of("o/r", &runs);
        assert_eq!(
            h.last_pr.as_ref().and_then(|r| r.conclusion),
            Some(RunConclusion::Success)
        );
        assert_eq!(
            h.last_pr.as_ref().map(|r| r.context.as_str()),
            Some("feat/new")
        );
    }

    #[test]
    fn running_collects_in_progress_runs() {
        let runs = [
            Run::new("CI", "in_progress", None).id(1).build(),
            Run::new("Deploy", "queued", None)
                .event("workflow_dispatch")
                .id(2)
                .build(),
            Run::new("CI", "completed", Some("success")).id(3).build(),
        ];
        let h = health_of("o/r", &runs);
        assert_eq!(
            h.running.len(),
            2,
            "both in_progress and a fresh queued run are 'running'"
        );
        assert!(h.running.iter().all(|r| r.is_running(now())));
    }

    // MARK: - needs approval / stuck classification

    #[test]
    fn a_waiting_run_needs_approval_and_is_not_running() {
        let runs = [
            Run::new("Release", "waiting", None).id(10).build(),
            Run::new("CI", "in_progress", None).id(11).build(),
        ];
        let h = health_of("Sassy-Dog/velovate", &runs);
        assert_eq!(
            h.needs_approval.len(),
            1,
            "waiting run is a needs-approval item"
        );
        assert_eq!(
            h.needs_approval.first().map(|r| r.title.as_str()),
            Some("Release")
        );
        assert!(h.needs_approval[0].needs_approval());
        assert_eq!(h.running.len(), 1, "only the in_progress run is running");
        assert!(
            !h.running
                .iter()
                .any(|r| r.status == Some(RunStatus::Waiting)),
            "waiting must not be 'running'"
        );
        assert!(h.is_healthy(), "a parked-for-approval repo isn't a failure");
    }

    #[test]
    fn a_stale_pending_run_is_stuck_not_running() {
        // A pending run created ~24h before "now" with no jobs.
        let runs = [Run::new("Release", "pending", None)
            .id(20)
            .created("2026-05-28T12:00:00Z")
            .build()];
        let h = health_of("o/r", &runs);
        assert_eq!(h.stuck.len(), 1, "a long-pending run reads as stuck");
        assert!(
            h.running.is_empty(),
            "a stuck run is not counted as running"
        );
        assert!(h.stuck[0].is_stuck(now()));
    }

    #[test]
    fn a_fresh_pending_run_is_running_not_stuck() {
        // 5m before now.
        let runs = [Run::new("Release", "pending", None)
            .id(21)
            .created("2026-05-29T12:00:00Z")
            .build()];
        let h = health_of("o/r", &runs);
        assert_eq!(h.running.len(), 1, "a freshly pending run is still running");
        assert!(h.stuck.is_empty(), "not stuck until past the threshold");
    }

    #[test]
    fn is_failed_covers_every_failing_conclusion() {
        for conclusion in ["failure", "timed_out", "cancelled", "startup_failure"] {
            let h = health_of("o/r", &[run("CI", "completed", Some(conclusion))]);
            assert_eq!(
                h.main.as_ref().map(RunRef::is_failed),
                Some(true),
                "{conclusion} should be a failure"
            );
        }
        let ok = health_of("o/r", &[run("CI", "completed", Some("success"))]);
        assert_eq!(ok.main.as_ref().map(RunRef::is_failed), Some(false));
    }

    #[test]
    fn a_repo_is_healthy_when_nothing_failed() {
        let clean = health_of(
            "o/r",
            &[
                run("CI", "completed", Some("success")),
                Run::new("CI", "completed", Some("success"))
                    .branch("feat/x")
                    .event("pull_request")
                    .build(),
            ],
        );
        assert!(clean.is_healthy());

        let failing = health_of("o/r", &[run("CI", "completed", Some("failure"))]);
        assert!(!failing.is_healthy());
    }

    // MARK: - watched workflows + event-gate relaxation

    /// The silent-failure incident: a `release.yml` run fires on `workflow_run`
    /// after `ci.yml`. With the legacy `event == "push"` gate it is dropped —
    /// green panel, failed release. Without a watch list, that is the
    /// (preserved) default.
    #[test]
    fn a_workflow_run_failure_is_invisible_without_a_watch_list() {
        let runs = [
            Run::new("CI", "completed", Some("success")).id(1).build(),
            Run::new("Release", "completed", Some("failure"))
                .event("workflow_run")
                .id(2)
                .build(),
        ];
        let h = health_of("Sassy-Dog/velovate", &runs);
        assert!(
            h.is_healthy(),
            "default view ignores the workflow_run failure (legacy behaviour preserved)"
        );
        assert_eq!(h.main.as_ref().map(|r| r.title.as_str()), Some("CI"));
    }

    /// Adding `release.yml` to the watch list relaxes the event gate so the
    /// `workflow_run` failure on main reddens the repo and names the workflow.
    #[test]
    fn a_watched_release_workflow_run_failure_reddens_the_repo() {
        let runs = [
            Run::new("CI", "completed", Some("success")).id(1).build(),
            Run::new("Release", "completed", Some("failure"))
                .event("workflow_run")
                .id(2)
                .build(),
        ];
        let h = watched("Sassy-Dog/velovate", &runs, &["release.yml"]);
        assert!(
            !h.is_healthy(),
            "watched release failure must redden the repo"
        );
        assert_eq!(
            h.main.as_ref().and_then(|r| r.conclusion),
            Some(RunConclusion::Failure)
        );
        assert_eq!(
            h.main.as_ref().map(|r| r.title.as_str()),
            Some("Release"),
            "the failing workflow is named"
        );
    }

    /// `schedule`-triggered runs on the default branch also count when watched.
    #[test]
    fn a_watched_schedule_failure_reddens_the_repo() {
        let runs = [Run::new("Nightly", "completed", Some("failure"))
            .event("schedule")
            .build()];
        let h = watched("o/r", &runs, &["Nightly"]);
        assert!(!h.is_healthy());
        assert_eq!(h.main.as_ref().map(|r| r.title.as_str()), Some("Nightly"));
    }

    /// Watch-list matching is case-insensitive and extension-agnostic.
    #[test]
    fn watch_matching_is_case_and_extension_insensitive() {
        let runs = [Run::new("Release", "completed", Some("failure"))
            .event("workflow_run")
            .build()];
        for key in [
            "release.yml",
            "Release.yaml",
            "RELEASE",
            "release",
            " release ",
        ] {
            let h = watched("o/r", &runs, &[key]);
            assert!(
                !h.is_healthy(),
                "key {key:?} should match the 'Release' run"
            );
        }
    }

    /// When a watch list is set, non-watched workflows are filtered out — a
    /// passing `ci.yml` doesn't mask a failing watched `release.yml`, and a
    /// failing non-watched workflow doesn't redden a repo that opted into only
    /// specific workflows.
    #[test]
    fn a_watch_list_filters_out_non_watched_workflows() {
        let runs = [
            Run::new("CI", "completed", Some("failure")).id(1).build(),
            Run::new("Release", "completed", Some("success"))
                .event("workflow_run")
                .id(2)
                .build(),
        ];
        let h = watched("o/r", &runs, &["release.yml"]);
        assert!(
            h.is_healthy(),
            "non-watched CI failure is filtered out when a watch list is set"
        );
        assert_eq!(h.main.as_ref().map(|r| r.title.as_str()), Some("Release"));
    }

    /// An empty watch list is identical to no watch list (default push+PR view).
    #[test]
    fn an_empty_watch_list_preserves_the_default_behaviour() {
        let runs = [
            Run::new("Release", "completed", Some("failure"))
                .event("workflow_run")
                .id(1)
                .build(),
            Run::new("CI", "completed", Some("success")).id(2).build(),
        ];
        let h = watched("o/r", &runs, &[]);
        assert!(
            h.is_healthy(),
            "empty list == legacy behaviour; workflow_run failure ignored"
        );
        assert_eq!(h.main.as_ref().map(|r| r.title.as_str()), Some("CI"));
    }

    /// A blank entry is not a workflow key — it must not turn a whitespace typo
    /// into an empty-scope watch list that hides every run.
    #[test]
    fn a_blank_watch_entry_is_ignored() {
        let runs = [
            Run::new("Release", "completed", Some("failure"))
                .event("workflow_run")
                .id(1)
                .build(),
            Run::new("CI", "completed", Some("success")).id(2).build(),
        ];
        let h = watched("o/r", &runs, &["   "]);
        assert!(h.is_healthy());
        assert_eq!(h.main.as_ref().map(|r| r.title.as_str()), Some("CI"));
    }

    /// A watched `workflow_run` failure on a non-default branch must not feed
    /// the main slot — only default-branch runs count toward repo health.
    #[test]
    fn a_watched_run_off_the_default_branch_does_not_feed_main() {
        let runs = [Run::new("Release", "completed", Some("failure"))
            .branch("release/1.2")
            .event("workflow_run")
            .build()];
        let h = watched("o/r", &runs, &["release.yml"]);
        assert!(
            h.main.is_none(),
            "a non-main watched run must not populate the main slot"
        );
        assert!(h.is_healthy());
    }

    // MARK: - open issue / PR counts (the "—"-vs-0 rule)

    #[test]
    fn pure_open_issue_count_subtracts_pull_requests() {
        assert_eq!(pure_open_issue_count(Some(10), Some(3)), Some(7));
    }

    /// The issues and PRs totals are sampled from two separate requests, so a
    /// transient skew could make PRs momentarily exceed the inclusive total —
    /// saturate at 0 rather than underflow.
    #[test]
    fn pure_open_issue_count_saturates_at_zero() {
        assert_eq!(pure_open_issue_count(Some(2), Some(5)), Some(0));
    }

    /// A failed fetch on either input yields `None` (renders "—"), never a
    /// wrong number and never a fabricated zero.
    #[test]
    fn pure_open_issue_count_is_none_when_either_input_is_missing() {
        assert_eq!(pure_open_issue_count(None, Some(3)), None);
        assert_eq!(pure_open_issue_count(Some(10), None), None);
        assert_eq!(pure_open_issue_count(None, None), None);
    }

    /// `None` and `Some(0)` are different answers — "we don't know" versus
    /// "there are none". Collapsing them is exactly the bug the "—" rule
    /// exists to prevent.
    #[test]
    fn unknown_is_not_zero() {
        assert_eq!(pure_open_issue_count(Some(0), Some(0)), Some(0));
        assert_ne!(pure_open_issue_count(None, Some(0)), Some(0));
    }

    #[test]
    fn health_threads_the_open_issue_and_pr_counts() {
        let h = health(
            "o/r",
            &[],
            None,
            RepoCounts {
                remote_branches: Some(9),
                open_issues_including_prs: Some(12),
                open_pull_requests: Some(4),
            },
            now(),
        );
        assert_eq!(h.open_issues, Some(8), "12 inclusive − 4 PRs = 8 pure");
        assert_eq!(h.open_prs, Some(4));
        assert_eq!(h.remote_branches, Some(9));
    }

    /// When the side-count fetches fail, the counts are `None`, not zero.
    #[test]
    fn health_leaves_the_open_counts_none_when_not_fetched() {
        let h = health_of("o/r", &[]);
        assert_eq!(h.open_issues, None);
        assert_eq!(h.open_prs, None);
        assert_eq!(h.remote_branches, None);
    }

    /// A repo that genuinely has zero branches reports zero — the "—" rule
    /// must not swallow a real count.
    #[test]
    fn health_keeps_a_genuine_zero_distinct_from_unknown() {
        let h = health(
            "o/r",
            &[],
            None,
            RepoCounts {
                remote_branches: Some(0),
                open_issues_including_prs: Some(0),
                open_pull_requests: Some(0),
            },
            now(),
        );
        assert_eq!(h.remote_branches, Some(0));
        assert_eq!(h.open_issues, Some(0));
        assert_eq!(h.open_prs, Some(0));
    }

    #[test]
    fn an_unreachable_repo_has_none_open_counts() {
        let h = RepoWorkflowHealth::unreachable("o/r");
        assert_eq!(h.open_issues, None);
        assert_eq!(h.open_prs, None);
        assert_eq!(h.remote_branches, None);
    }

    // MARK: - Merge-queue main health (merge_group evidence)

    #[test]
    fn a_merge_group_success_supersedes_an_older_push_failure_on_main() {
        // Merge-queue repos validate on gh-readonly-queue refs and may never
        // run push CI on main again (path filters) — a stale push failure must
        // be superseded by a newer queue-validated merge.
        let h = health_of(
            "o/r",
            &[
                Run::new("CI", "completed", Some("failure"))
                    .id(1)
                    .created("2026-05-29T11:00:00Z")
                    .build(),
                Run::new("CI", "completed", Some("success"))
                    .branch("gh-readonly-queue/main/pr-224-abc123")
                    .event("merge_group")
                    .id(2)
                    .created("2026-05-29T12:00:00Z")
                    .build(),
            ],
        );
        assert_eq!(
            h.main.as_ref().and_then(|r| r.conclusion),
            Some(RunConclusion::Success),
            "the queue validated exactly what landed on main"
        );
        assert!(h.is_healthy());
    }

    #[test]
    fn a_failed_merge_group_run_is_not_evidence_against_main() {
        // A rejected queue candidate never lands — the queue doing its job says
        // nothing about main's health. The newest push run keeps the main slot.
        let h = health_of(
            "o/r",
            &[
                Run::new("CI", "completed", Some("success"))
                    .id(1)
                    .created("2026-05-29T11:00:00Z")
                    .build(),
                Run::new("CI", "completed", Some("failure"))
                    .branch("gh-readonly-queue/main/pr-9-def456")
                    .event("merge_group")
                    .id(2)
                    .created("2026-05-29T12:00:00Z")
                    .build(),
            ],
        );
        assert_eq!(
            h.main.as_ref().and_then(|r| r.conclusion),
            Some(RunConclusion::Success)
        );
        assert!(h.is_healthy());
    }

    #[test]
    fn a_push_failure_newer_than_a_merge_group_success_stays_red() {
        // A direct push that breaks main AFTER the last queue merge is real.
        let h = health_of(
            "o/r",
            &[
                Run::new("CI", "completed", Some("success"))
                    .branch("gh-readonly-queue/main/pr-7-aaa")
                    .event("merge_group")
                    .id(1)
                    .created("2026-05-29T11:00:00Z")
                    .build(),
                Run::new("CI", "completed", Some("failure"))
                    .id(2)
                    .created("2026-05-29T12:00:00Z")
                    .build(),
            ],
        );
        assert_eq!(
            h.main.as_ref().and_then(|r| r.conclusion),
            Some(RunConclusion::Failure)
        );
        assert!(!h.is_healthy());
    }

    /// A queue namespace whose branch merely *starts with* "main" is a
    /// different branch's queue. `gh-readonly-queue/main-experiment/…` must not
    /// vouch for `main`.
    #[test]
    fn a_merge_group_for_a_prefix_sharing_branch_is_ignored() {
        assert!(targets_default_queue("gh-readonly-queue/main/pr-1-aaa"));
        assert!(!targets_default_queue(
            "gh-readonly-queue/main-experiment/pr-1-aaa"
        ));
        assert!(!targets_default_queue("gh-readonly-queue/main"));
        assert!(!targets_default_queue("refs/heads/main"));

        let h = health_of(
            "o/r",
            &[
                Run::new("CI", "completed", Some("failure"))
                    .id(1)
                    .created("2026-05-29T11:00:00Z")
                    .build(),
                Run::new("CI", "completed", Some("success"))
                    .branch("gh-readonly-queue/main-experiment/pr-3-xyz")
                    .event("merge_group")
                    .id(2)
                    .created("2026-05-29T12:00:00Z")
                    .build(),
            ],
        );
        assert_eq!(
            h.main.as_ref().and_then(|r| r.conclusion),
            Some(RunConclusion::Failure)
        );
        assert!(!h.is_healthy());
    }

    #[test]
    fn a_merge_group_for_another_branch_queue_is_ignored() {
        // Only queue refs targeting the default branch are main evidence.
        let h = health_of(
            "o/r",
            &[
                Run::new("CI", "completed", Some("failure"))
                    .id(1)
                    .created("2026-05-29T11:00:00Z")
                    .build(),
                Run::new("CI", "completed", Some("success"))
                    .branch("gh-readonly-queue/release/pr-3-xyz")
                    .event("merge_group")
                    .id(2)
                    .created("2026-05-29T12:00:00Z")
                    .build(),
            ],
        );
        assert_eq!(
            h.main.as_ref().and_then(|r| r.conclusion),
            Some(RunConclusion::Failure),
            "a release-queue run must not vouch for main"
        );
        assert!(!h.is_healthy());
    }

    #[test]
    fn watch_scoped_mode_also_accepts_a_merge_group_success() {
        let h = watched(
            "o/r",
            &[
                Run::new("CI", "completed", Some("failure"))
                    .id(1)
                    .created("2026-05-29T11:00:00Z")
                    .build(),
                Run::new("CI", "completed", Some("success"))
                    .branch("gh-readonly-queue/main/pr-5-bbb")
                    .event("merge_group")
                    .id(2)
                    .created("2026-05-29T12:00:00Z")
                    .build(),
            ],
            &["CI"],
        );
        assert_eq!(
            h.main.as_ref().and_then(|r| r.conclusion),
            Some(RunConclusion::Success)
        );
        assert!(h.is_healthy());
    }

    // MARK: - Timestamps + enum parsing

    /// `run_started_at` wins over `created_at` — a run queued for an hour then
    /// started a minute ago is one minute old, not sixty.
    #[test]
    fn started_at_prefers_run_started_at() {
        let run = Run::new("CI", "in_progress", None)
            .created("2026-05-29T11:00:00Z")
            .set("run_started_at", json!("2026-05-29T12:04:00Z"))
            .build();
        assert_eq!(
            started_date(&run),
            parse_timestamp("2026-05-29T12:04:00Z"),
            "run_started_at is the run's real start"
        );
    }

    /// The nullable fields are nullable in both directions: GitHub sends
    /// `conclusion: null` for a run in flight, and a payload that omits the
    /// key entirely must decode the same way rather than fail the whole repo.
    #[test]
    fn the_nullable_fields_decode_when_null_or_absent() {
        let minimal: WorkflowRun = serde_json::from_str(
            r#"{"id":1,"name":"CI","event":"push","status":"in_progress",
                "html_url":"https://x","created_at":"2026-05-29T12:00:00Z"}"#,
        )
        .expect("decode");
        assert_eq!(minimal.head_branch, None);
        assert_eq!(minimal.conclusion, None);
        assert_eq!(minimal.run_started_at, None);
        // Falls back to created_at when run_started_at is absent.
        assert_eq!(
            started_date(&minimal),
            parse_timestamp("2026-05-29T12:00:00Z")
        );
    }

    #[test]
    fn fractional_second_timestamps_parse() {
        assert!(parse_timestamp("2026-05-29T12:00:00.123Z").is_some());
        assert!(parse_timestamp("2026-05-29T12:00:00Z").is_some());
        assert!(parse_timestamp("not a date").is_none());
    }

    /// An unparseable `created_at` sorts oldest, so a garbage timestamp can
    /// never steal the main slot from a run whose age we can actually read.
    #[test]
    fn an_unparseable_created_at_sorts_oldest() {
        let h = health_of(
            "o/r",
            &[
                Run::new("CI", "completed", Some("failure"))
                    .id(1)
                    .created("garbage")
                    .build(),
                Run::new("CI", "completed", Some("success"))
                    .id(2)
                    .created("2026-05-29T10:00:00Z")
                    .build(),
            ],
        );
        assert_eq!(h.main.as_ref().map(|r| r.run_id), Some(2));
    }

    /// An unknown status is `None`, not a coerced known value — and a run with
    /// no readable status is neither running nor stuck nor awaiting approval.
    #[test]
    fn an_unknown_status_parses_to_none_and_classifies_nowhere() {
        assert_eq!(RunStatus::parse("teleported"), None);
        assert_eq!(RunConclusion::parse("vibes"), None);
        let h = health_of("o/r", &[run("CI", "teleported", Some("vibes"))]);
        assert!(h.running.is_empty());
        assert!(h.stuck.is_empty());
        assert!(h.needs_approval.is_empty());
        assert_eq!(h.main.as_ref().and_then(|r| r.conclusion), None);
        assert!(h.is_healthy(), "an unreadable conclusion is not a failure");
    }

    #[test]
    fn context_labels_name_the_branch_or_the_event() {
        let push_main = run_ref(&run("CI", "completed", Some("success")));
        assert_eq!(push_main.context, "main");

        let pr = run_ref(
            &Run::new("CI", "completed", None)
                .branch("feat/x")
                .event("pull_request")
                .build(),
        );
        assert_eq!(pr.context, "feat/x");

        let branchless_pr = run_ref(
            &Run::new("CI", "completed", None)
                .set("head_branch", json!(null))
                .event("pull_request")
                .build(),
        );
        assert_eq!(branchless_pr.context, "PR");

        let branchless_other = run_ref(
            &Run::new("CI", "completed", None)
                .set("head_branch", json!(null))
                .event("schedule")
                .build(),
        );
        assert_eq!(branchless_other.context, "schedule");
    }

    // MARK: - Fixture decode

    /// The DTOs decode a real `GET /repos/{slug}/actions/runs` payload and the
    /// mapping classifies it end to end.
    #[test]
    fn decodes_and_classifies_the_workflow_runs_fixture() {
        let resp: WorkflowRunsResponse = serde_json::from_str(RUNS_FIXTURE).expect("decode");
        assert_eq!(resp.total_count, 6);
        assert_eq!(resp.workflow_runs.len(), 6);

        let h = health(
            "Sassy-Dog/devcanopy",
            &resp.workflow_runs,
            None,
            RepoCounts {
                remote_branches: Some(37),
                open_issues_including_prs: Some(12),
                open_pull_requests: Some(4),
            },
            parse_timestamp("2026-05-29T12:05:00Z").expect("valid timestamp"),
        );
        assert_eq!(h.main.as_ref().map(|r| r.title.as_str()), Some("CI"));
        assert_eq!(
            h.main.as_ref().and_then(|r| r.conclusion),
            Some(RunConclusion::Success),
            "the merge-queue success is the newest main evidence"
        );
        assert_eq!(
            h.last_pr.as_ref().map(|r| r.context.as_str()),
            Some("feat/panels")
        );
        assert_eq!(h.running.len(), 1);
        assert_eq!(h.needs_approval.len(), 1);
        assert_eq!(h.stuck.len(), 1);
        assert!(h.is_healthy());
        assert_eq!(h.open_issues, Some(8));
        assert_eq!(h.remote_branches, Some(37));
    }
}
