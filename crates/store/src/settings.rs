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

/// The cadences the retired shared Refresh Interval offered, in seconds
/// (`RefreshInterval` in the original). Settings no longer offers it (#615);
/// what is left is the allow-list a stored `refresh_interval_secs` is read
/// through before it seeds the per-source cadences.
pub const REFRESH_INTERVAL_CHOICES: [u32; 3] = [30, 60, 300];

/// `RefreshInterval.default` — 1 minute. Still the default of the three
/// cadences that interval became ([`PanelInterval::GitHub`],
/// [`PanelInterval::Services`], [`PanelInterval::ClaudeRollups`]), so an
/// unconfigured store polls exactly as it always did.
pub const DEFAULT_REFRESH_INTERVAL_SECS: u32 = 60;

/// Inclusive bounds for the CPU-core grid's row span (`coreRowSpan`).
pub const CORE_ROW_SPAN_RANGE: std::ops::RangeInclusive<u8> = 1..=4;

/// The `coreRowSpan` default shared by Settings and `HostMetricsPanel`.
pub const DEFAULT_CORE_ROW_SPAN: u8 = 2;

/// Inclusive bounds, in points, for the gap between two rows of the same
/// in-panel list (`--row-gap` in `app/ui/app.css`).
///
/// The top is 16 because that is `viewmodel::cockpit::SPACING` — the gap
/// *between* cards. A list whose own rows sit further apart than the panels
/// holding them no longer reads as a list, so the outer gap is the natural
/// ceiling rather than a number picked to feel roomy.
pub const ROW_GAP_PX_RANGE: std::ops::RangeInclusive<u8> = 0..=16;

/// No gap at all — rows of one list sit flush.
///
/// This is a **decision**, not `u8::default()` happening to be zero. #363 gave
/// every in-panel list one rhythm and set it to 10, and the panel that renders
/// 21+ rows got taller for it; on a cockpit that lives full-screen on a second
/// monitor, fitting the rows in beats separating them. How much air a list wants
/// is taste rather than correctness, which is why the value is settable at all —
/// and why an operator who preferred #363's answer can type it back in.
pub const DEFAULT_ROW_GAP_PX: u8 = 0;

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
    /// The one GitHub poll that fills both the Repos and the Runners tiles.
    GitHub,
    /// The Services tile's third-party status pages.
    Services,
    /// The Usage panel's Claude token rollups: a local walk of `~/.claude/projects`.
    ClaudeRollups,
    /// The Usage panel's metered providers: Neon, Sentry and Vercel.
    UsageProviders,
    /// The Azure Cost panel's daily export read.
    AzureCost,
    /// The Sentry Crons panel.
    Crons,
    /// The Usage panel's Claude and Codex plan windows (`crates/subscription`).
    PlanUsage,
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
    ///
    /// It is also the identifier the Settings surface sends back
    /// ([`PanelInterval::parse`]), for the same reason [`SecretKey`] has one
    /// name on both sides: a second spelling is a second thing to get wrong.
    ///
    /// [`SecretKey`]: crate::SecretKey
    pub key: &'static str,
    /// Today's hardcoded cadence, so an unconfigured store behaves as it does now.
    pub default_secs: u32,
    /// The shortest cadence this source may be asked for. See
    /// [`PanelInterval::spec`] for why each one is what it is.
    pub floor_secs: u32,
    /// What Settings calls this panel.
    ///
    /// Here rather than beside the other display names in the shell because of
    /// the argument this whole struct is built on: a name that lives away from
    /// the floor it labels is a name that can end up over the wrong floor, and
    /// the row an operator reads is *"this panel, this minimum, this reason"* —
    /// three facts that are only true together.
    pub label: &'static str,
    /// Why this floor is where it is, in the operator's terms — a sentence
    /// fragment that completes "no faster than 5 seconds — …".
    ///
    /// A floor with no stated reason is indistinguishable from an arbitrary
    /// limit, and an operator who cannot see why they are being refused has
    /// only the store file left to argue with. Travelling in the spec is what
    /// keeps this sentence describing *this* number: the prose beside each arm
    /// below says the same thing to whoever edits the floor, and this says it to
    /// whoever hits it.
    pub floor_reason: &'static str,
}

