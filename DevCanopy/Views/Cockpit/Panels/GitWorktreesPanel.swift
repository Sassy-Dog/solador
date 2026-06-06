import SwiftUI

/// Git / Worktrees panel — scoped to the tracked portfolio repos, showing only
/// worktrees with changes (ahead/behind/dirty); clean worktrees are summarized so
/// the panel stays glanceable.
struct GitWorktreesPanel: CockpitPanelView {
    static let kind: CockpitPanelKind = .gitWorktrees

    @EnvironmentObject private var service: GitWorktreeService

    var body: some View {
        CockpitPanelContainer(kind: Self.kind, trailing: trailingLabel) {
            VStack(alignment: .leading, spacing: 8) {
                if service.repos.isEmpty {
                    Text("no portfolio repos found")
                        .font(CockpitTheme.mono(11))
                        .foregroundStyle(CockpitTheme.muted)
                        .frame(maxWidth: .infinity, alignment: .leading)
                } else {
                    ForEach(service.repos) { repo in
                        repoGroup(repo)
                    }
                }
                PanelStatusFooter(lastUpdated: service.lastUpdated, error: nil, staleAfter: 120)
            }
        }
    }

    private var trailingLabel: String? {
        guard !service.repos.isEmpty else { return nil }
        let changed = service.repos.reduce(0) { $0 + $1.worktrees.count }
        return changed == 0 ? "all clean" : "\(changed) changed"
    }

    @ViewBuilder
    private func repoGroup(_ repo: RepoWorktrees) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(repo.name.uppercased())
                .font(CockpitTheme.mono(9, weight: .bold))
                .foregroundStyle(CockpitTheme.muted)
            ForEach(repo.worktrees) { wt in
                row(wt)
            }
            if repo.worktrees.isEmpty {
                cleanLine("✓ all \(repo.cleanCount) clean")
            } else if repo.cleanCount > 0 {
                cleanLine("+ \(repo.cleanCount) clean")
            }
        }
    }

    private func row(_ wt: WorktreeStatus) -> some View {
        HStack(spacing: 7) {
            Circle().fill(dotColor(wt)).frame(width: 6, height: 6)
            Text(branchLabel(wt.info))
                .font(CockpitTheme.mono(11, weight: .bold))
                .foregroundStyle(CockpitTheme.ink)
                .lineLimit(1)
            Spacer()
            Text(suffix(wt))
                .font(CockpitTheme.mono(9))
                .foregroundStyle(dotColor(wt))
                .lineLimit(1)
        }
    }

    private func cleanLine(_ text: String) -> some View {
        Text(text)
            .font(CockpitTheme.mono(9))
            .foregroundStyle(CockpitTheme.muted)
    }

    private func branchLabel(_ info: WorktreeInfo) -> String {
        if info.isBare { return "(bare)" }
        if info.isDetached { return "(detached)" }
        return info.branch ?? "(unknown)"
    }

    /// amber = ahead or dirty; red = behind or diverged. (Clean rows aren't shown.)
    private func dotColor(_ wt: WorktreeStatus) -> Color {
        if wt.behind > 0 { return CockpitTheme.red }
        if wt.ahead > 0 || wt.isDirty { return CockpitTheme.amber }
        return CockpitTheme.green
    }

    private func suffix(_ wt: WorktreeStatus) -> String {
        var parts: [String] = []
        if wt.ahead > 0 { parts.append("↑\(wt.ahead)") }
        if wt.behind > 0 { parts.append("↓\(wt.behind)") }
        if wt.isDirty { parts.append("✎") }
        return parts.isEmpty ? "✓" : parts.joined(separator: " ")
    }
}
