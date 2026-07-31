import Foundation
import SwiftUI

// MARK: - Transport seam

/// The single Sentry REST read the Usage panel needs, behind a protocol so the fold in
/// `SentryUsageMapping` is unit-testable with a stub and no network — the same seam
/// `NeonConsumptionFetching` gives `NeonUsageService`.
protocol SentryStatsFetching: Sendable {
    /// Error-category event counts for `orgSlug` over `statsPeriod`, grouped by outcome.
    func errorOutcomes(orgSlug: String, statsPeriod: String) async throws -> SentryStatsResponse
}

/// `SentryStatsFetching` over the public Sentry API, authenticated with a token
/// carrying the read-only `org:read` scope as `Authorization: Bearer <token>`. The
/// token is never logged and never leaves this type.
struct URLSessionSentryStatsFetcher: SentryStatsFetching {
    /// `{org}` is substituted into the path; the org slug is the only variable part.
    static let endpointTemplate = "https://sentry.io/api/0/organizations/%@/stats_v2/"

    let token: String
    let session: URLSession

    init(token: String, session: URLSession = .shared) {
        self.token = token
        self.session = session
    }

    func errorOutcomes(orgSlug: String, statsPeriod: String) async throws -> SentryStatsResponse {
        // The slug lands in a path segment, so it must be percent-encoded rather than
        // interpolated raw — a stray `/` would otherwise retarget the request.
        guard let encodedSlug = orgSlug.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed),
              var components = URLComponents(string: String(format: Self.endpointTemplate, encodedSlug))
        else { throw SentryUsageError.invalidURL }

        components.queryItems = [
            URLQueryItem(name: "field", value: SentryStatsQuery.field),
            URLQueryItem(name: "category", value: SentryStatsQuery.category),
            URLQueryItem(name: "groupBy", value: SentryStatsQuery.groupBy),
            URLQueryItem(name: "statsPeriod", value: statsPeriod)
        ]
        guard let url = components.url else { throw SentryUsageError.invalidURL }

        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.timeoutInterval = 30
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")

        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw SentryUsageError.invalidResponse }
        guard (200 ... 299).contains(http.statusCode) else {
            throw SentryUsageError.http(status: http.statusCode, body: String(data: data, encoding: .utf8))
        }
        do {
            return try JSONDecoder().decode(SentryStatsResponse.self, from: data)
        } catch {
            throw SentryUsageError.decodingFailed(error.localizedDescription)
        }
    }
}

// MARK: - Service

/// Reads accepted error-event counts for one Sentry organization over a rolling 30-day
/// window and publishes a `SentryUsageSummary` onto the Usage panel. Singleton (like
/// `NeonUsageService`) so Settings can reconfigure it after the user saves or clears
/// the token.
///
/// The token lives in the Keychain only; the non-secret org slug and monthly event
/// quota live in `@AppStorage`/UserDefaults — the same split `AzureCostService` uses
/// for its SAS and monthly budget.
@MainActor
final class SentryUsageService: ObservableObject {
    static let shared = SentryUsageService()

    /// `@AppStorage`/UserDefaults key holding the Sentry organization slug (the
    /// `{organization_id_or_slug}` path segment). Non-secret, so it deliberately does
    /// *not* go in the Keychain.
    static let orgSlugDefaultsKey = "sentryOrgSlug"

    /// `@AppStorage`/UserDefaults key holding the user's monthly accepted-error quota.
    /// 0 means "no quota", which hides the panel's quota bar. Read by the panel — the
    /// service never needs it, since it only reports consumption.
    static let monthlyEventQuotaDefaultsKey = "sentryMonthlyEventQuota"

    /// Poll cadence. Quota consumption is slow-moving over a 30-day window, so this
    /// runs on its own fixed 1-hour cadence rather than the shared `RefreshInterval`
    /// setting — same treatment as the Azure cost export (#114) and Neon (#103).
    static let pollInterval: TimeInterval = 60 * 60

    @Published private(set) var summary: SentryUsageSummary?
    @Published private(set) var isLoading = false
    @Published private(set) var isConfigured: Bool
    @Published private(set) var lastUpdated: Date?
    @Published private(set) var lastError: String?

