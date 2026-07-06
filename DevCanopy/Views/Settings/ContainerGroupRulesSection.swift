import SwiftData
import SwiftUI

/// Settings editor for the container group rules consumed by the Containers panel.
/// Rules persist as JSON under the UserDefaults key "containerGroupRules"; the panel
/// observes the same key via `@AppStorage`, so edits apply live. A fresh install
/// shows the seeded defaults; the first edit persists whatever is on screen,
/// including a deliberately emptied list.
struct ContainerGroupRulesSection: View {
    @AppStorage("containerGroupRules") private var groupRulesData = Data()
    @Query(sort: \MonitoredHost.name) private var monitoredHosts: [MonitoredHost]

    var body: some View {
        let rules = ContainerGroupRule.load(from: groupRulesData)
        Section {
            ForEach(rules) { rule in
                ruleRow(rule, in: rules)
            }
            Button("Add Rule") {
                save(rules + [ContainerGroupRule(pattern: "", label: "")])
            }
            Text("Collapse folds matching containers into one \u{201C}label ×N\u{201D} row on the Containers panel; Hide removes their rows entirely (they still count in the header rollup). A rule only applies on its selected host, and a scoped Collapse rule shows a standing ×0 row there even with no matches. `*` matches any run of characters; everything else is literal and case-sensitive.")
                .font(.caption).foregroundStyle(.secondary)
        } header: {
            Text("Container Group Rules").font(.headline)
        }
    }

    /// Labels are hidden so the prompt renders inside the field — a titled TextField
    /// in a grouped Form shows its title as a leading label column, which wraps at
    /// settings-window widths.
    private func ruleRow(_ rule: ContainerGroupRule, in rules: [ContainerGroupRule]) -> some View {
        HStack {
            Picker("Action", selection: binding(for: rule.id, keyPath: \.action, default: .collapse)) {
                Text("Collapse").tag(ContainerRuleAction.collapse)
                Text("Hide").tag(ContainerRuleAction.hide)
            }
            .labelsHidden()
            .fixedSize()
            TextField("Pattern", text: binding(for: rule.id, keyPath: \.pattern, default: ""), prompt: Text("api-*"))
                .labelsHidden()
                .font(.body.monospaced())
            if rule.action == .collapse {
                Image(systemName: "arrow.right").font(.caption).foregroundStyle(.secondary)
                TextField(
                    "Group label",
                    text: binding(for: rule.id, keyPath: \.label, default: ""),
                    prompt: Text("group label")
                )
                .labelsHidden()
            }
            Picker("Host", selection: binding(for: rule.id, keyPath: \.host, default: nil)) {
                Text("All hosts").tag(String?.none)
                ForEach(hostScopeOptions(current: rule.host), id: \.self) { name in
                    Text(name).tag(String?.some(name))
                }
            }
            .labelsHidden()
            .fixedSize()
            Button(role: .destructive) {
                save(rules.filter { $0.id != rule.id })
            } label: {
                Image(systemName: "trash")
            }
            .buttonStyle(.borderless)
        }
    }

    /// Re-reads the persisted list on every access rather than capturing a snapshot,
    /// so concurrent edits (two fields of the same rule) can't clobber each other.
    private func binding<Value>(
        for id: UUID,
        keyPath: WritableKeyPath<ContainerGroupRule, Value>,
        default defaultValue: Value
    ) -> Binding<Value> {
        Binding(
            get: {
                ContainerGroupRule.load(from: groupRulesData)
                    .first(where: { $0.id == id })?[keyPath: keyPath] ?? defaultValue
            },
            set: { newValue in
                var updated = ContainerGroupRule.load(from: groupRulesData)
                guard let index = updated.firstIndex(where: { $0.id == id }) else { return }
                updated[index][keyPath: keyPath] = newValue
                save(updated)
            }
        )
    }

    /// "this machine" + every monitored host — plus the rule's current scope if it
    /// names a host that no longer exists, so the picker never shows blank.
    private func hostScopeOptions(current: String?) -> [String] {
        var options = [ContainerGroupRule.localHostScope] + monitoredHosts.map(\.name)
        if let current, !options.contains(current) {
            options.append(current)
        }
        return options
    }

    private func save(_ rules: [ContainerGroupRule]) {
        groupRulesData = ContainerGroupRule.encode(rules)
    }
}