impl PanelInterval {
    /// Every panel that has a settable cadence, in a stable order for UI.
    pub const ALL: [PanelInterval; 8] = [
        PanelInterval::Containers,
        PanelInterval::GitHub,
        PanelInterval::Services,
        PanelInterval::ClaudeRollups,
        PanelInterval::UsageProviders,
        PanelInterval::AzureCost,
        PanelInterval::Crons,
        PanelInterval::PlanUsage,
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
    /// | `GitHub` | 1m | the retired shared `refresh_interval_secs` default |
    /// | `Services` | 1m | the same, formerly riding `github_loop` |
    /// | `ClaudeRollups` | 1m | the same, formerly `usage_loop`'s outer sleep |
    /// | `UsageProviders` | 1h | `usage::PROVIDER_POLL_INTERVAL_SECS` |
    /// | `AzureCost` | 4h | `azurecost::POLL_INTERVAL` |
    /// | Crons | 1h | *shares* `usage::PROVIDER_POLL_INTERVAL_SECS` |
    /// | `PlanUsage` | 15m | new with the source; orca's own default |
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
                label: "Containers/VMs",
                floor_reason:
                    "every pass shells out to docker, podman and tart, and a container list changes on human timescales",
            },
            // The retired shared Refresh Interval (#615) drove this poll, the
            // Services pass and the Claude rollups from one number; it is three
            // cadences now, each defaulting to that interval's 60s. Floor 30s,
            // the shortest the old control offered, so a value someone already
            // chose stays valid. One pass spends several requests per tracked
            // repo (health, counts, a Ready read) plus the org's runners
            // against one PAT's hourly budget, which is why this one is not
            // lower.
            PanelInterval::GitHub => IntervalSpec {
                key: "github",
                default_secs: DEFAULT_REFRESH_INTERVAL_SECS,
                floor_secs: 30,
                label: "GitHub \u{2014} Repos and Runners",
                floor_reason:
                    "every pass spends several requests per tracked repo plus the org's runner list against one token's hourly rate-limit budget",
            },
            // Status pages are public and need no credential, but they are
            // someone else's servers: five or so vendors per pass, each its own
            // request. A vendor's state does not change on a shorter timescale
            // than the page itself is updated, so 30s is already generous.
            PanelInterval::Services => IntervalSpec {
                key: "services",
                default_secs: DEFAULT_REFRESH_INTERVAL_SECS,
                floor_secs: 30,
                label: "Services",
                floor_reason:
                    "every pass asks each watched vendor's public status page, and those are other people's servers that update on the order of minutes",
            },
            // A walk of `~/.claude/projects` on local disk: it costs no money,
            // only a read of every session log, and the logs grow with every
            // conversation. Floor 30s for the same compatibility reason as
            // GitHub's.
            PanelInterval::ClaudeRollups => IntervalSpec {
                key: "claude_rollups",
                default_secs: DEFAULT_REFRESH_INTERVAL_SECS,
                floor_secs: 30,
                label: "Usage \u{2014} Claude token rollups",
                floor_reason:
                    "every pass re-reads every session log under ~/.claude/projects, which grow with each conversation",
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
                label: "Usage — Neon, Sentry, Vercel",
                floor_reason:
                    "three vendor APIs answer on one pass, each against a rate-limit budget, and consumption figures move on the order of hours",
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
                label: "Azure Cost",
                floor_reason:
                    "the export is published about once a day, and every poll mints a SAS and may pull the whole blob, so a shorter cadence buys nothing and moves real egress",
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
                label: "Sentry Crons",
                floor_reason:
                    "it shares Sentry's rate-limit budget with the Usage panel's read, and a monitor's health is a persistence watch rather than a real-time alarm",
            },
            // New with the source, so there is no older constant to mirror.
            // Both numbers are orca's (`main/rate-limits/service/service-types.ts:96-100`):
            // a 15-minute poll because "Claude's usage endpoint has a tight
            // budget and quota is only informational; prefer a recent snapshot
            // over polling into 429s", and a 5-minute floor, which is orca's
            // debounce on a manual refresh. Both endpoints are private, so
            // being a quiet client is the price of reading them at all.
            PanelInterval::PlanUsage => IntervalSpec {
                key: "plan_usage",
                default_secs: 15 * 60,
                floor_secs: 300,
                label: "Usage — Claude and Codex plans",
                floor_reason:
                    "both plan endpoints are private and answer polling with rate limits, and a plan window moves over hours",
            },
        }
    }

    /// The panel the Settings surface named, or `None`.
    ///
    /// Matched against [`IntervalSpec::key`] — the on-disk key doubles as the
    /// wire identifier — so an unrecognised string is a *rejected* command
    /// rather than a silently-ignored save, the same closed-set treatment
    /// `SecretField::parse` gives credentials in the shell.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        PanelInterval::ALL
            .into_iter()
            .find(|panel| panel.spec().key == raw)
    }

    /// Whether this panel may be polled every `secs`, without storing anything.
    ///
    /// **The gate the Settings surface writes through.** A requested cadence is
    /// either accepted as asked or refused with a reason — never quietly moved
    /// to a number nobody chose. [`Settings::set_panel_interval_secs`] still
    /// clamps, and [`Settings::panel_interval_secs`] still clamps on read,
    /// because a hand-edited `store.json` reaches neither this function nor that
    /// setter; but a value that has been through here cannot be clamped by
    /// either, which is what makes "refused, not substituted" true of every path
    /// a human can actually type into.
    ///
    /// # Errors
    ///
    /// [`IntervalRejection::BelowFloor`] when `secs` is under
    /// [`IntervalSpec::floor_secs`]. There is deliberately no upper bound: a
    /// cadence that is too *slow* costs nobody anything and says so on the
    /// panel's own staleness footer.
    pub fn check_secs(self, secs: u32) -> Result<u32, IntervalRejection> {
        if secs < self.spec().floor_secs {
            Err(IntervalRejection::BelowFloor {
                panel: self,
                asked: secs,
            })
        } else {
            Ok(secs)
        }
    }
}

