import Foundation

/// Thin, PAT-authenticated GitHub REST client used by the cockpit's live GitHub
/// panels (`GHWorkflowsService`, `GHRunnersService`). It loads a fine-grained
/// Personal Access Token from the Keychain and performs authenticated GETs.
///
/// The legacy OAuth-PKCE flow, the per-launch git-monitor workflow fetch/persist
/// methods, and the `ServiceConnection`/`/user` write-back were removed in
/// issue #30 — PAT is the only supported auth path.
@MainActor
final class GitHubService: ObservableObject {
    static let shared = GitHubService()

    @Published var isAuthenticated = false

    private let baseURL = "https://api.github.com"
    private var accessToken: String?

    // Rate limiting
    private var rateLimitRemaining: Int = 60
    private var rateLimitReset: Date?

    init() {
        loadStoredToken()
    }

    // MARK: - Authentication

    /// (Re)loads a Personal Access Token from the Keychain and sets the
    /// authenticated state accordingly. This is the supported path for
    /// fine-grained PAT auth. Call once at startup and again whenever the user
    /// saves/clears a token in Settings. Reads the Keychain only — no network.
    func configureFromKeychain() {
        if let token = KeychainHelper.shared.loadGitHubToken(), !token.isEmpty {
            accessToken = token
            isAuthenticated = true
        } else {
            accessToken = nil
            isAuthenticated = false
        }
    }

    /// Whether a usable token is currently loaded (without exposing it).
    var hasToken: Bool {
        accessToken?.isEmpty == false
    }

    /// Performs an authenticated GET against an arbitrary REST endpoint and
    /// returns the raw data. Reuses the shared Bearer-token / rate-limit
    /// plumbing. Used by the live CI panels (GitHub Workflows, GitHub Runners).
    func getRaw(endpoint: String, queryItems: [URLQueryItem]? = nil) async throws -> Data {
        try await request(endpoint: endpoint, method: "GET", queryItems: queryItems)
    }

    /// Total number of branches for a repo, using the cheap `per_page=1` +
    /// `Link: rel="last"` trick: with one item per page, the last page number
    /// equals the branch count — one request, no pagination. Falls back to the
    /// returned array's length when there is no `Link` header (0 or 1 branch).
    func branchCount(for repo: String) async throws -> Int {
        try await openCount(endpoint: "/repos/\(repo)/branches")
    }

    /// Number of **open pull requests** for a repo, via the same `per_page=1` +
    /// `Link: rel="last"` count trick as `branchCount`. Requires the PAT's
    /// Pull requests (read) permission.
    func openPullRequestCount(for repo: String) async throws -> Int {
        try await openCount(
            endpoint: "/repos/\(repo)/pulls",
            extraQuery: [URLQueryItem(name: "state", value: "open")]
        )
    }

    /// Number of open items on a repo's **issues** endpoint. GitHub counts every
    /// pull request as an issue, so this total is `open issues + open PRs`; callers
    /// subtract the open-PR count to get a pure open-issue count. Requires the PAT's
    /// Issues (read) permission.
    func openIssuesIncludingPRsCount(for repo: String) async throws -> Int {
        try await openCount(
            endpoint: "/repos/\(repo)/issues",
            extraQuery: [URLQueryItem(name: "state", value: "open")]
        )
    }

    /// Shared implementation of the cheap `per_page=1` + `Link: rel="last"` count:
    /// with one item per page, the last page number equals the total. Falls back to
    /// the returned array's length when there is no `Link` header (0 or 1 item).
    private func openCount(endpoint: String, extraQuery: [URLQueryItem] = []) async throws -> Int {
        let (data, http) = try await requestWithResponse(
            endpoint: endpoint,
            method: "GET",
            queryItems: [URLQueryItem(name: "per_page", value: "1")] + extraQuery
        )
        if let last = Self.lastPage(fromLinkHeader: http.value(forHTTPHeaderField: "Link")) {
            return last
        }
        let array = (try? JSONSerialization.jsonObject(with: data)) as? [Any]
        return array?.count ?? 0
    }

    /// Parses a GitHub `Link` header and returns the `page=` value of the
    /// `rel="last"` URL (the total page count). Returns nil when the header is
    /// absent or has no `last` relation (single-page result). Pure — `nonisolated`
    /// so it's callable off the main actor and from tests.
    nonisolated static func lastPage(fromLinkHeader header: String?) -> Int? {
        guard let header else { return nil }
        // Each comma-separated part looks like: <https://…?per_page=1&page=37>; rel="last"
        for part in header.split(separator: ",") {
            guard part.contains("rel=\"last\"") else { continue }
            guard let open = part.firstIndex(of: "<"),
                  let close = part.firstIndex(of: ">"),
                  open < close,
                  let url = URLComponents(string: String(part[part.index(after: open) ..< close])),
                  let page = url.queryItems?.first(where: { $0.name == "page" })?.value
            else { return nil }
            return Int(page)
        }
        return nil
    }

