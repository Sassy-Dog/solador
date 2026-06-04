import Foundation
import SwiftData

/// Owns the live services for the configured remote hosts. Reads `MonitoredHost`
/// from SwiftData, builds a `RemoteHostMetricsService` (+ container polling) per
/// enabled host, and rebuilds when the host list changes. The cockpit panels read
/// `hosts` (merged with the local service) and `containersByHost`.
@MainActor
final class RemoteHostsCoordinator: ObservableObject {
    @Published private(set) var hosts: [RemoteHostMetricsService] = []
    @Published private(set) var containersByHost: [String: [ContainerInfo]] = [:]

    private let modelContext: ModelContext
    private var containerTasks: [UUID: Task<Void, Never>] = [:]

    init(modelContext: ModelContext) {
        self.modelContext = modelContext
    }

    /// Provisions a host from `DEVCANOPY_SEED_HOST` ("name|address|port|token") if
    /// one with that address isn't already configured. Useful for headless setup
    /// and first-run provisioning; no-op when the env var is unset.
    func seedFromEnvironmentIfNeeded() {
        guard let raw = ProcessInfo.processInfo.environment["DEVCANOPY_SEED_HOST"] else { return }
        let parts = raw.split(separator: "|", omittingEmptySubsequences: false).map(String.init)
        guard parts.count >= 2, !parts[0].isEmpty, !parts[1].isEmpty else { return }
        let name = parts[0], address = parts[1]
        let port = parts.count >= 3 ? (Int(parts[2]) ?? 7878) : 7878
        let token = parts.count >= 4 ? parts[3] : ""

        let existing = (try? modelContext.fetch(FetchDescriptor<MonitoredHost>())) ?? []
        guard !existing.contains(where: { $0.address == address }) else { return }

        let host = MonitoredHost(name: name, address: address, port: port)
        modelContext.insert(host)
        if !token.isEmpty { try? KeychainHelper.shared.saveHostToken(token, hostID: host.id) }
        try? modelContext.save()
    }

    /// Tear down existing services and rebuild from the current `MonitoredHost` list.
    /// Call after the host list changes (add/edit/remove/toggle).
    func reload() {
        stopAll()

        let descriptor = FetchDescriptor<MonitoredHost>(
            predicate: #Predicate { $0.enabled },
            sortBy: [SortDescriptor(\.name)]
        )
        let monitored = (try? modelContext.fetch(descriptor)) ?? []

        hosts = monitored.map { host in
            let token = KeychainHelper.shared.loadHostToken(hostID: host.id)
            let service = RemoteHostMetricsService(
                hostName: host.name, address: host.address, port: host.port, token: token
            )
            service.start()
            startContainerPolling(hostID: host.id, hostName: host.name, service: service)
            return service
        }
    }

    private func startContainerPolling(hostID: UUID, hostName: String, service: RemoteHostMetricsService) {
        containerTasks[hostID] = Task { [weak self] in
            while !Task.isCancelled {
                if let containers = try? await service.fetchContainers() {
                    self?.containersByHost[hostName] = containers
                }
                try? await Task.sleep(nanoseconds: 10_000_000_000)  // 10s
            }
        }
    }

    func stopAll() {
        hosts.forEach { $0.stop() }
        hosts = []
        containerTasks.values.forEach { $0.cancel() }
        containerTasks.removeAll()
        containersByHost.removeAll()
    }

    deinit {
        containerTasks.values.forEach { $0.cancel() }
    }
}
