//! The scalar preferences the original app keeps in `@AppStorage`/UserDefaults.
//!
//! Ground truth is `SettingsView` (plus
//! `RefreshInterval`, `CockpitBreakpoints`,
//! `LocalHostMetricsService`). Semantics are
//! mirrored, not APIs: UserDefaults hands back a zero for an unset key and the
//! original side launders that through `RefreshInterval(rawValue:) ?? .default`,
//! so the same "an out-of-range stored value reads as the default" rule is
//! enforced here on deserialize rather than at every call site.
//!
//! Nothing in here is a secret — credentials live in the OS credential store
//! (`crate::secrets`), never in this struct or the file it serialises into.

use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;

/// The cadences Settings offers, in seconds (`RefreshInterval` in the original).
pub const REFRESH_INTERVAL_CHOICES: [u32; 3] = [30, 60, 300];

/// `RefreshInterval.default` — 1 minute.
pub const DEFAULT_REFRESH_INTERVAL_SECS: u32 = 60;

/// Inclusive bounds for the CPU-core grid's row span (`coreRowSpan`).
pub const CORE_ROW_SPAN_RANGE: std::ops::RangeInclusive<u8> = 1..=4;

/// The `coreRowSpan` default shared by Settings and `HostMetricsPanel`.
pub const DEFAULT_CORE_ROW_SPAN: u8 = 2;

/// What the cockpit does when host cards no longer fit side by side.
///
/// Mirrors the original's `HostOverflowMode` (`stack`/`tabs`), including its
/// `HostOverflowMode(rawValue:) ?? .stack` tolerance: an unrecognised string
/// reads as `Stack`, the layout that cannot be unreadable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum HostOverflowMode {
    /// Stack the host cards vertically.
    #[default]
    Stack,
    /// Show one host at a time behind a tab bar.
    Tabs,
}

impl HostOverflowMode {
    /// The persisted spelling — identical to the original enum's `rawValue`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            HostOverflowMode::Stack => "stack",
            HostOverflowMode::Tabs => "tabs",
        }
    }
}

impl From<String> for HostOverflowMode {
    fn from(raw: String) -> Self {
        match raw.as_str() {
            "tabs" => HostOverflowMode::Tabs,
            _ => HostOverflowMode::Stack,
        }
    }
}

impl From<HostOverflowMode> for String {
    fn from(mode: HostOverflowMode) -> Self {
        mode.as_str().to_owned()
    }
}

/// A panel whose poll cadence the operator may set.
///
/// **There is deliberately no `Hosts` variant.** The 1 Hz host poll feeds the
/// history buffers behind the sparklines, and a setting there is an invitation
/// to stretch it until those charts stop meaning anything. It stays a constant
/// in the shell (`POLL_INTERVAL` in `app/src-tauri/src/main.rs`) precisely so it
/// cannot be reached from Settings. Agent health (10s) is out for the same
/// reason: it is liveness, not a panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PanelInterval {
    /// Containers/VMs — local docker/podman/tart plus every host's agent.
    Containers,
    /// The Usage panel's metered providers: Neon, Sentry and Vercel.
    UsageProviders,
    /// The Azure Cost panel's daily export read.
    AzureCost,
    /// The Sentry Crons panel.
    Crons,
}

/// Everything that is true of one [`PanelInterval`], in one place.
///
/// The key, the default and the floor travel together on purpose. Three
/// parallel tables — or one array indexed by variant order — is exactly the
/// shape that lets a newly added metered source pick up a neighbour's floor by
/// position and bill for it, and no test would notice. Here the compiler's
/// exhaustiveness check on [`PanelInterval::spec`] refuses to build until a new
/// variant has stated all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntervalSpec {
    /// The `panel_intervals` map key. Part of the on-disk format — renaming one
    /// silently discards the operator's setting for that panel.
    pub key: &'static str,
    /// Today's hardcoded cadence, so an unconfigured store behaves as it does now.
    pub default_secs: u32,
    /// The shortest cadence this source may be asked for. See
    /// [`PanelInterval::spec`] for why each one is what it is.
    pub floor_secs: u32,
}

impl PanelInterval {
    /// Every panel that has a settable cadence, in a stable order for UI.
    pub const ALL: [PanelInterval; 4] = [
        PanelInterval::Containers,
        PanelInterval::UsageProviders,
        PanelInterval::AzureCost,
        PanelInterval::Crons,
    ];

