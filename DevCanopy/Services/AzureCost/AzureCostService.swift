import Foundation
import SwiftUI

// MARK: - Errors

/// Failures from reading the Azure cost export. `isAuthFailure` is the signal the
/// panel turns into a "paste a new SAS" prompt — a SAS that expired or was revoked
/// answers blob requests with 403 / an `AuthenticationFailed` body.
enum AzureCostError: LocalizedError, Equatable {
    case invalidURL
    case invalidResponse
    case http(status: Int, body: String?)
    case noBlobs(prefix: String)
    case noCSV(run: String, prefix: String)
    case missingColumn(String)

    var errorDescription: String? {
        switch self {
        case .invalidURL:
            "Invalid blob URL — check the SAS URL in Settings"
        case .invalidResponse:
            "Invalid response from blob storage"
        case let .http(status, _):
            "Blob request failed (HTTP \(status))"
        case let .noBlobs(prefix):
            "No export found under \(prefix)"
        case let .noCSV(run, _):
            "No CSV partitions in run \(run)"
        case let .missingColumn(name):
            "Export CSV missing '\(name)' column"
        }
    }

    /// True when the failure looks like an expired/invalid SAS (the panel turns
    /// this into a "paste a new one" hint rather than a generic error).
    var isAuthFailure: Bool {
        guard case let .http(status, body) = self else { return false }
        if status == 401 || status == 403 { return true }
        return body?.contains("AuthenticationFailed") ?? false
    }
}

// MARK: - Blob transport seam

/// The two Blob REST operations the cost reader needs. Abstracted behind a
/// protocol so the list → pick-latest-run → aggregate logic is unit-testable with
/// a stub, no network. `path` / `prefix` are container-relative (the SAS already
/// pins the account + container).
protocol BlobFetching: Sendable {
    /// All blob names under `prefix`, following `<NextMarker>` pagination.
    func listBlobs(prefix: String) async throws -> [String]
    /// The UTF-8 text body of a single blob.
    func getBlobText(path: String) async throws -> String
}

/// `BlobFetching` over a container-scoped, read+list user-delegation SAS URL —
/// no Azure identity, no token minting. We just split the SAS URL into its
/// container base and query string and append the query to plain GETs (the SAS
/// *is* the credential, so there is no `Authorization` header).
struct URLSessionBlobFetcher: BlobFetching {
    /// e.g. `https://stsassydog.blob.core.windows.net/cost-exports` (no trailing slash).
    let containerBase: String
    /// The SAS query, e.g. `sv=...&sig=...` (no leading `?`).
    let sasQuery: String
    let session: URLSession

    init(sasURL: String, session: URLSession = .shared) {
        if let q = sasURL.firstIndex(of: "?") {
            containerBase = Self.trimTrailingSlash(String(sasURL[..<q]))
            sasQuery = String(sasURL[sasURL.index(after: q)...])
        } else {
            containerBase = Self.trimTrailingSlash(sasURL)
            sasQuery = ""
        }
        self.session = session
    }

    func listBlobs(prefix: String) async throws -> [String] {
        var names: [String] = []
        var marker = ""
        repeat {
            var urlString = "\(containerBase)?restype=container&comp=list&prefix=\(Self.encode(prefix))"
            if !marker.isEmpty { urlString += "&marker=\(Self.encode(marker))" }
            if !sasQuery.isEmpty { urlString += "&\(sasQuery)" }
            guard let url = URL(string: urlString) else { throw AzureCostError.invalidURL }
            let xml = try await Self.getText(url: url, session: session)
            let parsed = parseBlobListXML(xml)
            names.append(contentsOf: parsed.names)
            marker = parsed.nextMarker ?? ""
        } while !marker.isEmpty
        return names
    }

    func getBlobText(path: String) async throws -> String {
        let encodedPath = path
            .split(separator: "/", omittingEmptySubsequences: false)
            .map { Self.encode(String($0)) }
            .joined(separator: "/")
        var urlString = "\(containerBase)/\(encodedPath)"
        if !sasQuery.isEmpty { urlString += "?\(sasQuery)" }
        guard let url = URL(string: urlString) else { throw AzureCostError.invalidURL }
        return try await Self.getText(url: url, session: session)
    }

    // MARK: - HTTP

    private static func getText(url: URL, session: URLSession) async throws -> String {
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.timeoutInterval = 30
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw AzureCostError.invalidResponse }
        guard (200 ... 299).contains(http.statusCode) else {
            throw AzureCostError.http(status: http.statusCode, body: String(data: data, encoding: .utf8))
        }
        return String(data: data, encoding: .utf8) ?? ""
    }

    private static func trimTrailingSlash(_ s: String) -> String {
        s.hasSuffix("/") ? String(s.dropLast()) : s
    }

    /// Percent-encode a path/prefix segment the way `encodeURIComponent` does —
    /// notably encoding `/`, so a multi-segment prefix lists correctly.
    private static let allowed: CharacterSet = {
        var set = CharacterSet.alphanumerics
        set.insert(charactersIn: "-._~")
        return set
    }()

    private static func encode(_ value: String) -> String {
        value.addingPercentEncoding(withAllowedCharacters: allowed) ?? value
    }
}