/// Why a requested cadence was not stored.
///
/// One variant today, and an enum rather than a `String` so the refusal is a
/// *value* the caller can test against rather than a sentence it has to match
/// on. The sentence is [`IntervalRejection::user_message`], per the repo-wide
/// `user_message()` convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntervalRejection {
    /// Under the panel's floor. Nothing was written.
    BelowFloor {
        /// The panel whose floor stopped it.
        panel: PanelInterval,
        /// What the operator asked for.
        asked: u32,
    },
}

impl IntervalRejection {
    /// The floor that refused the request.
    #[must_use]
    pub const fn floor_secs(self) -> u32 {
        match self {
            IntervalRejection::BelowFloor { panel, .. } => panel.spec().floor_secs,
        }
    }

    /// What Settings shows the operator.
    ///
    /// Names the panel, the floor, the reason for the floor **and** what was
    /// asked. Dropping the last of those is what would make this read like a
    /// generic hint instead of an answer to something a person just typed.
    #[must_use]
    pub fn user_message(self) -> String {
        match self {
            IntervalRejection::BelowFloor { panel, asked } => {
                let spec = panel.spec();
                format!(
                    "Not saved — {} polls no faster than every {}: {}. You asked for {}.",
                    spec.label,
                    interval_label(spec.floor_secs),
                    spec.floor_reason,
                    interval_label(asked),
                )
            }
        }
    }
}

