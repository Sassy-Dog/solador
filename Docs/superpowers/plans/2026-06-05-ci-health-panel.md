# CI Health Panel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Evolve the cockpit's "Portfolio CI" panel into a "CI Health" panel that, across 6 curated repos, shows what's running now and what's failing (main / last-PR) so the user knows what to go look at.

**Architecture:** Reuse the existing per-repo `GET /repos/{repo}/actions/runs?per_page=30` fetch. Replace the "latest run only" mapping with a pure categorizer producing per-repo `main` / `lastPR` / `running` from the runs. The panel renders RUNNING and NEEDS ATTENTION sections plus a green-count line. Rename the panel kind `portfolioCI` → `ciHealth`.

**Tech Stack:** Swift, SwiftUI, SwiftData (existing models), XCTest, XcodeGen (`./Scripts/generate-project.sh` after adding files).

**Spec:** `Docs/superpowers/specs/2026-06-05-ci-health-panel-design.md`

**Working branch:** `feat/ci-health-panel` (already created; holds the spec commit).

**Build/test commands:** `./dev test` (build + tests). After adding/removing `.swift` files: `./Scripts/generate-project.sh` first (per project convention — `./dev` won't pick up new files otherwise).

---

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `DevCanopy/Services/GitHub/PortfolioRepos.swift` | modify | add devcanopy + platform slugs |
| `DevCanopy/Services/GitHub/PortfolioCIMapping.swift` | modify | add `RepoCIHealth`/`RunRef` + pure `health(repo:runs:)`; later remove old `RepoCIStatus`/`map` |
| `DevCanopy/Services/GitHub/PortfolioCIService.swift` | modify | publish `[RepoCIHealth]`; 60s interval |
| `DevCanopy/Views/Cockpit/CockpitPanel.swift` | modify | rename kind `portfolioCI` → `ciHealth`, title "CI Health" |
| `DevCanopy/Views/Cockpit/CockpitView.swift` | modify | switch case `ciHealth` |
| `DevCanopy/Views/Cockpit/Panels/CIHealthPanel.swift` | create | two-section panel view |
| `DevCanopy/Views/Cockpit/Panels/PortfolioCIPanel.swift` | delete | replaced by `CIHealthPanel` |
| `DevCanopyTests/PortfolioCIMappingTests.swift` | modify | tests for `health()` categorization |

---

## Task 1: Add devcanopy + platform to the curated repos

**Files:**
- Modify: `DevCanopy/Services/GitHub/PortfolioRepos.swift:11-16`

- [ ] **Step 1: Add the two slugs**

Replace the `slugs` array:

```swift
    /// owner/name slugs.
    static let slugs = [
        "Sassy-Dog/velovate",
        "Sassy-Dog/qr-ninja",
        "Sassy-Dog/tailored-tip",
        "Sassy-Dog/what2wear",
        "Sassy-Dog/devcanopy",
        "Sassy-Dog/platform"
    ]
```

- [ ] **Step 2: Build to confirm it compiles**

Run: `cd /Users/chris/Repos/sassy-dog/devcanopy/devcanopy && ./dev test`
Expected: `✅ All tests passed` (no behavior change yet; just more repos).

- [ ] **Step 3: Commit**

```bash
cd /Users/chris/Repos/sassy-dog/devcanopy/devcanopy
git add DevCanopy/Services/GitHub/PortfolioRepos.swift
git commit -m "feat(ci): track devcanopy and platform in the curated repo set

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Add the per-repo health categorizer (additive, TDD)

Add new types and a pure `health(repo:runs:)` alongside the existing `RepoCIStatus`/`map` (which stay until Task 3), so the tree keeps compiling.

**Files:**
- Modify: `DevCanopy/Services/GitHub/PortfolioCIMapping.swift`
- Test: `DevCanopyTests/PortfolioCIMappingTests.swift`

- [ ] **Step 1: Write the failing tests**

Append these tests to `DevCanopyTests/PortfolioCIMappingTests.swift`, just before the final closing `}` of the class. They reuse the existing `dto(...)` and `makeCreated(...)` helpers in that file.

```swift
    // MARK: - health() categorization

    func testHealthMainPicksLatestPushOnMain() {
        let runs = [
            makeCreated(dto(name: "CI", status: "completed", conclusion: "success", branch: "main", event: "push"), "2026-05-29T10:00:00Z"),
            makeCreated(dto(name: "CI", status: "completed", conclusion: "failure", branch: "main", event: "push"), "2026-05-29T11:00:00Z"), // newer
            makeCreated(dto(name: "CI", status: "completed", conclusion: "success", branch: "feat/x", event: "pull_request"), "2026-05-29T12:00:00Z")
        ]
        let h = PortfolioCIMapping.health(repo: "Sassy-Dog/velovate", runs: runs)
        XCTAssertEqual(h.main?.conclusion, .failure, "main = newest push-on-main run")
        XCTAssertEqual(h.main?.context, "main")
    }

    func testHealthLastPRPicksLatestPullRequestRun() {
        let runs = [
            makeCreated(dto(name: "CI", status: "completed", conclusion: "failure", branch: "feat/old", event: "pull_request"), "2026-05-29T10:00:00Z"),
            makeCreated(dto(name: "CI", status: "completed", conclusion: "success", branch: "feat/new", event: "pull_request"), "2026-05-29T12:00:00Z") // newer
        ]
        let h = PortfolioCIMapping.health(repo: "o/r", runs: runs)
        XCTAssertEqual(h.lastPR?.conclusion, .success)
        XCTAssertEqual(h.lastPR?.context, "feat/new")
    }

    func testHealthRunningCollectsInProgressRuns() {
        let runs = [
            dto(name: "CI", status: "in_progress", conclusion: nil, branch: "main", event: "push"),
            dto(name: "Deploy", status: "queued", conclusion: nil, branch: "main", event: "workflow_dispatch"),
            dto(name: "CI", status: "completed", conclusion: "success", branch: "main", event: "push")
        ]
        let h = PortfolioCIMapping.health(repo: "o/r", runs: runs)
        XCTAssertEqual(h.running.count, 2, "both in_progress and queued runs are 'running'")
        XCTAssertTrue(h.running.allSatisfy { $0.isRunning })
    }

    func testRunRefIsFailedAcrossFailingConclusions() {
        for c in ["failure", "timed_out", "cancelled", "startup_failure"] {
            let h = PortfolioCIMapping.health(repo: "o/r", runs: [dto(name: "CI", status: "completed", conclusion: c, branch: "main", event: "push")])
            XCTAssertEqual(h.main?.isFailed, true, "\(c) should be a failure")
        }
        let ok = PortfolioCIMapping.health(repo: "o/r", runs: [dto(name: "CI", status: "completed", conclusion: "success", branch: "main", event: "push")])
        XCTAssertEqual(ok.main?.isFailed, false)
    }

    func testRepoIsCleanWhenGreenAndNothingRunning() {
        let clean = PortfolioCIMapping.health(repo: "o/r", runs: [
            dto(name: "CI", status: "completed", conclusion: "success", branch: "main", event: "push"),
            dto(name: "CI", status: "completed", conclusion: "success", branch: "feat/x", event: "pull_request")
        ])
        XCTAssertTrue(clean.isClean)

        let failing = PortfolioCIMapping.health(repo: "o/r", runs: [
            dto(name: "CI", status: "completed", conclusion: "failure", branch: "main", event: "push")
        ])
        XCTAssertFalse(failing.isClean)
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd /Users/chris/Repos/sassy-dog/devcanopy/devcanopy && ./dev test 2>&1 | tail -20`
Expected: compile error — `health`, `RepoCIHealth`, `RunRef` are undefined.

- [ ] **Step 3: Add the new types and categorizer**

Append to `DevCanopy/Services/GitHub/PortfolioCIMapping.swift` (after the closing `}` of the `PortfolioCIMapping` enum, at end of file):

```swift