    /// The key, default and floor for this panel.
    ///
    /// **The floor is stated beside the source it protects**, not gathered into
    /// a central table of numbers, so that whoever adds a metered source sets
    /// its floor in the same edit that introduces the request. The literal form
    /// of that instruction — a `MIN_POLL_INTERVAL` const in each source's own
    /// module — is not available in this direction: three of these four sources
    /// live in `app/src-tauri`, which depends on this crate, so importing their
    /// constants here would invert the dependency. What is available, and what
    /// this is, is one self-contained arm per source carrying all three facts
    /// and citing the constant it mirrors.
    ///
    /// Each default is today's constant, verified against the tree:
    ///
    /// | panel | default | today's constant |
    /// |---|---|---|
    /// | Containers | 10s | `containers::POLL_INTERVAL_SECS` |
    /// | `UsageProviders` | 1h | `usage::PROVIDER_POLL_INTERVAL_SECS` |
    /// | `AzureCost` | 4h | `azurecost::POLL_INTERVAL` |
    /// | Crons | 1h | *shares* `usage::PROVIDER_POLL_INTERVAL_SECS` |
    #[must_use]
    pub const fn spec(self) -> IntervalSpec {
        match self {
            // `app/src-tauri/src/containers/mod.rs`'s `POLL_INTERVAL_SECS = 10`.
            // Floor 5s: the cost here is not money but a process spawn — every
            // pass shells out to `docker ps` — and a container list changes on
            // human timescales. 5s is the point below which the spawns cost
            // more than the freshness is worth.
            PanelInterval::Containers => IntervalSpec {
                key: "containers",
                default_secs: 10,
                floor_secs: 5,
            },
            // `app/src-tauri/src/usage.rs`'s `PROVIDER_POLL_INTERVAL_SECS = 60 * 60`.
            // Floor 300s: three vendor APIs (Neon, Sentry, Vercel) on one pass,
            // each with a rate-limit budget. Consumption figures move on the
            // order of hours, so anything under 5 minutes spends quota to learn
            // nothing — the reasoning already written on that constant.
            PanelInterval::UsageProviders => IntervalSpec {
                key: "usage_providers",
                default_secs: 60 * 60,
                floor_secs: 300,
            },
            // `crates/azurecost/src/lib.rs`'s `POLL_INTERVAL = 4 * 60 * 60`.
            // Floor 3600s, and this is the floor that exists to stop a spend
            // decision. The export is published roughly once a day; each poll
            // mints a SAS and may pull the blob, so a short cadence buys nothing
            // and moves real egress. It is also the pattern the metered-source
            // floors to come (Cost Explorer bills $0.01/request) will follow.
            PanelInterval::AzureCost => IntervalSpec {
                key: "azure_cost",
                default_secs: 4 * 60 * 60,
                floor_secs: 3600,
            },
            // Crons has **no constant of its own today**: `crons_loop` in
            // `app/src-tauri/src/main.rs` sleeps on `usage`'s
            // `PROVIDER_POLL_INTERVAL_SECS`, and the note there says two
            // constants "would be free to drift". Giving it a variant is what
            // separates them — deliberately, since the epic's premise is that a
            // panel's cadence is the operator's. The defaults stay equal, so an
            // unconfigured store keeps the shared hour; only an explicit edit
            // parts them. Same API and same rate-limit budget as the Sentry
            // read, hence the same 300s floor.
            PanelInterval::Crons => IntervalSpec {
                key: "crons",
                default_secs: 60 * 60,
                floor_secs: 300,
            },
        }
    }
}

/// What became of a requested cadence.
///
/// A setter that returned a bare `u32` would let a floored value read back as a
/// chosen one — the same fabrication as a defaulted number rendered like a
/// measured one. `Clamped` carries both halves so a caller can say *"4h is the
/// shortest this panel allows"* rather than silently showing a number nobody
/// asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClampOutcome {
    /// The request was at or above the floor and was stored as asked.
    Applied(u32),
    /// The request was below the floor; `applied` was stored instead.
    Clamped {
        /// What the caller asked for.
        asked: u32,
        /// What was actually stored — the panel's floor.
        applied: u32,
    },
}

impl ClampOutcome {
    /// The cadence now in effect, either way.
    #[must_use]
    pub const fn applied(self) -> u32 {
        match self {
            ClampOutcome::Applied(secs) | ClampOutcome::Clamped { applied: secs, .. } => secs,
        }
    }

