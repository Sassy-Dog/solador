//! PAT-authenticated GitHub REST client. Port of
//! `DevCanopy/Services/GitHub/GitHubService.swift`, plus the per-repo and
//! per-org fetch orchestration from `GHWorkflowsService.swift` /
//! `GHRunnersService.swift`.
//!
//! Read-only GETs only. The token is a fine-grained PAT and travels in an
//! `Authorization: Bearer` header — never in a URL, which is what keeps it out
//! of `reqwest`'s error strings and any log line built from them.

use chrono::{DateTime, Utc};
use futures_util::future::join3;
use serde::Deserialize;

use crate::link;
use crate::roster::{apply_fetch, RosterUpdate, RunnerRosterEntry};
use crate::runners::{self, RunnersResponse};
use crate::workflows::{self, RepoCounts, RepoWorkflowHealth, WorkflowRun, WorkflowRunsResponse};

pub const DEFAULT_BASE_URL: &str = "https://api.github.com";
pub const API_VERSION: &str = "2022-11-28";

/// How many runs the Repos panel classifies per repo.
const RUNS_PER_PAGE: &str = "30";
/// One page covers every self-hosted runner an org of this size registers.
const RUNNERS_PER_PAGE: &str = "100";

#[derive(Debug, thiserror::Error)]
pub enum GitHubError {
    #[error("not authenticated with GitHub")]
    NotAuthenticated,
    /// 403 with `X-RateLimit-Remaining: 0`. `reset` is `None` when GitHub did
    /// not send `X-RateLimit-Reset` — an unknown reset time, not "now".
    #[error("GitHub rate limit exceeded")]
    RateLimited { reset: Option<DateTime<Utc>> },
    #[error("GitHub returned HTTP {0}")]
    HttpStatus(u16),
    #[error("unreachable: {0}")]
    Unreachable(String),
    #[error("could not decode the GitHub payload: {0}")]
    DecodeFailed(String),
    /// The collection paginates by cursor, so the `per_page=1` last-page trick
    /// cannot count it. Deliberately an error: the alternative is reporting the
    /// page size as the total and undercounting every repo.
    #[error("cannot count via the last-page trick (cursor pagination)")]
    UnsupportedPagination,
}

impl GitHubError {
    /// Cause-specific guidance, so the operator chases the right layer.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            GitHubError::NotAuthenticated => {
                "GitHub rejected the token (401). Check the PAT in Settings → GitHub Token.".into()
            }
            GitHubError::RateLimited { reset: Some(reset) } => format!(
                "GitHub rate limit exceeded. Resets at {}.",
                reset.format("%Y-%m-%d %H:%M:%SZ")
            ),
            GitHubError::RateLimited { reset: None } => "GitHub rate limit exceeded.".to_string(),
            GitHubError::HttpStatus(code) => format!("GitHub returned HTTP {code}."),
            GitHubError::Unreachable(_) => {
                "Couldn't reach GitHub. Check the network connection.".into()
            }
            GitHubError::DecodeFailed(_) => {
                "GitHub responded but the payload didn't decode — likely an API contract change."
                    .into()
            }
            GitHubError::UnsupportedPagination => {
                "GitHub paginates this collection by cursor, so the count is unavailable.".into()
            }
        }
    }
}

