import Foundation

/// What a rule does with the containers its pattern matches: fold them into one
/// aggregate row, drop them from the panel entirely, or expect them — an expected
/// name renders individually while present and keeps a standing amber/red
/// presence row while absent instead of vanishing (ephemeral runner VMs recycle
/// out of `tart list` between jobs). Hiding affects rows only — the panel's
/// rollup counts deliberately still include hidden containers, so cruft building
/// up (unreaped VMs, exited job containers) stays visible in the numbers.
enum ContainerRuleAction: String, Codable {
    case collapse
    case hide
    case expect
}

/// A user-editable rule that collapses ephemeral containers (CI runner pools,
/// workflow-spawned job containers) into one aggregate row on the Containers panel —
/// so their churn can't resize the panel and shift the cockpit grid — or hides
/// never-interesting entries (base images) altogether.
struct ContainerGroupRule: Identifiable, Equatable {
    var id = UUID()
    var pattern: String
    var label: String
    var action: ContainerRuleAction = .collapse
    /// Host-section key this rule applies to (matching AND rendering); nil = all
    /// hosts. Scoped collapse rules render a standing ×0 row only on their host.
    var host: String?
    /// Collapse rules only: how many matches SHOULD exist. Renders ×matched/expected
    /// and warns amber when short. nil = no expectation (renders exactly as before).
    var expectedCount: Int?

    /// The panel's local host-section key — shared with the Settings picker so the
    /// two can't drift.
    static let localHostScope = "this machine"

    /// UserDefaults key backing the panel's `@AppStorage` — shared with the
    /// services that need the live rules outside a SwiftUI context.
    static let rulesDefaultsKey = "containerGroupRules"

    /// The current persisted rules, for callers outside SwiftUI (services).
    static func loadFromDefaults(_ defaults: UserDefaults = .standard) -> [ContainerGroupRule] {
        load(from: defaults.data(forKey: rulesDefaultsKey) ?? Data())
    }

    static let seededDefaults: [ContainerGroupRule] = [
        ContainerGroupRule(pattern: "sassydog-ghr-ubu-*", label: "ghr runners", host: "ubu-01"),
        ContainerGroupRule(pattern: "api-*", label: "workflow jobs", host: "ubu-01"),
        ContainerGroupRule(pattern: "ghcr.io/*", label: "", action: .hide)
    ]

    /// Empty or undecodable data means "never configured" (or a corrupt store) and
    /// yields the seeded defaults; a decoded empty array means the user deliberately
    /// cleared every rule and is respected as-is.
    static func load(from data: Data) -> [ContainerGroupRule] {
        guard !data.isEmpty,
              let rules = try? JSONDecoder().decode([ContainerGroupRule].self, from: data)
        else { return seededDefaults }
        return rules
    }

    static func encode(_ rules: [ContainerGroupRule]) -> Data {
        (try? JSONEncoder().encode(rules)) ?? Data()
    }
}

/// Codable lives in an extension so the memberwise initializer survives. Decoding
/// tolerates rules persisted before `action` existed by defaulting them to
/// `.collapse`; `encode(to:)` stays synthesized.
extension ContainerGroupRule: Codable {
    private enum CodingKeys: String, CodingKey {
        case id, pattern, label, action, host, expectedCount
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(UUID.self, forKey: .id)
        pattern = try container.decode(String.self, forKey: .pattern)
        label = try container.decode(String.self, forKey: .label)
        // Unknown action strings (from a newer build's rules) degrade to .collapse
        // rather than throwing — a throw here would trip load(from:)'s try? and
        // silently reset the user's entire rule list to seeded defaults.
        let rawAction = try container.decodeIfPresent(String.self, forKey: .action)
        action = rawAction.flatMap(ContainerRuleAction.init(rawValue:)) ?? .collapse
        host = try container.decodeIfPresent(String.self, forKey: .host)
        expectedCount = try container.decodeIfPresent(Int.self, forKey: .expectedCount)
    }
}

/// One collapsed row summarizing every container a rule matched within a host section.
/// A configured collapse rule is a standing row: it exists even with zero matches.
struct ContainerGroupAggregate: Identifiable, Equatable {
    /// The owning rule's id — not the label, which two rules may legitimately share.
    let id: UUID
    let label: String
    let total: Int
    let runningCount: Int
    /// nil when the group is empty — there is no container to derive a runtime from,
    /// and the panel must not display a fabricated one.
    let dominantRuntime: ContainerRuntime?
    /// The owning rule's expected match count; nil renders exactly as before.
    let expectedCount: Int?
}

/// Last-observed facts about one expected container name on one host — enough to
/// evaluate presence later without inventing data. `runtime` is nil for a
/// hand-typed expectation whose entity has never been observed.
struct ContainerPresenceRecord: Codable, Equatable {
    var lastSeen: Date
    var runtime: ContainerRuntime?
}

/// A standing row for an expected container that is absent from the current poll:
/// amber while recycling (normal ephemeral churn), red once missing beyond grace.
struct ExpectedAbsentContainer: Identifiable, Equatable {
    var id: String {
        name
    }

    let name: String
    /// nil → render no runtime tag (house rule: never display fabricated data).
    let runtime: ContainerRuntime?
    let state: PresenceState
}

/// One renderable row of a host section, present or absent — identity is the NAME,
/// so a VM flipping between present and absent keeps the same SwiftUI row.
enum ContainerDisplayRow: Identifiable, Equatable {
    case present(ContainerInfo)
    case absent(ExpectedAbsentContainer)