    /// Whether the request was moved.
    #[must_use]
    pub const fn was_clamped(self) -> bool {
        matches!(self, ClampOutcome::Clamped { .. })
    }
}

/// Every non-secret preference, in one serde-round-trippable struct.
///
/// `#[serde(default)]` on the container is what makes an older (or partial)
/// file load: a missing key takes this struct's `Default`, and unknown keys are
/// ignored, so a file written by a newer build still opens here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Poll cadence for the periodic services, in seconds. Always one of
    /// [`REFRESH_INTERVAL_CHOICES`] — anything else read from disk becomes
    /// [`DEFAULT_REFRESH_INTERVAL_SECS`].
    #[serde(deserialize_with = "de_refresh_interval_secs")]
    pub refresh_interval_secs: u32,
    /// How many grid rows the per-host CPU core chart spans. Always within
    /// [`CORE_ROW_SPAN_RANGE`]; an out-of-range value read from disk is clamped
    /// into it rather than dropped, since the intent still survives.
    #[serde(deserialize_with = "de_core_row_span")]
    pub core_row_span: u8,
    /// Host-card behaviour below the side-by-side breakpoint.
    pub host_overflow_mode: HostOverflowMode,
    /// Monthly Azure budget in USD. `0` means "no budget set" and hides the bar.
    pub azure_monthly_budget_usd: f64,
    /// Storage account holding the Azure cost export (non-secret — the SAS is
    /// minted per poll from the operator's own Entra session and never stored).
    ///
    /// Empty means unset, which the panel says out loud. This was a constant
    /// in a shell script until the app learned to mint its own SAS; a storage
    /// account is per-deployment and there is nothing to default it to.
    #[serde(default)]
    pub azure_storage_account: String,
    /// Blob container within that account, e.g. `cost-exports`. Empty means
    /// unset. Named by whoever configured the export, so it is not guessable
    /// either.
    #[serde(default)]
    pub azure_cost_container: String,
    /// GitHub organization whose self-hosted runners the Runners panel lists
    /// (non-secret; the token is a credential). Empty means unset.
    ///
    /// There is no default and there must not be one. This was a hardcoded
    /// constant until it became clear that every install was querying one
    /// particular organization's runners — a panel that could only ever work
    /// for its author. Unset is said out loud rather than rendered as an empty
    /// roster, which would be indistinguishable from an org with no runners.
    #[serde(default)]
    pub github_org: String,
    /// Neon organization id (non-secret; the API key is a credential).
    pub neon_org_id: String,
    /// Neon compute rate in USD per CU-hour (non-secret; `0` = unset, which
    /// hides the estimated-charges row). Entered by the operator from their
    /// plan's published pricing — the app ships no price table on purpose.
    #[serde(default)]
    pub neon_usd_per_cu_hour: f64,
    /// Neon storage rate in USD per GiB-month. Same rules as the compute rate.
    #[serde(default)]
    pub neon_usd_per_gib_month: f64,
    /// Sentry organization slug (non-secret; the token is a credential).
    pub sentry_org_slug: String,
    /// Monthly accepted-error quota. `0` means "no quota set".
    pub sentry_monthly_event_quota: u64,
    /// Vercel team id (non-secret; the API token is a credential). Blank for a
    /// personal account, which is why the client omits the parameter rather
    /// than sending it empty — Vercel answers 400 to `teamId=`.
    #[serde(default)]
    pub vercel_team_id: String,
    /// OpenClaw gateway URL (non-secret; the bearer token is a credential).
    pub openclaw_gateway_url: String,
    /// Fire an OS notification once when a tracked run transitions into the
    /// `waiting` deployment-protection gate (a human must approve it).
    ///
    /// the original's `WorkflowDisplayOptions.notifyOnApprovalNeeded`, which is
    /// likewise default-true and likewise has **no Settings control** — the
    /// preference persists and is read on every poll pass, but nothing in
    /// either app's UI writes it. Editing the store file by hand is the only
    /// way to turn it off, in both.
    pub notify_on_approval_needed: bool,
    /// Fire an OS notification when a watched third-party service changes
    /// availability — GitHub Actions going into a major outage, and coming back
    /// out of one.
    ///
    /// Default-true and, like its neighbour above, with **no Settings control**:
    /// editing the store file by hand is the only way to turn it off. The
    /// cockpit's availability chip is only true while someone is looking at it,
    /// and the point of a multi-hour outage is to stop looking.
    pub notify_on_service_change: bool,
    /// Mount paths hidden from the *local* machine's Volumes section. Remote
    /// hosts carry their own list on [`crate::Host`].
    pub local_hidden_volume_mounts: Vec<String>,
    /// Per-panel poll cadences, keyed by [`IntervalSpec::key`]. **Overrides
    /// only** — an absent key means "this panel has never been configured", not
    /// zero, and reads as its default. Empty is the unconfigured state, which is
    /// why the defaults are not written in here at construction: a stored copy
    /// of today's constant would be indistinguishable from a deliberate choice,
    /// and would pin the panel to that number the day the constant moves.
    ///
    /// Read it through [`Settings::panel_interval_secs`], never directly — that
    /// is where the floor is enforced, and a value hand-edited into this file
    /// gets no say in it. Unrecognised keys are ignored and preserved, so a
    /// store written by a newer build survives a round-trip through an older one.
    #[serde(default)]
    pub panel_intervals: BTreeMap<String, u32>,
}

