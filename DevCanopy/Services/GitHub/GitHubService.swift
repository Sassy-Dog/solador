import Foundation
import SwiftData
import AuthenticationServices
import CommonCrypto

@MainActor
final class GitHubService: NSObject, ObservableObject {
    static let shared = GitHubService()
    
    @Published var isAuthenticated = false
    @Published var currentUser: GitHubUser?
    
    private let baseURL = "https://api.github.com"
    private var accessToken: String?
    private let modelContext: ModelContext?
    
    // OAuth configuration
    private let clientId = "YOUR_GITHUB_OAUTH_APP_ID" // TODO: Move to config
    private let redirectURI = "devcanopy://oauth/github"
    private let scope = "repo workflow"
    
    // Rate limiting
    private var rateLimitRemaining: Int = 60
    private var rateLimitReset: Date?
    
    override init() {
        self.modelContext = nil
        super.init()
        loadStoredToken()
    }
    
    init(modelContext: ModelContext) {
        self.modelContext = modelContext
        super.init()
        loadStoredToken()
    }
    
    // MARK: - Authentication
    
    func authenticate() async throws {
        let authURL = URL(string: "https://github.com/login/oauth/authorize")!
        var components = URLComponents(url: authURL, resolvingAgainstBaseURL: false)!
        
        // Generate PKCE challenge
        let codeVerifier = generateCodeVerifier()
        let codeChallenge = generateCodeChallenge(from: codeVerifier)
        
        // Store verifier for exchange
        UserDefaults.standard.set(codeVerifier, forKey: "github_pkce_verifier")
        
        components.queryItems = [
            URLQueryItem(name: "client_id", value: clientId),
            URLQueryItem(name: "redirect_uri", value: redirectURI),
            URLQueryItem(name: "scope", value: scope),
            URLQueryItem(name: "state", value: UUID().uuidString),
            URLQueryItem(name: "code_challenge", value: codeChallenge),
            URLQueryItem(name: "code_challenge_method", value: "S256")
        ]
        
        guard let url = components.url else { throw GitHubError.invalidURL }
        
        // Present authentication session
        let session = ASWebAuthenticationSession(url: url, callbackURLScheme: "devcanopy") { callbackURL, error in
            if let error = error {
                appLogger.error("GitHub auth error: \(error)")
                return
            }
            
            guard let callbackURL = callbackURL,
                  let code = URLComponents(url: callbackURL, resolvingAgainstBaseURL: false)?
                    .queryItems?.first(where: { $0.name == "code" })?.value else {
                appLogger.error("No authorization code received")
                return
            }
            
            Task {
                await self.exchangeCodeForToken(code: code)
            }
        }
        
        session.presentationContextProvider = self
        session.prefersEphemeralWebBrowserSession = false
        
        if !session.start() {
            throw GitHubError.authenticationFailed
        }
    }
    
    private func exchangeCodeForToken(code: String) async {
        // In a real app, this would go through your backend
        // GitHub doesn't support PKCE for public clients yet, so this is a placeholder
        // You'd need to implement a backend service to handle the token exchange
        appLogger.warning("GitHub OAuth token exchange requires backend implementation")
    }
    
    func setAccessToken(_ token: String) {
        self.accessToken = token
        saveTokenToKeychain(token)
        self.isAuthenticated = true
        
        Task {
            try? await fetchCurrentUser()
        }
    }
    
    // MARK: - API Methods
    
    func fetchWorkflows(for repository: TrackedRepository) async throws -> [GitHubWorkflow] {
        guard let repoIdentifier = repository.githubRepoIdentifier else {
            throw GitHubError.noRepositoryIdentifier
        }
        
        let endpoint = "/repos/\(repoIdentifier)/actions/workflows"
        let data = try await request(endpoint: endpoint, method: "GET")
        
        let response = try JSONDecoder().decode(WorkflowsResponse.self, from: data)
        
        return response.workflows.map { dto in
            GitHubWorkflow(
                workflowId: dto.id,
                nodeId: dto.nodeId,
                name: dto.name,
                path: dto.path,
                state: WorkflowState(rawValue: dto.state) ?? .active
            )
        }
    }
    
