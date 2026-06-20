import Foundation
import HostMetricsKit

/// Why a remote agent request failed, classified so the UI can tell apart a
/// bad token from a broken wire contract from a dead VM — three failures that
/// otherwise all look like "unreachable" and send you SSHing into the wrong layer.
enum RemoteHostError: Error, Equatable {
    /// The agent answered with 401 — the bearer token is wrong or missing.
    case authFailed
    /// A non-2xx, non-401 HTTP response. Carries the status code.
    case httpStatus(Int)
    /// The response decoded into the wrong shape — typically an agent/app
    /// version skew after a redeploy (the busy-binary trap).
    case decodeFailed
    /// Couldn't reach the host at all (timeout, connection refused, bad URL).
    case unreachable

    /// Short, human-facing label used in card text and the Settings Test result.
    var shortLabel: String {
        switch self {
        case .authFailed: "auth failed"
        case let .httpStatus(code): "HTTP \(code)"
        case .decodeFailed: "decode failed"
        case .unreachable: "unreachable"
        }
    }
}

/// Outcome of the Settings "Test" probe: either the decoded health payload
/// (so we can show the live agent version + hostname) or a classified failure.
enum HostHealthResult {
    case ok(HealthInfo)
    case failure(RemoteHostError)
}

/// The `/v1/health` payload the agent serves. We decode `version`/`hostname`
/// to confirm a redeploy landed without SSHing in; `status`/`samplerStale`
/// distinguish a live host from one serving frozen samples.
struct HealthInfo: Decodable, Equatable {
    let status: String
    let hostname: String
    let version: String
    let sampleAgeSeconds: Int?
    let samplerStale: Bool?
}

/// Streams a remote host's metrics by polling its DevCanopy agent
/// (`GET /v1/snapshot`) over the tailnet with a bearer token, decoding the same
/// `HostSnapshot` shape the local collector produces. Feeds the shared history
/// buffers in `HostMetricsService`; publishes `connectionState` so an unreachable
/// host renders a muted card.
@MainActor
final class RemoteHostMetricsService: HostMetricsService {
    /// Shared decoder for everything the agent emits (ISO-8601 timestamps).
    /// Also used by the wire-contract test to lock the format.
    static let snapshotDecoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return decoder
    }()

    /// Per-request timeout. Deliberately forgiving so a high-latency or briefly
    /// flapping tailnet link — e.g. a Mac tethered to a phone — isn't misread as a
    /// dead host. Paired with `waitsForConnectivity = false`, the loop still fails
    /// fast per attempt and recovers fast; this just gives a slow-but-alive
    /// round-trip room to complete.
    static let requestTimeout: TimeInterval = 10

    /// Consecutive failed polls required before a host is marked down. Debounces
    /// momentary drops: a single missed poll never flips the card red or logs.
    static let failureThreshold = 2

    let address: String
    let port: Int
    private let token: String?
    private let session: URLSession

    /// Back-to-back failed polls; reset to 0 on any success. Once it reaches
    /// `failureThreshold` the host flips to `.failed`. Readable for tests.
    private(set) var consecutiveFailures = 0

    init(hostName: String, address: String, port: Int, token: String?) {
        self.address = address
        self.port = port
        self.token = token
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = Self.requestTimeout
        config.waitsForConnectivity = false
        session = URLSession(configuration: config)
        super.init(hostName: hostName, connectionState: .connecting)
        installLifecyclePause()
    }

    override func start(interval: TimeInterval = 1.0) {
        startInterval = interval
        guard task == nil else { return }
        task = Task { [weak self] in
            guard let self else { return }
            while !Task.isCancelled {
                await poll()
                try? await Task.sleep(nanoseconds: UInt64(interval * 1_000_000_000))
            }
        }
    }

    private func poll() async {
        do {
            let snap = try await fetchSnapshot()
            ingest(snap)
            recordSuccess()
        } catch let error as RemoteHostError {
            recordFailure(error)
        } catch {
            // Any non-classified throw is treated as unreachable.
            recordFailure(.unreachable, underlying: error)
        }
    }

    /// Marks a poll as succeeded: clears the failure streak, logs a single
    /// recovery line if we were previously down, then flips the card to connected.
    func recordSuccess() {
        if connectionState.isFailed {
            serviceLogger.info("Remote host \(hostName) \(address):\(port) recovered")
        }
        consecutiveFailures = 0
        setConnection(.connected)
    }

    /// Records a failed poll. Only flips the host to `.failed` (and logs) once the
    /// streak reaches `failureThreshold`, so a momentary drop is absorbed silently.
    /// Logs on the down-transition and on a genuine cause change — never per poll.
    func recordFailure(_ error: RemoteHostError, underlying: Error? = nil) {
        consecutiveFailures += 1
        guard consecutiveFailures >= Self.failureThreshold else { return }
        let newState = HostConnectionState.failed(error)
        if connectionState != newState {
            logFailure(path: "/v1/snapshot", error: error, underlying: underlying)
        }
        setConnection(newState)
    }

    private func fetchSnapshot() async throws -> HostSnapshot {
        let data = try await get(path: "/v1/snapshot")
        do {
            return try Self.snapshotDecoder.decode(HostSnapshot.self, from: data)
        } catch {
            throw RemoteHostError.decodeFailed
        }
    }

    /// Logs a remote failure with host/path/error kind — never the token.
    private func logFailure(path: String, error: RemoteHostError, underlying: Error? = nil) {
        let endpoint = "\(address):\(port)\(path)"
        if let underlying {
            serviceLogger.error(
                "Remote host \(hostName) \(endpoint) failed: \(error.shortLabel) (\(underlying.localizedDescription))"
            )
        } else {
            serviceLogger.error("Remote host \(hostName) \(endpoint) failed: \(error.shortLabel)")
        }
    }

    /// Fetches the remote host's containers (used by the containers panel).
    func fetchContainers() async throws -> [ContainerInfo] {
        let data = try await get(path: "/v1/containers")
        do {
            return try Self.snapshotDecoder.decode([ContainerInfo].self, from: data)
        } catch {
            throw RemoteHostError.decodeFailed
        }
    }

    /// Reachability + auth check for the Settings "Test" button. Decodes the
    /// health payload so the result can show the live agent version + hostname
    /// (confirming a redeploy landed) and distinguishes auth failure from a
    /// dead host or a decode break.
    func checkHealth() async -> HostHealthResult {
        do {
            let data = try await get(path: "/v1/health")
            let info = try Self.snapshotDecoder.decode(HealthInfo.self, from: data)
            return .ok(info)
        } catch let error as RemoteHostError {
            logFailure(path: "/v1/health", error: error)
            return .failure(error)
        } catch {
            // A non-2xx already throws RemoteHostError above; a throw here means
            // the body didn't decode into HealthInfo — a wire-contract break.
            logFailure(path: "/v1/health", error: .decodeFailed, underlying: error)
            return .failure(.decodeFailed)
        }
    }

    private func get(path: String) async throws -> Data {
        guard let url = URL(string: "http://\(address):\(port)\(path)") else {
            throw RemoteHostError.unreachable
        }
        var request = URLRequest(url: url)
        if let token { request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: request)
        } catch {
            throw RemoteHostError.unreachable
        }
        guard let http = response as? HTTPURLResponse else {
            throw RemoteHostError.unreachable
        }
        guard (200 ... 299).contains(http.statusCode) else {
            throw http.statusCode == 401
                ? RemoteHostError.authFailed
                : RemoteHostError.httpStatus(http.statusCode)
        }
        return data
    }
}
