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
use store::{Host, HostOverflowMode, SecretKey, Settings, TrackedRepo, DEFAULT_AGENT_PORT};
use uuid::Uuid;

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
    stored: &StoredSecrets,
) -> Value {
    json!({
        "title": OPEN_LABEL,
        "openLabel": OPEN_LABEL,
        "closeLabel": "Done",
        // The Swift window's tab order, minus OpenClaw: that tab configures a
        // gateway this shell has no client for yet, and a tab whose controls
        // drive nothing is worse than an absent one. It returns with the
        // OpenClaw slice; `Settings::openclaw_gateway_url` and
        // `SecretKey::OpenClawBearerToken` are already in the store for it.
        "tabs": [
            { "id": "general", "title": "General" },
            { "id": "github", "title": "GitHub" },
            { "id": "portfolio", "title": "Portfolio" },
            { "id": "hosts", "title": "Hosts" },
            { "id": "azure", "title": "Azure Cost" },
            { "id": "usage", "title": "Usage" },
            { "id": "about", "title": "About" },
        ],
        "general": general_tab(settings),
        "github": github_tab(stored),
        "portfolio": portfolio_tab(repos),
        "hosts": hosts_tab(settings, hosts, stored),
        "azure": azure_tab(settings, stored),
        "usage": usage_tab(settings, stored),
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

fn hosts_tab(settings: &Settings, hosts: &[Host], stored: &StoredSecrets) -> Value {
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
            hosts: [host.id].into_iter().collect(),
        };
        (settings, vec![host, off], store::seeded_repos(), stored)
    }

    #[test]
    fn the_payload_carries_every_tab_the_window_shows() {
        let (settings, hosts, repos, stored) = sample();
        let vm = view(&settings, &hosts, &repos, &stored);
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
                "about"
            ]
        );
        // Every tab id must address a section of the payload, or the frontend
        // renders a blank pane for a tab that exists.
        for id in ids {
            assert!(!vm[id].is_null(), "tab {id} has no payload section");
        }
        // OpenClaw is deferred, and deferred means absent -- not a tab whose
        // controls quietly drive nothing.
        assert!(vm["openclaw"].is_null());
    }

    #[test]
    fn the_general_tab_shows_the_stored_values_and_the_offered_choices() {
        let (settings, hosts, repos, stored) = sample();
        let vm = view(&settings, &hosts, &repos, &stored);
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
        let vm = view(&settings, &hosts, &repos, &stored);
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

    #[test]
    fn the_portfolio_tab_lists_repos_with_their_watched_workflows() {
        let (settings, hosts, mut repos, stored) = sample();
        repos[0].watched_workflows = Some(vec!["release.yml".into()]);
        repos[1].enabled = false;
        let vm = view(&settings, &hosts, &repos, &stored);
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
        let vm = view(&settings, &hosts, &repos, &stored);
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
        let raw = view(&settings, &hosts, &repos, &stored).to_string();
        // Every credential-store account name, so a section that ever grew a
        // value field under its own key would be caught here too.
        for account in [
            SecretKey::GitHubAccessToken.account(),
            SecretKey::NeonApiKey.account(),
            SecretKey::SentryUsageToken.account(),
            SecretKey::AzureCostSasUrl.account(),
            SecretKey::HostToken(hosts[0].id).account(),
        ] {
            assert!(!raw.contains(&account), "payload names {account}");
        }
        assert!(!raw.to_lowercase().contains("bearer"));
    }

    #[test]
    fn the_non_secret_provider_settings_are_shown_so_they_can_be_edited() {
        let (settings, hosts, repos, stored) = sample();
        let vm = view(&settings, &hosts, &repos, &stored);
        assert_eq!(vm["usage"]["neon"]["orgId"], "org-abc");
        assert_eq!(vm["usage"]["sentry"]["orgSlug"], "sassy-dog");
        assert_eq!(vm["usage"]["sentry"]["quota"], 50_000);
        assert_eq!(vm["azure"]["budget"]["value"], 250.0);
    }

    #[test]
    fn the_about_tab_names_the_app_and_its_version() {
        let (settings, hosts, repos, stored) = sample();
        let vm = view(&settings, &hosts, &repos, &stored);
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
        ];
        for field in fields {
            assert_eq!(SecretField::parse(field.id()), Some(field));
        }
        assert_eq!(SecretField::parse("openclaw"), None);
        assert_eq!(SecretField::parse(""), None);

        let mut accounts: Vec<String> = fields.iter().map(|f| f.key().account()).collect();
        accounts.sort();
        accounts.dedup();
        assert_eq!(accounts.len(), fields.len(), "two fields share one key");
    }
}
