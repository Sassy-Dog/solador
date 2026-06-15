import SwiftUI
import SwiftData

/// Settings tab to manage the tracked portfolio repos that drive the Portfolio
/// CI, CI Runners, and Git/Worktrees panels. Repos persist as `TrackedRepo`;
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
                    TextField("owner/name (e.g. Sassy-Dog/velovate)", text: $newSlug)
                        .onSubmit { add() }
                    Button("Add") { add() }
                        .disabled(!newSlug.contains("/"))
                }
                if let statusMessage {
                    Text(statusMessage).font(.caption).foregroundStyle(.secondary)
                }
                Text("Drives Portfolio CI, CI Runners, and Git/Worktrees. Disabled repos stay in the list but are skipped. Changes apply at the next refresh.")
                    .font(.caption).foregroundStyle(.secondary)
            } header: {
                Text("Add Repo").font(.headline)
            }
        }
        .formStyle(.grouped)
        .padding()
    }

    private func repoRow(_ repo: TrackedRepo) -> some View {
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
    }

    private func enabledBinding(_ repo: TrackedRepo) -> Binding<Bool> {
        Binding(
            get: { repo.enabled },
            set: { store.setEnabled($0, for: repo) }
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
