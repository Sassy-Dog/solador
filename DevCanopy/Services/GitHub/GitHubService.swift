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

    // MARK: - Network Request

    private func request(endpoint: String, method: String, queryItems: [URLQueryItem]? = nil) async throws -> Data {
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

        return data
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
