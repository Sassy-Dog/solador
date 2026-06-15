import Foundation

/// Per-worktree status combining the parsed worktree record with ahead/behind
/// and dirty state computed from `git status`.
struct WorktreeStatus: Sendable, Identifiable, Equatable {
    var id: String { info.id }
    let info: WorktreeInfo
    let ahead: Int
    let behind: Int
    let isDirty: Bool
}

/// A repository and its worktrees that have changes (clean ones are summarized
/// by `cleanCount` to keep the panel glanceable).
struct RepoWorktrees: Sendable, Identifiable, Equatable {
    var id: String { path }
    let name: String
    let path: String
    let worktrees: [WorktreeStatus]   // only worktrees with changes
    let cleanCount: Int               // worktrees that are clean & synced (hidden)
}

/// Discovers git repos under configured roots and reports their worktrees with
/// branch / ahead-behind / dirty state. Filesystem scanning and git invocation
/// happen off the main actor; results are published on the main actor.
@MainActor
final class GitWorktreeService: ObservableObject {
    @Published private(set) var repos: [RepoWorktrees] = []
    @Published private(set) var lastUpdated: Date?

    /// Roots to scan, with `~` expanded.
    private let roots: [String]
    /// Max directory depth to descend looking for `.git` entries.
    private let maxDepth: Int

    private var task: Task<Void, Never>?

    init(roots: [String] = ["~/Repos"], maxDepth: Int = 3) {
        self.roots = roots.map { ($0 as NSString).expandingTildeInPath }
        self.maxDepth = maxDepth
    }

    /// Initial scan plus a repeating refresh every ~30s.
    func start(interval: TimeInterval = 30) {
        guard task == nil else { return }
        task = Task { [weak self] in
            guard let self else { return }
            await self.refresh()
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: UInt64(interval * 1_000_000_000))
                if Task.isCancelled { break }
                await self.refresh()
            }
        }
    }

    func stop() {
        task?.cancel()
        task = nil
    }

    /// Stops the current polling loop and restarts it on a new cadence. Used
    /// when the user changes the Refresh Interval setting.
    func restart(interval: TimeInterval) {
        stop()
        start(interval: interval)
    }

    deinit { task?.cancel() }

    /// Scans + computes status off-actor, then publishes.
    func refresh() async {
        let roots = self.roots
        let maxDepth = self.maxDepth

        let scanned = await Task.detached { () -> [RepoWorktrees] in
            let repoPaths = Self.discoverRepos(roots: roots, maxDepth: maxDepth)
                // Only the tracked portfolio repos — keeps the panel focused.
                .filter { PortfolioRepos.matches(repoDirName: ($0 as NSString).lastPathComponent) }
            return repoPaths.compactMap { Self.repoWorktrees(at: $0) }
        }.value

        self.repos = scanned.sorted { $0.name.lowercased() < $1.name.lowercased() }
        self.lastUpdated = Date()
    }

    // MARK: - Discovery (off-actor)

    /// Finds directories containing a `.git` entry up to `maxDepth` below each
    /// root. Does not descend into a repo once found.
    nonisolated private static func discoverRepos(roots: [String], maxDepth: Int) -> [String] {
        let fm = FileManager.default
        var found: [String] = []

        func scan(_ dir: String, depth: Int) {
            // A directory with a `.git` entry is a repo root.
            if fm.fileExists(atPath: "\(dir)/.git") {
                found.append(dir)
                return  // don't descend into a repo
            }
            guard depth < maxDepth else { return }
            guard let entries = try? fm.contentsOfDirectory(atPath: dir) else { return }
            for entry in entries where !entry.hasPrefix(".") {
                let child = "\(dir)/\(entry)"
                var isDir: ObjCBool = false
                if fm.fileExists(atPath: child, isDirectory: &isDir), isDir.boolValue {
                    scan(child, depth: depth + 1)
                }
            }
        }

        for root in roots {
            var isDir: ObjCBool = false
            guard fm.fileExists(atPath: root, isDirectory: &isDir), isDir.boolValue else { continue }
            scan(root, depth: 0)
        }
        return found
    }

    /// Lists worktrees of the repo at `path` and computes per-worktree status.
    /// Returns nil if the repo errors or has no worktrees.
    nonisolated private static func repoWorktrees(at path: String) -> RepoWorktrees? {
        guard let listOutput = runGit(directory: path, ["worktree", "list", "--porcelain"]) else {
            return nil
        }
        let infos = WorktreeParsing.parseWorktreeList(listOutput)
        guard !infos.isEmpty else { return nil }

        let statuses: [WorktreeStatus] = infos.map { info in
            if info.isBare {
                return WorktreeStatus(info: info, ahead: 0, behind: 0, isDirty: false)
            }
            let statusOutput = runGit(directory: info.path, ["status", "--porcelain=v2", "--branch"]) ?? ""
            let parsed = GitStatusParser.parseStatus(from: statusOutput, revListOutput: nil)
            return WorktreeStatus(
                info: info,
                ahead: parsed.aheadCount,
                behind: parsed.behindCount,
                isDirty: parsed.hasUncommittedChanges
            )
        }

        // Show only worktrees with changes; summarize the rest as a clean count.
        let changed = statuses.filter { $0.ahead > 0 || $0.behind > 0 || $0.isDirty }
        let cleanCount = statuses.count - changed.count
        let name = (path as NSString).lastPathComponent
        return RepoWorktrees(name: name, path: path, worktrees: changed, cleanCount: cleanCount)
    }

    /// Runs `git -C <directory> <args>` capturing stdout. Returns nil on failure.
    nonisolated private static func runGit(directory: String, _ args: [String]) -> String? {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/git")
        process.arguments = ["-C", directory] + args

        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = Pipe()

        do {
            try process.run()
            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            guard process.terminationStatus == 0 else { return nil }
            return String(data: data, encoding: .utf8)
        } catch {
            return nil
        }
    }
}
