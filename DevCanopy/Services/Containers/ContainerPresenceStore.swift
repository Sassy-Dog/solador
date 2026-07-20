import Foundation

/// Remembers when each expected container name was last actually observed, per
/// host section, and when each host's data source last polled successfully.
/// This is the memory that lets the Containers panel keep a standing row for an
/// ephemeral VM while it is absent (recycling → missing) instead of letting the
/// row vanish with the VM.
///
/// Clock discipline: `lastSeen` only advances when the container's own runtime
/// polled successfully, and `lastSuccess` only advances on a successful poll —
/// so a failing source freezes every clock it owns rather than aging entities
/// toward a false "missing" alarm.
@MainActor
final class ContainerPresenceStore: ObservableObject {
    /// Keyed by `"<host>|<name>"` — host is the panel's section key
    /// (`ContainerGroupRule.localHostScope` or the remote host name).
    @Published private(set) var records: [String: ContainerPresenceRecord] = [:]

    /// Per-host last SUCCESSFUL poll time. Deliberately in-memory only: after a
    /// relaunch no absent rows appear until the first fresh look at that host.
    @Published private(set) var lastSuccess: [String: Date] = [:]

    static let defaultsKey = "containerPresenceRecords"

    private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        if let data = defaults.data(forKey: Self.defaultsKey),
           let decoded = try? JSONDecoder().decode([String: ContainerPresenceRecord].self, from: data)
        {
            records = decoded
        }
    }

    static func key(host: String, name: String) -> String {
        // Container/VM names can't contain "|"; host names realistically don't.
        "\(host)|\(name)"
    }

    /// The polled host's records, keyed by bare name — the shape partition() wants.
    func recordsByName(host: String) -> [String: ContainerPresenceRecord] {
        let prefix = "\(host)|"
        var byName: [String: ContainerPresenceRecord] = [:]
        for (key, record) in records where key.hasPrefix(prefix) {
            byName[String(key.dropFirst(prefix.count))] = record
        }
        return byName
    }

    /// Record a local poll. `succeeded` gates which runtimes' facts are fresh:
    /// containers of a failed runtime are retained UI rows, not sightings.
    func noteLocalPoll(
        containers: [ContainerInfo],
        succeeded: Set<ContainerRuntime>,
        rules: [ContainerGroupRule],
        now: Date = Date()
    ) {
        note(
            host: ContainerGroupRule.localHostScope,
            containers: containers,
            succeeded: succeeded,
            rules: rules,
            now: now
        )
    }

    /// Record a successful remote `/v1/containers` poll (one HTTP fetch — all
    /// runtimes on that host either reported or the call failed upstream).
    func noteRemotePoll(
        host: String,
        containers: [ContainerInfo],
        rules: [ContainerGroupRule],
        now: Date = Date()
    ) {
        note(host: host, containers: containers, succeeded: nil, rules: rules, now: now)
    }

    /// Drop one expectation immediately (right-click "Forget").
    func forget(host: String, name: String) {
        guard records.removeValue(forKey: Self.key(host: host, name: name)) != nil else { return }
        persist()
    }

    /// Shared update: upsert fresh sightings, seed exact-name expectations,
    /// prune records whose rule is gone, advance the host clock.
    /// `succeeded == nil` means the whole host reported (remote fetch).
    private func note(
        host: String,
        containers: [ContainerInfo],
        succeeded: Set<ContainerRuntime>?,
        rules: [ContainerGroupRule],
        now: Date
    ) {
        let applicable = rules.filter { ($0.host == nil || $0.host == host) && $0.action == .expect }
        var updated = records

        func isExpected(_ name: String) -> Bool {
            applicable.contains { ContainerGrouping.matches(name: name, pattern: $0.pattern) }
        }
        /// A nil record runtime (seeded, never observed) belongs to no source, so
        /// no failing source protects it.
        func hasFreshFacts(_ runtime: ContainerRuntime?) -> Bool {
            guard let succeeded else { return true }
            guard let runtime else { return true }
            return succeeded.contains(runtime)
        }

        for container in containers
            where isExpected(container.name) && hasFreshFacts(container.runtime)
        {
            updated[Self.key(host: host, name: container.name)] =
                ContainerPresenceRecord(lastSeen: now, runtime: container.runtime)
        }

        // Seed exact-name expectations never observed — the clock starts at rule
        // creation, so a typo'd or decommissioned name escalates to missing.
        // Globs learn observed names only; they never invent one.
        for rule in applicable where !rule.pattern.contains("*") {
            let key = Self.key(host: host, name: rule.pattern)
            if updated[key] == nil {
                updated[key] = ContainerPresenceRecord(lastSeen: now, runtime: nil)
            }
        }

        // Prune this host's records whose expect rule is gone — unless the
        // record's runtime is currently failing (frozen: no fresh facts, no
        // mutations of any kind).
        let prefix = "\(host)|"
        for (key, record) in updated where key.hasPrefix(prefix) {
            let name = String(key.dropFirst(prefix.count))
            if !isExpected(name), hasFreshFacts(record.runtime) {
                updated.removeValue(forKey: key)
            }
        }

        // Host-level clock: advances when anything reported successfully. (A
        // still-failing runtime's absent entities age against this clock — the
        // conservative alternative would hide real alarms, and its present
        // entities are protected by last-known retention upstream.)
        if succeeded.map({ !$0.isEmpty }) ?? true {
            lastSuccess[host] = now
        }

        if updated != records {
            records = updated
            persist()
        }
    }

    private func persist() {
        guard let data = try? JSONEncoder().encode(records) else { return }
        defaults.set(data, forKey: Self.defaultsKey)
    }
}