/// One workflow run distilled to what the CI Health panel renders.
struct RunRef: Equatable, Identifiable {
    let runID: Int64
    let title: String          // workflow name, e.g. "CI"
    let context: String        // "main" or the PR/branch name
    let conclusion: RunConclusion?
    let status: RunStatus?
    let startedAt: Date?
    let htmlURL: String

    var id: Int64 { runID }

    var isRunning: Bool {
        switch status {
        case .some(.inProgress), .some(.queued), .some(.requested), .some(.waiting), .some(.pending):
            return true
        default:
            return false
        }
    }

    var isFailed: Bool {
        switch conclusion {
        case .failure, .timedOut, .cancelled, .startupFailure:
            return true
        default:
            return false
        }
    }
}

/// Per-repo CI health: latest main run, latest PR run, and any running runs.
struct RepoCIHealth: Equatable, Identifiable {
    let repo: String           // "owner/name"
    let main: RunRef?
    let lastPR: RunRef?
    let running: [RunRef]

    var id: String { repo }
    var shortName: String { repo.split(separator: "/").last.map(String.init) ?? repo }

    /// Clean = nothing running and neither main nor lastPR failed.
    var isClean: Bool {
        running.isEmpty && !(main?.isFailed ?? false) && !(lastPR?.isFailed ?? false)
    }
}

