//! The Settings surface: the view-model the frontend paints, and the pure
//! rules behind every mutation it can make.
//!
//! Same discipline as the cockpit — every string the frontend shows is made
//! here, in Rust, so a label cannot drift between the two apps without a test
//! noticing. Ground truth is `DevCanopy/Views/Settings/` (`SettingsView.swift`,
//! `HostsSettingsView.swift`, `PortfolioSettingsView.swift`) plus
//! `Models/RefreshInterval.swift` and `Views/Cockpit/CockpitBreakpoints.swift`
//! for the two pickers' display names.
//!
//! Nothing in this module reads, writes, formats or returns a secret **value**.
//! Credentials travel between the frontend and `store::CredentialStore` and
//! stop there; what the view-model carries is a boolean per credential — that
//! is what the "stored" badge is drawn from.

use std::collections::HashSet;

use agentclient::AgentError;
use serde_json::{json, Value};
use store::settings::{CORE_ROW_SPAN_RANGE, REFRESH_INTERVAL_CHOICES};
use store::{
    ContainerGroupRule, ContainerRuleAction, Host, HostOverflowMode, SecretKey, Settings,
    TrackedRepo, DEFAULT_AGENT_PORT, LOCAL_HOST_SCOPE,
};
use uuid::Uuid;
use viewmodel::color;

use crate::openclaw;

/// The app's marketing version.
///
/// Hard-coded via the crate version (`app/src-tauri/Cargo.toml`, mirrored in
/// `tauri.conf.json`) rather than derived from git the way the Swift app's is
/// (`Scripts/get-version-info.sh`, CalVer per `Docs/VERSIONING.md`). Wiring the
/// shell into that derivation is [#15]; until then this is a deliberate,
/// documented placeholder rather than a number pretending to be a release.
///
/// [#15]: https://github.com/Sassy-Dog/devcanopy/issues/15
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The label on the button that opens this surface.
///
/// Lives here rather than in the cockpit payload's literal because both
/// payloads carry it: the cockpit needs it before Settings has ever been
/// opened, and the Settings view renders it as its own title.
pub const OPEN_LABEL: &str = "Settings";

/// Which credentials currently hold a value.
///
/// Booleans only, by construction: the badge needs "is something stored", and
/// a struct that could carry the value is a struct a value can leak out of.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StoredSecrets {
    pub github: bool,
    pub neon: bool,
    pub sentry: bool,
    pub azure: bool,
    /// Whether an OpenClaw *bearer* token is stored. The device key is not
    /// represented here at all: it is minted by the app, and whether one exists
    /// is answered by the device id the Device Pairing block shows.
    pub openclaw: bool,
    /// Host ids with a non-empty agent token.
    pub hosts: HashSet<Uuid>,
}

/// The credentials the Settings surface can write, as the frontend names them.
///
/// A closed set mapped to [`SecretKey`], so an unknown key from the webview is
/// a rejected command rather than a silently-ignored save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretField {
    GitHub,
    Neon,
    Sentry,
    Azure,
    /// The OpenClaw gateway's *bearer* token, which is optional — most gateways
    /// authenticate by device pairing instead. Deliberately not the device key:
    /// that is 32 raw bytes minted by this app, never typed by a human, and it
    /// has no field on this surface for exactly that reason.
    OpenClaw,
}

impl SecretField {
    /// The identifier the payload carries and the frontend sends back.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            SecretField::GitHub => "github",
            SecretField::Neon => "neon",
            SecretField::Sentry => "sentry",
            SecretField::Azure => "azure",
            SecretField::OpenClaw => "openclaw",
        }
    }

    /// The credential-store key this field writes.
    #[must_use]
    pub const fn key(self) -> SecretKey {
        match self {
            SecretField::GitHub => SecretKey::GitHubAccessToken,
            SecretField::Neon => SecretKey::NeonApiKey,
            SecretField::Sentry => SecretKey::SentryUsageToken,
            SecretField::Azure => SecretKey::AzureCostSasUrl,
            SecretField::OpenClaw => SecretKey::OpenClawBearerToken,
        }
    }

    /// Parses the identifier the frontend sent.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        [
            SecretField::GitHub,
            SecretField::Neon,
            SecretField::Sentry,
            SecretField::Azure,
            SecretField::OpenClaw,
        ]
        .into_iter()
        .find(|field| field.id() == raw)
    }
}

/// `RefreshInterval.displayName` (Swift).
#[must_use]
pub fn refresh_interval_label(secs: u32) -> String {
    match secs {
        30 => "30 seconds".to_owned(),
        60 => "1 minute".to_owned(),
        300 => "5 minutes".to_owned(),
        // Unreachable through the picker (the options come from
        // REFRESH_INTERVAL_CHOICES), and still rendered as the real number
        // rather than a default, so a store edited by hand reads as what it is.
        other => format!("{other} seconds"),
    }
}

/// `HostOverflowMode.displayName` (Swift).
#[must_use]
pub const fn host_overflow_label(mode: HostOverflowMode) -> &'static str {
    match mode {
        HostOverflowMode::Stack => "Stack vertically",
        HostOverflowMode::Tabs => "Show as tabs",
    }
}

/// The Settings "Test" button's result line, byte-for-byte the Swift strings
/// in `HostsSettingsView.test(_:)`.
///
/// The five failure/success shapes are the whole diagnostic value of the
/// button: a 401 and an unreachable host send an operator to different places,
/// and one generic "test failed" told them neither.
#[must_use]
pub fn health_result(result: &Result<wire::Health, AgentError>) -> String {
    match result {
        Ok(info) => {
            let mut line = format!("✓ {} · agent v{}", info.hostname, info.version);
            if info.sampler_stale == Some(true) {
                line.push_str(" · sampler stale");
            }
            line
        }
        Err(AgentError::AuthFailed) => "✗ auth failed (401) — check token".to_owned(),
        Err(AgentError::DecodeFailed(_)) => "✗ decode failed — agent/app version skew?".to_owned(),
        Err(AgentError::HttpStatus(code)) => format!("✗ HTTP {code}"),
        Err(AgentError::Unreachable(_)) => "✗ unreachable — host down or agent stopped".to_owned(),
    }
}

/// The Add-Host form's port field, with Swift's `Int(newPort) ?? 7878`
/// tolerance: an unparseable port is the default, not a rejected form.
#[must_use]
pub fn parse_port(raw: &str) -> u16 {
    raw.trim().parse().unwrap_or(DEFAULT_AGENT_PORT)
}

/// Whether a repo slug is addable, per `PortfolioStore.add(slug:)`.
///
/// Returns the trimmed slug. The duplicate check is case-insensitive because
/// GitHub's own is: `Sassy-Dog/velovate` and `sassy-dog/velovate` are one repo,
/// and tracking both would double every count it feeds.
#[must_use]
pub fn validated_slug(raw: &str, existing: &[TrackedRepo]) -> Option<String> {
    let slug = raw.trim();
    if !slug.contains('/') || slug.starts_with('/') || slug.ends_with('/') {
        return None;
    }
    if existing
        .iter()
        .any(|repo| repo.slug.eq_ignore_ascii_case(slug))
    {
        return None;
    }
    Some(slug.to_owned())
}

/// The comma-separated watched-workflow field, parsed the way
/// `PortfolioSettingsView.watchedBinding` parses it: split on commas and
/// newlines, trim, drop blanks. An empty result is `None` (the default
/// push+PR view), not `Some(vec![])`.
#[must_use]
pub fn parse_workflows(text: &str) -> Option<Vec<String>> {
    let parsed: Vec<String> = text
        .split([',', '\n'])
        .map(|part| part.trim().to_owned())
        .filter(|part| !part.is_empty())
        .collect();
    (!parsed.is_empty()).then_some(parsed)
}

