import Foundation

/// One worktree of a git repository.
struct WorktreeInfo: Identifiable, Equatable {
    var id: String {
        path
    }

    let path: String
    /// Branch name with `refs/heads/` stripped; nil when detached or bare.
    let branch: String?
    let head: String
    let isDetached: Bool
    let isBare: Bool
}

/// Pure, tested parsing for `git worktree list --porcelain`. No I/O here.
enum WorktreeParsing {
    /// Parses blank-line-separated porcelain records into `[WorktreeInfo]`.
    static func parseWorktreeList(_ output: String) -> [WorktreeInfo] {
        // Records are separated by blank lines. Each record begins with a
        // `worktree <path>` line, then attribute lines (HEAD/branch/detached/bare).
        var results: [WorktreeInfo] = []

        var path: String?
        var head = ""
        var branch: String?
        var isDetached = false
        var isBare = false

        func flush() {
            guard let path else { return }
            results.append(
                WorktreeInfo(
                    path: path,
                    branch: branch,
                    head: head,
                    isDetached: isDetached,
                    isBare: isBare
                )
            )
        }

        func reset() {
            path = nil
            head = ""
            branch = nil
            isDetached = false
            isBare = false
        }

        for rawLine in output.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = String(rawLine)
            if line.trimmingCharacters(in: .whitespaces).isEmpty {
                // Blank line terminates the current record.
                if path != nil { flush()
                    reset()
                }
                continue
            }

            if line.hasPrefix("worktree ") {
                // New record begins; flush any in-progress one.
                if path != nil { flush()
                    reset()
                }
                path = String(line.dropFirst("worktree ".count)).trimmingCharacters(in: .whitespaces)
            } else if line.hasPrefix("HEAD ") {
                head = String(line.dropFirst("HEAD ".count)).trimmingCharacters(in: .whitespaces)
            } else if line.hasPrefix("branch ") {
                let ref = String(line.dropFirst("branch ".count)).trimmingCharacters(in: .whitespaces)
                branch = ref.hasPrefix("refs/heads/")
                    ? String(ref.dropFirst("refs/heads/".count))
                    : ref
            } else if line == "detached" {
                isDetached = true
            } else if line == "bare" {
                isBare = true
            }
            // Unknown attribute lines are ignored (lenient).
        }

        // Flush a trailing record not followed by a blank line.
        if path != nil { flush() }

        return results
    }
}