/// A cadence in seconds, said in words: `10 seconds`, `1 minute`, `4 hours`.
///
/// One ladder, shared by the cadence rows, the refusal message and the refresh
/// interval's picker, because a floor described as `1 hour` in one place and
/// `3600 seconds` in another reads as two different limits. Exact units only —
/// 90 seconds is `90 seconds`, not `1.5 minutes` — since every number this
/// renders is one somebody has to be able to type back in.
#[must_use]
pub fn interval_label(secs: u32) -> String {
    let unit = |count: u32, name: &str| {
        if count == 1 {
            format!("1 {name}")
        } else {
            format!("{count} {name}s")
        }
    };
    if secs >= 3600 && secs.is_multiple_of(3600) {
        unit(secs / 3600, "hour")
    } else if secs >= 60 && secs.is_multiple_of(60) {
        unit(secs / 60, "minute")
    } else {
        unit(secs, "second")
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
    /// **Retired (#615): the shared Refresh Interval, read once and never written.**
    ///
    /// It used to drive the GitHub poll, the Services pass and the Claude
    /// rollups from one number. Each is its own [`PanelInterval`] now, and
    /// [`Settings::seed_cadences_from_legacy_refresh_interval`] copies a
    /// non-default value into all three the first time a store that has one
    /// loads, then empties this field. `skip_serializing` is what makes that
    /// one-shot: the first save after it drops the key from the file, so a
    /// cadence the operator later clears is not seeded again. `None` is "no
    /// such key" and so is anything not in [`REFRESH_INTERVAL_CHOICES`].
    #[serde(
        default,
        rename = "refresh_interval_secs",
        skip_serializing,
        deserialize_with = "de_legacy_refresh_interval_secs"
    )]
    pub legacy_refresh_interval_secs: Option<u32>,
    /// **Retired (#647): the global CPU core rows, read once and never written.**
    ///
    /// The value now lives on each Machines tile ([`crate::DashboardTile::core_row_span`]).
    /// [`crate::Store::open_in`] copies this onto every Machines tile and takes it;
    /// `skip_serializing` is what makes that one-shot, as for
    /// [`Settings::legacy_refresh_interval_secs`]. Clamped into
    /// [`CORE_ROW_SPAN_RANGE`], not dropped, since the intent survives.
    #[serde(
        default,
        rename = "core_row_span",
        skip_serializing,
        deserialize_with = "de_legacy_core_row_span"
    )]
    pub legacy_core_row_span: Option<u8>,
    /// **Retired (#647): the global row spacing, read once and never written.**
    /// Copied onto every tile ([`crate::DashboardTile::row_gap_px`]) the first
    /// time the store loads, then emptied. Clamped into [`ROW_GAP_PX_RANGE`].
    #[serde(
        default,
        rename = "row_gap_px",
        skip_serializing,
        deserialize_with = "de_legacy_row_gap_px"
    )]
    pub legacy_row_gap_px: Option<u8>,
    /// **Retired (#647): the global host-card overflow mode, read once and never
    /// written.** The fallback for [`crate::Store::open_in`]'s copy onto every
    /// Machines tile when the store holds no stored layout to take it from.
    #[serde(default, rename = "host_overflow_mode", skip_serializing)]
    pub legacy_host_overflow_mode: Option<HostOverflowMode>,
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
    /// **Legacy, migration input only.** The pre-v3 global GitHub org the
    /// Runners panel used to poll; `migrate_v2_to_v3` moves it onto
    /// [`crate::VendorAccount::orgs`] and clears it. Nothing else writes it,
    /// and the only remaining reader outside the migration is the poll's
    /// legacy identity, which keeps an *unmigrated* store's Runners panel
    /// alive until its migration completes.
    ///
    /// Kept serializing on purpose: an unmigrated store re-saved by this
    /// build must carry the org forward to the launch that can finally adopt
    /// it, or the retry loses the very value it exists to save.
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
    /// Send a scrubbed report when this app panics.
    ///
    /// **Off, and off is the commitment** — `AGENTS.md`'s "No telemetry or
    /// analytics by default", and #18's opt-in policy. The Settings toggle is
    /// the opt-in and nothing else is: not a DSN being present, not a build
    /// profile, not an inference from any other preference.
    ///
    /// `false` here is a *decision*, not an absence. A store file written
    /// before this key existed reads as off — asserted in
    /// [`a_store_file_without_the_crash_key_stays_opted_out`] rather than left
    /// to `#[serde(default)]`'s behaviour on a `bool` happening to be right,
    /// because "the default did the right thing" is not a property anyone
    /// notices breaking.
    ///
    /// The three-state read (`Unknown` when this could not be loaded at all)
    /// lives in `crashreport::OptIn`, where it can refuse; a `bool` here has no
    /// way to say "we could not look".
    ///
    /// **No field-level `#[serde(default)]`**, deliberately, unlike several of
    /// its neighbours. That attribute would default a missing key to
    /// `bool::default()` — which is `false`, and so happens to be right today —
    /// but it would decouple the on-disk default from [`Settings::default`], and
    /// this is the one preference where the default *is* the promise. The
    /// container's `#[serde(default)]` already fills a missing key from
    /// `Settings::default()`, so there is exactly one place the answer lives and
    /// exactly one place to break.
    pub crash_reporting_enabled: bool,
    /// Read Claude Code's saved sign-in to show the Claude plan's own limit
    /// windows in the Usage panel (`crates/subscription`).
    ///
    /// **Off, and the Settings toggle is the only opt-in**, for the reason
    /// [`Settings::crash_reporting_enabled`] gives plus one of its own: on a
    /// Mac the sign-in is a keychain item another program owns, and reading it
    /// can raise a keychain prompt. A prompt that appears unasked on launch is
    /// not acceptable, so nothing reads it until the operator says so. Same
    /// rule for the serde default: none at field level, so
    /// [`Settings::default`] is the one place the answer lives.
    ///
    /// This is a preference, not a credential. The token itself is never
    /// stored by Solador.
    pub claude_plan_usage_enabled: bool,
    /// Read Codex's saved ChatGPT sign-in (`~/.codex/auth.json`) to show the
    /// Codex plan's limit windows. Off by default, for the same reasons.
    pub codex_plan_usage_enabled: bool,
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
    /// Shared CPU/RAM limits and explicit per-machine overrides.
    pub machine_alerts: crate::machine_alerts::MachineAlerts,
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

    /// Whether this panel's cadence has ever been configured.
    ///
    /// `false` is "never chosen", which is *not* the same as "chosen, and equal
    /// to the default": the first follows the constant if it ever moves and the
    /// second pins the panel to today's number. Settings shows the difference,
    /// and [`Settings::clear_panel_interval`] is how an operator gets back to
    /// the first.
    #[must_use]
    pub fn panel_interval_is_configured(&self, panel: PanelInterval) -> bool {
        self.panel_intervals.contains_key(panel.spec().key)
    }

    /// Set this panel's cadence, reporting whether the floor moved it.
    ///
    /// Returns [`ClampOutcome`] rather than a bare `u32` so the caller cannot
    /// show a floored value as though it were the one requested.
    ///
    /// A value that has been through [`PanelInterval::check_secs`] is never
    /// clamped here — that is the gate the Settings surface writes through, so
    /// what an operator types is either stored as typed or refused out loud.
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

    /// Carries a store's retired shared Refresh Interval over to the three
    /// cadences it became (#615), so nobody's polling silently changes.
    ///
    /// **Seeded only when the value is not the default.** A stored 60 cannot be
    /// told from "never chosen" — `Settings` wrote it on every save — and
    /// seeding it would turn three controls to "configured" for an operator who
    /// chose nothing. Polling is identical either way. A key the store already
    /// holds is left alone.
    ///
    /// **One-shot, by emptying the legacy field**: it is never serialised, so
    /// the first save after this runs removes the key from disk, and a
    /// cadence cleared afterwards falls back to its default rather than being
    /// seeded again. Returns whether it seeded anything.
    pub fn seed_cadences_from_legacy_refresh_interval(&mut self) -> bool {
        let Some(secs) = self.legacy_refresh_interval_secs.take() else {
            return false;
        };
        if secs == DEFAULT_REFRESH_INTERVAL_SECS {
            return false;
        }
        for panel in [
            PanelInterval::GitHub,
            PanelInterval::Services,
            PanelInterval::ClaudeRollups,
        ] {
            self.panel_intervals
                .entry(panel.spec().key.to_owned())
                .or_insert(secs);
        }
        true
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
            legacy_refresh_interval_secs: None,
            legacy_core_row_span: None,
            legacy_row_gap_px: None,
            legacy_host_overflow_mode: None,
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
            // The one preference here that defaults *off*. See the field.
            crash_reporting_enabled: false,
            // Off: reading another program's sign-in is opt-in. See the fields.
            claude_plan_usage_enabled: false,
            codex_plan_usage_enabled: false,
            local_hidden_volume_mounts: Vec::new(),
            // Empty, not pre-filled with the defaults — see the field's note.
            panel_intervals: BTreeMap::new(),
            machine_alerts: crate::machine_alerts::MachineAlerts::default(),
        }
    }
}

