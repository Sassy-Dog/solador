import Foundation
import SwiftUI

// MARK: - Transport seam

/// The single Neon REST read the Usage panel needs, behind a protocol so the fold +
/// unit conversion in `NeonUsageMapping` are unit-testable with a stub and no
/// network — the same seam `BlobFetching` gives `AzureCostService`.
protocol NeonConsumptionFetching: Sendable {
    /// Org-wide consumption history over `[from, to]`, one monthly bucket per project.
    func consumptionHistory(orgID: String, from: Date, to: Date) async throws -> NeonConsumptionResponse
}

/// `NeonConsumptionFetching` over the public Neon API, authenticated with an
/// organization API key as `Authorization: Bearer <key>`. The key is never logged
/// and never leaves this type.
struct URLSessionNeonFetcher: NeonConsumptionFetching {
    static let endpoint = "https://console.neon.tech/api/v2/consumption_history/v2/projects"

    let apiKey: String
    let session: URLSession

    init(apiKey: String, session: URLSession = .shared) {
        self.apiKey = apiKey
        self.session = session
    }

    func consumptionHistory(orgID: String, from: Date, to: Date) async throws -> NeonConsumptionResponse {
        guard var components = URLComponents(string: Self.endpoint) else { throw NeonUsageError.invalidURL }
        components.queryItems = [
            URLQueryItem(name: "from", value: NeonUsageMapping.rfc3339(from)),
            URLQueryItem(name: "to", value: NeonUsageMapping.rfc3339(to)),
            // Monthly granularity: one bucket per project for the MTD window, which
            // is all the panel renders (and the smallest response for the job).
            URLQueryItem(name: "granularity", value: "monthly"),
            URLQueryItem(name: "org_id", value: orgID),
            URLQueryItem(name: "metrics", value: NeonMetric.requested.joined(separator: ","))
        ]
        guard let url = components.url else { throw NeonUsageError.invalidURL }

        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.timeoutInterval = 30
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("Bearer \(apiKey)", forHTTPHeaderField: "Authorization")

        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw NeonUsageError.invalidResponse }
        guard (200 ... 299).contains(http.statusCode) else {
            throw NeonUsageError.http(status: http.statusCode, body: String(data: data, encoding: .utf8))
        }
        do {
            return try JSONDecoder().decode(NeonConsumptionResponse.self, from: data)
        } catch {
            throw NeonUsageError.decodingFailed(error.localizedDescription)
        }
    }
}

// MARK: - Service

/// Reads month-to-date Neon consumption (compute + branch storage) for one
/// organization and publishes a `NeonUsageSummary` onto the Usage panel. Singleton
/// (like `AzureCostService`) so Settings can reconfigure it after the user saves or
/// clears the API key.
///
/// The API key lives in the Keychain only; the non-secret `org_id` lives in
/// `@AppStorage`/UserDefaults, the same split `AzureCostService` uses for its SAS
/// and monthly budget.
@MainActor
final class NeonUsageService: ObservableObject {
    static let shared = NeonUsageService()

    /// `@AppStorage`/UserDefaults key holding the Neon organization id. Non-secret,
    /// so it deliberately does *not* go in the Keychain.
    static let orgIDDefaultsKey = "neonOrgID"

    /// Poll cadence. Consumption is slow-moving (billing-grade aggregates), so this
    /// runs on its own fixed 1-hour cadence rather than the shared `RefreshInterval`
    /// setting — same treatment as the Azure cost export (#114).
    static let pollInterval: TimeInterval = 60 * 60

    @Published private(set) var summary: NeonUsageSummary?
    @Published private(set) var isLoading = false
    @Published private(set) var isConfigured: Bool
    @Published private(set) var lastUpdated: Date?
    @Published private(set) var lastError: String?

    /// Builds the transport from an API key. Overridable so a test can inject a stub;
    /// the default talks to the real Neon API.
    private let makeFetcher: (String) -> NeonConsumptionFetching
    /// Reads the API key. The default is the Keychain — the only place production
    /// ever stores it. Injectable purely so tests can drive the service without
    /// writing a credential into the developer's login keychain.
    private let apiKeyProvider: () -> String?
    /// Reads the org id from UserDefaults (written by Settings' `@AppStorage`).
    private let orgIDProvider: () -> String?

    private var task: Task<Void, Never>?

    init(
        makeFetcher: @escaping (String) -> NeonConsumptionFetching = { URLSessionNeonFetcher(apiKey: $0) },
        apiKeyProvider: @escaping () -> String? = { KeychainHelper.shared.loadNeonAPIKey() },
        orgIDProvider: @escaping () -> String? = { UserDefaults.standard.string(forKey: NeonUsageService.orgIDDefaultsKey) }
    ) {
        self.makeFetcher = makeFetcher
        self.apiKeyProvider = apiKeyProvider
        self.orgIDProvider = orgIDProvider
        isConfigured = apiKeyProvider()?.isEmpty == false
    }

    func refresh(now: Date = Date()) async {
        guard let apiKey = apiKeyProvider(), !apiKey.isEmpty else {
            // No key: the panel hides the Neon section entirely, so drop everything
            // rather than leave a stale figure behind a hidden section.
            isConfigured = false
            summary = nil
            lastError = nil
            isLoading = false
            return
        }
        isConfigured = true

        let orgID = (orgIDProvider() ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        guard !orgID.isEmpty else {
            // The endpoint requires org_id; without it there is nothing to ask for,
            // so show the `—` path with a message telling the user what's missing.
            summary = nil
            lastError = NeonUsageError.missingOrgID.errorDescription
            isLoading = false
            return
        }

        isLoading = true
        let window = NeonUsageMapping.monthToDateWindow(now: now)
        do {
            let response = try await makeFetcher(apiKey)
                .consumptionHistory(orgID: orgID, from: window.from, to: window.to)
            let fresh = NeonUsageMapping.summarize(response)
            summary = fresh
            lastUpdated = Date()
            // A successful call that reports nothing is the plan-gate / empty-org
            // case: the figures stay nil (`—`) and the footer says why in plain
            // language instead of showing a fabricated 0.
            lastError = fresh.projectCount == 0 ? Self.noConsumptionMessage : nil
        } catch {
            // Keep the last-good summary (if any) and surface the failure in the
            // footer — consumption is hourly-at-best, so carrying last-good forward
            // beats blanking the section on one transient failure.
            lastError = Self.friendlyMessage(for: error)
        }
        isLoading = false
    }

    /// Initial load plus a repeating refresh on the fixed 1-hour cadence.
    func start(interval: TimeInterval = NeonUsageService.pollInterval) {
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

    /// Re-read key/org presence and refresh now — called after the user saves or
    /// clears the Neon settings so the panel updates without waiting for the poll.
    func configureFromKeychain() {
        isConfigured = apiKeyProvider()?.isEmpty == false
        Task { await refresh() }
    }

    deinit { task?.cancel() }

    /// Shown when the API answers successfully but reports no consumption at all —
    /// an empty org, the wrong org id, or a plan without consumption history
    /// (Launch / Scale / Agent / Enterprise only).
    static let noConsumptionMessage =
        "no Neon consumption reported — check the org ID, or the plan may not include consumption history"

    static func friendlyMessage(for error: Error) -> String {
        if let neonError = error as? NeonUsageError {
            if neonError.isAuthFailure {
                return "API key invalid — paste a new one in Settings"
            }
            if case let .http(status, _) = neonError, status == 404 {
                return "Neon org not found — check the org ID in Settings"
            }
            return neonError.errorDescription ?? "couldn't refresh Neon usage"
        }
        return error.localizedDescription
    }
}