extension PortfolioCIMapping {
    /// Categorize a repo's runs into main / lastPR / running. Pure; "newest" is by
    /// createdAt descending. Assumes the default branch is `main`.
    static func health(repo: String, runs: [WorkflowRunDTO]) -> RepoCIHealth {
        let sorted = runs.sorted { createdDate($0) > createdDate($1) }
        let main = sorted.first { $0.event == "push" && $0.headBranch == "main" }.map(ref)
        let lastPR = sorted.first { $0.event == "pull_request" }.map(ref)
        let running = sorted.map(ref).filter { $0.isRunning }
        return RepoCIHealth(repo: repo, main: main, lastPR: lastPR, running: running)
    }

    private static func ref(_ run: WorkflowRunDTO) -> RunRef {
        RunRef(
            runID: run.id,
            title: run.name,
            context: contextLabel(run),
            conclusion: run.conclusion.flatMap { RunConclusion(rawValue: $0) },
            status: RunStatus(rawValue: run.status),
            startedAt: startedDate(run),
            htmlURL: run.htmlUrl
        )
    }

    private static func contextLabel(_ run: WorkflowRunDTO) -> String {
        if run.event == "push" && run.headBranch == "main" { return "main" }
        if run.event == "pull_request" { return run.headBranch ?? "PR" }
        return run.headBranch ?? run.event
    }

    private static func startedDate(_ run: WorkflowRunDTO) -> Date? {
        let s = run.runStartedAt ?? run.createdAt
        return ISO8601DateFormatter().date(from: s) ?? isoFractional.date(from: s)
    }
}
```

Note: `createdDate` and `isoFractional` are `private static` on `PortfolioCIMapping` but accessible here because this extension is in the same file.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd /Users/chris/Repos/sassy-dog/devcanopy/devcanopy && ./dev test 2>&1 | tail -8`
Expected: `✅ All tests passed` (old `map` tests + new `health` tests).

- [ ] **Step 5: Commit**

```bash
cd /Users/chris/Repos/sassy-dog/devcanopy/devcanopy
git add DevCanopy/Services/GitHub/PortfolioCIMapping.swift DevCanopyTests/PortfolioCIMappingTests.swift
git commit -m "feat(ci): per-repo health categorizer (main/lastPR/running)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Cut over — service, panel rename, new view; remove old mapping

Switch the service + cockpit to the new model and delete the now-unused `RepoCIStatus`/`map` and old panel. Ends compiling + green.

**Files:**
- Modify: `DevCanopy/Services/GitHub/PortfolioCIService.swift`
- Modify: `DevCanopy/Views/Cockpit/CockpitPanel.swift`
- Modify: `DevCanopy/Views/Cockpit/CockpitView.swift`
- Create: `DevCanopy/Views/Cockpit/Panels/CIHealthPanel.swift`
- Delete: `DevCanopy/Views/Cockpit/Panels/PortfolioCIPanel.swift`
- Modify: `DevCanopy/Services/GitHub/PortfolioCIMapping.swift` (remove old types)
- Modify: `DevCanopyTests/PortfolioCIMappingTests.swift` (remove old-map tests)

- [ ] **Step 1: Rewrite `PortfolioCIService.swift` to publish `[RepoCIHealth]` at 60s**

Replace the whole file body with:

```swift
import Foundation
import SwiftUI

/// Fetches per-repo CI health (main / last-PR / running) for the curated repo set
/// using a fine-grained PAT (via `GitHubService`). Network off the main actor;
/// results publish on the main actor. Per-repo failures are isolated.
@MainActor
final class PortfolioCIService: ObservableObject {
    /// The tracked portfolio (shared source of truth with runners + worktrees).
    static let configuredRepos = PortfolioRepos.slugs

    @Published private(set) var health: [RepoCIHealth] = []
    @Published private(set) var isAuthenticated = false
    @Published private(set) var isLoading = false

    private let github: GitHubService
    private var task: Task<Void, Never>?

    init(github: GitHubService = .shared) {
        self.github = github
    }