/// The watched-workflow list as the text field shows it.
#[must_use]
pub fn workflows_text(repo: &TrackedRepo) -> String {
    repo.watched_workflows
        .as_deref()
        .unwrap_or_default()
        .join(", ")
}

/// `ContainerRuleAction`'s picker labels (Swift's
/// `ContainerGroupRulesSection.ruleRow`).
#[must_use]
pub const fn rule_action_label(action: ContainerRuleAction) -> &'static str {
    match action {
        ContainerRuleAction::Collapse => "Collapse",
        ContainerRuleAction::Hide => "Hide",
        ContainerRuleAction::Expect => "Expect",
    }
}

/// One editable field of a container group rule — the Rust counterpart of the
/// `WritableKeyPath`s Swift's bindings are built over.
///
/// A closed set, mapped from the identifier the frontend sends, so an unknown
/// field is a rejected command rather than a silently-ignored edit. Same rule
/// as [`SecretField`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleField {
    Action,
    Pattern,
    Label,
    /// Collapse only: how many matches there should be.
    Expected,
    /// The host section the rule applies to. Empty string = every host.
    Host,
}

impl RuleField {
    /// Every field, so the payload and the parser cannot drift apart.
    pub const ALL: [RuleField; 5] = [
        RuleField::Action,
        RuleField::Pattern,
        RuleField::Label,
        RuleField::Expected,
        RuleField::Host,
    ];

    /// The identifier the payload carries and the frontend sends back.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            RuleField::Action => "action",
            RuleField::Pattern => "pattern",
            RuleField::Label => "label",
            RuleField::Expected => "expected",
            RuleField::Host => "host",
        }
    }

    /// Parses the identifier the frontend sent.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        RuleField::ALL.into_iter().find(|field| field.id() == raw)
    }
}

/// The expected-count field's `String -> Option<u32>` shim.
///
/// Swift's `expectedCountBinding` in one expression, and the rule it encodes is
/// the point: **empty, non-numeric, zero or negative input clears the
/// expectation** rather than coercing to `0`. An expectation of zero is no
/// expectation, and the panel must not render `×0/0` — a fabricated number the
/// operator never typed. (`parse::<u32>` rejects `-1` and an overflowing
/// figure outright, where Swift's `Int` parses them and the `> 0` guard then
/// clears them; both arrive at `None`.)
#[must_use]
pub fn parse_expected_count(raw: &str) -> Option<u32> {
    raw.trim().parse::<u32>().ok().filter(|count| *count > 0)
}

/// A blank rule, as **Add Rule** appends it.
///
/// `Collapse` with an empty pattern and label, matching Swift's
/// `ContainerGroupRule(pattern: "", label: "")` — an empty pattern matches only
/// the empty name, so a half-filled row cannot start hiding containers before
/// the operator has finished typing.
#[must_use]
pub fn new_rule() -> ContainerGroupRule {
    ContainerGroupRule::new("", "", ContainerRuleAction::Collapse)
}

/// Writes one field of one rule, in place.
///
/// The pure half of the concurrent-edit guard: the caller re-reads the
/// persisted list, this writes **one** field into it, and the caller writes the
/// whole list back. That is Swift's per-`keyPath` binding, which re-reads
/// `groupRulesData` on every access precisely so editing a rule's label cannot
/// clobber the pattern someone changed a moment earlier — a whole-row write
/// from a captured snapshot would.
///
/// Returns `false` when the edit addressed nothing (an index no longer in the
/// list, or an action string no picker can produce), and leaves `rules`
/// untouched — the counterpart of Swift's `guard let index … else { return }`.
#[must_use]
pub fn apply_rule_edit(
    rules: &mut [ContainerGroupRule],
    index: usize,
    field: RuleField,
    value: &str,
) -> bool {
    let Some(rule) = rules.get_mut(index) else {
        return false;
    };
    match field {
        RuleField::Action => match ContainerRuleAction::parse(value) {
            Some(action) => rule.action = action,
            None => return false,
        },
        RuleField::Pattern => rule.pattern = value.to_owned(),
        RuleField::Label => rule.label = value.to_owned(),
        RuleField::Expected => rule.expected_count = parse_expected_count(value),
        // The picker's "All hosts" is the empty string on the wire, because
        // `null` and `""` both survive a JSON round-trip but only one of them
        // survives a `<select>`'s value.
        RuleField::Host => {
            rule.host = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            }
        }
    }
    true
}

/// The General tab's three values, laundered through the same rules the store
/// enforces on deserialize: an unoffered cadence reads as the default, a
/// row span outside 1–4 is clamped, an unknown overflow mode is `stack`.
///
/// Applied on the way *in* as well as on the way out so a webview that sends a
/// value no picker can produce cannot write one to disk either.
#[must_use]
pub fn normalized_general(refresh_interval_secs: u32, core_row_span: u8, mode: &str) -> Settings {
    Settings {
        refresh_interval_secs: if REFRESH_INTERVAL_CHOICES.contains(&refresh_interval_secs) {
            refresh_interval_secs
        } else {
            store::settings::DEFAULT_REFRESH_INTERVAL_SECS
        },
        core_row_span: core_row_span
            .clamp(*CORE_ROW_SPAN_RANGE.start(), *CORE_ROW_SPAN_RANGE.end()),
        host_overflow_mode: HostOverflowMode::from(mode.to_owned()),
        ..Settings::default()
    }
}

/// The whole Settings payload.
///
/// A pure function of the store's three sections plus the credential badges —
/// no `Store`, no keyring, no I/O — so it is unit-testable and dumpable as a
/// fixture (`--dump-settings`) without a store file or a keychain prompt.
#[must_use]
pub fn view(
    settings: &Settings,
    hosts: &[Host],
    repos: &[TrackedRepo],
    rules: &[ContainerGroupRule],
    stored: &StoredSecrets,
    openclaw: &openclaw::SettingsFacts,
) -> Value {
    json!({
        "title": OPEN_LABEL,
        "openLabel": OPEN_LABEL,
        "closeLabel": "Done",
        // The Swift window's tab order.
        "tabs": [
            { "id": "general", "title": "General" },
            { "id": "github", "title": "GitHub" },
            { "id": "portfolio", "title": "Portfolio" },
            { "id": "hosts", "title": "Hosts" },
            { "id": "azure", "title": "Azure Cost" },
            { "id": "usage", "title": "Usage" },
            { "id": "openclaw", "title": "OpenClaw" },
            { "id": "about", "title": "About" },
        ],
        "general": general_tab(settings),
        "github": github_tab(stored),
        "portfolio": portfolio_tab(repos),
        "hosts": hosts_tab(settings, hosts, rules, stored),
        "azure": azure_tab(settings, stored),
        "usage": usage_tab(settings, stored),
        "openclaw": openclaw_tab(settings, stored, openclaw),
        "about": about_tab(),
    })
}

