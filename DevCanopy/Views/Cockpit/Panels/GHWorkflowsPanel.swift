import AppKit
import SwiftUI

/// Repos panel — a fixed row per watched repo with glanceable counts (open
/// issues/PRs, local/remote branches, worktrees, running jobs) plus the elapsed
/// time of the longest-running job. The row count is fixed (one per watched
/// repo), so the card never resizes as CI activity comes and goes. Workflow,
/// issue, and PR data come from `GHWorkflowsService` (fine-grained PAT in the
/// Keychain); branch/worktree counts come from the local `GitWorktreeService`.
struct GHWorkflowsPanel: CockpitPanelView {
    static let kind: CockpitPanelKind = .ghWorkflows

    @EnvironmentObject private var service: GHWorkflowsService
    @EnvironmentObject private var worktreeService: GitWorktreeService

    /// What a repo's status dot communicates, in descending urgency. The dot
    /// collapses what used to be separate RUNNING / NEEDS APPROVAL / STUCK /
    /// NEEDS ATTENTION sections into one fixed-size signal.
    private enum RepoStatus {
        case unreachable // runs couldn't be fetched (auth/network)
        case failed // main or last-PR run failed, or a queued run is stuck
        case needsApproval // a run is parked at a deployment-protection gate
        case running // actively executing, nothing wrong
        case healthy // green across the board
    }

    private struct RepoRow: Identifiable {
        let id: String // "owner/name"
        let name: String // short name shown in the row
        let slug: String // "owner/name" for the Actions URL
        let running: Int
        let longest: String // elapsed of the oldest running run; "" if none
        let openIssues: Int? // open issues (PRs excluded); nil when the GitHub fetch failed
        let openPRs: Int? // open pull requests; nil when the GitHub fetch failed
        let localBranches: Int? // nil when the repo isn't found on disk
        let remoteBranches: Int? // nil when the GitHub branch fetch failed
        let worktrees: Int? // nil when the repo isn't found on disk
        let status: RepoStatus
    }

    // Fixed column widths — the cockpit's monospace font makes these align cleanly.
    private let issuesW: CGFloat = 52
    private let prsW: CGFloat = 34
    private let localW: CGFloat = 44
    private let remoteW: CGFloat = 52
    private let wtW: CGFloat = 34
    private let jobsW: CGFloat = 40
    private let longestW: CGFloat = 56