impl Settings {
    /// This panel's cadence in seconds: the operator's override if there is one,
    /// otherwise today's constant.
    ///
    /// **The floor is applied here, on read.** Enforcing it only in
    /// [`Settings::set_panel_interval_secs`] would leave it bypassable by
    /// editing `store.json` — and this is the one setting where a bypass costs
    /// money rather than tidiness. Clamping at the single point of consumption
    /// means no path reaches a below-floor cadence.
    #[must_use]
    pub fn panel_interval_secs(&self, panel: PanelInterval) -> u32 {
        let spec = panel.spec();
        self.panel_intervals
            .get(spec.key)
            .map_or(spec.default_secs, |&secs| secs.max(spec.floor_secs))
    }

    /// Set this panel's cadence, reporting whether the floor moved it.
    ///
    /// Returns [`ClampOutcome`] rather than a bare `u32` so the caller cannot
    /// show a floored value as though it were the one requested.
    pub fn set_panel_interval_secs(&mut self, panel: PanelInterval, secs: u32) -> ClampOutcome {
        let spec = panel.spec();
        let applied = secs.max(spec.floor_secs);
        self.panel_intervals.insert(spec.key.to_owned(), applied);
        if applied == secs {
            ClampOutcome::Applied(applied)
        } else {
            ClampOutcome::Clamped {
                asked: secs,
                applied,
            }
        }
    }

    /// Drop this panel's override, returning it to today's constant.
    ///
    /// Distinct from setting it *to* the default: this restores "never
    /// configured", so the panel follows the constant if it later moves.
    pub fn clear_panel_interval(&mut self, panel: PanelInterval) {
        self.panel_intervals.remove(panel.spec().key);
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            refresh_interval_secs: DEFAULT_REFRESH_INTERVAL_SECS,
            core_row_span: DEFAULT_CORE_ROW_SPAN,
            host_overflow_mode: HostOverflowMode::default(),
            azure_monthly_budget_usd: 0.0,
            azure_storage_account: String::new(),
            azure_cost_container: String::new(),
            github_org: String::new(),
            neon_org_id: String::new(),
            neon_usd_per_cu_hour: 0.0,
            neon_usd_per_gib_month: 0.0,
            sentry_org_slug: String::new(),
            sentry_monthly_event_quota: 0,
            vercel_team_id: String::new(),
            openclaw_gateway_url: String::new(),
            notify_on_approval_needed: true,
            notify_on_service_change: true,
            local_hidden_volume_mounts: Vec::new(),
            // Empty, not pre-filled with the defaults — see the field's note.
            panel_intervals: BTreeMap::new(),
        }
    }
}

/// `null` or a cadence Settings never offers reads as the default, the way
/// `RefreshInterval(rawValue:) ?? .default` does in the original.
fn de_refresh_interval_secs<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    let raw = Option::<u32>::deserialize(d)?;
    Ok(raw
        .filter(|secs| REFRESH_INTERVAL_CHOICES.contains(secs))
        .unwrap_or(DEFAULT_REFRESH_INTERVAL_SECS))
}