    func refresh() async {
        github.configureFromKeychain()
        let authed = github.hasToken
        self.isAuthenticated = authed

        guard authed else {
            self.health = []
            return
        }

        isLoading = true
        var results: [RepoCIHealth] = []
        for repo in Self.configuredRepos {
            results.append(await fetchHealth(for: repo))
        }
        self.health = results
        self.isLoading = false
    }

    /// Fetches recent runs for a repo and categorizes them. Any error yields an
    /// empty (clean) health so one repo failing doesn't break the rest.
    private func fetchHealth(for repo: String) async -> RepoCIHealth {
        let endpoint = "/repos/\(repo)/actions/runs"
        let query = [URLQueryItem(name: "per_page", value: "30")]
        do {
            let data = try await github.getRaw(endpoint: endpoint, queryItems: query)
            let response = try JSONDecoder().decode(WorkflowRunsResponse.self, from: data)
            return PortfolioCIMapping.health(repo: repo, runs: response.workflowRuns)
        } catch {
            appLogger.debug("CI Health: \(repo) fetch failed: \(error.localizedDescription)")
            return RepoCIHealth(repo: repo, main: nil, lastPR: nil, running: [])
        }
    }

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

    deinit { task?.cancel() }
}
```

- [ ] **Step 2: Rename the panel kind in `CockpitPanel.swift`**

In `DevCanopy/Views/Cockpit/CockpitPanel.swift`:

Change the enum case (line ~11) from `case portfolioCI` to `case ciHealth`.

In the `title` switch, replace:
```swift
        case .portfolioCI: return "Portfolio CI"
```
with:
```swift
        case .ciHealth: return "CI Health"
```

In the `systemImage` switch, replace:
```swift
        case .portfolioCI: return "checkmark.seal"
```
with:
```swift
        case .ciHealth: return "checkmark.seal"
```

In `CockpitLayout.hostsForward`, replace the placement:
```swift
                CockpitPlacement(kind: .portfolioCI, span: .half),
```
with:
```swift
                CockpitPlacement(kind: .ciHealth, span: .half),
```

- [ ] **Step 3: Update the panel router in `CockpitView.swift`**

In `DevCanopy/Views/Cockpit/CockpitView.swift`, replace:
```swift
        case .portfolioCI: PortfolioCIPanel()
```
with:
```swift
        case .ciHealth: CIHealthPanel()
```

- [ ] **Step 4: Create `DevCanopy/Views/Cockpit/Panels/CIHealthPanel.swift`**

```swift
import SwiftUI
import AppKit

/// CI Health panel — what's running now and what's failing across the curated
/// repos, so it's clear what needs a look. Authenticates with a fine-grained PAT
/// from the Keychain (set in Settings).
struct CIHealthPanel: CockpitPanelView {
    static let kind: CockpitPanelKind = .ciHealth

    @EnvironmentObject private var service: PortfolioCIService

    private struct RunningItem: Identifiable {
        let repo: String
        let ref: RunRef
        var id: Int64 { ref.runID }
    }
    private struct AttentionItem: Identifiable {
        let repo: String
        let which: String
        let ref: RunRef
        var id: String { "\(repo):\(ref.runID):\(which)" }
    }