fn general_tab(settings: &Settings) -> Value {
    let overflow_options: Vec<Value> = [HostOverflowMode::Stack, HostOverflowMode::Tabs]
        .into_iter()
        .map(|mode| json!({ "value": mode.as_str(), "label": host_overflow_label(mode) }))
        .collect();
    json!({
        "heading": "General Settings",
        "refreshInterval": {
            "label": "Refresh Interval",
            "value": settings.refresh_interval_secs,
            "options": REFRESH_INTERVAL_CHOICES
                .iter()
                .map(|secs| json!({ "value": secs, "label": refresh_interval_label(*secs) }))
                .collect::<Vec<_>>(),
            // Honest about the gap rather than silent: the shell polls every
            // host once a second because one history sample is one fixed time
            // slice (see POLL_INTERVAL), and it has none of the periodic
            // services this cadence governs in the Swift app. The preference
            // is stored for parity; nothing here reads it yet.
            "help": "Cadence for the periodic services. Host metrics poll every second regardless — that cadence is the charts' time axis.",
        },
        "coreRowSpan": {
            "label": "CPU core rows",
            "value": settings.core_row_span,
            "min": CORE_ROW_SPAN_RANGE.start(),
            "max": CORE_ROW_SPAN_RANGE.end(),
            "help": "How many rows tall the per-core CPU grid is on every host card.",
        },
        "hostOverflow": {
            "label": "When host cards don't fit",
            "value": settings.host_overflow_mode.as_str(),
            "options": overflow_options,
            "help": "Stacking keeps every host visible; tabs keep the cockpit short.",
        },
        "saveLabel": "Apply",
    })
}

/// The shared shape of every credential section: one field, Save, Clear, and a
/// badge that says whether something is stored — never what.
fn secret_section(field: SecretField, label: &str, stored: bool, badge: &str, help: &str) -> Value {
    json!({
        "key": field.id(),
        "fieldLabel": label,
        "saveLabel": "Save",
        "clearLabel": "Clear",
        "storedLabel": badge,
        "stored": stored,
        "help": help,
    })
}

fn github_tab(stored: &StoredSecrets) -> Value {
    json!({
        "heading": "GitHub Token",
        "secret": secret_section(
            SecretField::GitHub,
            "Fine-grained PAT",
            stored.github,
            "Token stored",
            "Used by the Repos panel. Grant the fine-grained PAT read access to Actions (workflow runs), Contents (remote branch counts), Issues (open-issue counts), and Pull requests (open-PR counts). Stored in your OS credential store.",
        ),
    })
}

fn portfolio_tab(repos: &[TrackedRepo]) -> Value {
    json!({
        "heading": "Tracked Repos",
        "empty": "No tracked repos yet. Add one below as owner/name.",
        "workflowsLabel": "Watched workflows (comma-separated, e.g. release.yml)",
        "deleteLabel": "Delete",
        "rows": repos
            .iter()
            .map(|repo| json!({
                "slug": repo.slug,
                "enabled": repo.enabled,
                "workflows": workflows_text(repo),
            }))
            .collect::<Vec<_>>(),
        "add": {
            "heading": "Add Repo",
            "slugLabel": "owner/name (e.g. Sassy-Dog/velovate)",
            "buttonLabel": "Add",
            "help": "Drives the Repos and GitHub Runners panels. Disabled repos stay in the list but are skipped. Watched workflows: leave blank for the default ci.yml view, or list extra workflows (e.g. release.yml) whose failures should redden the panel — matched by display name or filename, case-insensitive.",
        },
    })
}

/// The host-scope picker's options for one rule: "All hosts", "this machine",
/// every stored host by name — and the rule's own scope when it names a host
/// that no longer exists.
///
/// That last case is `hostScopeOptions(current:)`'s whole reason to exist: a
/// picker whose selection is absent from its options renders **blank**, so a
/// rule scoped to a host you removed would read as unscoped while still
/// matching nothing. Ordered by name, matching the Swift view's
/// `@Query(sort: \MonitoredHost.name)`.
fn rule_host_options(hosts: &[Host], current: Option<&str>) -> Vec<Value> {
    let mut names: Vec<String> = hosts.iter().map(|host| host.name.clone()).collect();
    names.sort();
    let mut options = vec![LOCAL_HOST_SCOPE.to_owned()];
    options.extend(names);
    if let Some(current) = current {
        if !options.iter().any(|option| option == current) {
            options.push(current.to_owned());
        }
    }
    // "All hosts" leads, and its value is the empty string — see `RuleField::Host`.
    std::iter::once(json!({ "value": "", "label": "All hosts" }))
        .chain(
            options
                .into_iter()
                .map(|name| json!({ "value": name, "label": name })),
        )
        .collect()
}

/// The Container Group Rules editor.
///
/// Rows are addressed by **index**, not by an id: the persisted rule model has
/// none (order is the contract — matching is first-match-wins), and every
/// mutation is a read-modify-write of the whole list under the store's lock, so
/// the index a row was rendered with addresses the same rule the frontend is
/// looking at. A row whose index no longer exists is a rejected edit, not a
/// misdirected one — see [`apply_rule_edit`].
fn rules_section(rules: &[ContainerGroupRule], hosts: &[Host]) -> Value {
    json!({
        "heading": "Container Group Rules",
        "addLabel": "Add Rule",
        "deleteLabel": "Delete",
        "actionLabel": "Action",
        "actions": ContainerRuleAction::ALL
            .into_iter()
            .map(|action| json!({ "value": action.as_str(), "label": rule_action_label(action) }))
            .collect::<Vec<_>>(),
        "patternLabel": "Pattern",
        "patternPrompt": "api-*",
        "labelLabel": "Group label",
        "labelPrompt": "group label",
        "expectedLabel": "Expected count",
        "expectedPrompt": "expected ×",
        // The separator Swift draws as an `arrow.right` SF Symbol between the
        // pattern and what it collapses into. A glyph the frontend picked would
        // be a string this file did not make.
        "arrow": "→",
        "hostLabel": "Host",
        "rows": rules
            .iter()
            .enumerate()
            .map(|(index, rule)| json!({
                "index": index,
                "action": rule.action.as_str(),
                "pattern": rule.pattern,
                "label": rule.label,
                // A string, not a number: the field's empty state is "no
                // expectation", and a `0` here is exactly the fabricated
                // number `parse_expected_count` exists to refuse.
                "expected": rule.expected_count.map(|n| n.to_string()).unwrap_or_default(),
                "host": rule.host.clone().unwrap_or_default(),
                // Only a Collapse rule has an aggregate to label or count.
                // Hide renders no row at all, and Expect's row is the entity's
                // own name.
                "collapseOnly": rule.action == ContainerRuleAction::Collapse,
                "hostOptions": rule_host_options(hosts, rule.host.as_deref()),
            }))
            .collect::<Vec<_>>(),
        "help": "Collapse folds matching containers into one \u{201C}label ×N\u{201D} row on the Containers panel; Hide removes their rows entirely (they still count in the header rollup); Expect keeps a standing presence row for matching names — amber while briefly absent (recycling), red once absent past 5 minutes (missing). Expect globs track names actually observed at least once; only an exact name alarms without ever being seen. A Collapse rule's expected count renders ×matched/expected and warns amber when short. A rule only applies on its selected host, and a scoped Collapse rule shows a standing ×0 row there even with no matches. `*` matches any run of characters; everything else is literal and case-sensitive.",
    })
}