    /// Builds the transport from a token. Overridable so a test can inject a stub; the
    /// default talks to the real Sentry API.
    private let makeFetcher: (String) -> SentryStatsFetching
    /// Reads the token. The default is the Keychain — the only place production ever
    /// stores it. Injectable purely so tests can drive the service without writing a
    /// credential into the developer's login keychain.
    private let tokenProvider: () -> String?
    /// Reads the org slug from UserDefaults (written by Settings' `@AppStorage`).
    private let orgSlugProvider: () -> String?

    private var task: Task<Void, Never>?

    init(
        makeFetcher: @escaping (String) -> SentryStatsFetching = { URLSessionSentryStatsFetcher(token: $0) },
        tokenProvider: @escaping () -> String? = { KeychainHelper.shared.loadSentryUsageToken() },
        orgSlugProvider: @escaping () -> String? = {
            UserDefaults.standard.string(forKey: SentryUsageService.orgSlugDefaultsKey)
        }
    ) {
        self.makeFetcher = makeFetcher
        self.tokenProvider = tokenProvider
        self.orgSlugProvider = orgSlugProvider
        isConfigured = tokenProvider()?.isEmpty == false
    }

    func refresh() async {
        guard let token = tokenProvider(), !token.isEmpty else {
            // No token: the panel hides the Sentry section entirely, so drop everything
            // rather than leave a stale figure behind a hidden section.
            isConfigured = false
            summary = nil
            lastError = nil
            isLoading = false
            return
        }
        isConfigured = true

        let orgSlug = (orgSlugProvider() ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        guard !orgSlug.isEmpty else {
            // The slug is a path segment; without it there is nothing to ask for, so
            // show the `—` path with a message naming what's missing.
            summary = nil
            lastError = SentryUsageError.missingOrgSlug.errorDescription
            isLoading = false
            return
        }

        isLoading = true
        do {
            let response = try await makeFetcher(token)
                .errorOutcomes(orgSlug: orgSlug, statsPeriod: SentryStatsQuery.statsPeriod)
            let fresh = SentryUsageMapping.summarize(response)
            summary = fresh
            lastUpdated = Date()
            // A successful call that reports no outcome groups measured nothing: the
            // figure stays nil (`—`) and the footer says why, instead of a fake 0.
            lastError = fresh.outcomeGroupCount == 0 ? Self.noStatsMessage : nil
        } catch {
            // Keep the last-good summary (if any) and surface the failure in the footer
            // — the window is 30 days wide, so carrying last-good forward beats
            // blanking the section on one transient failure.
            lastError = Self.friendlyMessage(for: error)
        }
        isLoading = false
    }

    /// Initial load plus a repeating refresh on the fixed 1-hour cadence.
    func start(interval: TimeInterval = SentryUsageService.pollInterval) {
        guard task == nil else { return }
        task = Task { [weak self] in
            guard let self else { return }
            await refresh()
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: UInt64(interval * 1_000_000_000))
                if Task.isCancelled { break }
                await refresh()
            }
        }
    }

    func stop() {
        task?.cancel()
        task = nil
    }

    /// Stops the current polling loop and restarts it on a new cadence.
    func restart(interval: TimeInterval) {
        stop()
        start(interval: interval)
    }

    /// Re-read token/slug presence and refresh now — called after the user saves or
    /// clears the Sentry settings so the panel updates without waiting for the poll.
    func configureFromKeychain() {
        isConfigured = tokenProvider()?.isEmpty == false
        Task { await refresh() }
    }

    deinit { task?.cancel() }

    /// Shown when the API answers successfully but reports no outcome groups at all —
    /// the wrong org slug, or an org that has ingested nothing in the window.
    static let noStatsMessage =
        "no Sentry event stats reported — check the org slug in Settings"

    static func friendlyMessage(for error: Error) -> String {
        if let sentryError = error as? SentryUsageError {
            if sentryError.isAuthFailure {
                return "token invalid — paste a new one in Settings"
            }
            if case let .http(status, _) = sentryError, status == 404 {
                return "Sentry org not found — check the org slug in Settings"
            }
            return sentryError.errorDescription ?? "couldn't refresh Sentry usage"
        }
        return error.localizedDescription
    }
}