    func fetchWorkflowRuns(for workflow: GitHubWorkflow, repository: TrackedRepository, limit: Int = 10) async throws -> [GitHubWorkflowRun] {
        guard let repoIdentifier = repository.githubRepoIdentifier else {
            throw GitHubError.noRepositoryIdentifier
        }
        
        let endpoint = "/repos/\(repoIdentifier)/actions/workflows/\(workflow.workflowId)/runs"
        let queryItems = [
            URLQueryItem(name: "per_page", value: String(limit)),
            URLQueryItem(name: "status", value: "completed")
        ]
        
        let data = try await request(endpoint: endpoint, method: "GET", queryItems: queryItems)
        let response = try JSONDecoder().decode(WorkflowRunsResponse.self, from: data)
        
        return response.workflowRuns.map { dto in
            let run = GitHubWorkflowRun(
                runId: dto.id,
                runNumber: dto.runNumber,
                nodeId: dto.nodeId,
                name: dto.name,
                displayTitle: dto.displayTitle,
                status: RunStatus(rawValue: dto.status) ?? .completed,
                headSha: dto.headSha,
                event: dto.event,
                htmlUrl: dto.htmlUrl,
                workflowUrl: dto.workflowUrl
            )
            
            run.conclusion = dto.conclusion.flatMap { RunConclusion(rawValue: $0) }
            run.headBranch = dto.headBranch
            run.createdAt = ISO8601DateFormatter().date(from: dto.createdAt) ?? Date()
            run.updatedAt = ISO8601DateFormatter().date(from: dto.updatedAt) ?? Date()
            run.runStartedAt = dto.runStartedAt.flatMap { ISO8601DateFormatter().date(from: $0) }
            run.completedAt = dto.runAttemptedAt.flatMap { ISO8601DateFormatter().date(from: $0) }
            
            return run
        }
    }
    
    func fetchLatestRuns(for repository: TrackedRepository) async throws -> [GitHubWorkflowRun] {
        guard let repoIdentifier = repository.githubRepoIdentifier else {
            throw GitHubError.noRepositoryIdentifier
        }
        
        let endpoint = "/repos/\(repoIdentifier)/actions/runs"
        let queryItems = [
            URLQueryItem(name: "per_page", value: "20"),
            URLQueryItem(name: "branch", value: repository.currentBranch ?? "main")
        ]
        
        let data = try await request(endpoint: endpoint, method: "GET", queryItems: queryItems)
        let response = try JSONDecoder().decode(WorkflowRunsResponse.self, from: data)
        
        // Group by workflow and get latest for each
        var latestRuns: [Int64: GitHubWorkflowRun] = [:]
        
        for dto in response.workflowRuns {
            let run = GitHubWorkflowRun(
                runId: dto.id,
                runNumber: dto.runNumber,
                nodeId: dto.nodeId,
                name: dto.name,
                displayTitle: dto.displayTitle,
                status: RunStatus(rawValue: dto.status) ?? .completed,
                headSha: dto.headSha,
                event: dto.event,
                htmlUrl: dto.htmlUrl,
                workflowUrl: dto.workflowUrl
            )
            
            run.conclusion = dto.conclusion.flatMap { RunConclusion(rawValue: $0) }
            run.headBranch = dto.headBranch
            run.createdAt = ISO8601DateFormatter().date(from: dto.createdAt) ?? Date()
            
            // Only keep the latest run for each workflow
            if latestRuns[dto.workflowId] == nil || latestRuns[dto.workflowId]!.createdAt < run.createdAt {
                latestRuns[dto.workflowId] = run
            }
        }
        
        return Array(latestRuns.values)
    }
    
    // MARK: - User Info
    
    func fetchCurrentUser() async throws {
        let data = try await request(endpoint: "/user", method: "GET")
        let user = try JSONDecoder().decode(GitHubUser.self, from: data)
        self.currentUser = user
        
        // Store in service connection
        if let modelContext = modelContext {
            let connection = ServiceConnection(service: .github, accountIdentifier: user.login)
            connection.accountName = user.name
            modelContext.insert(connection)
            try? modelContext.save()
        }
    }
    
    // MARK: - Network Request
    
