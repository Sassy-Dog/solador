import Foundation
import HostMetricsKit

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

    let address: String
    let port: Int
    private let token: String?
    private let session: URLSession

    init(hostName: String, address: String, port: Int, token: String?) {
        self.address = address
        self.port = port
        self.token = token
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 5
        config.waitsForConnectivity = false
        self.session = URLSession(configuration: config)
        super.init(hostName: hostName, connectionState: .connecting)
    }

    override func start(interval: TimeInterval = 1.0) {
        guard task == nil else { return }
        task = Task { [weak self] in
            guard let self else { return }
            while !Task.isCancelled {
                await self.poll()
                try? await Task.sleep(nanoseconds: UInt64(interval * 1_000_000_000))
            }
        }
    }

    private func poll() async {
        do {
            let snap = try await fetchSnapshot()
            ingest(snap)
            setConnection(.connected)
        } catch {
            setConnection(.unreachable)
        }
    }

    private func fetchSnapshot() async throws -> HostSnapshot {
        let data = try await get(path: "/v1/snapshot")
        return try Self.snapshotDecoder.decode(HostSnapshot.self, from: data)
    }

    /// Fetches the remote host's containers (used by the containers panel).
    func fetchContainers() async throws -> [ContainerInfo] {
        let data = try await get(path: "/v1/containers")
        return try Self.snapshotDecoder.decode([ContainerInfo].self, from: data)
    }

    /// Lightweight reachability + auth check for the Settings "Test" button.
    func checkHealth() async -> Bool {
        (try? await get(path: "/v1/health")) != nil
    }

    private func get(path: String) async throws -> Data {
        guard let url = URL(string: "http://\(address):\(port)\(path)") else {
            throw URLError(.badURL)
        }
        var request = URLRequest(url: url)
        if let token { request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, (200...299).contains(http.statusCode) else {
            throw URLError(.badServerResponse)
        }
        return data
    }
}