    var name: String {
        switch self {
        case let .present(container): container.name
        case let .absent(absent): absent.name
        }
    }

    var id: String {
        name
    }
}

/// Result of partitioning one host section's containers against the rules.
struct ContainerPartition: Equatable {
    let individual: [ContainerInfo]
    let aggregates: [ContainerGroupAggregate]
    let expectedAbsent: [ExpectedAbsentContainer]
}

/// Pure grouping logic for the Containers panel. No I/O here.
enum ContainerGrouping {
    /// Whether `name` matches the rule `pattern`. Contract: the whole name must match
    /// (anchored, not substring); `*` matches any run of characters, including none;
    /// every other character is literal — regex metacharacters like `.` must not leak
    /// through; matching is case-sensitive.
    static func matches(name: String, pattern: String) -> Bool {
        let escaped = pattern
            .components(separatedBy: "*")
            .map(NSRegularExpression.escapedPattern(for:))
            .joined(separator: ".*")
        guard let regex = try? NSRegularExpression(pattern: "^\(escaped)$") else { return false }
        return regex.firstMatch(in: name, range: NSRange(name.startIndex..., in: name)) != nil
    }

    /// Splits one host section's containers into individually-rendered rows — sorted
    /// by name so the order is stable regardless of `ps` arrival order (newest-first)
    /// — and one aggregate per applicable collapse rule, in rule order. Only rules
    /// scoped to `host` (or unscoped) apply, for matching and rendering alike. A
    /// configured collapse rule is a standing row: it emits its aggregate even with
    /// zero matches. The first matching rule wins, which also arbitrates
    /// collapse-vs-hide by rule order; containers claimed by a hide rule are dropped
    /// from the output entirely.
    static func partition(
        _ containers: [ContainerInfo],
        rules: [ContainerGroupRule],
        host: String,
        presence: [String: ContainerPresenceRecord] = [:],
        now: Date? = nil,
        grace: TimeInterval = Presence.defaultGrace,
        matcher: (String, String) -> Bool = ContainerGrouping.matches
    ) -> ContainerPartition {
        let applicable = rules.filter { $0.host == nil || $0.host == host }
        var matched: [UUID: [ContainerInfo]] = [:]
        var individual: [ContainerInfo] = []

        for container in containers {
            if let rule = applicable.first(where: { matcher(container.name, $0.pattern) }) {
                switch rule.action {
                case .collapse: matched[rule.id, default: []].append(container)
                case .hide: break
                // Expected containers always render as their own row while present;
                // first-match-wins lets an expect rule shield a name from later
                // collapse/hide rules.
                case .expect: individual.append(container)
                }
            } else {
                individual.append(container)
            }
        }

        individual.sort {
            ($0.name, $0.runtime.rawValue) < ($1.name, $1.runtime.rawValue)
        }

        let aggregates = applicable.compactMap { rule -> ContainerGroupAggregate? in
            guard rule.action == .collapse else { return nil }
            let members = matched[rule.id] ?? []
            return ContainerGroupAggregate(
                id: rule.id,
                label: rule.label,
                total: members.count,
                runningCount: members.filter(\.isRunning).count,
                dominantRuntime: dominantRuntime(of: members),
                expectedCount: rule.expectedCount
            )
        }

        // Absent-expected rows: every remembered name that is missing from the
        // current poll and claimed by an expect rule (first-match-wins, so a hide
        // rule ordered above the expect rule suppresses the absent row exactly as
        // it would the present one). `now` is the host's last successful poll —
        // nil means we've never successfully looked, so never alarm.
        var expectedAbsent: [ExpectedAbsentContainer] = []
        if let now {
            let presentNames = Set(containers.map(\.name))
            for (name, record) in presence where !presentNames.contains(name) {
                guard let rule = applicable.first(where: { matcher(name, $0.pattern) }),
                      rule.action == .expect else { continue }
                expectedAbsent.append(ExpectedAbsentContainer(
                    name: name,
                    runtime: record.runtime,
                    state: Presence.state(isPresent: false, lastSeen: record.lastSeen, now: now, grace: grace)
                ))
            }
            expectedAbsent.sort { $0.name < $1.name }
        }

        return ContainerPartition(
            individual: individual,
            aggregates: aggregates,
            expectedAbsent: expectedAbsent
        )
    }

    /// Merges present and absent rows into one name-sorted list. Name-keyed
    /// identity keeps a row in place as its entity flips present ↔ absent.
    static func displayRows(
        individual: [ContainerInfo],
        absent: [ExpectedAbsentContainer]
    ) -> [ContainerDisplayRow] {
        (individual.map(ContainerDisplayRow.present) + absent.map(ContainerDisplayRow.absent))
            .sorted { $0.name < $1.name }
    }

    /// Most frequent runtime among the matched containers; ties break toward the
    /// smaller raw value for determinism. nil for an empty group — never invent one.
    private static func dominantRuntime(of members: [ContainerInfo]) -> ContainerRuntime? {
        let counts = Dictionary(grouping: members, by: \.runtime).mapValues(\.count)
        let best = counts.max { lhs, rhs in
            if lhs.value != rhs.value {
                return lhs.value < rhs.value
            }
            return lhs.key.rawValue > rhs.key.rawValue
        }
        return best?.key
    }
}
