import Foundation

/// Pure parsing helpers for Claude Code's local JSONL logs. Each helper is
/// side-effect free so it can be unit-tested; the surrounding service handles
/// file discovery and threading.
enum ClaudeUsageLog {
    /// Shared formatter for the ISO-8601 timestamps with fractional seconds
    /// (e.g. `2026-05-29T13:18:22.932Z`).
    private static let isoFormatter: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()

    private static let isoFormatterNoFraction: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime]
        return f
    }()

    /// Derives a short project name from a session's `cwd`. Strips any
    /// worktree suffix (`/.claude/worktrees/...`, `/.warp-worktrees/...`) so all
    /// agents working a repo attribute to one project, then takes the last path
    /// component of the repo root.
    static func projectName(fromCWD cwd: String) -> String {
        guard !cwd.isEmpty else { return "unknown" }

        var path = cwd
        for marker in ["/.claude/worktrees/", "/.warp-worktrees/"] {
            if let range = path.range(of: marker) {
                path = String(path[..<range.lowerBound])
            }
        }

        let component = (path as NSString)
            .standardizingPath
            .split(separator: "/")
            .last
            .map(String.init)

        return component?.isEmpty == false ? component! : "unknown"
    }

    /// Parses a single JSONL line into a `UsageRecord`, or nil if the line is
    /// not an assistant message carrying usage, or is malformed. Never throws.
    static func parseLine(_ line: String) -> UsageRecord? {
        guard let data = line.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return nil }

        guard (obj["type"] as? String) == "assistant" else { return nil }
        guard let message = obj["message"] as? [String: Any],
              let usage = message["usage"] as? [String: Any]
        else { return nil }

        let timestampString = obj["timestamp"] as? String ?? ""
        let timestamp = isoFormatter.date(from: timestampString)
            ?? isoFormatterNoFraction.date(from: timestampString)
            ?? Date.distantPast

        let requestId = obj["requestId"] as? String ?? ""
        let model = message["model"] as? String ?? "unknown"
        let cwd = obj["cwd"] as? String ?? ""

        return UsageRecord(
            timestamp: timestamp,
            requestId: requestId,
            model: model,
            project: projectName(fromCWD: cwd),
            inputTokens: usage["input_tokens"] as? Int ?? 0,
            outputTokens: usage["output_tokens"] as? Int ?? 0,
            cacheCreationTokens: usage["cache_creation_input_tokens"] as? Int ?? 0,
            cacheReadTokens: usage["cache_read_input_tokens"] as? Int ?? 0
        )
    }
}