/// Extract `<Name>` values and the optional `<NextMarker>` from a Blob List
/// response. Pure + dependency-free (string scan, not a full XML parse) — the
/// blob names are simple paths with no entity-escaped characters.
func parseBlobListXML(_ xml: String) -> (names: [String], nextMarker: String?) {
    let names = extractTagValues("Name", in: xml)
    let marker = extractTagValues("NextMarker", in: xml).first
    return (names, (marker?.isEmpty == false) ? marker : nil)
}

private func extractTagValues(_ tag: String, in xml: String) -> [String] {
    let open = "<\(tag)>"
    let close = "</\(tag)>"
    var values: [String] = []
    var searchStart = xml.startIndex
    while let openRange = xml.range(of: open, range: searchStart ..< xml.endIndex),
          let closeRange = xml.range(of: close, range: openRange.upperBound ..< xml.endIndex)
    {
        values.append(String(xml[openRange.upperBound ..< closeRange.lowerBound]))
        searchStart = closeRange.upperBound
    }
    return values
}

// MARK: - Reader (pure orchestration over a BlobFetching)

/// Lists an export's month folder, picks the lexically-greatest run (newest), sums
/// every CSV partition, and folds MTD + best-effort prior-month into a summary.
/// Free of the service/Keychain so it can be driven by a stub fetcher in tests.
enum AzureCostReader {
    /// Container-relative root prefixes for the two platform exports.
    static let mtdPrefix = "daily/mc-platform-daily-actualcost"
    static let priorPrefix = "last-month/mc-platform-lastmonth-actualcost"
    static let topN = 5

    /// Read one export's latest run for `month` and aggregate it. Layout:
    /// `{rootPrefix}/{monthRange}/{runTimestamp}/{runGuid}/000001.csv` (+ `_manifest.json`).
    static func readExport(
        fetcher: BlobFetching,
        rootPrefix: String,
        month: Date
    ) async throws -> AzureCostAggregate {
        let prefix = "\(rootPrefix)/\(monthRangeFolder(month))/"
        let names = try await fetcher.listBlobs(prefix: prefix)
        guard !names.isEmpty else { throw AzureCostError.noBlobs(prefix: prefix) }

        /// The path segment right after the month folder is the run timestamp
        /// (YYYYMMDDHHMM, lexically sortable) — pick the newest run.
        func runOf(_ name: String) -> String {
            guard name.hasPrefix(prefix) else { return "" }
            return name.dropFirst(prefix.count)
                .split(separator: "/", maxSplits: 1, omittingEmptySubsequences: false)
                .first.map(String.init) ?? ""
        }
        let latestRun = names.map(runOf).max() ?? ""
        let csvBlobs = names.filter { runOf($0) == latestRun && $0.hasSuffix(".csv") }
        guard !csvBlobs.isEmpty else { throw AzureCostError.noCSV(run: latestRun, prefix: prefix) }

        // Sum every partition (large months split into 000001.csv, 000002.csv, …),
        // merging the already-keyed group/type breakdowns across partitions.
        var total = 0.0
        var byResourceGroup: [String: Double] = [:]
        var byMeterCategory: [String: Double] = [:]
        for blob in csvBlobs {
            let part = try await aggregateCostCsv(fetcher.getBlobText(path: blob))
            total += part.total
            for resource in part.byResource {
                byResourceGroup[resource.name, default: 0] += resource.cost
            }
            for type in part.byType {
                byMeterCategory[type.name, default: 0] += type.cost
            }
        }
        func sorted(_ totals: [String: Double]) -> [AzureResourceCost] {
            totals
                .map { AzureResourceCost(name: $0.key, cost: $0.value) }
                .sorted { $0.cost > $1.cost }
        }
        return AzureCostAggregate(
            total: total,
            byResource: sorted(byResourceGroup),
            byType: sorted(byMeterCategory)
        )
    }

