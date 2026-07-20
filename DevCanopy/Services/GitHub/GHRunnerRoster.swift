import Foundation

/// One remembered self-hosted runner name. The org's ephemeral runners
/// de-register between jobs, so GitHub's registered list alone can't say "mac-s2
/// should exist" — this auto-learned roster is the missing memory.
struct RunnerRosterEntry: Codable, Equatable {
    var name: String
    var os: RunnerOS
    var lastSeen: Date
}

/// A remembered runner currently absent from GitHub's registered list: amber
/// while recycling (normal ephemeral churn), red once missing beyond grace.
struct GHRunnerAbsence: Identifiable, Equatable {
    var id: String {
        name
    }

    let name: String
    let os: RunnerOS
    let state: PresenceState
}

/// One renderable Runners-panel row. Identity is the NAME for both cases —
/// ephemeral runners get a fresh numeric id on every re-registration, so id-keyed
/// rows would tear down and rebuild each cycle even while nothing visibly changed.
enum GHRunnerDisplayRow: Identifiable, Equatable {
    case registered(GHRunner)
    case absent(GHRunnerAbsence)

    var name: String {
        switch self {
        case let .registered(runner): runner.name
        case let .absent(absence): absence.name
        }
    }

    var os: RunnerOS {
        switch self {
        case let .registered(runner): runner.os
        case let .absent(absence): absence.os
        }
    }

    var id: String {
        name
    }
}

/// Pure roster arithmetic — no I/O, no wall clock. The service owns persistence
/// and only calls these on a SUCCESSFUL registered-runners fetch, which is what
/// freezes every absence clock while GitHub is unreachable.
enum GHRunnerRosterLogic {
    /// Names absent this long are silently dropped — covers decommissioned
    /// runners and rotated hash-suffixed names without manual cleanup.
    static let ageOut: TimeInterval = 24 * 3600

    static let defaultsKey = "ghRunnerRoster"

    /// Upserts a sighting for every registered name, keeps absent entries
    /// untouched, and ages out long-gone ones.
    static func updated(
        roster: [RunnerRosterEntry],
        registered: [GHRunner],
        now: Date
    ) -> [RunnerRosterEntry] {
        var byName = Dictionary(roster.map { ($0.name, $0) }, uniquingKeysWith: { _, last in last })
        for runner in registered {
            byName[runner.name] = RunnerRosterEntry(name: runner.name, os: runner.os, lastSeen: now)
        }
        return byName.values
            .filter { now.timeIntervalSince($0.lastSeen) < ageOut }
            .sorted { $0.name < $1.name }
    }

    /// Roster entries missing from the registered list, as presence states.
    static func absences(
        roster: [RunnerRosterEntry],
        registered: [GHRunner],
        now: Date,
        grace: TimeInterval = Presence.defaultGrace
    ) -> [GHRunnerAbsence] {
        let registeredNames = Set(registered.map(\.name))
        return roster
            .filter { !registeredNames.contains($0.name) }
            .map { entry in
                GHRunnerAbsence(
                    name: entry.name,
                    os: entry.os,
                    state: Presence.state(isPresent: false, lastSeen: entry.lastSeen, now: now, grace: grace)
                )
            }
            .sorted { $0.name < $1.name }
    }

    /// Registered + absent merged in the panel's display order: macOS first,
    /// then Linux, then other; name within — so an absent runner holds the exact
    /// slot it occupied while registered.
    static func displayRows(
        registered: [GHRunner],
        absent: [GHRunnerAbsence]
    ) -> [GHRunnerDisplayRow] {
        func rank(_ os: RunnerOS) -> Int {
            switch os {
            case .macOS: 0
            case .linux: 1
            case .other: 2
            }
        }
        return (registered.map(GHRunnerDisplayRow.registered) + absent.map(GHRunnerDisplayRow.absent))
            .sorted {
                if rank($0.os) != rank($1.os) {
                    return rank($0.os) < rank($1.os)
                }
                return $0.name.localizedStandardCompare($1.name) == .orderedAscending
            }
    }
}