    private func request(endpoint: String, method: String, queryItems: [URLQueryItem]? = nil) async throws -> Data {
        guard let accessToken = accessToken else {
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
           let timestamp = Double(resetTime) {
            rateLimitReset = Date(timeIntervalSince1970: timestamp)
        }
        
        guard (200...299).contains(httpResponse.statusCode) else {
            if httpResponse.statusCode == 401 {
                throw GitHubError.notAuthenticated
            } else if httpResponse.statusCode == 403 && rateLimitRemaining == 0 {
                throw GitHubError.rateLimitExceeded(resetTime: rateLimitReset ?? Date())
            }
            throw GitHubError.httpError(statusCode: httpResponse.statusCode)
        }
        
        return data
    }
    
    // MARK: - Keychain
    
    private func saveTokenToKeychain(_ token: String) {
        do {
            try KeychainHelper.shared.saveGitHubToken(token)
            appLogger.info("GitHub token saved to keychain")
        } catch {
            appLogger.error("Failed to save GitHub token to keychain: \(error)")
        }
    }
    
    private func loadStoredToken() {
        if let token = KeychainHelper.shared.loadGitHubToken() {
            self.accessToken = token
            self.isAuthenticated = true
            
            Task {
                try? await fetchCurrentUser()
            }
        }
    }
    
    func disconnect() {
        self.accessToken = nil
        self.isAuthenticated = false
        self.currentUser = nil
        KeychainHelper.shared.deleteGitHubToken()
    }
    
    // MARK: - PKCE Helpers
    
    private func generateCodeVerifier() -> String {
        let verifierData = Data((0..<32).map { _ in UInt8.random(in: 0...255) })
        return verifierData.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
    
    private func generateCodeChallenge(from verifier: String) -> String {
        guard let data = verifier.data(using: .utf8) else { return "" }
        var hash = [UInt8](repeating: 0, count: Int(CC_SHA256_DIGEST_LENGTH))
        data.withUnsafeBytes {
            _ = CC_SHA256($0.baseAddress, CC_LONG(data.count), &hash)
        }
        return Data(hash).base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}

// MARK: - ASWebAuthenticationPresentationContextProviding

extension GitHubService: ASWebAuthenticationPresentationContextProviding {
    func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        NSApp.keyWindow ?? NSApp.windows.first!
    }
}

// MARK: - Error Types

enum GitHubError: LocalizedError {
    case notAuthenticated
    case noRepositoryIdentifier
    case invalidURL
    case invalidResponse
    case authenticationFailed
    case rateLimitExceeded(resetTime: Date)
    case httpError(statusCode: Int)
    
    var errorDescription: String? {
        switch self {
        case .notAuthenticated:
            return "Not authenticated with GitHub"
        case .noRepositoryIdentifier:
            return "Repository has no GitHub identifier"
        case .invalidURL:
            return "Invalid URL"
        case .invalidResponse:
            return "Invalid response from GitHub"
        case .authenticationFailed:
            return "Authentication failed"
        case .rateLimitExceeded(let resetTime):
            return "GitHub rate limit exceeded. Resets at \(resetTime.formatted())"
        case .httpError(let code):
            return "HTTP error: \(code)"
        }
    }
}

// MARK: - Response DTOs

struct GitHubUser: Codable {
    let login: String
    let id: Int
    let nodeId: String
    let avatarUrl: String
    let name: String?
    let email: String?
    let publicRepos: Int
    let privateRepos: Int?
    
    enum CodingKeys: String, CodingKey {
        case login, id, name, email
        case nodeId = "node_id"
        case avatarUrl = "avatar_url"
        case publicRepos = "public_repos"
        case privateRepos = "private_repos"
    }
}

struct WorkflowsResponse: Codable {
    let totalCount: Int
    let workflows: [WorkflowDTO]
    
    enum CodingKeys: String, CodingKey {
        case totalCount = "total_count"
        case workflows
    }
}

struct WorkflowDTO: Codable {
    let id: Int64
    let nodeId: String
    let name: String
    let path: String
    let state: String
    let createdAt: String
    let updatedAt: String
    let url: String
    let htmlUrl: String
    let badgeUrl: String
    
    enum CodingKeys: String, CodingKey {
        case id, name, path, state, url
        case nodeId = "node_id"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case htmlUrl = "html_url"
        case badgeUrl = "badge_url"
    }
}

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