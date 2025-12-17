import Foundation

struct GitStatusParser {
    
    struct GitStatus {
        let branch: String?
        let trackingBranch: String?
        let hasUncommittedChanges: Bool
        let aheadCount: Int
        let behindCount: Int
        let isDetached: Bool
    }
    
    static func parseStatus(from statusOutput: String, revListOutput: String?) -> GitStatus {
        var branch: String?
        var trackingBranch: String?
        var hasUncommittedChanges = false
        var aheadCount = 0
        var behindCount = 0
        var isDetached = false
        
        let lines = statusOutput.components(separatedBy: .newlines)
        
        for line in lines {
            // Parse porcelain v2 format
            if line.hasPrefix("# branch.head ") {
                let head = line.dropFirst("# branch.head ".count).trimmingCharacters(in: .whitespaces)
                if head == "(detached)" {
                    isDetached = true
                    branch = "HEAD"
                } else {
                    branch = head
                }
            } else if line.hasPrefix("# branch.upstream ") {
                trackingBranch = line.dropFirst("# branch.upstream ".count).trimmingCharacters(in: .whitespaces)
            } else if line.hasPrefix("# branch.ab ") {
                // Parse ahead/behind from porcelain v2 format
                let ab = line.dropFirst("# branch.ab ".count)
                let parts = ab.components(separatedBy: " ")
                if parts.count >= 2 {
                    aheadCount = Int(parts[0].replacingOccurrences(of: "+", with: "")) ?? 0
                    behindCount = abs(Int(parts[1]) ?? 0) // Behind is negative, so take absolute value
                }
            }
            
            // Check for file changes (porcelain v2 format)
            if line.hasPrefix("1 ") || line.hasPrefix("2 ") {
                // These indicate tracked file changes
                hasUncommittedChanges = true
            } else if line.hasPrefix("? ") {
                // Untracked files
                hasUncommittedChanges = true
            } else if line.hasPrefix("u ") {
                // Unmerged files
                hasUncommittedChanges = true
            }
        }
        
        // Override with rev-list output if available (more accurate)
        if let revList = revListOutput {
            let parts = revList.trimmingCharacters(in: .whitespacesAndNewlines).components(separatedBy: "\t")
            if parts.count == 2 {
                aheadCount = Int(parts[0]) ?? 0
                behindCount = Int(parts[1]) ?? 0
            }
        }
        
        return GitStatus(
            branch: branch,
            trackingBranch: trackingBranch,
            hasUncommittedChanges: hasUncommittedChanges,
            aheadCount: aheadCount,
            behindCount: behindCount,
            isDetached: isDetached
        )
    }
    
    static func parseRemoteBranches(from output: String) -> [String] {
        let lines = output.components(separatedBy: .newlines)
        return lines.compactMap { line in
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix("origin/") {
                return trimmed
            }
            return nil
        }
    }
}