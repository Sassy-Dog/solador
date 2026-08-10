import SwiftData
import SwiftUI

/// Settings tab to manage the tracked portfolio repos that drive the GitHub
/// Workflows, GitHub Runners, and Git/Worktrees panels. Repos persist as `TrackedRepo`;
/// edits go through `PortfolioStore`, which republishes the live set and refreshes
/// the dependent panels immediately.
struct PortfolioSettingsView: View {
    @EnvironmentObject private var store: PortfolioStore
    @Query(sort: \TrackedRepo.slug) private var repos: [TrackedRepo]

    @State private var newSlug = ""
    @State private var statusMessage: String?

    var body: some View {
        Form {
            Section {
                if repos.isEmpty {
                    Text("No tracked repos yet. Add one below as owner/name.")
                        .font(.caption).foregroundStyle(.secondary)
                } else {
                    ForEach(repos) { repo in repoRow(repo) }
                }
            } header: {
                Text("Tracked Repos").font(.headline)
            }

            Section {
                HStack {
                    TextField("owner/name (e.g. acme/gadget)", text: $newSlug)
                        .onSubmit { add() }
                    Button("Add") { add() }
                        .disabled(!newSlug.contains("/"))
                }
                if let statusMessage {
                    Text(statusMessage).font(.caption).foregroundStyle(.secondary)
                }
                Text("Drives GitHub Workflows, GitHub Runners, and Git/Worktrees. Disabled repos stay in the list but are skipped. Changes apply at the next refresh. Watched workflows: leave blank for the default ci.yml view, or list extra workflows (e.g. release.yml) whose failures should redden the panel — matched by display name or filename, case-insensitive.")
                    .font(.caption).foregroundStyle(.secondary)
            } header: {
                Text("Add Repo").font(.headline)
            }
        }
        .formStyle(.grouped)
        .padding()
    }

    private func repoRow(_ repo: TrackedRepo) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(repo.slug)
                    .font(.body.monospaced())
                    .foregroundStyle(repo.enabled ? .primary : .secondary)
                Spacer()
                Toggle("", isOn: enabledBinding(repo)).labelsHidden()
                Button(role: .destructive) { store.remove(repo) } label: {
                    Image(systemName: "trash")
                }
                .buttonStyle(.borderless)
            }
            TextField("Watched workflows (comma-separated, e.g. release.yml)", text: watchedBinding(repo))
                .textFieldStyle(.roundedBorder)
                .font(.caption.monospaced())
                .disabled(!repo.enabled)
        }
        .padding(.vertical, 2)
    }

    private func enabledBinding(_ repo: TrackedRepo) -> Binding<Bool> {
        Binding(
            get: { repo.enabled },
            set: { store.setEnabled($0, for: repo) }
        )
    }

    /// Edits the per-repo watched-workflow list as a comma-separated string.
    /// Empty preserves today's push+PR view; add e.g. `release.yml` to surface a
    /// release-pipeline failure on the health panel (issue #76).
    private func watchedBinding(_ repo: TrackedRepo) -> Binding<String> {
        Binding(
            get: { (repo.watchedWorkflows ?? []).joined(separator: ", ") },
            set: { newValue in
                let parsed = newValue
                    .split(whereSeparator: { $0 == "," || $0 == "\n" })
                    .map { $0.trimmingCharacters(in: .whitespaces) }
                    .filter { !$0.isEmpty }
                store.setWatchedWorkflows(parsed, for: repo)
            }
        )
    }

    private func add() {
        let slug = newSlug.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !slug.isEmpty else { return }
        if store.add(slug: slug) != nil {
            statusMessage = "Added \(slug)."
            newSlug = ""
        } else {
            statusMessage = "Skipped — invalid or already tracked."
        }
    }
}
