import Foundation
import SwiftUI

/// Reads Claude Code's local JSONL logs and publishes a usage `UsageSummary`.
/// All file I/O + JSON parsing happens off the main actor; the summary is
/// published on the main actor.
///
/// Performance: there can be ~1600 files / hundreds of MB on disk. We only open
/// files modified within the last 8 days (covers the 7d window with slack) and
/// read them line-by-line, parsing only assistant lines that carry usage.
@MainActor
final class ClaudeUsageService: ObservableObject {
    @Published private(set) var summary: UsageSummary?
    @Published private(set) var isLoading = false
    @Published private(set) var lastUpdated: Date?
    @Published private(set) var lastError: String?

    /// Optional ceilings used to render progress bars. When nil, the panel shows
    /// raw amounts with no percentage. Kept simple — adjust here if desired.
    let fiveHourTokenLimit: Int? = nil
    let weeklyTokenLimit: Int? = nil

    private var task: Task<Void, Never>?

    /// Root of Claude Code's per-project logs.
    private static var projectsDir: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".claude/projects", isDirectory: true)
    }

    func refresh() async {
        isLoading = true
        let now = Date()
        let dir = Self.projectsDir
        let dirExists = FileManager.default.fileExists(atPath: dir.path)
        let summary = await Task.detached(priority: .utility) {
            Self.loadSummary(projectsDir: dir, now: now)
        }.value
        self.summary = summary
        self.lastError = dirExists ? nil : "no ~/.claude/projects"
        self.lastUpdated = Date()
        self.isLoading = false
    }

    /// Initial load plus a repeating refresh.
    func start(interval: TimeInterval = 60) {
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

    // MARK: - Off-actor loading

    nonisolated private static func loadSummary(projectsDir: URL, now: Date) -> UsageSummary {
        let fm = FileManager.default
        let cutoff = now.addingTimeInterval(-8 * 24 * 3600) // 8 days of slack over the 7d window

        guard let enumerator = fm.enumerator(
            at: projectsDir,
            includingPropertiesForKeys: [.contentModificationDateKey, .isRegularFileKey],
            options: [.skipsHiddenFiles]
        ) else {
            return UsageSummary()
        }

        var records: [UsageRecord] = []

        for case let url as URL in enumerator {
            guard url.pathExtension == "jsonl" else { continue }
            guard let values = try? url.resourceValues(forKeys: [.contentModificationDateKey, .isRegularFileKey]),
                  values.isRegularFile == true,
                  let modified = values.contentModificationDate,
                  modified >= cutoff
            else { continue }

            appendRecords(from: url, into: &records)
        }

        return ClaudeUsageAggregator.summarize(records: records, now: now)
    }

    /// Streams a file line-by-line, parsing usage records. Resilient: malformed
    /// lines and read errors are skipped.
    nonisolated private static func appendRecords(from url: URL, into records: inout [UsageRecord]) {
        guard let handle = try? FileHandle(forReadingFrom: url) else { return }
        defer { try? handle.close() }

        var buffer = Data()
        let newline = UInt8(ascii: "\n")
        let chunkSize = 1 << 16 // 64 KB

        while let chunk = try? handle.read(upToCount: chunkSize), !chunk.isEmpty {
            buffer.append(chunk)
            while let idx = buffer.firstIndex(of: newline) {
                let lineData = buffer.subdata(in: buffer.startIndex..<idx)
                buffer.removeSubrange(buffer.startIndex...idx)
                if let line = String(data: lineData, encoding: .utf8),
                   let record = ClaudeUsageLog.parseLine(line) {
                    records.append(record)
                }
            }
        }
        // Trailing line without newline.
        if !buffer.isEmpty,
           let line = String(data: buffer, encoding: .utf8),
           let record = ClaudeUsageLog.parseLine(line) {
            records.append(record)
        }
    }
}