/// Clamped, not defaulted: a stored `9` means "as many rows as possible", and
/// the top of the range is a truer reading of that than the default is.
fn de_core_row_span<'de, D: Deserializer<'de>>(d: D) -> Result<u8, D::Error> {
    let raw = Option::<u8>::deserialize(d)?;
    Ok(raw.map_or(DEFAULT_CORE_ROW_SPAN, |span| {
        span.clamp(*CORE_ROW_SPAN_RANGE.start(), *CORE_ROW_SPAN_RANGE.end())
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_original_app() {
        let s = Settings::default();
        assert_eq!(s.refresh_interval_secs, 60);
        assert_eq!(s.core_row_span, 2);
        assert_eq!(s.host_overflow_mode, HostOverflowMode::Stack);
        assert_eq!(s.azure_monthly_budget_usd, 0.0);
        assert_eq!(s.sentry_monthly_event_quota, 0);
        assert!(
            s.github_org.is_empty(),
            "no org may be guessed on anyone's behalf"
        );
        assert!(s.neon_org_id.is_empty());
        assert!(s.sentry_org_slug.is_empty());
        assert!(s.openclaw_gateway_url.is_empty());
        assert!(s.notify_on_approval_needed);
        assert!(s.local_hidden_volume_mounts.is_empty());
    }

    /// The two preferences with no UI on either side: a store file written
    /// before either existed must still opt *in*, because the original's
    /// `WorkflowDisplayOptions()` default is `true` and an upgrade that
    /// silently disabled the alert would look exactly like the feature not
    /// working. The same argument covers the service-change alert, which nobody
    /// can turn back on from the app if an upgrade turns it off.
    #[test]
    fn a_store_file_without_the_notify_keys_still_opts_in() {
        let s: Settings = serde_json::from_str(r#"{"core_row_span":3}"#).expect("deserialize");
        assert!(s.notify_on_approval_needed);
        assert!(s.notify_on_service_change);

        let off: Settings =
            serde_json::from_str(r#"{"notify_on_approval_needed":false}"#).expect("deserialize");
        assert!(!off.notify_on_approval_needed);
        assert!(
            off.notify_on_service_change,
            "the two alerts are independent switches"
        );

        let quiet: Settings =
            serde_json::from_str(r#"{"notify_on_service_change":false}"#).expect("deserialize");
        assert!(!quiet.notify_on_service_change);
        assert!(quiet.notify_on_approval_needed);
    }

    #[test]
    fn round_trips_through_json() {
        let s = Settings {
            refresh_interval_secs: 300,
            core_row_span: 4,
            host_overflow_mode: HostOverflowMode::Tabs,
            azure_monthly_budget_usd: 125.5,
            azure_storage_account: "acmestorage".into(),
            azure_cost_container: "cost-exports".into(),
            github_org: "acme".into(),
            neon_org_id: "org-abc".into(),
            neon_usd_per_cu_hour: 0.175,
            neon_usd_per_gib_month: 0.5,
            sentry_org_slug: "acme".into(),
            sentry_monthly_event_quota: 50_000,
            vercel_team_id: "team_fixture".into(),
            openclaw_gateway_url: "https://gateway.example".into(),
            notify_on_approval_needed: false,
            notify_on_service_change: false,
            local_hidden_volume_mounts: vec!["/Volumes/Backup".into()],
            panel_intervals: BTreeMap::from([
                ("containers".to_owned(), 30),
                ("azure_cost".to_owned(), 6 * 3600),
            ]),
        };
        let json = serde_json::to_string(&s).expect("serialize");
        assert_eq!(
            serde_json::from_str::<Settings>(&json).expect("deserialize"),
            s
        );
    }

    #[test]
    fn host_overflow_mode_serialises_as_the_original_raw_value() {
        let json = serde_json::to_string(&HostOverflowMode::Tabs).expect("serialize");
        assert_eq!(json, "\"tabs\"");
        assert_eq!(
            serde_json::from_str::<HostOverflowMode>("\"stack\"").expect("deserialize"),
            HostOverflowMode::Stack
        );
    }

    #[test]
    fn unknown_host_overflow_mode_reads_as_stack() {
        let s: Settings =
            serde_json::from_str(r#"{"host_overflow_mode":"carousel"}"#).expect("deserialize");
        assert_eq!(s.host_overflow_mode, HostOverflowMode::Stack);
    }

    #[test]
    fn missing_fields_take_the_defaults() {
        let s: Settings =
            serde_json::from_str(r#"{"neon_org_id":"org-abc"}"#).expect("deserialize");
        assert_eq!(s.neon_org_id, "org-abc");
        assert_eq!(
            s,
            Settings {
                neon_org_id: "org-abc".into(),
                ..Settings::default()
            }
        );
    }

    #[test]
    fn unknown_fields_are_tolerated() {
        let s: Settings = serde_json::from_str(
            r#"{"core_row_span":3,"a_field_from_a_newer_build":{"nested":true}}"#,
        )
        .expect("deserialize");
        assert_eq!(s.core_row_span, 3);
    }

    #[test]
    fn out_of_range_refresh_interval_reads_as_the_default() {
        for raw in ["0", "45", "null"] {
            let json = format!(r#"{{"refresh_interval_secs":{raw}}}"#);
            let s: Settings = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(s.refresh_interval_secs, 60, "raw {raw}");
        }
        for secs in REFRESH_INTERVAL_CHOICES {
            let json = format!(r#"{{"refresh_interval_secs":{secs}}}"#);
            let s: Settings = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(s.refresh_interval_secs, secs);
        }
    }

    #[test]
    fn out_of_range_core_row_span_is_clamped() {
        let cases = [("0", 1), ("9", 4), ("null", 2), ("1", 1), ("4", 4)];
        for (raw, want) in cases {
            let json = format!(r#"{{"core_row_span":{raw}}}"#);
            let s: Settings = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(s.core_row_span, want, "raw {raw}");
        }
    }

    #[test]
    fn neon_rates_default_to_unset() {
        let s = Settings::default();
        assert_eq!(s.neon_usd_per_cu_hour, 0.0);
        assert_eq!(s.neon_usd_per_gib_month, 0.0);
    }

    /// A store written before the rates existed must still deserialize.
    #[test]
    fn neon_rates_tolerate_a_store_written_before_they_existed() {
        let s: Settings =
            serde_json::from_str(r#"{"neon_org_id":"org-abc"}"#).expect("deserialize");
        assert_eq!(s.neon_usd_per_cu_hour, 0.0);
        assert_eq!(s.neon_usd_per_gib_month, 0.0);
    }

    #[test]
    fn defaults_match_todays_constants() {
        let s = Settings::default();
        assert_eq!(s.panel_interval_secs(PanelInterval::UsageProviders), 3600);
        assert_eq!(s.panel_interval_secs(PanelInterval::AzureCost), 4 * 3600);
    }

    #[test]
    fn a_below_floor_interval_is_clamped_and_the_clamp_is_reported() {
        let mut s = Settings::default();
        assert_eq!(
            s.set_panel_interval_secs(PanelInterval::AzureCost, 30),
            ClampOutcome::Clamped {
                asked: 30,
                applied: 3600
            }
        );
        assert_eq!(s.panel_interval_secs(PanelInterval::AzureCost), 3600);
    }

    #[test]
    fn a_value_at_or_above_the_floor_is_applied_unchanged() {
        let mut s = Settings::default();
        assert_eq!(
            s.set_panel_interval_secs(PanelInterval::UsageProviders, 7200),
            ClampOutcome::Applied(7200)
        );
    }

    /// The other two defaults, which the issue's test does not cover. Each is
    /// today's constant: `containers::POLL_INTERVAL_SECS` and the hour
    /// `crons_loop` borrows from `usage::PROVIDER_POLL_INTERVAL_SECS`.
    #[test]
    fn every_panels_default_is_todays_constant() {
        let s = Settings::default();
        assert_eq!(s.panel_interval_secs(PanelInterval::Containers), 10);
        assert_eq!(s.panel_interval_secs(PanelInterval::Crons), 3600);
        // An unconfigured store stores nothing at all — the defaults are not
        // written in, so they are free to follow their constants.
        assert!(s.panel_intervals.is_empty());
    }

    /// **The load-bearing back-compat assertion.** Built by hand rather than by
    /// round-tripping the current serializer, which always emits the key and so
    /// could never catch its absence.
    #[test]
    fn a_store_written_before_panel_intervals_existed_loads_as_defaults() {
        let s: Settings = serde_json::from_str(
            r#"{"refresh_interval_secs":30,"core_row_span":3,"neon_org_id":"org-abc"}"#,
        )
        .expect("a store.json with no panel_intervals key must still load");
        assert!(s.panel_intervals.is_empty());
        for panel in PanelInterval::ALL {
            assert_eq!(
                s.panel_interval_secs(panel),
                panel.spec().default_secs,
                "{panel:?} must behave exactly as it does today"
            );
        }
        // The rest of the file still parsed.
        assert_eq!(s.refresh_interval_secs, 30);
        assert_eq!(s.core_row_span, 3);
    }

    /// The floor holds on *read*, so hand-editing the file cannot buy a cadence
    /// the setter refuses. This is the path that would otherwise cost money.
    #[test]
    fn a_hand_edited_below_floor_value_is_still_floored_on_read() {
        let s: Settings =
            serde_json::from_str(r#"{"panel_intervals":{"azure_cost":1,"containers":0}}"#)
                .expect("deserialize");
        assert_eq!(s.panel_interval_secs(PanelInterval::AzureCost), 3600);
        assert_eq!(s.panel_interval_secs(PanelInterval::Containers), 5);
    }

    /// A key from a build that knows a panel this one does not must survive the
    /// round-trip rather than being dropped on the next save.
    #[test]
    fn an_unknown_panel_key_is_ignored_but_preserved() {
        let s: Settings =
            serde_json::from_str(r#"{"panel_intervals":{"a_panel_from_a_newer_build":42}}"#)
                .expect("deserialize");
        assert_eq!(s.panel_interval_secs(PanelInterval::Containers), 10);
        let json = serde_json::to_string(&s).expect("serialize");
        assert!(json.contains("a_panel_from_a_newer_build"), "{json}");
    }

    #[test]
    fn clearing_an_override_restores_todays_constant() {
        let mut s = Settings::default();
        s.set_panel_interval_secs(PanelInterval::Containers, 60);
        assert_eq!(s.panel_interval_secs(PanelInterval::Containers), 60);
        s.clear_panel_interval(PanelInterval::Containers);
        assert!(
            s.panel_intervals.is_empty(),
            "not merely set to the default"
        );
        assert_eq!(s.panel_interval_secs(PanelInterval::Containers), 10);
    }

    /// Exactly at the floor is a choice, not a clamp.
    #[test]
    fn a_value_exactly_at_the_floor_is_applied_not_clamped() {
        let mut s = Settings::default();
        for panel in PanelInterval::ALL {
            let floor = panel.spec().floor_secs;
            let outcome = s.set_panel_interval_secs(panel, floor);
            assert_eq!(outcome, ClampOutcome::Applied(floor), "{panel:?}");
            assert!(!outcome.was_clamped(), "{panel:?}");
            assert_eq!(outcome.applied(), floor);
        }
    }

    /// Both halves of a clamp are carried, and `applied()` reads either variant.
    #[test]
    fn a_clamp_reports_what_was_asked_as_well_as_what_was_applied() {
        let mut s = Settings::default();
        for panel in PanelInterval::ALL {
            let floor = panel.spec().floor_secs;
            let outcome = s.set_panel_interval_secs(panel, floor - 1);
            assert_eq!(
                outcome,
                ClampOutcome::Clamped {
                    asked: floor - 1,
                    applied: floor,
                },
                "{panel:?}"
            );
            assert!(outcome.was_clamped());
            assert_eq!(outcome.applied(), floor);
            assert_eq!(s.panel_interval_secs(panel), floor, "{panel:?}");
        }
    }

    /// No floor may exceed its own default, or the shipped cadence would be
    /// illegal to re-select after any edit.
    #[test]
    fn every_floor_is_at_or_below_its_default() {
        for panel in PanelInterval::ALL {
            let spec = panel.spec();
            assert!(
                spec.floor_secs <= spec.default_secs,
                "{panel:?}: floor {} exceeds default {}",
                spec.floor_secs,
                spec.default_secs
            );
            assert!(spec.floor_secs > 0, "{panel:?}: a zero floor is no floor");
        }
    }

    /// Keys are the on-disk format and are distinct per panel; a collision would
    /// silently make two panels share one setting.
    #[test]
    fn panel_keys_are_unique_and_stable() {
        let keys: Vec<&str> = PanelInterval::ALL.iter().map(|p| p.spec().key).collect();
        assert_eq!(
            keys,
            ["containers", "usage_providers", "azure_cost", "crons"]
        );
        let unique: std::collections::BTreeSet<&str> = keys.iter().copied().collect();
        assert_eq!(unique.len(), keys.len(), "duplicate panel_intervals key");
    }
}