    var body: some View {
        let loading = service.isLoading && service.health.isEmpty

        return CockpitPanelContainer(
            kind: Self.kind,
            trailing: trailing(
                running: totalRunning,
                approval: totalApproval,
                stuck: totalStuck,
                attention: totalAttention,
                unreadable: unreadableCount,
                loading: loading
            )
        ) {
            if !service.isAuthenticated {
                muted("connect a GitHub token in Settings")
            } else if loading {
                muted("loading…")
            } else {
                VStack(alignment: .leading, spacing: 6) {
                    headerRow
                    ForEach(rows) { repoRow($0) }
                    healthLine(attention: totalAttention, unreadable: unreadableCount)
                        .padding(.top, 4)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    // MARK: - Row model

    /// One row per watched repo, sorted by name for a stable order. Joins the
    /// GitHub workflow health with local worktree/branch counts using the same
    /// normalized name the two services already share (`PortfolioRepos`).
    private var rows: [RepoRow] {
        let wtByName = Dictionary(
            worktreeService.repos.map { (PortfolioRepos.normalize($0.name), $0) },
            uniquingKeysWith: { first, _ in first }
        )
        return service.health
            .sorted { $0.shortName.lowercased() < $1.shortName.lowercased() }
            .map { h in
                let wt = wtByName[PortfolioRepos.normalize(h.shortName)]
                let oldestRunning = h.running.compactMap(\.startedAt).min()
                return RepoRow(
                    id: h.repo,
                    name: h.shortName,
                    slug: h.repo,
                    running: h.running.count,
                    longest: elapsed(oldestRunning),
                    openIssues: h.openIssues,
                    openPRs: h.openPRs,
                    localBranches: wt?.localBranches,
                    remoteBranches: h.remoteBranches,
                    worktrees: wt?.worktreeCount,
                    status: status(for: h)
                )
            }
    }

    /// Status-dot precedence: the most urgent condition wins. Unreachable and
    /// failed/stuck are problems (red / muted); needs-approval asks for a human;
    /// running is mere activity; otherwise healthy.
    private func status(for h: RepoWorkflowHealth) -> RepoStatus {
        if !h.reachable { return .unreachable }
        if (h.main?.isFailed ?? false) || (h.lastPR?.isFailed ?? false) || !h.stuck.isEmpty {
            return .failed
        }
        if !h.needsApproval.isEmpty { return .needsApproval }
        if !h.running.isEmpty { return .running }
        return .healthy
    }

    // MARK: - Aggregates (fixed-size header + footer)

    private var totalRunning: Int {
        service.health.reduce(0) { $0 + $1.running.count }
    }

    private var totalApproval: Int {
        service.health.reduce(0) { $0 + $1.needsApproval.count }
    }

    private var totalStuck: Int {
        service.health.reduce(0) { $0 + $1.stuck.count }
    }

    private var totalAttention: Int {
        service.health.reduce(0) { acc, h in
            acc + ((h.main?.isFailed ?? false) ? 1 : 0) + ((h.lastPR?.isFailed ?? false) ? 1 : 0)
        }
    }

    private var unreadableCount: Int {
        service.health.count(where: { !$0.reachable })
    }

    private func trailing(
        running: Int,
        approval: Int,
        stuck: Int,
        attention: Int,
        unreadable: Int,
        loading: Bool
    ) -> String? {
        guard service.isAuthenticated, !loading else { return nil }
        var parts: [String] = []
        if approval > 0 { parts.append("\(approval) needs approval") }
        if stuck > 0 { parts.append("\(stuck) stuck") }
        if running > 0 { parts.append("\(running) running") }
        if attention > 0 { parts.append("\(attention) failed") }
        if unreadable > 0 { parts.append("\(unreadable) unreadable") }
        return parts.isEmpty ? "all green" : parts.joined(separator: " · ")
    }

    // MARK: - Rows

    private var headerRow: some View {
        HStack(spacing: 8) {
            Color.clear.frame(width: 6, height: 1) // aligns with the status dot
            headerLabel("REPO", width: nil)
            Spacer(minLength: 8)
            headerLabel("ISSUES", width: issuesW)
            headerLabel("PRS", width: prsW)
            headerLabel("REMOTE", width: remoteW)
            headerLabel("LOCAL", width: localW)
            headerLabel("WT", width: wtW)
            headerLabel("JOBS", width: jobsW)
            headerLabel("LONGEST", width: longestW)
        }
    }

    private func headerLabel(_ text: String, width: CGFloat?) -> some View {
        Text(text)
            .font(CockpitTheme.mono(9, weight: .bold))
            .foregroundStyle(CockpitTheme.muted)
            .frame(width: width, alignment: width == nil ? .leading : .trailing)
    }

    private func repoRow(_ row: RepoRow) -> some View {
        HStack(spacing: 8) {
            statusDot(row.status)
            Text(row.name)
                .font(CockpitTheme.mono(11, weight: .bold))
                .foregroundStyle(CockpitTheme.ink)
                .lineLimit(1)
            Spacer(minLength: 8)
            countCell(row.openIssues, width: issuesW)
            countCell(row.openPRs, width: prsW)
            countCell(row.remoteBranches, width: remoteW)
            countCell(row.localBranches, width: localW)
            countCell(row.worktrees, width: wtW)
            countCell(row.running, width: jobsW, nonZeroColor: CockpitTheme.amber)
            Text(row.longest.isEmpty ? "·" : row.longest)
                .font(CockpitTheme.mono(11))
                .foregroundStyle(row.longest.isEmpty ? CockpitTheme.muted : CockpitTheme.amber)
                .frame(width: longestW, alignment: .trailing)
        }
        .contentShape(Rectangle())
        .onTapGesture { openActions(row.slug) }
    }

    /// A right-aligned count. Renders "—" when the value is unknown (repo not on
    /// disk, or GitHub fetch failed) and dims a real zero so non-zero pops.
    /// `nonZeroColor` tints a non-zero value — used to render JOBS in amber (the
    /// most ephemeral column), pairing it with the amber LONGEST cell beside it.
    private func countCell(
        _ value: Int?,
        width: CGFloat,
        nonZeroColor: Color = CockpitTheme.ink
    ) -> some View {
        Group {
            if let value {
                Text("\(value)").foregroundStyle(value == 0 ? CockpitTheme.muted : nonZeroColor)
            } else {
                Text("—").foregroundStyle(CockpitTheme.muted)
            }
        }
        .font(CockpitTheme.mono(11))
        .frame(width: width, alignment: .trailing)
    }

    @ViewBuilder
    private func statusDot(_ status: RepoStatus) -> some View {
        if status == .needsApproval {
            // Blinking to read as "act now" — a human must approve the gate.
            BlinkingDot(color: CockpitTheme.amber)
        } else {
            Circle().fill(color(for: status)).frame(width: 6, height: 6)
        }
    }

    private func color(for status: RepoStatus) -> Color {
        switch status {
        case .unreachable: CockpitTheme.muted
        case .failed: CockpitTheme.red
        case .needsApproval: CockpitTheme.amber
        case .running: CockpitTheme.amber
        case .healthy: CockpitTheme.green
        }
    }

    private func openActions(_ slug: String) {
        if let url = URL(string: "https://github.com/\(slug)/actions") {
            NSWorkspace.shared.open(url)
        }
    }

    /// Reassurance line. "Healthy" excludes only failed/unreachable repos — a
    /// repo that is merely running still counts as healthy, so the fraction never
    /// implies a problem just because a build is in flight.
    @ViewBuilder
    private func healthLine(attention: Int, unreadable: Int) -> some View {
        let total = service.health.count
        let healthy = service.health.count(where: { $0.isHealthy })
        if attention == 0, unreadable == 0 {
            label("✓ all \(total) healthy", CockpitTheme.green)
        } else {
            label("✓ \(healthy)/\(total) healthy", CockpitTheme.green)
        }
    }

    // MARK: - Helpers

    private func muted(_ text: String) -> some View {
        Text(text).font(CockpitTheme.mono(11)).foregroundStyle(CockpitTheme.muted)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func label(_ text: String, _ color: Color) -> some View {
        Text(text).font(CockpitTheme.mono(10)).foregroundStyle(color)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func elapsed(_ date: Date?) -> String {
        guard let date else { return "" }
        let s = Int(max(0, Date().timeIntervalSince(date)))
        if s < 60 { return "\(s)s" }
        if s < 3600 { return "\(s / 60)m" }
        return "\(s / 3600)h\((s % 3600) / 60)m"
    }
}

/// A status dot that pulses its opacity to draw the eye — used for a repo with a
/// run parked at an approval gate, vs the steady dot used otherwise.
private struct BlinkingDot: View {
    let color: Color
    @State private var dim = false

    var body: some View {
        Circle()
            .fill(color)
            .frame(width: 6, height: 6)
            .opacity(dim ? 0.25 : 1.0)
            .animation(.easeInOut(duration: 0.7).repeatForever(autoreverses: true), value: dim)
            .onAppear { dim = true }
    }
}