    // MARK: - Network Request

    private func request(endpoint: String, method: String, queryItems: [URLQueryItem]? = nil) async throws -> Data {
        try await requestWithResponse(endpoint: endpoint, method: method, queryItems: queryItems).0
    }

    private func requestWithResponse(
        endpoint: String,
        method: String,
        queryItems: [URLQueryItem]? = nil
    ) async throws -> (Data, HTTPURLResponse) {
        guard let accessToken else {
            throw GitHubError.notAuthenticated
        }

        var components = URLComponents(string: baseURL + endpoint)!
        components.queryItems = queryItems

        guard let url = components.url else {
            throw GitHubError.invalidURL
        }

        var request = URLRequest(url: url)
        request.httpMethod = method
        request.setValue("Bearer \(accessToken)", forHTTPHeaderField: "Authorization")
        request.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
        request.setValue("2022-11-28", forHTTPHeaderField: "X-GitHub-Api-Version")

        let (data, response) = try await URLSession.shared.data(for: request)

        guard let httpResponse = response as? HTTPURLResponse else {
            throw GitHubError.invalidResponse
        }

        // Update rate limit info
        if let remaining = httpResponse.value(forHTTPHeaderField: "X-RateLimit-Remaining") {
            rateLimitRemaining = Int(remaining) ?? 0
        }

        if let resetTime = httpResponse.value(forHTTPHeaderField: "X-RateLimit-Reset"),
           let timestamp = Double(resetTime)
        {
            rateLimitReset = Date(timeIntervalSince1970: timestamp)
        }

        guard (200 ... 299).contains(httpResponse.statusCode) else {
            if httpResponse.statusCode == 401 {
                throw GitHubError.notAuthenticated
            } else if httpResponse.statusCode == 403, rateLimitRemaining == 0 {
                throw GitHubError.rateLimitExceeded(resetTime: rateLimitReset ?? Date())
            }
            throw GitHubError.httpError(statusCode: httpResponse.statusCode)
        }

        return (data, httpResponse)
    }

    // MARK: - Keychain

    private func loadStoredToken() {
        if let token = KeychainHelper.shared.loadGitHubToken(), !token.isEmpty {
            accessToken = token
            isAuthenticated = true
        }
    }

    func disconnect() {
        accessToken = nil
        isAuthenticated = false
        KeychainHelper.shared.deleteGitHubToken()
    }
}

// MARK: - Error Types

enum GitHubError: LocalizedError {
    case notAuthenticated
    case invalidURL
    case invalidResponse
    case rateLimitExceeded(resetTime: Date)
    case httpError(statusCode: Int)

    var errorDescription: String? {
        switch self {
        case .notAuthenticated:
            "Not authenticated with GitHub"
        case .invalidURL:
            "Invalid URL"
        case .invalidResponse:
            "Invalid response from GitHub"
        case let .rateLimitExceeded(resetTime):
            "GitHub rate limit exceeded. Resets at \(resetTime.formatted())"
        case let .httpError(code):
            "HTTP error: \(code)"
        }
    }
}

// MARK: - Response DTOs

struct WorkflowRunsResponse: Codable {
    let totalCount: Int
    let workflowRuns: [WorkflowRunDTO]

    enum CodingKeys: String, CodingKey {
        case totalCount = "total_count"
        case workflowRuns = "workflow_runs"
    }
}

struct WorkflowRunDTO: Codable {
    let id: Int64
    let name: String
    let nodeId: String
    let headBranch: String?
    let headSha: String
    let runNumber: Int
    let workflowId: Int64
    let event: String
    let status: String
    let conclusion: String?
    let workflowUrl: String
    let htmlUrl: String
    let createdAt: String
    let updatedAt: String
    let runStartedAt: String?
    let runAttemptedAt: String?
    let actor: ActorDTO?
    let triggeringActor: ActorDTO?
    let displayTitle: String

    enum CodingKeys: String, CodingKey {
        case id, name, event, status, conclusion, actor
        case nodeId = "node_id"
        case headBranch = "head_branch"
        case headSha = "head_sha"
        case runNumber = "run_number"
        case workflowId = "workflow_id"
        case workflowUrl = "workflow_url"
        case htmlUrl = "html_url"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case runStartedAt = "run_started_at"
        case runAttemptedAt = "run_attempted_at"
        case triggeringActor = "triggering_actor"
        case displayTitle = "display_title"
    }
}

struct ActorDTO: Codable {
    let login: String
    let id: Int
    let nodeId: String
    let avatarUrl: String
    let gravatarId: String
    let type: String

    enum CodingKeys: String, CodingKey {
        case login, id, type
        case nodeId = "node_id"
        case avatarUrl = "avatar_url"
        case gravatarId = "gravatar_id"
    }
}