fn hosts_tab(
    settings: &Settings,
    hosts: &[Host],
    rules: &[ContainerGroupRule],
    stored: &StoredSecrets,
) -> Value {
    json!({
        "heading": "Remote Hosts",
        "empty": "No remote hosts yet. Add one below, then it appears in the cockpit.",
        "testLabel": "Test",
        "testingLabel": "Testing…",
        "deleteLabel": "Delete",
        "unhideLabel": "Unhide",
        "tokenStoredLabel": "Token stored",
        "noTokenLabel": "No token",
        "rows": hosts
            .iter()
            .map(|host| json!({
                "id": host.id.to_string(),
                "name": host.name,
                "endpoint": format!("{}:{}", host.address, host.port),
                "enabled": host.enabled,
                "tokenStored": stored.hosts.contains(&host.id),
                "hiddenVolumes": host.hidden_volume_mounts,
            }))
            .collect::<Vec<_>>(),
        // Rendered only when it has entries. This shell has no local-machine
        // collector (HostMetricsKit is Swift-only), so nothing here can *add*
        // a mount — the section exists so a list written by a future local
        // collector, or by hand, can still be undone.
        "localHidden": {
            "heading": "Hidden Volumes — this machine",
            "mounts": settings.local_hidden_volume_mounts,
        },
        "add": {
            "heading": "Add Host",
            "nameLabel": "Name (e.g. ubu-3xdv)",
            "addressLabel": "Address (Tailscale IP or MagicDNS name)",
            "portLabel": "Port",
            "portDefault": DEFAULT_AGENT_PORT.to_string(),
            "tokenLabel": "Agent token",
            "buttonLabel": "Add Host",
            "help": "The agent serves metrics on the host's tailnet address. The token is stored in your OS credential store, never in the settings file.",
        },
        // Same tab as Swift's, and for the same reason: the rules are scoped by
        // host, so the picker that names one belongs beside the list that
        // defines them.
        "rules": rules_section(rules, hosts),
    })
}

fn azure_tab(settings: &Settings, stored: &StoredSecrets) -> Value {
    json!({
        "heading": "Azure Cost",
        "budget": {
            "heading": "Budget",
            "label": "Monthly budget (USD)",
            "value": settings.azure_monthly_budget_usd,
            "help": "Powers the projected-vs-budget bar on the Azure Cost panel. Leave at 0 to hide the bar.",
            "saveLabel": "Apply",
        },
        "secretHeading": "Azure Cost SAS",
        "secret": secret_section(
            SecretField::Azure,
            "SAS URL",
            stored.azure,
            "SAS stored",
            "Used by the Azure Cost panel. Paste a container-scoped, read+list SAS URL for the cost-exports container on stsassydog. The SAS is the only credential — no Azure sign-in. Stored in your OS credential store.",
        ),
    })
}

fn usage_tab(settings: &Settings, stored: &StoredSecrets) -> Value {
    json!({
        "heading": "Usage",
        "saveLabel": "Apply",
        "neon": {
            "heading": "Neon",
            "orgIdLabel": "Organization ID",
            "orgId": settings.neon_org_id,
            "secret": secret_section(
                SecretField::Neon,
                "Organization API key",
                stored.neon,
                "Key stored",
                "Powers the Neon rows on the Usage panel (month-to-date compute and branch storage). Create an organization API key in the Neon console — it is scoped to the org's projects and, unlike a personal key, isn't tied to one user account. Stored in your OS credential store; the org ID is not a secret and is stored as a normal preference.",
            ),
        },
        "sentry": {
            "heading": "Sentry",
            "orgSlugLabel": "Organization slug",
            "orgSlug": settings.sentry_org_slug,
            "quotaLabel": "Monthly error quota (events)",
            "quota": settings.sentry_monthly_event_quota,
            "quotaHelp": "Leave the quota at 0 to hide the quota bar.",
            "secret": secret_section(
                SecretField::Sentry,
                "API token",
                stored.sentry,
                "Token stored",
                "Powers the Sentry row on the Usage panel (accepted error events over the last 30 days). Create a personal token under User settings → Auth Tokens, or an internal-integration token, with the read-only org:read scope — organization auth tokens carry a fixed CI-oriented scope set that doesn't include it. Stored in your OS credential store; the org slug and quota are not secrets and are stored as normal preferences.",
            ),
        },
    })
}

/// The Device Pairing block's status row: one word for the connection, plus the
/// colour that word is worth.
///
/// Port of `OpenClawSettingsView.connectionRow`, including the one place it
/// disagrees with the panel: a disconnect is **amber** here and red there. The
/// panel is a glance across the whole cockpit, where red means "go look"; this
/// row is what you see *after* you have gone and looked, on the screen where
/// you fix it, and shouting there adds nothing.
#[must_use]
pub fn openclaw_status(connection: &openclaw::RuntimeConnectionState) -> (String, u32) {
    use openclaw::RuntimeConnectionState as State;
    match connection {
        State::Connected => ("Connected".to_owned(), color::GREEN),
        State::Connecting => ("Connecting…".to_owned(), color::AMBER),
        State::Idle => ("Idle".to_owned(), color::MUTED),
        State::Disconnected { reason } => (reason.clone(), color::AMBER),
    }
}

/// The OpenClaw tab: the gateway URL, the optional bearer token, and the device
/// pairing block.
///
/// The pairing block is the reason this tab is worth having a live half at all.
/// Without it an operator is told the gateway rejected them and left to find the
/// device id and the approve command themselves — which is the fingerprint of
/// the very key this app minted and never showed anyone.
fn openclaw_tab(
    settings: &Settings,
    stored: &StoredSecrets,
    facts: &openclaw::SettingsFacts,
) -> Value {
    let (status_text, status_color) = openclaw_status(&facts.connection);
    json!({
        "heading": "OpenClaw Gateway",
        "gateway": {
            "label": "Gateway URL",
            // The Swift field's placeholder, which doubles as the format hint.
            "placeholder": "ws://host:7878  or  wss://host",
            "value": settings.openclaw_gateway_url,
            "saveLabel": "Save",
        },
        "secret": secret_section(
            SecretField::OpenClaw,
            "Bearer token (optional)",
            stored.openclaw,
            "Token stored",
            "DevCanopy monitors an OpenClaw agent farm over a WebSocket. Most gateways authenticate via device pairing (below); a bearer token is only needed if the gateway requires one. The gateway's controlUi.allowedOrigins must permit this host. Stored in your OS credential store.",
        ),
        "pairingHeading": "Device Pairing",
        "statusLabel": "Status",
        "status": { "text": status_text, "color": color::hex(status_color) },
        "deviceLabel": "Device ID",
        // Null until a key exists. Rendering an empty row instead would claim
        // an identity that has not been minted.
        "deviceId": facts.device_id,
        "noDeviceLabel": "Device key is generated on first connect.",
        "pairing": facts.pairing.as_ref().map(|pairing| json!({
            "explanation": match pairing.kind {
                openclaw::PairingKind::ScopeUpgrade =>
                    "The gateway needs to approve broader scopes for this device.",
                openclaw::PairingKind::FirstPair =>
                    "This device isn't paired yet. Approve it on the gateway host:",
            },
            // The literal line to paste, or nothing — never a command with a
            // placeholder where the request id should be.
            "command": pairing
                .request_id
                .as_ref()
                .map(|request| format!("openclaw devices approve {request}")),
            // The gateway's own words, shown verbatim when it sent any.
            "hint": pairing.remediation_hint,
            "retryLabel": "Retry now",
        })),
    })
}