    var body: some View {
        CockpitPanelContainer(kind: Self.kind, trailing: summary) {
            if !service.isAuthenticated {
                muted("connect a GitHub token in Settings")
            } else {
                let running = runningItems
                let attention = attentionItems
                VStack(alignment: .leading, spacing: 12) {
                    if !running.isEmpty {
                        sectionHeader("RUNNING", running.count)
                        ForEach(running) { runningRow($0) }
                    }
                    if !attention.isEmpty {
                        sectionHeader("NEEDS ATTENTION", attention.count)
                        ForEach(attention) { attentionRow($0) }
                    }
                    greenLine(running: running.count, attention: attention.count)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    // MARK: Derived items

    private var runningItems: [RunningItem] {
        service.health
            .flatMap { h in h.running.map { RunningItem(repo: h.shortName, ref: $0) } }
            .sorted { ($0.ref.startedAt ?? .distantFuture) < ($1.ref.startedAt ?? .distantFuture) }
    }

    private var attentionItems: [AttentionItem] {
        service.health.flatMap { h -> [AttentionItem] in
            var items: [AttentionItem] = []
            if let m = h.main, m.isFailed {
                items.append(AttentionItem(repo: h.shortName, which: "main", ref: m))
            }
            if let p = h.lastPR, p.isFailed {
                items.append(AttentionItem(repo: h.shortName, which: "PR " + p.context, ref: p))
            }
            return items
        }
    }

    private var summary: String {
        let r = runningItems.count
        let a = attentionItems.count
        if r == 0 && a == 0 { return "all green" }
        return "\(r) running · \(a) failed"
    }

    // MARK: Rows

    private func sectionHeader(_ title: String, _ count: Int) -> some View {
        Text("\(title) (\(count))")
            .font(CockpitTheme.mono(10, weight: .bold))
            .foregroundStyle(CockpitTheme.muted)
    }

    private func runningRow(_ item: RunningItem) -> some View {
        rowChrome(url: item.ref.htmlURL) {
            HStack(spacing: 7) {
                Circle().fill(CockpitTheme.amber).frame(width: 6, height: 6)
                Text(item.repo).font(CockpitTheme.mono(11, weight: .bold)).foregroundStyle(CockpitTheme.ink).lineLimit(1)
                Text("\(item.ref.title) · \(item.ref.context)")
                    .font(CockpitTheme.mono(9)).foregroundStyle(CockpitTheme.muted).lineLimit(1)
                Spacer()
                Text(elapsed(item.ref.startedAt)).font(CockpitTheme.mono(9)).foregroundStyle(CockpitTheme.amber)
            }
        }
    }

    private func attentionRow(_ item: AttentionItem) -> some View {
        rowChrome(url: item.ref.htmlURL) {
            HStack(spacing: 7) {
                Circle().fill(CockpitTheme.red).frame(width: 6, height: 6)
                Text(item.repo).font(CockpitTheme.mono(11, weight: .bold)).foregroundStyle(CockpitTheme.ink).lineLimit(1)
                Text(item.which).font(CockpitTheme.mono(9)).foregroundStyle(CockpitTheme.muted).lineLimit(1)
                Spacer()
                Text("failed · \(relative(item.ref.startedAt))").font(CockpitTheme.mono(9)).foregroundStyle(CockpitTheme.red)
            }
        }
    }

    @ViewBuilder
    private func greenLine(running: Int, attention: Int) -> some View {
        let total = service.health.count
        let green = service.health.filter { $0.isClean }.count
        if running == 0 && attention == 0 {
            label("✓ All \(total) repos green", CockpitTheme.green)
        } else if green > 0 {
            label("✓ \(green)/\(total) repos green", CockpitTheme.green)
        }
    }

    // MARK: Helpers

    private func rowChrome<Content: View>(url: String, @ViewBuilder _ content: () -> Content) -> some View {
        content()
            .contentShape(Rectangle())
            .onTapGesture {
                if let u = URL(string: url) { NSWorkspace.shared.open(u) }
            }
    }

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

    private func relative(_ date: Date?) -> String {
        guard let date else { return "recently" }
        let s = Int(max(0, Date().timeIntervalSince(date)))
        if s < 3600 { return "\(max(1, s / 60))m ago" }
        if s < 86400 { return "\(s / 3600)h ago" }
        return "\(s / 86400)d ago"
    }
}
```

- [ ] **Step 5: Delete the old panel**

```bash
cd /Users/chris/Repos/sassy-dog/devcanopy/devcanopy
git rm DevCanopy/Views/Cockpit/Panels/PortfolioCIPanel.swift
```

- [ ] **Step 6: Remove the now-unused old mapping types**

In `DevCanopy/Services/GitHub/PortfolioCIMapping.swift`, delete the old `RepoHealth` enum (lines ~3-9), the `RepoCIStatus` struct (lines ~11-36), and inside the `PortfolioCIMapping` enum delete `map(repo:runs:)`, `deriveHealth(...)`, and `isRelease(...)`. Keep `createdDate`, `isoFractional`, and everything added in Task 2 (`RunRef`, `RepoCIHealth`, `health`, `ref`, `contextLabel`, `startedDate`).

After editing, the `PortfolioCIMapping` enum should contain only: `health(repo:runs:)` (in the extension), the private `ref`, `contextLabel`, `startedDate` (extension), and the private `createdDate` + `isoFractional` (move these into the extension or keep in the enum body — either compiles since same file). The file's top-level types are `RunRef` and `RepoCIHealth`.

- [ ] **Step 7: Remove the old-map tests**

In `DevCanopyTests/PortfolioCIMappingTests.swift`, delete the tests that reference the removed `map`/`RepoCIStatus` API: `testRepoShortNameTakesPartAfterSlash`, `testMapPicksLatestCIRunByCreatedAt`, `testSuccessHealthIsGood`, `testFailureHealthIsBad`, `testInProgressHealthIsRunning`, `testNoRunsHealthIsUnknown`, `testReleaseRunDetectedSeparatelyFromCI`. Keep the `dto(...)` and `makeCreated(...)` helpers (used by the Task 2 tests) and the Task 2 `health()` tests.

Add a short-name test for the new type (replacing the deleted one):

```swift
    func testRepoCIHealthShortName() {
        let h = PortfolioCIMapping.health(repo: "Sassy-Dog/velovate", runs: [])
        XCTAssertEqual(h.shortName, "velovate")
        XCTAssertTrue(h.isClean, "no runs = nothing running, nothing failed")
    }
```

- [ ] **Step 8: Regenerate the project (a file was added and one removed)**

Run: `cd /Users/chris/Repos/sassy-dog/devcanopy/devcanopy && ./Scripts/generate-project.sh`
Expected: `✅ Xcode project generated successfully`

- [ ] **Step 9: Build + test**

Run: `cd /Users/chris/Repos/sassy-dog/devcanopy/devcanopy && ./dev test 2>&1 | tail -10`
Expected: `✅ All tests passed`. If the compiler reports a leftover reference to `RepoCIStatus`, `PortfolioCIPanel`, or `.portfolioCI`, fix that reference (grep: `grep -rn "RepoCIStatus\|PortfolioCIPanel\|portfolioCI\|\.statuses" DevCanopy DevCanopyTests`).

- [ ] **Step 10: Commit**

```bash
cd /Users/chris/Repos/sassy-dog/devcanopy/devcanopy
git add -A
git commit -m "feat(cockpit): CI Health panel (running + needs-attention sections)

Replace the Portfolio CI panel (single health dot per repo) with a CI Health
panel that lists currently-running workflows and failing main/last-PR runs across
the 6 curated repos, with a green-count line and click-through to the run. Reuses
the existing runs fetch; refresh 120s -> 60s. Renames the panel kind to ciHealth.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Verify in the running app

- [ ] **Step 1: Launch**

Run: `cd /Users/chris/Repos/sassy-dog/devcanopy/devcanopy && osascript -e 'tell application "DevCanopy" to quit' 2>/dev/null; ./dev run`
(Allow ~60s to build + launch.)

- [ ] **Step 2: Confirm the panel**

In the cockpit (row 3, left of Git/Worktrees), confirm the panel titled **CI HEALTH**:
- Trailing summary shows `N running · M failed` (or `all green`).
- If anything is running, a **RUNNING (n)** section lists `repo · workflow · context · elapsed`.
- If main or last-PR failed for any repo, a **NEEDS ATTENTION (n)** section lists them in red.
- A `✓ … repos green` line shows.
- Clicking a row opens that run in the browser.
- (Sanity) With no token configured, it shows "connect a GitHub token in Settings".

- [ ] **Step 3: (Optional) screenshot for the record**

Capture the cockpit window (DevCanopy must be frontmost) to confirm visually, then continue.

---

## Self-Review

**Spec coverage:**
- Curated 6 repos → Task 1. ✓
- Reuse existing fetch, no new endpoints → Task 3 Step 1 (same `/actions/runs` call). ✓
- Categorize main/lastPR/running, failure detection, isClean → Task 2 (`health`, `RunRef`, `RepoCIHealth`) + tests. ✓
- main = push@main newest; lastPR = newest pull_request; running = any in-progress → Task 2 `health()` + tests. ✓
- Rename panel → CI Health (`ciHealth`) → Task 3 Steps 2-3. ✓
- RUNNING + NEEDS ATTENTION sections + green line + click-through + no-token state → Task 3 Step 4 (`CIHealthPanel`). ✓
- 120s → 60s → Task 3 Step 1 (`start(interval: 60)`). ✓
- PR context = head branch → `contextLabel` / `which: "PR " + context`. ✓
- Tests for categorization → Task 2. ✓
- `CockpitLayout` invariant test still passes (kind renamed, still placed once) → no test change needed. ✓

**Placeholder scan:** none — every step has full code/commands and expected output.

**Type consistency:** `RepoCIHealth` (`repo`, `main`, `lastPR`, `running`, `shortName`, `isClean`), `RunRef` (`runID`, `title`, `context`, `conclusion`, `status`, `startedAt`, `htmlURL`, `isRunning`, `isFailed`), `PortfolioCIMapping.health(repo:runs:)`, `PortfolioCIService.health`, kind `.ciHealth`, `CIHealthPanel` — names match across Tasks 2-4. The service property renamed `statuses` → `health`; the panel reads `service.health`. ✓