pub struct GitHubClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl GitHubClient {
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self::with_base_url(DEFAULT_BASE_URL, token)
    }

    /// # Invariant: no credentials in `base_url`
    ///
    /// `base_url` must be scheme/host/port only. The token is a separate
    /// argument and travels only via `bearer_auth()`, which puts it in a
    /// header. URLs leak where headers do not: `reqwest` attaches the request
    /// URL to its errors and does not redact userinfo, so anything embedded
    /// here can reach a log line or an operator-facing error string.
    ///
    /// Exists so tests (and only tests) can point the client at a mock server.
    #[must_use]
    pub fn with_base_url(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        GitHubClient {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token: token.into(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                // GitHub's REST API rejects requests without a User-Agent with
                // a blanket 403, token permissions notwithstanding. reqwest
                // sends none by default (URLSession always does, which is why
                // the Swift port never hit this). Observed live, issue #186.
                .user_agent(concat!("DevCanopy/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client"),
        }
    }

    /// Whether a usable token is loaded — without exposing it.
    #[must_use]
    pub fn has_token(&self) -> bool {
        !self.token.is_empty()
    }

    /// An authenticated GET against an arbitrary REST endpoint, returning the
    /// raw body.
    pub async fn get_raw(&self, path: &str, query: &[(&str, &str)]) -> Result<String, GitHubError> {
        Ok(self.get(path, query).await?.body)
    }

    /// The one request path every endpoint goes through: bearer auth, the two
    /// GitHub-mandated headers, status classification, body read.
    ///
    /// Unlike `crates/agentclient`, which pins an exact 200 against an agent we
    /// ship ourselves, this accepts any 2xx: GitHub is a third-party API whose
    /// success codes are its own to choose, and a client that hard-fails on an
    /// unexpected-but-successful code would break on GitHub's schedule rather
    /// than ours.
    async fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<Response, GitHubError> {
        if !self.has_token() {
            return Err(GitHubError::NotAuthenticated);
        }
        let resp = self
            .http
            .get(format!("{}{path}", self.base_url))
            .query(query)
            .bearer_auth(&self.token)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .send()
            .await
            .map_err(|e| GitHubError::Unreachable(e.to_string()))?;

        let status = resp.status().as_u16();
        let link = header(&resp, "link");
        // Read the rate-limit pair off *this* response. A 403 is only a rate
        // limit when this very response says the budget is spent; a 403 for a
        // missing scope must stay a plain HTTP 403 so the operator fixes the
        // PAT instead of waiting for a reset that will not help.
        let remaining = header(&resp, "x-ratelimit-remaining").and_then(|v| v.parse::<u64>().ok());
        let reset = header(&resp, "x-ratelimit-reset")
            .and_then(|v| v.parse::<i64>().ok())
            .and_then(|secs| DateTime::from_timestamp(secs, 0));

        if !(200..300).contains(&status) {
            return Err(match status {
                401 => GitHubError::NotAuthenticated,
                403 if remaining == Some(0) => GitHubError::RateLimited { reset },
                other => GitHubError::HttpStatus(other),
            });
        }

        let body = resp
            .text()
            .await
            .map_err(|e| GitHubError::Unreachable(e.to_string()))?;
        Ok(Response { body, link })
    }

    /// Total number of branches for a repo.
    pub async fn branch_count(&self, repo: &str) -> Result<u32, GitHubError> {
        self.open_count(&format!("/repos/{repo}/branches"), &[])
            .await
    }

    /// Number of **open** pull requests for a repo. Requires the PAT's
    /// Pull requests (read) permission.
    pub async fn open_pull_request_count(&self, repo: &str) -> Result<u32, GitHubError> {
        self.open_count(&format!("/repos/{repo}/pulls"), &[("state", "open")])
            .await
    }

    /// Open issue+PR count, read from the repo object's accurate
    /// `open_issues_count`. GitHub counts every pull request as an issue, so
    /// this is `open issues + open PRs`; callers subtract the open-PR count for
    /// a pure open-issue count.
    ///
    /// This deliberately does NOT use the `/issues` list endpoint: `/issues`
    /// serves cursor-based pagination (`rel="next"` with an opaque `after=`
    /// cursor, no `rel="last"`), so the `per_page=1` last-page trick cannot
    /// count it and would silently undercount every repo with >1 open issue
    /// to 1.
    pub async fn open_issues_including_prs_count(&self, repo: &str) -> Result<u32, GitHubError> {
        let body = self.get_raw(&format!("/repos/{repo}"), &[]).await?;
        let repo: RepoDto = serde_json::from_str(&body).map_err(decode_failed)?;
        Ok(repo.open_issues_count)
    }

    /// The cheap `per_page=1` + `Link: rel="last"` count: with one item per
    /// page, the last page number equals the total.
    ///
    /// Falls back to the returned array's length only when there is genuinely a
    /// single page (no `Link` header at all). When the header advertises a
    /// `rel="next"` but no `rel="last"`, the total is unknowable via this trick,
    /// so this errors rather than silently undercounting to the page size.
    async fn open_count(&self, path: &str, extra: &[(&str, &str)]) -> Result<u32, GitHubError> {
        let mut query = vec![("per_page", "1")];
        query.extend_from_slice(extra);
        let resp = self.get(path, &query).await?;

        if let Some(last) = link::last_page(resp.link.as_deref()) {
            return Ok(last);
        }
        if link::has_next_page(resp.link.as_deref()) {
            // More pages exist but the count is unknowable from the header —
            // refuse to lie with a per_page=1 undercount.
            return Err(GitHubError::UnsupportedPagination);
        }
        // Single page: the body IS the collection. A body that is not an array
        // is skew, not an empty collection — reporting 0 there would invent a
        // count out of a payload we failed to understand.
        let page: Vec<serde_json::Value> =
            serde_json::from_str(&resp.body).map_err(decode_failed)?;
        Ok(u32::try_from(page.len()).unwrap_or(u32::MAX))
    }

    /// Recent workflow runs for one repo — the Repos panel's raw input.
    pub async fn workflow_runs(&self, repo: &str) -> Result<Vec<WorkflowRun>, GitHubError> {
        let body = self
            .get_raw(
                &format!("/repos/{repo}/actions/runs"),
                &[("per_page", RUNS_PER_PAGE)],
            )
            .await?;
        let resp: WorkflowRunsResponse = serde_json::from_str(&body).map_err(decode_failed)?;
        Ok(resp.workflow_runs)
    }

    /// One repo's full health row.
    ///
    /// Infallible on purpose: a repo whose runs cannot be fetched comes back
    /// [`RepoWorkflowHealth::unreachable`] so one failing repo never takes the
    /// rest of the panel with it.
    ///
    /// The three side counts are best-effort and fired concurrently. Any of
    /// them failing — including a PAT missing the Issues or Pull requests scope
    /// — yields `None`, which renders as "—" and never marks the repo
    /// unreachable: its runs decoded fine, and a missing scope is not an
    /// outage.
    pub async fn repo_health(
        &self,
        repo: &str,
        watched: Option<&[String]>,
        now: DateTime<Utc>,
    ) -> RepoWorkflowHealth {
        let Ok(runs) = self.workflow_runs(repo).await else {
            return RepoWorkflowHealth::unreachable(repo);
        };
        let (branches, open_prs, open_issues_incl_prs) = join3(
            self.branch_count(repo),
            self.open_pull_request_count(repo),
            self.open_issues_including_prs_count(repo),
        )
        .await;

        workflows::health(
            repo,
            &runs,
            watched,
            RepoCounts {
                remote_branches: branches.ok(),
                open_issues_including_prs: open_issues_incl_prs.ok(),
                open_pull_requests: open_prs.ok(),
            },
            now,
        )
    }

    /// Every self-hosted runner registered with the org.
    pub async fn org_runners(&self, org: &str) -> Result<Vec<runners::GhRunner>, GitHubError> {
        let body = self
            .get_raw(
                &format!("/orgs/{org}/actions/runners"),
                &[("per_page", RUNNERS_PER_PAGE)],
            )
            .await?;
        let resp: RunnersResponse = serde_json::from_str(&body).map_err(decode_failed)?;
        Ok(runners::map(&resp.runners))
    }

    /// Fetch the org's runners and fold them into `roster`.
    ///
    /// The roster is only ever advanced through here, and this only returns
    /// `Ok` on a successful fetch — so a failing GitHub leaves the caller
    /// holding its previous roster with every absence clock frozen at the last
    /// successful poll.
    pub async fn runner_roster(
        &self,
        org: &str,
        roster: &[RunnerRosterEntry],
        now: DateTime<Utc>,
        grace_secs: i64,
    ) -> Result<RosterUpdate, GitHubError> {
        let registered = self.org_runners(org).await?;
        Ok(apply_fetch(roster, &registered, now, grace_secs))
    }
}

struct Response {
    body: String,
    link: Option<String>,
}

/// Minimal decode of `GET /repos/{owner}/{repo}` — only the accurate
/// `open_issues_count` (open issues + open PRs) the Repos panel needs.
#[derive(Debug, Clone, Deserialize)]
pub struct RepoDto {
    pub open_issues_count: u32,
}

fn header(resp: &reqwest::Response, name: &str) -> Option<String> {
    resp.headers().get(name)?.to_str().ok().map(str::to_string)
}

/// One place that turns a deserialisation failure into `DecodeFailed`, so every
/// endpoint reports an API contract change identically.
fn decode_failed(e: serde_json::Error) -> GitHubError {
    GitHubError::DecodeFailed(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::{PresenceState, DEFAULT_GRACE_SECS};
    use crate::runners::{RunnerOs, RunnerState};
    use crate::workflows::RunConclusion;
    use wiremock::matchers::{header as header_matcher, header_exists, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const RUNS_FIXTURE: &str = include_str!("../tests/fixtures/workflow_runs.json");
    const RUNNERS_FIXTURE: &str = include_str!("../tests/fixtures/runners.json");

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-29T12:05:00Z")
            .expect("valid timestamp")
            .with_timezone(&Utc)
    }

    fn json(body: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(body, "application/json")
    }

    /// A mock GitHub that answers `route` with `template` and nothing else.
    async fn github_replying(route: &str, template: ResponseTemplate) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(template)
            .mount(&server)
            .await;
        server
    }

    /// The three headers GitHub's REST API expects on every request. The
    /// matchers ARE the assertion: without them the mock never matches and the
    /// call comes back `HttpStatus(404)`.
    #[tokio::test]
    async fn sends_the_bearer_token_and_the_api_version_headers() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widget/actions/runs"))
            .and(header_matcher("authorization", "Bearer ghp_s3cret"))
            .and(header_matcher("accept", "application/vnd.github+json"))
            .and(header_matcher("x-github-api-version", API_VERSION))
            .and(header_exists("user-agent"))
            .and(query_param("per_page", RUNS_PER_PAGE))
            .respond_with(json(RUNS_FIXTURE))
            .mount(&server)
            .await;

        let runs = GitHubClient::with_base_url(server.uri(), "ghp_s3cret")
            .workflow_runs("acme/widget")
            .await
            .expect("should decode");
        assert_eq!(runs.len(), 6);
        assert_eq!(runs[0].name, "CI");
    }

    /// No token means no request at all — the client must not fire an
    /// unauthenticated call and let GitHub explain the problem.
    #[tokio::test]
    async fn an_empty_token_is_not_authenticated_before_any_request() {
        let server = MockServer::start().await;
        let client = GitHubClient::with_base_url(server.uri(), "");
        assert!(!client.has_token());
        let err = client.workflow_runs("o/r").await.unwrap_err();
        assert!(matches!(err, GitHubError::NotAuthenticated));
        assert!(server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty());
    }

    #[tokio::test]
    async fn a_401_is_not_authenticated() {
        let server = github_replying("/repos/o/r/actions/runs", ResponseTemplate::new(401)).await;
        let err = GitHubClient::with_base_url(server.uri(), "stale")
            .workflow_runs("o/r")
            .await
            .unwrap_err();
        assert!(matches!(err, GitHubError::NotAuthenticated), "got {err:?}");
        assert!(err.user_message().contains("token"));
    }

    /// A 403 whose response says the budget is spent is a rate limit, and it
    /// carries the reset instant so the panel can say when to look again.
    #[tokio::test]
    async fn a_403_with_no_remaining_budget_is_rate_limited_with_its_reset() {
        let server = github_replying(
            "/repos/o/r/actions/runs",
            ResponseTemplate::new(403)
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("x-ratelimit-reset", "1780000000"),
        )
        .await;
        let err = GitHubClient::with_base_url(server.uri(), "t")
            .workflow_runs("o/r")
            .await
            .unwrap_err();
        let GitHubError::RateLimited { reset } = err else {
            panic!("expected RateLimited, got {err:?}");
        };
        assert_eq!(reset, DateTime::from_timestamp(1_780_000_000, 0));
    }

    /// The other 403: a PAT missing a scope. Budget intact, so it must stay a
    /// plain HTTP 403 — telling the operator to wait for a reset would send
    /// them to fix the wrong thing.
    #[tokio::test]
    async fn a_403_with_budget_remaining_is_a_plain_http_error() {
        let server = github_replying(
            "/repos/o/r/actions/runs",
            ResponseTemplate::new(403).insert_header("x-ratelimit-remaining", "4999"),
        )
        .await;
        let err = GitHubClient::with_base_url(server.uri(), "t")
            .workflow_runs("o/r")
            .await
            .unwrap_err();
        assert!(matches!(err, GitHubError::HttpStatus(403)), "got {err:?}");
        assert!(err.user_message().contains("403"));
    }

    /// A 403 with no rate-limit headers at all is likewise not a rate limit —
    /// "no budget stated" is not "no budget left".
    #[tokio::test]
    async fn a_403_without_rate_limit_headers_is_a_plain_http_error() {
        let server = github_replying("/repos/o/r/actions/runs", ResponseTemplate::new(403)).await;
        let err = GitHubClient::with_base_url(server.uri(), "t")
            .workflow_runs("o/r")
            .await
            .unwrap_err();
        assert!(matches!(err, GitHubError::HttpStatus(403)), "got {err:?}");
    }

    /// Two different failures must not read identically to the operator.
    #[test]
    fn status_user_messages_name_the_code() {
        assert_eq!(
            GitHubError::HttpStatus(503).user_message(),
            "GitHub returned HTTP 503."
        );
        assert_eq!(
            GitHubError::HttpStatus(500).user_message(),
            "GitHub returned HTTP 500."
        );
    }

    /// An unknown reset must not be dressed up as a timestamp — the message
    /// says the limit was hit and stops there.
    #[test]
    fn a_rate_limit_without_a_reset_does_not_invent_one() {
        let message = GitHubError::RateLimited { reset: None }.user_message();
        assert_eq!(message, "GitHub rate limit exceeded.");
        assert!(!message.contains("Resets"));
    }

    #[tokio::test]
    async fn malformed_json_is_decode_failed() {
        let server = github_replying("/repos/o/r/actions/runs", json("{\"nope\":1}")).await;
        let err = GitHubClient::with_base_url(server.uri(), "t")
            .workflow_runs("o/r")
            .await
            .unwrap_err();
        assert!(matches!(err, GitHubError::DecodeFailed(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn an_unroutable_host_is_unreachable() {
        let err = GitHubClient::with_base_url("http://127.0.0.1:1", "t")
            .workflow_runs("o/r")
            .await
            .unwrap_err();
        assert!(matches!(err, GitHubError::Unreachable(_)), "got {err:?}");
    }

    // MARK: - The per_page=1 + rel="last" count trick

    #[tokio::test]
    async fn branch_count_reads_the_last_page_number() {
        let server = github_replying(
            "/repos/o/r/branches",
            json("[{\"name\":\"main\"}]").insert_header(
                "link",
                "<https://api.github.com/repositories/1/branches?per_page=1&page=2>; rel=\"next\", \
                 <https://api.github.com/repositories/1/branches?per_page=1&page=37>; rel=\"last\"",
            ),
        )
        .await;
        let count = GitHubClient::with_base_url(server.uri(), "t")
            .branch_count("o/r")
            .await
            .expect("count");
        assert_eq!(count, 37, "page 37 of 1-per-page == 37 branches");
    }

    /// No `Link` header at all is a genuinely single-page result: the body IS
    /// the whole collection, so its length is the count.
    #[tokio::test]
    async fn a_single_page_falls_back_to_the_array_length() {
        let server = github_replying("/repos/o/r/branches", json("[{\"name\":\"main\"}]")).await;
        let count = GitHubClient::with_base_url(server.uri(), "t")
            .branch_count("o/r")
            .await
            .expect("count");
        assert_eq!(count, 1);
    }

    /// An empty collection is a real answer of zero, not an unknown.
    #[tokio::test]
    async fn an_empty_collection_counts_as_zero() {
        let server = github_replying("/repos/o/r/pulls", json("[]")).await;
        let count = GitHubClient::with_base_url(server.uri(), "t")
            .open_pull_request_count("o/r")
            .await
            .expect("count");
        assert_eq!(count, 0);
    }

    /// The load-bearing one. Cursor pagination advertises `rel="next"` with no
    /// `rel="last"`; the total is unknowable, and answering "1" (the page size)
    /// would undercount every repo with more than one item. It must refuse.
    #[tokio::test]
    async fn cursor_pagination_refuses_to_undercount() {
        let server = github_replying(
            "/repos/o/r/pulls",
            json("[{\"number\":1}]").insert_header(
                "link",
                "<https://api.github.com/repositories/1/pulls?per_page=1&after=Y3Vyc29y&page=2>; \
                 rel=\"next\"",
            ),
        )
        .await;
        let err = GitHubClient::with_base_url(server.uri(), "t")
            .open_pull_request_count("o/r")
            .await
            .unwrap_err();
        assert!(
            matches!(err, GitHubError::UnsupportedPagination),
            "a cursor-paginated collection must not report the page size as the total, got {err:?}"
        );
    }

    /// A single-page body that is not an array is skew, not an empty
    /// collection — reporting 0 would invent a count from a payload we failed
    /// to understand.
    #[tokio::test]
    async fn a_non_array_single_page_body_is_decode_failed_not_zero() {
        let server =
            github_replying("/repos/o/r/branches", json("{\"message\":\"Not Found\"}")).await;
        let err = GitHubClient::with_base_url(server.uri(), "t")
            .branch_count("o/r")
            .await
            .unwrap_err();
        assert!(matches!(err, GitHubError::DecodeFailed(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn open_pull_request_count_asks_for_open_state_only() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls"))
            .and(query_param("state", "open"))
            .and(query_param("per_page", "1"))
            .respond_with(json("[]").insert_header(
                "link",
                "<https://api.github.com/x?per_page=1&page=4>; rel=\"last\"",
            ))
            .mount(&server)
            .await;
        let count = GitHubClient::with_base_url(server.uri(), "t")
            .open_pull_request_count("o/r")
            .await
            .expect("count");
        assert_eq!(count, 4);
    }

    /// The issue+PR total comes from the repo object, NOT the cursor-paginated
    /// `/issues` list.
    #[tokio::test]
    async fn open_issues_come_from_the_repo_object() {
        let server = github_replying(
            "/repos/acme/toolkit",
            json("{\"name\":\"platform\",\"open_issues_count\":2}"),
        )
        .await;
        let count = GitHubClient::with_base_url(server.uri(), "t")
            .open_issues_including_prs_count("acme/toolkit")
            .await
            .expect("count");
        assert_eq!(count, 2);
    }

    // MARK: - repo_health orchestration

    /// Mount the four per-repo endpoints a healthy fetch touches.
    async fn full_repo_server() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/actions/runs"))
            .respond_with(json(RUNS_FIXTURE))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/branches"))
            .respond_with(json("[]").insert_header(
                "link",
                "<https://api.github.com/x?per_page=1&page=37>; rel=\"last\"",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls"))
            .respond_with(json("[]").insert_header(
                "link",
                "<https://api.github.com/x?per_page=1&page=4>; rel=\"last\"",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r"))
            .respond_with(json("{\"open_issues_count\":12}"))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn repo_health_threads_the_runs_and_the_side_counts() {
        let server = full_repo_server().await;
        let health = GitHubClient::with_base_url(server.uri(), "t")
            .repo_health("o/r", None, now())
            .await;

        assert!(health.reachable);
        assert_eq!(
            health.main.as_ref().and_then(|r| r.conclusion),
            Some(RunConclusion::Success)
        );
        assert_eq!(health.remote_branches, Some(37));
        assert_eq!(health.open_prs, Some(4));
        assert_eq!(health.open_issues, Some(8), "12 inclusive − 4 PRs");
    }

    /// A failed runs fetch marks that repo unreachable — and only that repo.
    #[tokio::test]
    async fn a_failed_runs_fetch_marks_only_that_repo_unreachable() {
        let broken = github_replying("/repos/o/r/actions/runs", ResponseTemplate::new(500)).await;
        let unreachable = GitHubClient::with_base_url(broken.uri(), "t")
            .repo_health("o/r", None, now())
            .await;
        assert!(!unreachable.reachable);
        assert!(!unreachable.is_healthy());
        assert_eq!(unreachable.open_issues, None);
        assert_eq!(unreachable.remote_branches, None);

        let healthy_server = full_repo_server().await;
        let healthy = GitHubClient::with_base_url(healthy_server.uri(), "t")
            .repo_health("o/r", None, now())
            .await;
        assert!(
            healthy.reachable,
            "one repo's failure must not affect another's"
        );
    }

    /// A PAT missing the Issues/Pull requests scope fails only the side counts.
    /// Those render "—" (None) while the repo stays reachable — its runs
    /// decoded fine, and a missing scope is not an outage.
    #[tokio::test]
    async fn missing_side_count_scopes_render_unknown_not_zero() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/actions/runs"))
            .respond_with(json(RUNS_FIXTURE))
            .mount(&server)
            .await;
        // Everything else 403s the way a scope-starved PAT does.
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let health = GitHubClient::with_base_url(server.uri(), "t")
            .repo_health("o/r", None, now())
            .await;
        assert!(health.reachable, "the runs decoded — the repo is reachable");
        assert_eq!(health.remote_branches, None);
        assert_eq!(health.open_prs, None);
        assert_eq!(
            health.open_issues, None,
            "an unfetchable count is unknown, never zero"
        );
    }

    /// Half the inputs is not an answer: the issue count needs both totals, so
    /// a working repo object with a failing PR count still renders "—".
    #[tokio::test]
    async fn a_half_known_issue_count_stays_unknown() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/actions/runs"))
            .respond_with(json(RUNS_FIXTURE))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r"))
            .respond_with(json("{\"open_issues_count\":12}"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/branches"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let health = GitHubClient::with_base_url(server.uri(), "t")
            .repo_health("o/r", None, now())
            .await;
        assert_eq!(health.open_prs, None);
        assert_eq!(
            health.open_issues, None,
            "12 inclusive with an unknown PR count is not 12 issues"
        );
    }

    // MARK: - Runner roster

    #[tokio::test]
    async fn org_runners_decode_and_map() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/orgs/acme/actions/runners"))
            .and(query_param("per_page", RUNNERS_PER_PAGE))
            .respond_with(json(RUNNERS_FIXTURE))
            .mount(&server)
            .await;

        let runners = GitHubClient::with_base_url(server.uri(), "t")
            .org_runners("acme")
            .await
            .expect("decode");
        assert_eq!(runners.len(), 4);
        assert_eq!(runners[0].os, RunnerOs::MacOs);
        assert_eq!(runners[0].state, RunnerState::Busy);
    }

    /// The clock-freeze contract at the client boundary: a failed fetch returns
    /// `Err` and hands back no roster, so the caller keeps the one it had and
    /// every absence clock stays where the last successful poll left it.
    #[tokio::test]
    async fn a_failed_runner_fetch_returns_err_and_advances_nothing() {
        let server =
            github_replying("/orgs/acme/actions/runners", ResponseTemplate::new(500)).await;
        let client = GitHubClient::with_base_url(server.uri(), "t");

        let seeded = vec![RunnerRosterEntry {
            name: "mac-s2".into(),
            os: RunnerOs::MacOs,
            last_seen: now(),
        }];
        let err = client
            .runner_roster(
                "acme",
                &seeded,
                now() + chrono::TimeDelta::seconds(3_600),
                DEFAULT_GRACE_SECS,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, GitHubError::HttpStatus(500)), "got {err:?}");
        assert_eq!(
            seeded[0].last_seen,
            now(),
            "the caller's roster is untouched"
        );
    }

    #[tokio::test]
    async fn a_successful_runner_fetch_learns_the_roster_and_reports_absences() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/orgs/acme/actions/runners"))
            .respond_with(json(RUNNERS_FIXTURE))
            .mount(&server)
            .await;

        // A runner remembered from an earlier poll that GitHub no longer lists.
        let seeded = vec![RunnerRosterEntry {
            name: "mac-gone".into(),
            os: RunnerOs::MacOs,
            last_seen: now() - chrono::TimeDelta::seconds(600),
        }];
        let update = GitHubClient::with_base_url(server.uri(), "t")
            .runner_roster("acme", &seeded, now(), DEFAULT_GRACE_SECS)
            .await
            .expect("fetch");

        assert_eq!(update.runners.len(), 4);
        assert_eq!(update.summary.online, 3);
        assert_eq!(update.roster.len(), 5, "4 registered + 1 remembered");
        assert_eq!(
            update.absent.first().map(|a| a.state),
            Some(PresenceState::Missing { absence_secs: 600 })
        );
    }
}
