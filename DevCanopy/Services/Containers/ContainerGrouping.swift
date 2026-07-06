import Foundation

/// What a rule does with the containers its pattern matches: fold them into one
/// aggregate row, or drop them from the panel entirely. Hiding affects rows only —
/// the panel's rollup counts deliberately still include hidden containers, so cruft
/// building up (unreaped VMs, exited job containers) stays visible in the numbers.
enum ContainerRuleAction: String, Codable {
    case collapse
    case hide
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

    /// The panel's local host-section key — shared with the Settings picker so the
    /// two can't drift.
    static let localHostScope = "this machine"

    static let seededDefaults: [ContainerGroupRule] = [
        ContainerGroupRule(pattern: "sassydog-ghr-ubu-*", label: "ghr runners", host: "ubu-3xdv"),
        ContainerGroupRule(pattern: "api-*", label: "workflow jobs", host: "ubu-3xdv"),
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
        case id, pattern, label, action, host
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(UUID.self, forKey: .id)
        pattern = try container.decode(String.self, forKey: .pattern)
        label = try container.decode(String.self, forKey: .label)
        action = try container.decodeIfPresent(ContainerRuleAction.self, forKey: .action) ?? .collapse
        host = try container.decodeIfPresent(String.self, forKey: .host)
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
        matcher: (String, String) -> Bool = ContainerGrouping.matches
    ) -> (individual: [ContainerInfo], aggregates: [ContainerGroupAggregate]) {
        let applicable = rules.filter { $0.host == nil || $0.host == host }
        var matched: [UUID: [ContainerInfo]] = [:]
        var individual: [ContainerInfo] = []

        for container in containers {
            if let rule = applicable.first(where: { matcher(container.name, $0.pattern) }) {
                switch rule.action {
                case .collapse: matched[rule.id, default: []].append(container)
                case .hide: break
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
                dominantRuntime: dominantRuntime(of: members)
            )
        }

        return (individual, aggregates)
    }

    /// Most frequent runtime among the matched containers; ties break toward the
    /// smaller raw value for determinism. nil for an empty group — never invent one.
    private static func dominantRuntime(of members: [ContainerInfo]) -> ContainerRuntime? {
        let counts = Dictionary(grouping: members, by: \.runtime).mapValues(\.count)
        let best = counts.max { lhs, rhs in
            if lhs.value != rhs.value { return lhs.value < rhs.value }
            return lhs.key.rawValue > rhs.key.rawValue
        }
        return best?.key
    }
}