/// `null` or a cadence the old control never offered reads as absent, the way
/// `RefreshInterval(rawValue:) ?? .default` did in the original: a hand-edited
/// 7 seeds nothing, rather than seeding three cadences with a number nobody
/// could have chosen.
fn de_legacy_refresh_interval_secs<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<u32>, D::Error> {
    let raw = Option::<u32>::deserialize(d)?;
    Ok(raw.filter(|secs| REFRESH_INTERVAL_CHOICES.contains(secs)))
}

/// Clamped, not defaulted: a stored `9` means "as many rows as possible", and
/// the top of the range is a truer reading of that than the default is.
pub(crate) fn clamp_core_row_span(span: u8) -> u8 {
    span.clamp(*CORE_ROW_SPAN_RANGE.start(), *CORE_ROW_SPAN_RANGE.end())
}

/// Clamped, not defaulted, for [`clamp_core_row_span`]'s reason: a hand-edited
/// `40` means "as much air as this will give me", and snapping back to zero
/// would look like the edit was ignored.
pub(crate) fn clamp_row_gap_px(px: u8) -> u8 {
    px.clamp(*ROW_GAP_PX_RANGE.start(), *ROW_GAP_PX_RANGE.end())
}

fn de_legacy_core_row_span<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u8>, D::Error> {
    Ok(Option::<u8>::deserialize(d)?.map(clamp_core_row_span))
}

