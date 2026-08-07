//! The scalar preferences the Swift app keeps in `@AppStorage`/UserDefaults.
//!
//! Ground truth is `DevCanopy/Views/Settings/SettingsView.swift` (plus
//! `Models/RefreshInterval.swift`, `Views/Cockpit/CockpitBreakpoints.swift`,
//! `Services/HostMetrics/LocalHostMetricsService.swift`). Semantics are
//! mirrored, not APIs: UserDefaults hands back a zero for an unset key and the
//! Swift side launders that through `RefreshInterval(rawValue:) ?? .default`,
//! so the same "an out-of-range stored value reads as the default" rule is
//! enforced here on deserialize rather than at every call site.
//!
//! Nothing in here is a secret — credentials live in the OS credential store
//! (`crate::secrets`), never in this struct or the file it serialises into.

use serde::{Deserialize, Deserializer, Serialize};

/// The cadences Settings offers, in seconds (`RefreshInterval` in Swift).
pub const REFRESH_INTERVAL_CHOICES: [u32; 3] = [30, 60, 300];

/// `RefreshInterval.default` — 1 minute.
pub const DEFAULT_REFRESH_INTERVAL_SECS: u32 = 60;

/// Inclusive bounds for the CPU-core grid's row span (`coreRowSpan`).
pub const CORE_ROW_SPAN_RANGE: std::ops::RangeInclusive<u8> = 1..=4;

/// The `coreRowSpan` default shared by Settings and `HostMetricsPanel`.
pub const DEFAULT_CORE_ROW_SPAN: u8 = 2;

/// What the cockpit does when host cards no longer fit side by side.
///
/// Mirrors Swift's `HostOverflowMode` (`stack`/`tabs`), including its
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
    /// The persisted spelling — identical to the Swift enum's `rawValue`.
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
    /// OpenClaw gateway URL (non-secret; the bearer token is a credential).
    pub openclaw_gateway_url: String,
    /// Fire an OS notification once when a tracked run transitions into the
    /// `waiting` deployment-protection gate (a human must approve it).
    ///
    /// Swift's `WorkflowDisplayOptions.notifyOnApprovalNeeded`, which is
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
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            refresh_interval_secs: DEFAULT_REFRESH_INTERVAL_SECS,
            core_row_span: DEFAULT_CORE_ROW_SPAN,
            host_overflow_mode: HostOverflowMode::default(),
            azure_monthly_budget_usd: 0.0,
            neon_org_id: String::new(),
            neon_usd_per_cu_hour: 0.0,
            neon_usd_per_gib_month: 0.0,
            sentry_org_slug: String::new(),
            sentry_monthly_event_quota: 0,
            openclaw_gateway_url: String::new(),
            notify_on_approval_needed: true,
            notify_on_service_change: true,
            local_hidden_volume_mounts: Vec::new(),
        }
    }
}

/// `null` or a cadence Settings never offers reads as the default, the way
/// `RefreshInterval(rawValue:) ?? .default` does in Swift.
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
    fn defaults_match_the_swift_app() {
        let s = Settings::default();
        assert_eq!(s.refresh_interval_secs, 60);
        assert_eq!(s.core_row_span, 2);
        assert_eq!(s.host_overflow_mode, HostOverflowMode::Stack);
        assert_eq!(s.azure_monthly_budget_usd, 0.0);
        assert_eq!(s.sentry_monthly_event_quota, 0);
        assert!(s.neon_org_id.is_empty());
        assert!(s.sentry_org_slug.is_empty());
        assert!(s.openclaw_gateway_url.is_empty());
        assert!(s.notify_on_approval_needed);
        assert!(s.local_hidden_volume_mounts.is_empty());
    }

    /// The two preferences with no UI on either side: a store file written
    /// before either existed must still opt *in*, because Swift's
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
            neon_org_id: "org-abc".into(),
            neon_usd_per_cu_hour: 0.175,
            neon_usd_per_gib_month: 0.5,
            sentry_org_slug: "sassy-dog".into(),
            sentry_monthly_event_quota: 50_000,
            openclaw_gateway_url: "https://gateway.example".into(),
            notify_on_approval_needed: false,
            notify_on_service_change: false,
            local_hidden_volume_mounts: vec!["/Volumes/Backup".into()],
        };
        let json = serde_json::to_string(&s).expect("serialize");
        assert_eq!(
            serde_json::from_str::<Settings>(&json).expect("deserialize"),
            s
        );
    }

    #[test]
    fn host_overflow_mode_serialises_as_the_swift_raw_value() {
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
}