fn about_tab() -> Value {
    json!({
        "name": "DevCanopy",
        "version": format!("Version {VERSION}"),
        "tagline": "Monitor your development infrastructure",
        "links": [
            { "label": "GitHub Repository", "url": "https://github.com/Sassy-Dog/devcanopy" },
            { "label": "Report an Issue", "url": "https://github.com/Sassy-Dog/devcanopy/issues" },
            { "label": "Documentation", "url": "https://devcanopy.app/docs" },
        ],
        "copyright": "© 2024 Sassy Dog",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::ContainerRuleAction as Action;

    fn health(hostname: &str, version: &str, stale: Option<bool>) -> wire::Health {
        wire::Health {
            status: "ok".to_owned(),
            hostname: hostname.to_owned(),
            version: version.to_owned(),
            sample_age_seconds: Some(2),
            sampler_stale: stale,
        }
    }

    // MARK: the Test button's five result strings

    #[test]
    fn a_healthy_agent_reports_its_hostname_and_version() {
        assert_eq!(
            health_result(&Ok(health("ubu-3xdv", "0.4.0", Some(false)))),
            "✓ ubu-3xdv · agent v0.4.0"
        );
    }

    /// The suffix exists so a redeploy that left the sampler wedged is visible
    /// from Settings — the agent still answers `/v1/health` with a 200 in that
    /// state, so a bare ✓ would call a frozen agent healthy.
    #[test]
    fn a_stale_sampler_is_appended_not_swallowed() {
        assert_eq!(
            health_result(&Ok(health("ubu-3xdv", "0.4.0", Some(true)))),
            "✓ ubu-3xdv · agent v0.4.0 · sampler stale"
        );
        // An agent too old to send the field is not "stale" — it is silent.
        assert_eq!(
            health_result(&Ok(health("ubu-3xdv", "0.3.0", None))),
            "✓ ubu-3xdv · agent v0.3.0"
        );
    }

    #[test]
    fn every_failure_names_the_layer_to_check() {
        assert_eq!(
            health_result(&Err(AgentError::AuthFailed)),
            "✗ auth failed (401) — check token"
        );
        assert_eq!(
            health_result(&Err(AgentError::DecodeFailed("expected `,`".into()))),
            "✗ decode failed — agent/app version skew?"
        );
        assert_eq!(
            health_result(&Err(AgentError::HttpStatus(503))),
            "✗ HTTP 503"
        );
        assert_eq!(
            health_result(&Err(AgentError::HttpStatus(500))),
            "✗ HTTP 500"
        );
        assert_eq!(
            health_result(&Err(AgentError::Unreachable("connection refused".into()))),
            "✗ unreachable — host down or agent stopped"
        );
    }

    /// The transport detail inside `Unreachable`/`DecodeFailed` is diagnostic
    /// noise for this line, and can carry a URL. It must not reach the row.
    #[test]
    fn a_failure_line_never_leaks_the_underlying_error_text() {
        let line = health_result(&Err(AgentError::Unreachable(
            "error sending request for url (http://100.87.202.125:7878/v1/health)".into(),
        )));
        assert!(!line.contains("100.87.202.125"), "{line}");
        assert!(!line.contains("http"), "{line}");
    }

    // MARK: the Add-Host form

    #[test]
    fn a_port_falls_back_to_the_agent_default_rather_than_rejecting_the_form() {
        assert_eq!(parse_port("9000"), 9000);
        assert_eq!(parse_port(" 9000 "), 9000);
        for raw in ["", "seven", "-1", "99999"] {
            assert_eq!(parse_port(raw), DEFAULT_AGENT_PORT, "raw {raw:?}");
        }
    }

    // MARK: the Portfolio form

    #[test]
    fn a_slug_must_look_like_owner_name() {
        let existing = [];
        assert_eq!(
            validated_slug("  Sassy-Dog/velovate  ", &existing).as_deref(),
            Some("Sassy-Dog/velovate")
        );
        for raw in ["velovate", "/velovate", "Sassy-Dog/", "", "   "] {
            assert_eq!(validated_slug(raw, &existing), None, "raw {raw:?}");
        }
    }

    /// Case-insensitive, because GitHub's own identity is: tracking both
    /// spellings would double every count the slug feeds.
    #[test]
    fn a_duplicate_slug_is_rejected_whatever_its_case() {
        let existing = [TrackedRepo::new("Sassy-Dog/velovate")];
        assert_eq!(validated_slug("Sassy-Dog/velovate", &existing), None);
        assert_eq!(validated_slug("sassy-dog/VELOVATE", &existing), None);
        assert_eq!(
            validated_slug("Sassy-Dog/qr-ninja", &existing).as_deref(),
            Some("Sassy-Dog/qr-ninja")
        );
    }

    #[test]
    fn the_watched_workflow_field_round_trips_through_its_text() {
        assert_eq!(
            parse_workflows("release.yml, deploy.yml"),
            Some(vec!["release.yml".to_owned(), "deploy.yml".to_owned()])
        );
        // Blank clears it back to the default push+PR view -- `Some(vec![])`
        // would persist an "empty list" the Swift model spells as `nil`.
        for raw in ["", "   ", ",,", "\n"] {
            assert_eq!(parse_workflows(raw), None, "raw {raw:?}");
        }
        let mut repo = TrackedRepo::new("Sassy-Dog/velovate");
        repo.watched_workflows = parse_workflows(" release.yml ,, deploy.yml\n");
        assert_eq!(workflows_text(&repo), "release.yml, deploy.yml");
        repo.watched_workflows = parse_workflows(&workflows_text(&repo));
        assert_eq!(workflows_text(&repo), "release.yml, deploy.yml");
    }

    // MARK: the General tab

    #[test]
    fn general_values_are_laundered_on_the_way_in_too() {
        let s = normalized_general(45, 9, "carousel");
        assert_eq!(s.refresh_interval_secs, 60);
        assert_eq!(s.core_row_span, 4);
        assert_eq!(s.host_overflow_mode, HostOverflowMode::Stack);

        let s = normalized_general(300, 1, "tabs");
        assert_eq!(s.refresh_interval_secs, 300);
        assert_eq!(s.core_row_span, 1);
        assert_eq!(s.host_overflow_mode, HostOverflowMode::Tabs);
    }

    #[test]
    fn the_picker_labels_match_the_swift_display_names() {
        assert_eq!(refresh_interval_label(30), "30 seconds");
        assert_eq!(refresh_interval_label(60), "1 minute");
        assert_eq!(refresh_interval_label(300), "5 minutes");
        assert_eq!(
            host_overflow_label(HostOverflowMode::Stack),
            "Stack vertically"
        );
        assert_eq!(host_overflow_label(HostOverflowMode::Tabs), "Show as tabs");
    }

    // MARK: the payload

    fn sample() -> (Settings, Vec<Host>, Vec<TrackedRepo>, StoredSecrets) {
        let mut settings = Settings {
            refresh_interval_secs: 300,
            core_row_span: 3,
            host_overflow_mode: HostOverflowMode::Tabs,
            neon_org_id: "org-abc".into(),
            sentry_org_slug: "sassy-dog".into(),
            sentry_monthly_event_quota: 50_000,
            azure_monthly_budget_usd: 250.0,
            ..Settings::default()
        };
        settings.local_hidden_volume_mounts = vec!["/Volumes/Backup".into()];

        let mut host = Host::new("ubu-3xdv", "100.87.202.125");
        host.port = 9000;
        host.hidden_volume_mounts = vec!["/mnt/scratch".into()];
        let mut off = Host::new("mac-mini", "100.64.0.2");
        off.enabled = false;

        let stored = StoredSecrets {
            github: true,
            neon: false,
            sentry: true,
            azure: false,
            openclaw: false,
            hosts: [host.id].into_iter().collect(),
        };
        (settings, vec![host, off], store::seeded_repos(), stored)
    }

    /// The default OpenClaw facts: nothing connected, nothing paired, no key.
    /// Tests that care about a live state build their own.
    fn facts() -> openclaw::SettingsFacts {
        openclaw::SettingsFacts::default()
    }

    /// [`view`] over the seeded container rules — what every test that is not
    /// *about* the rules wants. The rules tests below call `view` directly with
    /// a list of their own.
    fn view_of(
        settings: &Settings,
        hosts: &[Host],
        repos: &[TrackedRepo],
        stored: &StoredSecrets,
        openclaw: &openclaw::SettingsFacts,
    ) -> Value {
        view(
            settings,
            hosts,
            repos,
            &store::seeded_rules(),
            stored,
            openclaw,
        )
    }

    #[test]
    fn the_payload_carries_every_tab_the_window_shows() {
        let (settings, hosts, repos, stored) = sample();
        let vm = view_of(&settings, &hosts, &repos, &stored, &facts());
        let ids: Vec<&str> = vm["tabs"]
            .as_array()
            .expect("tabs")
            .iter()
            .map(|tab| tab["id"].as_str().expect("id"))
            .collect();
        assert_eq!(
            ids,
            vec![
                "general",
                "github",
                "portfolio",
                "hosts",
                "azure",
                "usage",
                "openclaw",
                "about"
            ]
        );
        // Every tab id must address a section of the payload, or the frontend
        // renders a blank pane for a tab that exists.
        for id in ids {
            assert!(!vm[id].is_null(), "tab {id} has no payload section");
        }
    }

    #[test]
    fn the_general_tab_shows_the_stored_values_and_the_offered_choices() {
        let (settings, hosts, repos, stored) = sample();
        let vm = view_of(&settings, &hosts, &repos, &stored, &facts());
        let general = &vm["general"];
        assert_eq!(general["refreshInterval"]["value"], 300);
        assert_eq!(general["coreRowSpan"]["value"], 3);
        assert_eq!(general["hostOverflow"]["value"], "tabs");

        let labels: Vec<&str> = general["refreshInterval"]["options"]
            .as_array()
            .expect("options")
            .iter()
            .map(|o| o["label"].as_str().expect("label"))
            .collect();
        assert_eq!(labels, vec!["30 seconds", "1 minute", "5 minutes"]);
        assert_eq!(general["coreRowSpan"]["min"], 1);
        assert_eq!(general["coreRowSpan"]["max"], 4);
    }

    #[test]
    fn the_hosts_tab_lists_every_host_enabled_or_not_with_its_endpoint() {
        let (settings, hosts, repos, stored) = sample();
        let vm = view_of(&settings, &hosts, &repos, &stored, &facts());
        let rows = vm["hosts"]["rows"].as_array().expect("rows");
        // Settings edits a *configuration*, so a disabled host must still be
        // listed -- the cockpit is what filters on `enabled`.
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["name"], "ubu-3xdv");
        assert_eq!(rows[0]["endpoint"], "100.87.202.125:9000");
        assert_eq!(rows[0]["enabled"], true);
        assert_eq!(rows[0]["tokenStored"], true);
        assert_eq!(rows[0]["hiddenVolumes"][0], "/mnt/scratch");
        assert_eq!(rows[0]["id"], hosts[0].id.to_string());

        assert_eq!(rows[1]["enabled"], false);
        assert_eq!(rows[1]["tokenStored"], false);
        assert_eq!(vm["hosts"]["localHidden"]["mounts"][0], "/Volumes/Backup");
        assert_eq!(vm["hosts"]["add"]["portDefault"], "7878");
    }

    // MARK: the container group rules editor

    /// A rule list covering all three actions, both host-scope shapes and both
    /// sides of the expected-count field.
    fn rules() -> Vec<ContainerGroupRule> {
        let mut collapse =
            ContainerGroupRule::new("api-*", "workflow jobs", Action::Collapse).on_host("ubu-3xdv");
        collapse.expected_count = Some(4);
        vec![
            collapse,
            ContainerGroupRule::new("ghcr.io/*", "", Action::Hide),
            ContainerGroupRule::new("build-vm", "", Action::Expect).on_host(LOCAL_HOST_SCOPE),
        ]
    }

    fn rules_of(vm: &Value) -> &Value {
        &vm["hosts"]["rules"]
    }

    #[test]
    fn the_rules_editor_shows_every_persisted_field_of_every_rule() {
        let (settings, hosts, repos, stored) = sample();
        let vm = view(&settings, &hosts, &repos, &rules(), &stored, &facts());
        let rows = rules_of(&vm)["rows"].as_array().expect("rule rows");
        assert_eq!(rows.len(), 3);

        // Rows are addressed by index, and the index is the position in the
        // persisted list — order is the rule engine's contract (first match
        // wins), so a row that reported the wrong one would edit the wrong rule.
        assert_eq!(rows[0]["index"], 0);
        assert_eq!(rows[0]["action"], "collapse");
        assert_eq!(rows[0]["pattern"], "api-*");
        assert_eq!(rows[0]["label"], "workflow jobs");
        assert_eq!(rows[0]["host"], "ubu-3xdv");
        assert_eq!(rows[0]["collapseOnly"], true);

        assert_eq!(rows[1]["index"], 1);
        assert_eq!(rows[1]["action"], "hide");
        // "All hosts" is the empty string on the wire, not null: a `<select>`
        // can only carry a string, and null would arrive as the literal "null".
        assert_eq!(rows[1]["host"], "");
        assert_eq!(rows[1]["collapseOnly"], false);

        assert_eq!(rows[2]["action"], "expect");
        assert_eq!(rows[2]["host"], LOCAL_HOST_SCOPE);
        assert_eq!(rows[2]["collapseOnly"], false);

        // The picker offers exactly the three actions the engine implements.
        let actions: Vec<&str> = rules_of(&vm)["actions"]
            .as_array()
            .expect("actions")
            .iter()
            .map(|a| a["value"].as_str().expect("value"))
            .collect();
        assert_eq!(actions, vec!["collapse", "hide", "expect"]);
        assert_eq!(rules_of(&vm)["actions"][0]["label"], "Collapse");
        assert_eq!(rules_of(&vm)["actions"][2]["label"], "Expect");
        assert_eq!(rules_of(&vm)["addLabel"], "Add Rule");
        assert_eq!(rules_of(&vm)["patternPrompt"], "api-*");
        assert_eq!(rules_of(&vm)["expectedPrompt"], "expected ×");
    }

    /// An expectation is a **string** in the payload, and its unset state is
    /// empty — never `0`. A `0` here would render as an expectation the
    /// operator never set, which is the fabricated number the whole
    /// `Option<u32>` exists to refuse.
    #[test]
    fn an_unset_expected_count_is_empty_not_zero() {
        let (settings, hosts, repos, stored) = sample();
        let vm = view(&settings, &hosts, &repos, &rules(), &stored, &facts());
        let rows = rules_of(&vm)["rows"].as_array().expect("rule rows");
        assert_eq!(rows[0]["expected"], "4");
        assert_eq!(rows[1]["expected"], "");
        assert_eq!(rows[2]["expected"], "");
        for row in rows {
            assert!(row["expected"].is_string(), "{row}");
        }
    }

    /// A picker whose selection is absent from its options renders blank — so a
    /// rule scoped to a host that has since been removed would read as
    /// unscoped while still matching nothing on every host.
    #[test]
    fn the_host_picker_keeps_a_scope_whose_host_no_longer_exists() {
        let (settings, hosts, repos, stored) = sample();
        let orphan = vec![
            ContainerGroupRule::new("legacy-*", "legacy", Action::Collapse).on_host("retired-box"),
        ];
        let vm = view(&settings, &hosts, &repos, &orphan, &stored, &facts());
        let options: Vec<&str> = rules_of(&vm)["rows"][0]["hostOptions"]
            .as_array()
            .expect("host options")
            .iter()
            .map(|o| o["value"].as_str().expect("value"))
            .collect();
        // "All hosts", this machine, both stored hosts by name, then the orphan.
        assert_eq!(
            options,
            vec!["", LOCAL_HOST_SCOPE, "mac-mini", "ubu-3xdv", "retired-box"]
        );
        assert_eq!(
            rules_of(&vm)["rows"][0]["hostOptions"][0]["label"],
            "All hosts"
        );

        // A scope that *does* exist is not duplicated into the list.
        let scoped =
            vec![ContainerGroupRule::new("api-*", "jobs", Action::Collapse).on_host("ubu-3xdv")];
        let vm = view(&settings, &hosts, &repos, &scoped, &stored, &facts());
        let options = rules_of(&vm)["rows"][0]["hostOptions"]
            .as_array()
            .expect("host options")
            .clone();
        assert_eq!(options.len(), 4);
    }

    #[test]
    fn an_empty_rule_list_renders_the_editor_with_no_rows() {
        let (settings, hosts, repos, stored) = sample();
        let vm = view(&settings, &hosts, &repos, &[], &stored, &facts());
        assert!(rules_of(&vm)["rows"]
            .as_array()
            .expect("rule rows")
            .is_empty());
        // The chrome is still there: an emptied list is a configuration, and
        // Add Rule is how it stops being one.
        assert_eq!(rules_of(&vm)["addLabel"], "Add Rule");
    }

    // MARK: the rule mutations

    #[test]
    fn every_rule_field_id_round_trips() {
        for field in RuleField::ALL {
            assert_eq!(RuleField::parse(field.id()), Some(field));
        }
        assert_eq!(RuleField::parse(""), None);
        assert_eq!(RuleField::parse("expectedCount"), None);
    }

    /// One field at a time, into a freshly-read list — the port of Swift's
    /// per-`keyPath` bindings. Editing the label must leave the pattern,
    /// action, scope and count exactly as they were on disk.
    #[test]
    fn an_edit_writes_one_field_and_leaves_the_rest_of_the_rule_alone() {
        let mut list = rules();
        let before = list[0].clone();
        assert!(apply_rule_edit(&mut list, 0, RuleField::Label, "ci jobs"));
        assert_eq!(list[0].label, "ci jobs");
        assert_eq!(list[0].pattern, before.pattern);
        assert_eq!(list[0].action, before.action);
        assert_eq!(list[0].host, before.host);
        assert_eq!(list[0].expected_count, before.expected_count);
        // …and no other rule moved.
        assert_eq!(list[1], rules()[1]);
        assert_eq!(list[2], rules()[2]);
    }

    #[test]
    fn each_field_writes_the_value_it_names() {
        let mut list = rules();
        assert!(apply_rule_edit(&mut list, 1, RuleField::Action, "expect"));
        assert_eq!(list[1].action, Action::Expect);
        assert!(apply_rule_edit(&mut list, 1, RuleField::Pattern, "vm-*"));
        assert_eq!(list[1].pattern, "vm-*");
        assert!(apply_rule_edit(&mut list, 1, RuleField::Host, "ubu-3xdv"));
        assert_eq!(list[1].host.as_deref(), Some("ubu-3xdv"));
        assert!(apply_rule_edit(&mut list, 1, RuleField::Expected, "7"));
        assert_eq!(list[1].expected_count, Some(7));
    }

    /// "All hosts" is the empty string, and it must reach the store as `None`
    /// — a rule scoped to the literal `""` would apply to no host at all.
    #[test]
    fn the_empty_host_scope_means_every_host() {
        let mut list = rules();
        assert!(apply_rule_edit(&mut list, 0, RuleField::Host, ""));
        assert_eq!(list[0].host, None);
        assert!(list[0].applies_to("ubu-3xdv"));
        assert!(list[0].applies_to(LOCAL_HOST_SCOPE));
    }

    /// Swift's `Int(newValue) .flatMap { $0 > 0 ? $0 : nil }`, case for case:
    /// anything that is not a positive whole number **clears** the expectation.
    #[test]
    fn a_blank_or_nonsensical_expected_count_clears_the_expectation() {
        assert_eq!(parse_expected_count("4"), Some(4));
        assert_eq!(parse_expected_count("  12  "), Some(12));
        for raw in ["", "   ", "0", "-1", "two", "3.5", "99999999999999999999"] {
            assert_eq!(parse_expected_count(raw), None, "raw {raw:?}");
        }

        let mut list = rules();
        assert_eq!(list[0].expected_count, Some(4));
        assert!(apply_rule_edit(&mut list, 0, RuleField::Expected, ""));
        assert_eq!(list[0].expected_count, None, "cleared, never coerced to 0");
    }

    /// An index that no longer names a rule, or an action no picker can
    /// produce, must change nothing — Swift's `guard let index … else
    /// { return }`, and the reason an unknown action is rejected here where the
    /// *file* decoder tolerates one.
    #[test]
    fn an_edit_that_addresses_nothing_leaves_the_list_untouched() {
        let mut list = rules();
        let before = list.clone();

        assert!(!apply_rule_edit(&mut list, 3, RuleField::Pattern, "x"));
        assert!(!apply_rule_edit(
            &mut list,
            usize::MAX,
            RuleField::Label,
            "x"
        ));
        assert!(!apply_rule_edit(
            &mut list,
            0,
            RuleField::Action,
            "quarantine"
        ));
        assert!(!apply_rule_edit(&mut list, 0, RuleField::Action, ""));
        assert_eq!(list, before);

        assert!(!apply_rule_edit(
            &mut Vec::new(),
            0,
            RuleField::Pattern,
            "x"
        ));
    }

    /// **Add Rule** appends a blank Collapse rule, and its empty pattern is
    /// load-bearing: it matches only the empty name, so a half-typed row cannot
    /// start collapsing or hiding containers before the operator is done.
    #[test]
    fn a_new_rule_starts_blank_and_matches_nothing_real() {
        let rule = new_rule();
        assert_eq!(rule.action, Action::Collapse);
        assert_eq!(rule.pattern, "");
        assert_eq!(rule.label, "");
        assert_eq!(rule.host, None);
        assert_eq!(rule.expected_count, None);
        assert!(!rule.matches("api-1"));
        assert!(!rule.matches("anything"));
    }

    #[test]
    fn the_portfolio_tab_lists_repos_with_their_watched_workflows() {
        let (settings, hosts, mut repos, stored) = sample();
        repos[0].watched_workflows = Some(vec!["release.yml".into()]);
        repos[1].enabled = false;
        let vm = view_of(&settings, &hosts, &repos, &stored, &facts());
        let rows = vm["portfolio"]["rows"].as_array().expect("rows");
        assert_eq!(rows.len(), repos.len());
        assert_eq!(rows[0]["slug"], repos[0].slug);
        assert_eq!(rows[0]["workflows"], "release.yml");
        assert_eq!(rows[1]["enabled"], false);
        assert_eq!(rows[1]["workflows"], "");
    }

    #[test]
    fn a_stored_credential_is_a_badge_and_nothing_more() {
        let (settings, hosts, repos, stored) = sample();
        let vm = view_of(&settings, &hosts, &repos, &stored, &facts());
        assert_eq!(vm["github"]["secret"]["stored"], true);
        assert_eq!(vm["github"]["secret"]["storedLabel"], "Token stored");
        assert_eq!(vm["usage"]["neon"]["secret"]["stored"], false);
        assert_eq!(vm["usage"]["sentry"]["secret"]["stored"], true);
        assert_eq!(vm["azure"]["secret"]["stored"], false);

        // Each section names the key its Save/Clear sends back.
        assert_eq!(vm["github"]["secret"]["key"], "github");
        assert_eq!(vm["usage"]["neon"]["secret"]["key"], "neon");
        assert_eq!(vm["usage"]["sentry"]["secret"]["key"], "sentry");
        assert_eq!(vm["azure"]["secret"]["key"], "azure");
    }

    /// The whole reason `StoredSecrets` is booleans: a payload that could
    /// carry a credential is a payload that will eventually be logged, and it
    /// crosses into a webview where the DOM is inspectable.
    #[test]
    fn the_payload_can_carry_no_credential_value_at_all() {
        let (settings, hosts, repos, stored) = sample();
        let raw = view_of(&settings, &hosts, &repos, &stored, &facts()).to_string();
        // Every credential-store account name, so a section that ever grew a
        // value field under its own key would be caught here too.
        for account in [
            SecretKey::GitHubAccessToken.account(),
            SecretKey::NeonApiKey.account(),
            SecretKey::SentryUsageToken.account(),
            SecretKey::AzureCostSasUrl.account(),
            SecretKey::OpenClawBearerToken.account(),
            // The one credential that is raw key material. Nothing on this
            // surface may name it, let alone carry it.
            SecretKey::OpenClawDeviceKey.account(),
            SecretKey::HostToken(hosts[0].id).account(),
        ] {
            assert!(!raw.contains(&account), "payload names {account}");
        }

        // …and structurally: a credential section is a label, two button
        // labels, a badge and a boolean. The moment one grows a field that
        // could *hold* a value, this fails — which a substring search over the
        // rendered payload could not do, because it cannot know what the value
        // would have been.
        let vm = view_of(&settings, &hosts, &repos, &stored, &facts());
        for secret in [
            &vm["github"]["secret"],
            &vm["usage"]["neon"]["secret"],
            &vm["usage"]["sentry"]["secret"],
            &vm["azure"]["secret"],
            &vm["openclaw"]["secret"],
        ] {
            let mut keys: Vec<&str> = secret
                .as_object()
                .expect("a secret section")
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(
                keys,
                [
                    "clearLabel",
                    "fieldLabel",
                    "help",
                    "key",
                    "saveLabel",
                    "stored",
                    "storedLabel"
                ]
            );
            assert!(secret["stored"].is_boolean());
        }
    }

    /// The device id is a *public* SHA-256 fingerprint the operator has to read
    /// off this screen and approve on the gateway — the opposite of a secret,
    /// and the reason the Device Pairing block exists. The seed behind it never
    /// appears here in any form.
    #[test]
    fn the_pairing_block_shows_the_fingerprint_and_the_approve_command() {
        let (settings, hosts, repos, stored) = sample();
        let live = openclaw::fixture_state(openclaw::Fixture::Pairing).settings_facts();
        let vm = view_of(&settings, &hosts, &repos, &stored, &live);
        let tab = &vm["openclaw"];

        assert_eq!(tab["deviceId"], live.device_id.expect("a device id"));
        assert_eq!(
            tab["pairing"]["explanation"],
            "This device isn't paired yet. Approve it on the gateway host:"
        );
        assert_eq!(
            tab["pairing"]["command"],
            "openclaw devices approve req-7f31"
        );
        assert_eq!(tab["pairing"]["retryLabel"], "Retry now");
        assert_eq!(tab["status"]["text"], "awaiting device pairing");
    }

    /// Nothing configured yet: no pairing block at all, and a sentence saying
    /// the key does not exist rather than an empty Device ID row claiming an
    /// identity that has never been minted.
    #[test]
    fn with_no_device_key_the_tab_says_so_instead_of_showing_a_blank_id() {
        let (settings, hosts, repos, stored) = sample();
        let vm = view_of(&settings, &hosts, &repos, &stored, &facts());
        let tab = &vm["openclaw"];

        assert!(tab["deviceId"].is_null());
        assert_eq!(
            tab["noDeviceLabel"],
            "Device key is generated on first connect."
        );
        assert!(tab["pairing"].is_null());
        assert_eq!(tab["status"]["text"], "Idle");
        assert_eq!(tab["gateway"]["value"], "");
    }

    /// The status row is amber for a disconnect where the *panel* is red. Two
    /// audiences: the cockpit glance says "go look", and this screen — the one
    /// you went and looked at — says what to fix without shouting it twice.
    #[test]
    fn the_status_row_names_each_connection_state_in_its_own_colour() {
        use self::openclaw::RuntimeConnectionState as State;
        let hex = viewmodel::color::hex;
        assert_eq!(
            openclaw_status(&State::Connected),
            ("Connected".to_owned(), viewmodel::color::GREEN)
        );
        assert_eq!(
            openclaw_status(&State::Connecting),
            ("Connecting…".to_owned(), viewmodel::color::AMBER)
        );
        assert_eq!(
            openclaw_status(&State::Idle),
            ("Idle".to_owned(), viewmodel::color::MUTED)
        );
        let (text, colour) = openclaw_status(&State::Disconnected {
            reason: "gateway rejected: nope".to_owned(),
        });
        assert_eq!(text, "gateway rejected: nope", "the gateway's own words");
        assert_eq!(hex(colour), hex(viewmodel::color::AMBER));
    }

    #[test]
    fn the_non_secret_provider_settings_are_shown_so_they_can_be_edited() {
        let (settings, hosts, repos, stored) = sample();
        let vm = view_of(&settings, &hosts, &repos, &stored, &facts());
        assert_eq!(vm["usage"]["neon"]["orgId"], "org-abc");
        assert_eq!(vm["usage"]["sentry"]["orgSlug"], "sassy-dog");
        assert_eq!(vm["usage"]["sentry"]["quota"], 50_000);
        assert_eq!(vm["azure"]["budget"]["value"], 250.0);
    }

    #[test]
    fn the_about_tab_names_the_app_and_its_version() {
        let (settings, hosts, repos, stored) = sample();
        let vm = view_of(&settings, &hosts, &repos, &stored, &facts());
        assert_eq!(vm["about"]["name"], "DevCanopy");
        assert_eq!(vm["about"]["version"], format!("Version {VERSION}"));
        assert_eq!(vm["about"]["links"].as_array().expect("links").len(), 3);
    }

    #[test]
    fn every_secret_field_id_round_trips_and_maps_to_its_own_key() {
        let fields = [
            SecretField::GitHub,
            SecretField::Neon,
            SecretField::Sentry,
            SecretField::Azure,
            SecretField::OpenClaw,
        ];
        for field in fields {
            assert_eq!(SecretField::parse(field.id()), Some(field));
        }
        assert_eq!(SecretField::parse(""), None);
        // The device key is minted, never typed, so it is deliberately not a
        // writable field — a webview asking to set one is a rejected command.
        assert_eq!(SecretField::parse("openclaw_device_key"), None);

        let mut accounts: Vec<String> = fields.iter().map(|f| f.key().account()).collect();
        accounts.sort();
        accounts.dedup();
        assert_eq!(accounts.len(), fields.len(), "two fields share one key");
    }
}