fn de_legacy_row_gap_px<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u8>, D::Error> {
    Ok(Option::<u8>::deserialize(d)?.map(clamp_row_gap_px))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_original_app() {
        let s = Settings::default();
        assert_eq!(s.legacy_refresh_interval_secs, None);
        assert_eq!(s.legacy_core_row_span, None);
        assert_eq!(s.legacy_row_gap_px, None);
        assert_eq!(s.legacy_host_overflow_mode, None);
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
        assert!(
            !s.crash_reporting_enabled,
            "crash reporting is off by default and the toggle is the only opt-in"
        );
        assert!(!s.claude_plan_usage_enabled && !s.codex_plan_usage_enabled);
        assert!(s.local_hidden_volume_mounts.is_empty());
    }

    /// A store file written before crash reporting existed — which is every
    /// store file in the wild — must read as opted **out**.
    ///
    /// Asserted rather than assumed. `#[serde(default)]` on a `bool` does give
    /// `false`, and `false` is the commitment; but "the language default
    /// happened to match the policy" is not something anyone would notice
    /// changing, and the whole point of this feature is that the default is the
    /// promise. The neighbouring notify keys default the *other* way, so this
    /// file already proves that a default here is a choice, not a convention.
    #[test]
    fn a_store_file_without_the_crash_key_stays_opted_out() {
        for stored in [
            r#"{}"#,
            r#"{"github_org":"acme"}"#,
            // The shape of a real pre-feature store: several keys, none of them
            // this one.
            r#"{"refresh_interval_secs":300,"github_org":"acme","notify_on_service_change":false}"#,
        ] {
            let s: Settings = serde_json::from_str(stored).expect("deserialize");
            assert!(
                !s.crash_reporting_enabled,
                "an upgrade must not opt anyone in: {stored}"
            );
        }
        // And an explicit yes is still honoured, or the toggle does nothing.
        let opted_in: Settings =
            serde_json::from_str(r#"{"crash_reporting_enabled":true}"#).expect("deserialize");
        assert!(opted_in.crash_reporting_enabled);
    }

    /// Reading another program's saved sign-in is opt-in per provider, and
    /// a store file written before the keys existed — every one in the wild —
    /// reads as **off**. The same argument as the crash key: an upgrade that
    /// started reading Claude Code's keychain item would raise a prompt nobody
    /// asked for.
    #[test]
    fn a_store_file_without_the_plan_usage_keys_reads_no_sign_in() {
        for stored in [
            r#"{}"#,
            r#"{"crash_reporting_enabled":true,"neon_org_id":"org-1"}"#,
        ] {
            let s: Settings = serde_json::from_str(stored).expect("deserialize");
            assert!(!s.claude_plan_usage_enabled, "{stored}");
            assert!(!s.codex_plan_usage_enabled, "{stored}");
        }
        let claude_only: Settings =
            serde_json::from_str(r#"{"claude_plan_usage_enabled":true}"#).expect("deserialize");
        assert!(claude_only.claude_plan_usage_enabled);
        assert!(
            !claude_only.codex_plan_usage_enabled,
            "the two sources are independent switches"
        );
    }

    /// The two preferences with no UI on either side: a store file written
    /// before either existed must still opt *in*, because the original's
    /// `WorkflowDisplayOptions()` default is `true` and an upgrade that
    /// silently disabled the alert would look exactly like the feature not
    /// working. The same argument covers the service-change alert, which nobody
    /// can turn back on from the app if an upgrade turns it off.
    #[test]
    fn a_store_file_without_the_notify_keys_still_opts_in() {
        let s: Settings = serde_json::from_str(r#"{"github_org":"acme"}"#).expect("deserialize");
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
            legacy_refresh_interval_secs: None,
            legacy_core_row_span: None,
            legacy_row_gap_px: None,
            legacy_host_overflow_mode: None,
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
            crash_reporting_enabled: true,
            claude_plan_usage_enabled: true,
            codex_plan_usage_enabled: true,
            local_hidden_volume_mounts: vec!["/Volumes/Backup".into()],
            panel_intervals: BTreeMap::from([
                ("containers".to_owned(), 30),
                ("azure_cost".to_owned(), 6 * 3600),
            ]),
            machine_alerts: crate::machine_alerts::MachineAlerts::default(),
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
        assert_eq!(s.legacy_host_overflow_mode, Some(HostOverflowMode::Stack));
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
            r#"{"github_org":"acme","a_field_from_a_newer_build":{"nested":true}}"#,
        )
        .expect("deserialize");
        assert_eq!(s.github_org, "acme");
    }

    #[test]
    fn out_of_range_legacy_refresh_interval_reads_as_absent() {
        for raw in ["0", "45", "null"] {
            let json = format!(r#"{{"refresh_interval_secs":{raw}}}"#);
            let s: Settings = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(s.legacy_refresh_interval_secs, None, "raw {raw}");
        }
        for secs in REFRESH_INTERVAL_CHOICES {
            let json = format!(r#"{{"refresh_interval_secs":{secs}}}"#);
            let s: Settings = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(s.legacy_refresh_interval_secs, Some(secs));
        }
    }

    /// The retired shared Refresh Interval becomes the three cadences it drove,
    /// so nobody's polling changes (#615).
    #[test]
    fn a_non_default_legacy_refresh_interval_seeds_the_three_cadences_it_drove() {
        for secs in [30, 300] {
            let mut s: Settings =
                serde_json::from_str(&format!(r#"{{"refresh_interval_secs":{secs}}}"#))
                    .expect("deserialize");
            assert!(s.seed_cadences_from_legacy_refresh_interval());
            for panel in [
                PanelInterval::GitHub,
                PanelInterval::Services,
                PanelInterval::ClaudeRollups,
            ] {
                assert_eq!(s.panel_interval_secs(panel), secs, "{panel:?}");
                assert!(s.panel_interval_is_configured(panel), "{panel:?}");
            }
            // Only those three: the others never rode the shared interval.
            assert_eq!(s.panel_intervals.len(), 3);
        }
    }

    /// 60 is what every save wrote, so it cannot be told from "never chosen";
    /// seeding it would mark three controls configured that nobody touched.
    #[test]
    fn a_default_legacy_refresh_interval_seeds_nothing() {
        let mut s: Settings =
            serde_json::from_str(r#"{"refresh_interval_secs":60}"#).expect("deserialize");
        assert!(!s.seed_cadences_from_legacy_refresh_interval());
        assert!(s.panel_intervals.is_empty());
        assert_eq!(s.legacy_refresh_interval_secs, None);
    }

    /// One-shot: the field is emptied by the seed and never written, so a
    /// cadence the operator clears afterwards is not seeded again on the next
    /// load.
    #[test]
    fn seeding_is_one_shot_and_the_legacy_key_is_never_written_back() {
        let mut s: Settings =
            serde_json::from_str(r#"{"refresh_interval_secs":300}"#).expect("deserialize");
        assert!(s.seed_cadences_from_legacy_refresh_interval());
        assert!(
            !s.seed_cadences_from_legacy_refresh_interval(),
            "a second pass must find nothing left to seed"
        );
        s.clear_panel_interval(PanelInterval::GitHub);
        let json = serde_json::to_string(&s).expect("serialize");
        assert!(
            !json.contains("refresh_interval_secs"),
            "the retired key must leave the file on the first save: {json}"
        );
        let mut reloaded: Settings = serde_json::from_str(&json).expect("deserialize");
        assert!(!reloaded.seed_cadences_from_legacy_refresh_interval());
        assert!(!reloaded.panel_interval_is_configured(PanelInterval::GitHub));
        assert!(reloaded.panel_interval_is_configured(PanelInterval::Services));
    }

    /// A cadence the store already holds is the operator's, not the seed's.
    #[test]
    fn seeding_never_overwrites_a_cadence_already_chosen() {
        let mut s: Settings = serde_json::from_str(
            r#"{"refresh_interval_secs":300,"panel_intervals":{"github":30}}"#,
        )
        .expect("deserialize");
        s.seed_cadences_from_legacy_refresh_interval();
        assert_eq!(s.panel_interval_secs(PanelInterval::GitHub), 30);
        assert_eq!(s.panel_interval_secs(PanelInterval::Services), 300);
    }

    /// Every value the old control offered must stay a legal cadence for the
    /// panels it seeds, or seeding would hand the store a number its own floor
    /// then moves.
    #[test]
    fn every_old_choice_clears_the_floor_of_every_cadence_it_seeds() {
        for panel in [
            PanelInterval::GitHub,
            PanelInterval::Services,
            PanelInterval::ClaudeRollups,
        ] {
            for secs in REFRESH_INTERVAL_CHOICES {
                assert!(panel.check_secs(secs).is_ok(), "{panel:?} at {secs}s");
            }
            assert_eq!(panel.spec().default_secs, DEFAULT_REFRESH_INTERVAL_SECS);
        }
    }

    #[test]
    fn out_of_range_legacy_core_row_span_is_clamped() {
        let cases = [
            ("0", Some(1)),
            ("9", Some(4)),
            ("null", None),
            ("1", Some(1)),
        ];
        for (raw, want) in cases {
            let json = format!(r#"{{"core_row_span":{raw}}}"#);
            let s: Settings = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(s.legacy_core_row_span, want, "raw {raw}");
        }
    }

    #[test]
    fn out_of_range_legacy_row_gap_is_clamped() {
        let cases = [
            ("40", Some(16)),
            ("0", Some(0)),
            ("7", Some(7)),
            ("null", None),
        ];
        for (raw, want) in cases {
            let json = format!(r#"{{"row_gap_px":{raw}}}"#);
            let s: Settings = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(s.legacy_row_gap_px, want, "raw {raw}");
        }
    }

    /// The three retired view keys are read and never written back.
    #[test]
    fn the_retired_view_keys_are_never_serialised() {
        let s: Settings = serde_json::from_str(
            r#"{"core_row_span":3,"row_gap_px":5,"host_overflow_mode":"tabs"}"#,
        )
        .expect("deserialize");
        assert_eq!(s.legacy_core_row_span, Some(3));
        let json = serde_json::to_string(&s).expect("serialize");
        for key in ["core_row_span", "row_gap_px", "host_overflow_mode"] {
            assert!(
                !json.contains(key),
                "{key} must not be written back: {json}"
            );
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
            r#"{"refresh_interval_secs":30,"github_org":"acme","neon_org_id":"org-abc"}"#,
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
        assert_eq!(s.legacy_refresh_interval_secs, Some(30));
        assert_eq!(s.github_org, "acme");
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

    /// The gate the Settings surface writes through: at or above the floor is
    /// accepted **as asked**, below it is refused and nothing is stored.
    #[test]
    fn check_secs_accepts_at_the_floor_and_refuses_below_it() {
        for panel in PanelInterval::ALL {
            let floor = panel.spec().floor_secs;
            assert_eq!(panel.check_secs(floor), Ok(floor), "{panel:?}");
            assert_eq!(
                panel.check_secs(floor * 3),
                Ok(floor * 3),
                "{panel:?}: an accepted value comes back unchanged, never rounded"
            );
            assert_eq!(
                panel.check_secs(floor - 1),
                Err(IntervalRejection::BelowFloor {
                    panel,
                    asked: floor - 1,
                }),
                "{panel:?}"
            );
            assert_eq!(
                panel.check_secs(0),
                Err(IntervalRejection::BelowFloor { panel, asked: 0 }),
                "{panel:?}: zero is a request, not an unset field"
            );
        }
    }

    /// **The property that makes "refused, not clamped" true.** Anything
    /// `check_secs` accepts, the setter stores untouched — so no value an
    /// operator types can arrive on disk as a different number without them
    /// being told.
    #[test]
    fn a_checked_value_is_never_clamped_by_the_setter() {
        let mut s = Settings::default();
        for panel in PanelInterval::ALL {
            let spec = panel.spec();
            for asked in [spec.floor_secs, spec.floor_secs + 1, spec.default_secs] {
                let checked = panel.check_secs(asked).expect("at or above the floor");
                let outcome = s.set_panel_interval_secs(panel, checked);
                assert_eq!(outcome, ClampOutcome::Applied(asked), "{panel:?}");
                assert_eq!(s.panel_interval_secs(panel), asked, "{panel:?}");
            }
        }
    }

    /// A refusal has to be readable by whoever hit it: which panel, which floor,
    /// why that floor, and what they asked for.
    #[test]
    fn a_refusal_names_the_panel_the_floor_the_reason_and_the_request() {
        let rejection = PanelInterval::AzureCost
            .check_secs(30)
            .expect_err("30s is below the 1h floor");
        assert_eq!(rejection.floor_secs(), 3600);
        let message = rejection.user_message();
        for expected in [
            "Azure Cost",
            "1 hour",
            "published about once a day",
            "30 seconds",
        ] {
            assert!(
                message.contains(expected),
                "{expected:?} missing from {message:?}"
            );
        }
    }

    /// Every panel states all five facts, and the two new ones are not blank —
    /// an unnamed panel or an unexplained floor is a row an operator cannot act
    /// on.
    #[test]
    fn every_panel_states_a_label_and_why_its_floor_exists() {
        for panel in PanelInterval::ALL {
            let spec = panel.spec();
            assert!(!spec.label.is_empty(), "{panel:?}: unnamed");
            assert!(
                !spec.floor_reason.is_empty(),
                "{panel:?}: unexplained floor"
            );
            assert!(
                !spec.floor_reason.ends_with('.'),
                "{panel:?}: the reason completes a sentence, so it must not end one"
            );
            // It is spliced in after one em dash already ("No faster than 1
            // hour — …"), and a second inside it reads as a sentence that has
            // lost its way.
            assert!(
                !spec.floor_reason.contains('—'),
                "{panel:?}: the row already spends its em dash before this fragment"
            );
            assert!(
                panel
                    .check_secs(spec.floor_secs - 1)
                    .expect_err("below the floor")
                    .user_message()
                    .contains(spec.label),
                "{panel:?}: a refusal must name the panel it is about"
            );
        }
    }

    /// The Settings surface sends the on-disk key back; an unknown one is
    /// refused rather than resolved to a neighbour.
    #[test]
    fn a_panel_parses_from_its_key_and_nothing_else() {
        for panel in PanelInterval::ALL {
            assert_eq!(PanelInterval::parse(panel.spec().key), Some(panel));
        }
        for unknown in ["", "hosts", "Containers", "azure-cost", "a_newer_build"] {
            assert_eq!(PanelInterval::parse(unknown), None, "{unknown:?}");
        }
    }

    /// "Never configured" and "configured to today's default" are different
    /// states, and the row an operator reads has to tell them apart.
    #[test]
    fn configured_is_distinct_from_holding_the_default_value() {
        let mut s = Settings::default();
        assert!(!s.panel_interval_is_configured(PanelInterval::Containers));
        s.set_panel_interval_secs(
            PanelInterval::Containers,
            PanelInterval::Containers.spec().default_secs,
        );
        assert!(
            s.panel_interval_is_configured(PanelInterval::Containers),
            "an explicit choice equal to the default is still a choice"
        );
        s.clear_panel_interval(PanelInterval::Containers);
        assert!(!s.panel_interval_is_configured(PanelInterval::Containers));
    }

    #[test]
    fn interval_label_says_exact_units_and_nothing_else() {
        let cases = [
            (1, "1 second"),
            (5, "5 seconds"),
            (30, "30 seconds"),
            (60, "1 minute"),
            (90, "90 seconds"),
            (300, "5 minutes"),
            (3600, "1 hour"),
            (5400, "90 minutes"),
            (4 * 3600, "4 hours"),
        ];
        for (secs, want) in cases {
            assert_eq!(interval_label(secs), want, "{secs}");
        }
        // Every number this app can put in front of an operator round-trips
        // through the ladder rather than falling off it.
        for panel in PanelInterval::ALL {
            let spec = panel.spec();
            assert!(!interval_label(spec.floor_secs).is_empty());
            assert!(!interval_label(spec.default_secs).is_empty());
        }
    }

    /// Keys are the on-disk format and are distinct per panel; a collision would
    /// silently make two panels share one setting.
    #[test]
    fn panel_keys_are_unique_and_stable() {
        let keys: Vec<&str> = PanelInterval::ALL.iter().map(|p| p.spec().key).collect();
        assert_eq!(
            keys,
            [
                "containers",
                "github",
                "services",
                "claude_rollups",
                "usage_providers",
                "azure_cost",
                "crons",
                "plan_usage"
            ]
        );
        let unique: std::collections::BTreeSet<&str> = keys.iter().copied().collect();
        assert_eq!(unique.len(), keys.len(), "duplicate panel_intervals key");
    }
}
