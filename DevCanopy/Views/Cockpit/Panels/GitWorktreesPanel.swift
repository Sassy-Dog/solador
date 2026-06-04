import SwiftUI

/// Git / Worktrees panel — branch + ahead/behind + dirty state per worktree,
/// grouped by repository.
struct GitWorktreesPanel: CockpitPanelView {
    static let kind: CockpitPanelKind = .gitWorktrees

    @EnvironmentObject private var service: GitWorktreeService

    var body: some View {
        CockpitPanelContainer(kind: Self.kind, trailing: trailingLabel) {
            if service.repos.isEmpty {
                Text("no repositories found")
                    .font(CockpitTheme.mono(11))
                    .foregroundStyle(CockpitTheme.muted)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(service.repos) { repo in
                        repoGroup(repo)
                    }
                }
            }
        }
    }

    private var trailingLabel: String? {
        guard !service.repos.isEmpty else { return nil }
        let worktrees = service.repos.reduce(0) { $0 + $1.worktrees.count }
        return "\(service.repos.count) repos · \(worktrees) wt"
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
        }
    }

    private func row(_ wt: WorktreeStatus) -> some View {
        HStack(spacing: 7) {
            Circle()
                .fill(dotColor(wt))
                .frame(width: 6, height: 6)
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

    private func branchLabel(_ info: WorktreeInfo) -> String {
        if info.isBare { return "(bare)" }
        if info.isDetached { return "(detached)" }
        return info.branch ?? "(unknown)"
    }

    /// green = clean & synced; amber = ahead or dirty; red = behind or diverged.
    private func dotColor(_ wt: WorktreeStatus) -> Color {
        if wt.behind > 0 { return CockpitTheme.red }
        if wt.ahead > 0 || wt.isDirty { return CockpitTheme.amber }
        return CockpitTheme.green
    }

    /// Compact status suffix like `↑2 ·3✎` or `✓`.
    private func suffix(_ wt: WorktreeStatus) -> String {
        if wt.info.isBare { return "" }
        var parts: [String] = []
        if wt.ahead > 0 { parts.append("↑\(wt.ahead)") }
        if wt.behind > 0 { parts.append("↓\(wt.behind)") }
        if wt.isDirty { parts.append("✎") }
        return parts.isEmpty ? "✓" : parts.joined(separator: " ")
    }
}