    /// Month-to-date spend, prior-month total, and the top resource groups. MTD is
    /// required (a failure propagates); prior-month is best-effort (a missing
    /// export yields prior = 0, never blanks the card).
    static func fetchSummary(
        fetcher: BlobFetching,
        mtdPrefix: String = AzureCostReader.mtdPrefix,
        priorPrefix: String = AzureCostReader.priorPrefix,
        topN: Int = AzureCostReader.topN,
        now: Date = Date()
    ) async throws -> AzureCostSummary {
        // Headline = the current month's export. On the 1st-of-month rollover gap the
        // current month hasn't been exported yet, so fall back to the last completed
        // month (whose daily folder is still present) — the card shows real figures,
        // stamped with the month they cover, rather than an empty error state.
        var coveredMonth = now
        let mtd: AzureCostAggregate
        do {
            mtd = try await readExport(fetcher: fetcher, rootPrefix: mtdPrefix, month: now)
        } catch AzureCostError.noBlobs {
            coveredMonth = priorMonthDate(from: now)
            mtd = try await readExport(fetcher: fetcher, rootPrefix: mtdPrefix, month: coveredMonth)
        }
        let isFallback = monthRangeFolder(coveredMonth) != monthRangeFolder(now)

        // Prior month is best-effort (a missing export folds to 0, never blanks the
        // card) and is taken relative to whichever month we're actually showing.
        var spendPriorMonth = 0.0
        do {
            let prior = try await readExport(fetcher: fetcher, rootPrefix: priorPrefix, month: priorMonthDate(from: coveredMonth))
            spendPriorMonth = prior.total
        } catch {
            // Don't fail the whole fetch if only the prior-month export is missing
            // (e.g. before its first run) — MTD is the headline figure.
        }

        return AzureCostSummary(
            spendMTD: mtd.total,
            spendPriorMonth: spendPriorMonth,
            // A completed (fallback) month projects to itself; the current month is
            // linearly extrapolated by elapsed days.
            spendProjected: isFallback ? mtd.total : projectMonthlySpend(mtd.total, now: now),
            byResource: Array(mtd.byResource.prefix(topN)),
            byType: Array(mtd.byType.prefix(topN)),
            asOfMonth: isFallback ? coveredMonth : nil,
            error: nil
        )
    }
}

// MARK: - Service

/// Reads Azure spend from the platform-owned daily Cost Management export over a
/// container-scoped SAS URL stored in the Keychain, and publishes an
/// `AzureCostSummary`. Singleton (like `GitHubService`) so Settings can reconfigure
/// it after the user saves/clears the SAS. No Azure SDK, no OAuth — plain GETs.
@MainActor
final class AzureCostService: ObservableObject {
    static let shared = AzureCostService()

    @Published private(set) var summary: AzureCostSummary?
    @Published private(set) var isLoading = false
    @Published private(set) var isConfigured: Bool
    @Published private(set) var lastUpdated: Date?
    @Published private(set) var lastError: String?

    /// Builds the blob transport from a SAS URL. Overridable so a test can inject a
    /// stub; the default reads blobs over the real SAS.
    private let makeFetcher: (String) -> BlobFetching

    private var task: Task<Void, Never>?

    init(makeFetcher: @escaping (String) -> BlobFetching = { URLSessionBlobFetcher(sasURL: $0) }) {
        self.makeFetcher = makeFetcher
        isConfigured = KeychainHelper.shared.loadAzureCostSAS()?.isEmpty == false
    }

    func refresh() async {
        guard let sas = KeychainHelper.shared.loadAzureCostSAS(), !sas.isEmpty else {
            isConfigured = false
            summary = nil
            lastError = nil
            isLoading = false
            return
        }
        isConfigured = true
        isLoading = true
        let fetcher = makeFetcher(sas)
        do {
            summary = try await AzureCostReader.fetchSummary(fetcher: fetcher)
            lastError = nil
            lastUpdated = Date()
        } catch {
            // Keep the last-good summary (if any) and surface the failure in the
            // footer; the export is daily, so carrying last-good forward is the
            // documented detective-control behavior.
            lastError = Self.friendlyMessage(for: error)
        }
        isLoading = false
    }

    /// Initial load plus a repeating refresh.
    func start(interval: TimeInterval = 60) {
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

    /// Stops the current polling loop and restarts it on a new cadence (used when
    /// the user changes the Refresh Interval setting).
    func restart(interval: TimeInterval) {
        stop()
        start(interval: interval)
    }

    /// Re-read SAS presence and refresh now — called after the user saves/clears
    /// the SAS URL in Settings so the panel updates without waiting for the poll.
    func configureFromKeychain() {
        isConfigured = KeychainHelper.shared.loadAzureCostSAS()?.isEmpty == false
        Task { await refresh() }
    }

    deinit { task?.cancel() }

    static func friendlyMessage(for error: Error) -> String {
        if let azureError = error as? AzureCostError {
            if azureError.isAuthFailure {
                return "SAS expired or invalid — paste a new one in Settings"
            }
            // `.noBlobs` only reaches here when neither the current month nor the
            // last completed month has an export (fetchSummary falls back to the
            // prior month, and a missing prior-month export is swallowed) — i.e. no
            // recent cost data exists at all. If an earlier summary is on screen,
            // refresh() keeps it and the footer appends "· last ok …".
            if case .noBlobs = azureError {
                return "no recent cost export found"
            }
            return azureError.errorDescription ?? "couldn't refresh Azure cost"
        }
        return error.localizedDescription
    }
}
