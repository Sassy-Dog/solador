//! Which third-party services we watch, and the transitions worth a banner.
//!
//! The cockpit's availability verdict is only true while someone is looking at
//! it, and during an outage the whole point is to stop looking. This is the
//! other half: the app notices that GitHub went down — or came back — and says
//! so once.
//!
//! Everything here is pure. [`StatusWatch::observe`] takes one pass's readings
//! and answers with the notices it produced; delivery, the part that needs a
//! live `AppHandle` and an OS willing to show a banner, lives in `main.rs`. Same
//! split as [`crate::github::notify`], for the same reason: the rule that
//! decides *whether* to alert is testable without a notification centre.
//!
//! Three rules, inherited from `ApprovalWatch`:
//!
//! **Transition, not state.** GitHub Actions stayed in `major_outage` for hours
//! on 2026-08-06. One banner when it started and one when it ended is a signal;
//! one every sixty seconds is noise.
//!
//! **The first reading only seeds.** Launching mid-outage must not announce an
//! outage that was already under way.
//!
//! **The baseline advances even when the preference is off.** Turning
//! notifications off and back on must not replay everything that happened in
//! between.
//!
//! And one that is this module's own, because the source can fail in a way a
//! repo list cannot:
//!
//! **Unknown is not a transition.** A statuspage we could not reach is not a
//! status, so an unreadable pass leaves the baseline exactly where it was.
//! Treating `None → Operational` as a recovery would fire a "GitHub is back!"
//! every time a CDN blip resolved, having never said it was down.
//! `ApprovalWatch` documents the mirror-image wart it chose to live with (an
//! unreachable repo re-alerting on its return); here the source is a single
//! endpoint rather than one call per repo, so the honest reading is available
//! and worth taking.

use crate::panel::Configured;
use serde_json::{json, Value};
use servicestatus::{ComponentStatus, Incident, ServiceStatus};
use std::collections::BTreeMap;
use store::{CredentialStore, SecretKey, StatusVendor, Store, VendorKind};
use uuid::Uuid;
use viewmodel::cockpit::PanelKind;
use viewmodel::color;

/// A service whose availability the cockpit watches.
///
/// **Not a closed set.** The five named variants are the vendors this build
/// ships an adapter for; [`Custom`](ServiceId::Custom) is an operator-added
/// status page (#294), keyed by its store record. Which of them are actually
/// watched is [`active_vendors`]'s answer, derived from configuration — this
/// enum names what *can* be watched, never what is.
///
/// `BTreeMap`-keyed rather than `HashMap`, so the notices a pass emits come out
/// in a stable order — three banners in a different sequence on every launch
/// would be a diff nobody asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ServiceId {
    GitHub,
    Anthropic,
    Vercel,
    Neon,
    Azure,
    /// An operator-added Atlassian Statuspage, identified by its
    /// [`StatusVendor::id`]. It names itself from its record rather than from
    /// this build, which is why [`label`](Self::label) and
    /// [`subject`](Self::subject) have nothing to say about it.
    Custom(Uuid),
}

impl ServiceId {
    /// The vendors this build ships an adapter for, in the order the Services
    /// panel lists them — roughly by how loudly this stack notices each one
    /// going down.
    ///
    /// **This is not the watched set.** It used to be, and that was the bug
    /// #284 exists to fix: five vendors hardcoded here are one operator's
    /// stack, shipped to everyone. [`active_vendors`] derives the watched set
    /// from configuration; this array is the menu it derives *from*, and the
    /// order it derives in.
    pub const ALL: [ServiceId; 5] = [
        ServiceId::GitHub,
        ServiceId::Anthropic,
        ServiceId::Vercel,
        ServiceId::Neon,
        ServiceId::Azure,
    ];

    /// The stable key the frontend addresses a row by. Never the label: a
    /// display name is free to change and an id is not.
    #[must_use]
    pub fn id(self) -> String {
        match self {
            ServiceId::GitHub => "github".to_owned(),
            ServiceId::Anthropic => "anthropic".to_owned(),
            ServiceId::Vercel => "vercel".to_owned(),
            ServiceId::Neon => "neon".to_owned(),
            ServiceId::Azure => "azure".to_owned(),
            // The record's id, which is the only stable name an operator-added
            // vendor has — the same `vendor-{uuid}` spelling `SecretKey`
            // already uses for a per-account credential.
            ServiceId::Custom(id) => format!("vendor-{id}"),
        }
    }

    /// The vendor, for the banner's title. Short: a notification is glanced at.
    ///
    /// `None` for [`Custom`](Self::Custom): this build does not name an
    /// operator's vendor, their record does, and [`ActiveVendor::label`] is the
    /// one place every renderer reads a label from. Answering with the id here
    /// would put `vendor-9f3c…` in a notification title, which is a fabricated
    /// name wearing a true one's clothes.
    #[must_use]
    pub fn label(self) -> Option<&'static str> {
        match self {
            ServiceId::GitHub => Some("GitHub"),
            ServiceId::Anthropic => Some("Anthropic"),
            ServiceId::Vercel => Some("Vercel"),
            ServiceId::Neon => Some("Neon"),
            ServiceId::Azure => Some("Azure"),
            ServiceId::Custom(_) => None,
        }
    }

    /// The specific thing we watch, for the banner's body. Not the vendor —
    /// "GitHub is operational again" would overclaim from one component, and
    /// every one of these is a single component of a much larger service.
    ///
    /// `None` for [`Custom`](Self::Custom), for [`label`](Self::label)'s reason.
    #[must_use]
    pub fn subject(self) -> Option<&'static str> {
        match self {
            ServiceId::GitHub => Some("GitHub Actions"),
            ServiceId::Anthropic => Some("the Claude API"),
            ServiceId::Vercel => Some("Vercel Builds"),
            ServiceId::Neon => Some("Neon"),
            ServiceId::Azure => Some("Azure"),
            ServiceId::Custom(_) => None,
        }
    }

    /// Whether this vendor can say "everything is fine".
    ///
    /// Azure alone cannot: its feed lists active incidents and never publishes
    /// health, so a quiet feed is *no known incidents* rather than operational.
    /// The panel words its healthy row differently for that reason, and the
    /// distinction is here rather than in the renderer so there is one place to
    /// read it from. An operator-added vendor is an Atlassian Statuspage, which
    /// publishes a component status, so it answers like the other four.
    #[must_use]
    pub fn publishes_health(self) -> bool {
        !matches!(self, ServiceId::Azure)
    }
}

/// One vendor whose availability the cockpit watches this pass.
///
/// Carries its own strings rather than deriving them from
/// [`service`](Self::service), because an operator-added vendor has none in
/// this build: its label and the component it watches are the operator's, and
/// this is where every renderer reads them from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveVendor {
    /// The stable key the watch and the panel address this vendor by.
    pub service: ServiceId,
    /// The vendor, for the row and the banner's title.
    pub label: String,
    /// The specific component being watched, for the banner's body.
    pub subject: String,
    /// Where its status is read from.
    pub source: VendorSource,
}

/// How a watched vendor's status is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VendorSource {
    /// One of the vendors this build ships an adapter for — see [`read`],
    /// which knows both the endpoint *and* which component of it matters.
    Builtin,
    /// An operator-added Atlassian Statuspage (#294): the page they probed and
    /// the component they picked out of it. Both halves, because a vendor is
    /// not a URL — `CLAUDE_API_COMPONENT` is the API rather than `claude.ai`
    /// for exactly that reason, and that judgement is theirs to make.
    Statuspage {
        base_url: String,
        component_id: String,
    },
}

impl ActiveVendor {
    /// One of [`ServiceId::ALL`], watched. `None` for a
    /// [`ServiceId::Custom`], which this build cannot name — those are built
    /// from their record by [`ActiveVendor::from_record`].
    fn builtin(service: ServiceId) -> Option<Self> {
        Some(ActiveVendor {
            service,
            label: service.label()?.to_owned(),
            subject: service.subject()?.to_owned(),
            source: VendorSource::Builtin,
        })
    }

    /// An operator-added vendor, as its record describes it.
    fn from_record(vendor: &StatusVendor) -> Self {
        let label = vendor.label.trim();
        let component = vendor.component_label.trim();
        ActiveVendor {
            service: ServiceId::Custom(vendor.id),
            label: label.to_owned(),
            // The component, qualified by the vendor, for the same reason the
            // built-in subjects are ("GitHub Actions", not "GitHub"): a banner
            // reading "API: major outage" names nothing. When the operator
            // called the component after the vendor, or left it blank, the
            // vendor's own name is the whole of it — "Neon Neon" is not a
            // second fact.
            subject: if component.is_empty() || component.eq_ignore_ascii_case(label) {
                label.to_owned()
            } else {
                format!("{label} {component}")
            },
            source: VendorSource::Statuspage {
                base_url: vendor.base_url.clone(),
                component_id: vendor.component_id.clone(),
            },
        }
    }
}

/// Which vendors' availability this cockpit watches — **derived from
/// configuration, never shipped**.
///
/// A vendor is watched when that vendor's data is already in the cockpit. A
/// stranger with one token sees one tile; the operator this app was written on
/// sees the same five they saw before, because all five are configured. Nobody
/// gets a shipped opinion — the same rule `CLAUDE.md` records for container
/// grouping, where a shipped example rule *"silently groups a stranger's
/// containers by a rule they never wrote"*. A shipped vendor list is that
/// error with a different noun.
///
/// **Per vendor, not per account.** A status page is one page however many
/// credentials point at it, so three GitHub accounts derive one tile. That
/// falls out of the shape: each vendor contributes a single [`Configured`], and
/// a single `Configured` cannot produce a second tile.
///
/// **`Unknown` is not `Absent`.** A credential store that refused to answer has
/// not told us there is no token, so its vendor is left out of this pass rather
/// than derived from a guess — and, because
/// [`StatusWatch::observe`] forgets whatever leaves the pass, the pass that
/// gets an answer seeds instead of alerting. A locked keychain is silent in
/// both directions, which is the only honest thing it can be.
///
/// `claude_usage_present` is Anthropic's trigger, and it is the named
/// exception: there is no Anthropic credential to look for, so the signal is
/// whether the Usage panel found Claude rollups to show. It must be an answer
/// the caller has actually established — "not read yet" is `false`, so the tile
/// appears when the data does rather than ahead of it.
///
/// Operator-added vendors (#294) are appended after the derived ones, in the
/// order the operator arranged them, and a disabled one is not watched.
///
/// **Re-derived every pass**, never captured at startup: that is what makes an
/// add, a remove or an enable-toggle in Settings apply on the next poll rather
/// than on the next launch — the same rule `poll_github` follows for the token
/// it fetches with. [`ServiceStatuses::watching`] is where the answer lands,
/// and [`StatusWatch::observe`] is what forgets a vendor that has left it.
pub fn active_vendors(
    store: &Store,
    credentials: &dyn CredentialStore,
    claude_usage_present: bool,
) -> Vec<ActiveVendor> {
    // In `ServiceId::ALL`'s order, so upgrading to a derived list does not also
    // reshuffle the rows of an operator for whom nothing else changed.
    let derived = [
        (ServiceId::GitHub, github_configured(store, credentials)),
        (
            ServiceId::Anthropic,
            // The one vendor with no credential: usage data is the evidence.
            if claude_usage_present {
                Configured::Present
            } else {
                Configured::Absent
            },
        ),
        (
            ServiceId::Vercel,
            credential_configured(credentials, SecretKey::VercelApiToken),
        ),
        (
            ServiceId::Neon,
            credential_configured(credentials, SecretKey::NeonApiKey),
        ),
        (ServiceId::Azure, azure_configured(store)),
    ];

    let mut vendors: Vec<ActiveVendor> = derived
        .into_iter()
        // `is_present`, never `!is_absent`: `Unknown` is neither.
        .filter(|(_, configured)| configured.is_present())
        .filter_map(|(service, _)| ActiveVendor::builtin(service))
        .collect();

    vendors.extend(
        store
            .status_vendors()
            .iter()
            .filter(|vendor| vendor.enabled)
            .map(ActiveVendor::from_record),
    );
    vendors
}

/// Whether GitHub is configured — **one answer for the vendor**, however many
/// accounts hold a credential.
///
/// A fold over the v2 account list (#288), which is what keeps the cardinality
/// right: this returns a single [`Configured`], so three GitHub accounts cannot
/// produce a second tile. Disabled accounts *"stay configured but are not
/// polled"*, so a vendor whose every account is switched off has nothing in the
/// cockpit and is not watched.
///
/// The credential read is the fallback for **no GitHub account at all**, not
/// for "none enabled": that is a v1 store whose `migrate_v1_to_v2` has not run,
/// and the reason it has not run may be the very credential store this asks.
/// `Absent` and `Unknown` are the two answers that distinguishes, and collapsing
/// them here would let a locked keychain read as "there is no GitHub".
fn github_configured(store: &Store, credentials: &dyn CredentialStore) -> Configured {
    let mut accounts = store
        .accounts()
        .iter()
        .filter(|account| account.vendor == VendorKind::GitHub)
        .peekable();
    if accounts.peek().is_none() {
        return credential_configured(credentials, SecretKey::GitHubAccessToken);
    }
    if accounts.any(|account| account.enabled) {
        Configured::Present
    } else {
        Configured::Absent
    }
}

/// Whether a credential is stored, keeping "there is none" apart from "we could
/// not ask" — the same three-way read `main.rs`'s `read_credential` makes, minus
/// the value, which this has no reason to hold.
fn credential_configured(credentials: &dyn CredentialStore, key: SecretKey) -> Configured {
    match credentials.secret(key) {
        Ok(Some(value)) if !value.trim().is_empty() => Configured::Present,
        Ok(_) => Configured::Absent,
        Err(e) => {
            // The account name only — `SecretError` is value-free by
            // construction and this must stay that way.
            eprintln!("could not read a stored credential: {e}");
            Configured::Unknown
        }
    }
}

/// Whether the Azure cost export's address is configured.
///
/// Azure is the one derived vendor with no credential to look for: the panel
/// mints a container-scoped SAS per poll from the operator's own `az` session
/// and stores nothing. Its evidence is the export's address, and **both halves
/// of it** — `poll_azure` gates on the pair, so an account without a container
/// is a panel with no Azure data in it.
fn azure_configured(store: &Store) -> Configured {
    let settings = store.settings();
    if settings.azure_storage_account.trim().is_empty()
        || settings.azure_cost_container.trim().is_empty()
    {
        Configured::Absent
    } else {
        Configured::Present
    }
}

/// One pass's reading for one service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading<'a> {
    pub service: ServiceId,
    /// How to name this vendor in a banner — from its record for an
    /// operator-added vendor, from the table for one of this build's own.
    ///
    /// Carried rather than looked up from [`Reading::service`], because
    /// [`ServiceId::label`] has no answer for a [`ServiceId::Custom`] and a
    /// notice must never be worded from a name this build invented.
    pub label: &'a str,
    /// The specific component being watched, for the banner's body. See
    /// [`Reading::label`] for why it travels with the reading.
    pub subject: &'a str,
    /// `None` when the statuspage could not be read, or answered with a status
    /// word this build does not recognise. Not a status — see the module doc.
    pub status: Option<ComponentStatus>,
    /// The active incident's name, when the vendor published one.
    pub incident: Option<&'a str>,
}

/// One notification, already worded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusNotice {
    /// `"GitHub · recovered"` / `"GitHub · major outage"`.
    pub title: String,
    /// `"GitHub Actions is operational again."`.
    pub body: String,
}

impl StatusNotice {
    fn new(reading: &Reading<'_>, previous: ComponentStatus, current: ComponentStatus) -> Self {
        if current == ComponentStatus::Operational {
            return Self {
                title: format!("{} · recovered", reading.label),
                body: format!(
                    "{} is operational again, after {}.",
                    reading.subject,
                    previous.label()
                ),
            };
        }
        let incident = reading
            .incident
            .map(|name| format!(" Incident: {name}."))
            .unwrap_or_default();
        Self {
            title: format!("{} · {}", reading.label, current.label()),
            body: format!("{}: {}.{incident}", reading.subject, current.label()),
        }
    }
}

/// The last *known* status per service — the memory that turns a repeated
/// reading into a one-off event.
///
/// There is no global `seeded` flag, unlike [`crate::github::notify::ApprovalWatch`]:
/// seeding is per service, and "no previous entry for this service" already says
/// it. That matters once more than one vendor is watched, because they are added
/// to the map at whatever pass each one first answers — a global flag would let
/// the second vendor's very first reading fire a banner.
///
/// The key set follows the pass, so it holds a baseline only for the vendors
/// currently being watched. See [`observe`](Self::observe).
#[derive(Debug, Default)]
pub struct StatusWatch {
    seen: BTreeMap<ServiceId, ComponentStatus>,
}

impl StatusWatch {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Diff one pass's readings against the last, and answer with the notices to
    /// deliver.
    ///
    /// `enabled` is the store's `notify_on_service_change`, re-read every pass so
    /// a change applies without a relaunch. It suppresses the *notices*, never
    /// the bookkeeping.
    ///
    /// A vendor **absent from `readings`** is one this watch is no longer
    /// watching — the vendor list is the operator's (#284), so it shrinks when
    /// they remove one — and its baseline is dropped with it. Adding that
    /// vendor back seeds afresh, because what its page said while nobody was
    /// looking is *unknown*, not the last value we happened to hold: alerting
    /// off a stale baseline announces a transition nobody had, for a state
    /// that was true before the vendor was added.
    ///
    /// Membership is **presence in the pass, not the reading in it**. A vendor
    /// whose page could not be read is still watched: it arrives with
    /// `status: None`, keeps its baseline, and the next successful read is
    /// compared against the last thing we actually knew. Forgetting on an
    /// unreadable pass instead would swallow the recovery that follows it.
    pub fn observe(&mut self, readings: &[Reading<'_>], enabled: bool) -> Vec<StatusNotice> {
        self.seen
            .retain(|service, _| readings.iter().any(|r| r.service == *service));

        let mut notices = Vec::new();
        for reading in readings {
            // An unreadable page leaves the baseline untouched, so the next
            // successful read is compared against the last thing we actually
            // knew rather than against nothing.
            let Some(current) = reading.status else {
                continue;
            };
            let previous = self.seen.insert(reading.service, current);
            // Outside the `enabled` check on purpose: the insert above has
            // already happened, so a disabled stretch advances the baseline and
            // does not replay on re-enable.
            if !enabled {
                continue;
            }
            match previous {
                // First known reading for this service: seed, say nothing.
                None => {}
                Some(previous) if previous == current => {}
                Some(previous) => notices.push(StatusNotice::new(reading, previous, current)),
            }
        }
        notices
    }
}

/// One vendor's last read, plus why the last refresh did not happen.
#[derive(Debug, Default, Clone)]
pub struct ServiceEntry {
    /// The last **successful** read. Deliberately kept through a failure: a
    /// page that answered a minute ago is better evidence than nothing, and a
    /// vendor's status does not change on the timescale of one dropped request.
    pub status: Option<ServiceStatus>,
    pub error: Option<String>,
}

/// Every watched vendor's availability, as the app holds it — **and which
/// vendors those are**.
///
/// The watched set travels *with* the readings rather than beside them, because
/// the panel and the notifier must never be reading two different lists. A row
/// painted for a vendor no pass polled, and a vendor polled and then left off
/// the panel, are the two halves of the same bug (#375); one lock holding one
/// answer is what makes them unrepresentable.
#[derive(Debug, Default)]
pub struct ServiceStatuses {
    /// What [`active_vendors`] answered on the last pass.
    ///
    /// `None` until a pass has declared one, and that is **not** the empty
    /// list: "the watched set has not been derived yet" and "nothing is
    /// configured to watch" are different facts, and [`view`] words them apart.
    /// Same [`Configured`] discipline every other panel follows — a defaulted
    /// state is as much a fabrication as a defaulted number.
    vendors: Option<Vec<ActiveVendor>>,
    entries: BTreeMap<ServiceId, ServiceEntry>,
}

impl ServiceStatuses {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare the vendors this pass watches, and **forget every reading that
    /// is not one of them**.
    ///
    /// The watched set is the operator's (#284), so it changes while the app is
    /// running: adding a status page in Settings puts a vendor into it,
    /// removing or disabling one takes it out. Dropping the departed vendor's
    /// entry is what stops a re-added vendor from rendering the status it
    /// carried when it left as though that had just been read — the identical
    /// rule [`StatusWatch::observe`] applies to its baseline, for the identical
    /// reason: what a page said while nobody was looking is *unknown*, not the
    /// last value we happened to be holding.
    ///
    /// Called **before** the pass's results are folded in, so `succeeded` and
    /// `failed` cannot write an entry this immediately discards.
    pub fn watching(&mut self, vendors: Vec<ActiveVendor>) {
        self.entries
            .retain(|service, _| vendors.iter().any(|vendor| vendor.service == *service));
        self.vendors = Some(vendors);
    }

    /// The vendors the last pass watched — `None` when no pass has declared a
    /// set yet, which is not the same as an empty one.
    #[must_use]
    pub fn watched(&self) -> Option<&[ActiveVendor]> {
        self.vendors.as_deref()
    }

    pub fn succeeded(&mut self, service: ServiceId, status: ServiceStatus) {
        let entry = self.entries.entry(service).or_default();
        entry.status = Some(status);
        entry.error = None;
    }

    /// Records why a refresh failed **without** dropping the last good read.
    pub fn failed(&mut self, service: ServiceId, message: impl Into<String>) {
        self.entries.entry(service).or_default().error = Some(message.into());
    }

    #[must_use]
    pub fn get(&self, service: ServiceId) -> Option<&ServiceEntry> {
        self.entries.get(&service)
    }

    /// This pass's readings, for [`StatusWatch::observe`].
    ///
    /// A vendor whose last read **failed** reports `None` even though
    /// [`ServiceEntry::status`] still holds its last good value. The two
    /// consumers want different things from the same state: the panel renders
    /// the retained status because a minute-old reading beats a blank chip,
    /// while the watch must not treat a value it did not just observe as a
    /// fresh observation — that would make an unreachable page look like a
    /// steady state forever, and `StatusWatch` would never notice the recovery
    /// when the page came back saying something new.
    ///
    /// Walks the **watched** list [`watching`](Self::watching) was last handed,
    /// never [`ServiceId::ALL`]: the set is derived from configuration (#284),
    /// so it holds the operator's own status pages alongside this build's, and
    /// each vendor's `label` and `subject` come from its
    /// [`ActiveVendor`] record rather than from [`ServiceId::label`] — which
    /// answers `None` for a [`ServiceId::Custom`] by design, and would have
    /// dropped every operator-added vendor on the floor between the poll and
    /// the banner.
    ///
    /// Empty before the first pass, which is exactly right: a watch with
    /// nothing to compare has nothing to say, and [`StatusWatch::observe`]
    /// forgets nothing it was never told about.
    #[must_use]
    pub fn readings(&self) -> Vec<Reading<'_>> {
        self.watched()
            .unwrap_or_default()
            .iter()
            .map(|vendor| {
                let fresh = self
                    .entries
                    .get(&vendor.service)
                    .filter(|e| e.error.is_none())
                    .and_then(|e| e.status.as_ref());
                Reading {
                    service: vendor.service,
                    label: &vendor.label,
                    subject: &vendor.subject,
                    status: fresh.and_then(|s| s.component),
                    incident: fresh
                        .and_then(|s| s.incident.as_ref())
                        .map(|i| i.name.as_str()),
                }
            })
            .collect()
    }
}

/// Shown until a pass has derived the watched set. The panel has genuinely not
/// looked yet, and the alternative — an empty body under "all clear" — is the
/// empty green panel this whole area exists to make impossible.
pub const LOADING_MESSAGE: &str = "reading status pages…";

/// Shown once a pass *has* looked and found nothing to watch. The watched set
/// is derived from configuration (#284), so a cockpit with no credentials and
/// no added status pages watches nothing — and says so, rather than reporting
/// the happy path about vendors it is not reading.
pub const NOTHING_WATCHED_MESSAGE: &str =
    "nothing to watch yet — vendors appear as you configure them in Settings";

/// The Services panel payload: one row per watched vendor.
///
/// Every string and colour is decided here, like every other panel's. The row
/// order is the **watched list's**, which [`active_vendors`] builds in
/// [`ServiceId::ALL`]'s order and then appends the operator's own pages to, in
/// the order they arranged them. Fixed either way, so a vendor never moves
/// between polls — a list that re-sorted itself as things broke would be
/// unreadable in exactly the moment it matters.
///
/// Three states, worded apart: no pass yet, a pass that watches nothing, and
/// rows. Collapsing the first two into "all clear" over an empty body would be
/// the panel claiming coverage it does not have.
#[must_use]
pub fn view(statuses: &ServiceStatuses) -> Value {
    let kind = PanelKind::Services;
    let watched = statuses.watched();
    let rows: Vec<Value> = watched
        .unwrap_or_default()
        .iter()
        .map(|vendor| row(vendor, statuses))
        .collect();
    // "2 degraded" / "all clear". Counted from the rendered rows so the
    // trailing label can never disagree with what is under it — and left empty
    // when there are none, because both "all clear" and "0 degraded" are
    // verdicts about vendors nobody read.
    let degraded = rows.iter().filter(|r| r["degraded"] == json!(true)).count();
    let trailing = match (rows.is_empty(), degraded) {
        (true, _) => String::new(),
        (false, 0) => "all clear".to_owned(),
        (false, n) => format!("{n} degraded"),
    };
    json!({
        "id": kind.id(),
        "title": kind.title(),
        "trailing": trailing,
        // One sentence, not a blank body: an unconfigured panel and a panel
        // still filling in must not look the same, and neither may look like a
        // clean bill of health. Same shape as the Containers panel's.
        "empty": match watched {
            None => json!({ "message": LOADING_MESSAGE }),
            Some(_) if rows.is_empty() => json!({ "message": NOTHING_WATCHED_MESSAGE }),
            Some(_) => Value::Null,
        },
        "rows": rows,
        // Drives the frontend's refresh cadence while the panel fills in —
        // published rather than inferred from every row reading "Unknown",
        // which stopped being the pre-first-pass rendering when the vendor list
        // became the operator's.
        "loading": watched.is_none(),
    })
}

/// One watched vendor's row. Total, unlike the [`ServiceId`]-keyed version
/// before it: an [`ActiveVendor`] carries its own label and subject, so there
/// is no vendor this can fail to name and none it needs to invent a name for.
fn row(vendor: &ActiveVendor, statuses: &ServiceStatuses) -> Value {
    let service = vendor.service;
    let (label, subject) = (vendor.label.as_str(), vendor.subject.as_str());
    let entry = statuses.get(service);
    let status = entry.and_then(|e| e.status.as_ref());
    let component = status.and_then(|s| s.component);

    let (state, color) = match component {
        Some(ComponentStatus::Operational) => ("Operational", color::GREEN_DIM),
        Some(ComponentStatus::MajorOutage) => ("Major Outage", color::RED),
        Some(ComponentStatus::PartialOutage) => ("Partial Outage", color::AMBER),
        Some(ComponentStatus::DegradedPerformance) => ("Degraded", color::AMBER),
        // Nothing decoded. For Azure that is the *healthy* reading, because its
        // feed lists incidents and never publishes health — so the two are
        // worded apart rather than sharing one muted "unknown" that would be
        // true of one and misleading about the other.
        None if entry.is_some_and(|e| e.error.is_none()) && !service.publishes_health() => {
            ("No Incidents", color::GREEN_DIM)
        }
        None => ("Unknown", color::MUTED),
    };

    // The reason a refresh failed explains why the row is not newer; it never
    // replaces the row, because the last good reading still stands.
    let detail = match (
        entry.and_then(|e| e.error.as_deref()),
        status.and_then(|s| s.incident.as_ref()),
    ) {
        (Some(error), _) => format!("{subject} — {error}"),
        (None, Some(incident)) => {
            format!("{subject}: {} ({}).", incident.name, incident.impact)
        }
        (None, None) => subject.to_owned(),
    };

    json!({
        "id": service.id(),
        "label": label,
        "state": state,
        "color": color::hex(color),
        "detail": detail,
        // Published rather than derived from the colour: the panel's trailing
        // count reads this, and counting amber pixels would be a second
        // definition of "degraded" free to disagree with the first.
        "degraded": component.is_some_and(ComponentStatus::is_degraded),
    })
}

/// A fixture covering every rendering the Services panel has: one healthy
/// vendor, one degraded, one in a major outage, one Azure-style "no incidents",
/// and one never read.
///
/// Watches the five built-ins — the fully-configured machine, which is the one
/// the panel's rows are worth a picture of. The empty and still-loading
/// renderings are the *absence* of this, and are covered where they belong: in
/// this module's tests and, for the frontend, by an inline override.
#[must_use]
pub fn fixture_statuses() -> ServiceStatuses {
    let mut s = ServiceStatuses::new();
    s.watching(
        ServiceId::ALL
            .iter()
            .filter_map(|&id| ActiveVendor::builtin(id))
            .collect(),
    );
    let status = |c: Option<ComponentStatus>, incident: Option<&str>| ServiceStatus {
        component: c,
        incident: incident.map(|name| Incident {
            name: name.to_owned(),
            impact: "critical".to_owned(),
        }),
    };
    s.succeeded(
        ServiceId::GitHub,
        status(Some(ComponentStatus::Operational), None),
    );
    s.succeeded(
        ServiceId::Anthropic,
        status(
            Some(ComponentStatus::MajorOutage),
            Some("Elevated API errors"),
        ),
    );
    s.succeeded(
        ServiceId::Vercel,
        status(Some(ComponentStatus::DegradedPerformance), None),
    );
    // Azure's healthy reading: nothing decoded, and no error either.
    s.succeeded(ServiceId::Azure, status(None, None));
    // Neon is left untouched — the never-read row.
    s
}

/// Read one **built-in** vendor's status page.
///
/// The three transports differ enough to need their own clients and the same
/// enough to answer one type, which is the whole point of `crates/servicestatus`.
///
/// # Errors
/// [`servicestatus::StatusError`] as each adapter classifies it.
///
/// # Panics
/// On a [`ServiceId::Custom`], which carries no page. **Call [`read_vendor`]
/// instead** — it takes the whole [`ActiveVendor`], so the page an
/// operator-added vendor carries never has to be reconstructed from its id, and
/// this arm stays unreachable by construction.
///
/// The alternative was returning a [`servicestatus::StatusError`], and every
/// variant of that blames a status page — for a vendor whose page this build
/// was never given, that is a fabricated failure, and it would reach the panel
/// wearing a real one's words.
pub async fn read(service: ServiceId) -> Result<ServiceStatus, servicestatus::StatusError> {
    match service {
        ServiceId::GitHub => github::status::client().status().await,
        // `status.claude.com`, not `status.anthropic.com` — the latter 302s
        // here, and a redirect is a convenience a vendor can retire.
        ServiceId::Anthropic => {
            servicestatus::StatusPageClient::new("https://status.claude.com", CLAUDE_API_COMPONENT)
                .status()
                .await
        }
        ServiceId::Vercel => {
            servicestatus::StatusPageClient::new(
                "https://www.vercel-status.com",
                VERCEL_BUILDS_COMPONENT,
            )
            .status()
            .await
        }
        ServiceId::Neon => {
            servicestatus::StatusIoClient::new(servicestatus::statusio::NEON_PAGE_ID, None)
                .status()
                .await
        }
        ServiceId::Azure => servicestatus::AzureFeedClient::new().status().await,
        ServiceId::Custom(id) => {
            unreachable!("operator-added vendor {id} has no built-in status page")
        }
    }
}

/// Read one watched vendor's status page, whoever configured it.
///
/// The total reader over [`active_vendors`]'s list, and the one the poll pass
/// takes: it routes an operator-added vendor to the page **its record
/// carries**, which is the only place that page exists.
/// `crates/servicestatus`'s Statuspage adapter is already parameterized by base
/// URL, so this adds no transport — #284 moves the vendor list from code to
/// data and nothing else.
///
/// # Errors
/// [`servicestatus::StatusError`] as each adapter classifies it.
pub async fn read_vendor(
    vendor: &ActiveVendor,
) -> Result<ServiceStatus, servicestatus::StatusError> {
    match &vendor.source {
        VendorSource::Builtin => read(vendor.service).await,
        VendorSource::Statuspage {
            base_url,
            component_id,
        } => {
            servicestatus::StatusPageClient::new(base_url, component_id)
                .status()
                .await
        }
    }
}

/// `Claude API (api.anthropic.com)` on `status.claude.com`. The API rather than
/// `claude.ai`: this stack calls the API, and the web app can be down while it
/// is fine.
const CLAUDE_API_COMPONENT: &str = "k8w3r06qmzrp";

/// `Builds` on `www.vercel-status.com`. Builds rather than the edge network:
/// a Vercel outage this cockpit cares about is one that stops a deploy.
const VERCEL_BUILDS_COMPONENT: &str = "7ckq6xr6nsbv";

/// Whether a monitored host is answering.
///
/// Two states, not the card's five: `connecting` and `sampler-stalled` are
/// facts about a host we *can* reach, and a banner for either would fire on
/// every launch and every agent restart respectively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reachability {
    Reachable,
    Unreachable,
}

/// One pass's verdict for one host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostReading<'a> {
    /// The stable id, so a rename does not read as a new host.
    pub id: &'a str,
    pub name: &'a str,
    /// `None` before the host's first poll settles — the same "not a status"
    /// rule the statuspage readings follow.
    pub state: Option<Reachability>,
}

/// The last known reachability of each monitored host.
///
/// A separate watch from [`StatusWatch`] rather than another `ServiceId`
/// variant: a host answers with [`Reachability`] rather than a
/// [`ComponentStatus`], and the two are worded apart to the last sentence.
/// Both key sets change at runtime and both forget what leaves the pass, for
/// the same reason — removal in Settings is not an outage.
///
/// They differ in what an *unsettled* entry means. This watch drops a host
/// whose verdict is `None`, because that is a host whose first poll has not
/// landed and there is nothing to keep; [`StatusWatch`] keeps a vendor whose
/// page it could not read, because that vendor has a baseline worth comparing
/// the next successful read against.
#[derive(Debug, Default)]
pub struct HostWatch {
    seen: BTreeMap<String, Reachability>,
}

impl HostWatch {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget every host's baseline, so the next pass seeds instead of firing.
    ///
    /// Called when the machine resumes from sleep. The tailnet takes a few
    /// seconds to come back, so the first polls after a lid-open fail — and
    /// with the debounce at two ticks that would banner "unreachable" and then
    /// "back online" for every host, every single time. Re-seeding reuses the
    /// rule this watch already has for a host it has never seen, which is
    /// exactly what a host on the far side of a suspend is.
    pub fn reset(&mut self) {
        self.seen.clear();
    }

    /// Diff one poll pass against the last.
    ///
    /// Hosts absent from `readings` are **forgotten**, not reported: a host
    /// removed in Settings has not gone down, and carrying its last state
    /// forever would fire a spurious "back online" if it were ever re-added.
    pub fn observe(&mut self, readings: &[HostReading<'_>], enabled: bool) -> Vec<StatusNotice> {
        self.seen
            .retain(|id, _| readings.iter().any(|r| r.id == id && r.state.is_some()));

        let mut notices = Vec::new();
        for reading in readings {
            let Some(current) = reading.state else {
                continue;
            };
            let previous = self.seen.insert(reading.id.to_owned(), current);
            if !enabled {
                continue;
            }
            match previous {
                None => {}
                Some(previous) if previous == current => {}
                Some(_) => notices.push(match current {
                    Reachability::Reachable => StatusNotice {
                        title: format!("{} · back online", reading.name),
                        body: format!("{} is answering again.", reading.name),
                    },
                    Reachability::Unreachable => StatusNotice {
                        title: format!("{} · unreachable", reading.name),
                        body: format!(
                            "Couldn't reach {}. Check the host is up and the agent is running.",
                            reading.name
                        ),
                    },
                }),
            }
        }
        notices
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::{CredentialStore, MemoryCredentialStore, SecretError, StatusVendor, VendorAccount};
    use uuid::Uuid;

    /// The watched set of a fully-configured store: this build's five, in the
    /// order the panel lists them.
    fn builtins() -> Vec<ActiveVendor> {
        ServiceId::ALL
            .iter()
            .filter_map(|&id| ActiveVendor::builtin(id))
            .collect()
    }

    fn reading(status: Option<ComponentStatus>) -> Reading<'static> {
        Reading {
            service: ServiceId::GitHub,
            label: "GitHub",
            subject: "GitHub Actions",
            status,
            incident: None,
        }
    }

    #[test]
    fn the_first_known_reading_only_seeds() {
        let mut watch = StatusWatch::new();
        assert!(watch
            .observe(&[reading(Some(ComponentStatus::MajorOutage))], true)
            .is_empty());
    }

    /// Launching mid-outage says nothing; the recovery an hour later is the
    /// banner worth having.
    #[test]
    fn a_recovery_after_an_outage_fires_once() {
        let mut watch = StatusWatch::new();
        watch.observe(&[reading(Some(ComponentStatus::MajorOutage))], true);

        let notices = watch.observe(&[reading(Some(ComponentStatus::Operational))], true);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "GitHub · recovered");
        assert_eq!(
            notices[0].body,
            "GitHub Actions is operational again, after major outage."
        );

        // …and the next identical pass is silent.
        assert!(watch
            .observe(&[reading(Some(ComponentStatus::Operational))], true)
            .is_empty());
    }

    #[test]
    fn going_down_fires_too_and_names_the_incident() {
        let mut watch = StatusWatch::new();
        watch.observe(&[reading(Some(ComponentStatus::Operational))], true);

        let notices = watch.observe(
            &[Reading {
                incident: Some("Incident with Actions"),
                ..reading(Some(ComponentStatus::MajorOutage))
            }],
            true,
        );
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "GitHub · major outage");
        assert_eq!(
            notices[0].body,
            "GitHub Actions: major outage. Incident: Incident with Actions."
        );
    }

    /// Escalation inside an outage is still news — `partial_outage` becoming
    /// `major_outage` is the moment to stop waiting and go and look.
    #[test]
    fn a_worsening_status_is_its_own_transition() {
        let mut watch = StatusWatch::new();
        watch.observe(&[reading(Some(ComponentStatus::PartialOutage))], true);
        let notices = watch.observe(&[reading(Some(ComponentStatus::MajorOutage))], true);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "GitHub · major outage");
    }

    /// The rule this module exists to get right. A statuspage we could not read
    /// is not a status: it must neither fire on the way in nor, when the page
    /// comes back saying the same thing it said before, fire on the way out.
    #[test]
    fn an_unreadable_page_is_not_a_transition_in_either_direction() {
        let mut watch = StatusWatch::new();
        watch.observe(&[reading(Some(ComponentStatus::MajorOutage))], true);

        assert!(
            watch.observe(&[reading(None)], true).is_empty(),
            "losing the page is not a recovery"
        );
        assert!(
            watch
                .observe(&[reading(Some(ComponentStatus::MajorOutage))], true)
                .is_empty(),
            "…and getting it back, unchanged, is not a new outage"
        );

        // The real recovery still lands.
        assert_eq!(
            watch
                .observe(&[reading(Some(ComponentStatus::Operational))], true)
                .len(),
            1
        );
    }

    /// …and an unknown *first* reading must not seed, or the first real status
    /// would look like a change.
    #[test]
    fn an_unknown_first_reading_seeds_nothing() {
        let mut watch = StatusWatch::new();
        assert!(watch.observe(&[reading(None)], true).is_empty());
        assert!(
            watch
                .observe(&[reading(Some(ComponentStatus::MajorOutage))], true)
                .is_empty(),
            "the first status we actually know is still a seed"
        );
    }

    /// Adding a vendor whose page is already amber must not fire a
    /// notification for a state that was true before it was added — the same
    /// seeding rule [`crate::github::notify::ApprovalWatch`] follows for
    /// approval gates.
    ///
    /// The vendor here is one the watch has held a baseline for before, which
    /// is what a vendor removed in Settings and later re-added is. Its state
    /// while nobody was watching is **unknown**, not the last thing we saw, so
    /// the pass that brings it back is a seed.
    #[test]
    fn a_newly_added_vendors_first_reading_seeds_and_does_not_alert() {
        let mut watch = StatusWatch::new();
        // Watched, then removed in Settings…
        watch.observe(&[reading(Some(ComponentStatus::Operational))], true);
        assert!(
            watch.observe(&[], true).is_empty(),
            "removing a vendor is not an event"
        );

        // …and added back while its page is already amber.
        let first = watch.observe(&[reading(Some(ComponentStatus::DegradedPerformance))], true);
        assert!(
            first.is_empty(),
            "first sight of a vendor is a baseline, not an event: {first:?}"
        );

        let second = watch.observe(&[reading(Some(ComponentStatus::MajorOutage))], true);
        assert_eq!(
            second.len(),
            1,
            "a real change after the baseline does alert"
        );
    }

    /// The boundary the rule above must not cross. Membership is presence in
    /// the pass; the reading in it is a separate question. A vendor whose page
    /// could not be read is still being watched, so its baseline stays put and
    /// the recovery that follows is still announced — dropping it here would
    /// swallow exactly the banner this module exists for.
    #[test]
    fn a_vendor_present_but_unreadable_keeps_its_baseline() {
        let mut watch = StatusWatch::new();
        watch.observe(&[reading(Some(ComponentStatus::MajorOutage))], true);
        assert!(watch.observe(&[reading(None)], true).is_empty());

        let notices = watch.observe(&[reading(Some(ComponentStatus::Operational))], true);
        assert_eq!(
            notices.len(),
            1,
            "an unreadable pass is not a removal, so the recovery still fires"
        );
        assert_eq!(notices[0].title, "GitHub · recovered");
    }

    /// Turning notifications off must not queue a backlog: the baseline keeps
    /// moving, so re-enabling reports the world as it is, not as it was.
    #[test]
    fn disabled_passes_still_advance_the_baseline() {
        let mut watch = StatusWatch::new();
        watch.observe(&[reading(Some(ComponentStatus::Operational))], true);

        assert!(watch
            .observe(&[reading(Some(ComponentStatus::MajorOutage))], false)
            .is_empty());
        assert!(
            watch
                .observe(&[reading(Some(ComponentStatus::MajorOutage))], true)
                .is_empty(),
            "re-enabling must not replay the outage that began while it was off"
        );
    }

    /// The seam `poll_github_status` sits on, which is otherwise only exercised
    /// by I/O: a decoded `ServiceStatus` becomes a `Reading`, and the
    /// outage→recovery pair produces exactly one banner naming the incident on
    /// the way in and none on the way back to steady state.
    ///
    /// Built from `parse_summary` rather than hand-assembled `ComponentStatus`
    /// values, so a change to the payload shape fails here too.
    #[test]
    fn a_decoded_payload_drives_the_watch_end_to_end() {
        const ACTIONS: &str = github::status::ACTIONS_COMPONENT_ID;
        let page = |actions: &str, incidents: &str| {
            format!(
                r#"{{"components":[{{"id":"{}","name":"Actions","status":"{actions}"}}],
                    "incidents":[{incidents}]}}"#,
                ACTIONS
            )
        };
        let outage = servicestatus::statuspage::parse_summary(
            &page(
                "major_outage",
                r#"{"name":"Incident with Actions","impact":"critical"}"#,
            ),
            ACTIONS,
        )
        .expect("decodes");
        let healthy = servicestatus::statuspage::parse_summary(&page("operational", ""), ACTIONS)
            .expect("decodes");

        // A nested `fn`, not a closure: the returned `Reading` borrows from its
        // argument, and only elision on a real signature expresses that.
        fn as_reading(s: &servicestatus::ServiceStatus) -> Reading<'_> {
            Reading {
                service: ServiceId::GitHub,
                label: "GitHub",
                subject: "GitHub Actions",
                status: s.component,
                incident: s.incident.as_ref().map(|i| i.name.as_str()),
            }
        }

        let mut watch = StatusWatch::new();
        assert!(
            watch.observe(&[as_reading(&healthy)], true).is_empty(),
            "launching into a healthy world says nothing"
        );

        let down = watch.observe(&[as_reading(&outage)], true);
        assert_eq!(down.len(), 1);
        assert_eq!(down[0].title, "GitHub · major outage");
        assert!(
            down[0].body.contains("Incident with Actions"),
            "{:?}",
            down[0]
        );

        let up = watch.observe(&[as_reading(&healthy)], true);
        assert_eq!(up.len(), 1);
        assert_eq!(up[0].title, "GitHub · recovered");

        assert!(
            watch.observe(&[as_reading(&healthy)], true).is_empty(),
            "and it settles"
        );
    }

    // MARK: - the Services panel

    #[test]
    fn the_panel_renders_one_row_per_vendor_in_a_fixed_order() {
        let vm = view(&fixture_statuses());
        let ids: Vec<&str> = vm["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|r| r["id"].as_str().expect("id"))
            .collect();
        assert_eq!(
            ids,
            ServiceId::ALL.iter().map(|s| s.id()).collect::<Vec<_>>(),
            "a list that re-sorted itself as things broke would be unreadable"
        );
        assert_eq!(vm["id"], "services");
        assert_eq!(vm["title"], "Services");
    }

    /// Azure's feed lists incidents and never publishes health, so its healthy
    /// reading is a *weaker* claim than everyone else's and has to be worded
    /// as one. Both are green; only one says "Operational".
    #[test]
    fn azures_quiet_feed_reads_as_no_incidents_not_operational() {
        let vm = view(&fixture_statuses());
        let row = |id: &str| {
            vm["rows"]
                .as_array()
                .expect("rows")
                .iter()
                .find(|r| r["id"] == id)
                .expect("row")
                .clone()
        };
        assert_eq!(row("azure")["state"], "No Incidents");
        assert_eq!(row("github")["state"], "Operational");
        assert_ne!(
            row("azure")["state"],
            row("github")["state"],
            "two different claims must not share one word"
        );
        // Both healthy, so both green — the wording carries the difference.
        assert_eq!(row("azure")["color"], row("github")["color"]);
        assert_eq!(row("azure")["degraded"], false);
    }

    /// GitHub's health is painted twice — as a Services row, and as the
    /// availability chip beside the Repos and Runners titles — and both must
    /// use the same word for it.
    ///
    /// They are two renderings of one `ComponentStatus`, so a screen calling it
    /// *GitHub OK* in the header while the row beneath called it *Operational*
    /// invited the reading that the two measure different things. This pins the
    /// two literals together across the crate boundary, which is the only place
    /// they can be compared: `crates/github` cannot see the app, and the app's
    /// own table is a `match` arm rather than a shared constant.
    #[test]
    fn the_chip_and_the_services_row_call_a_healthy_github_the_same_thing() {
        let vm = view(&fixture_statuses());
        let github = vm["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .find(|r| r["id"] == "github")
            .expect("github row")
            .clone();
        assert_eq!(github["state"], github::status::ALL_GOOD_LABEL);
        // And in the same green, or one would read as the weaker claim. The
        // row carries the rendered hex, not the raw channel value.
        assert_eq!(github["color"], color::hex(color::GREEN_DIM));
    }

    /// A vendor nobody has read yet is muted and says so. Never green: a check
    /// that cannot answer must not report the happy path.
    #[test]
    fn an_unread_vendor_is_unknown_and_never_green() {
        let vm = view(&fixture_statuses());
        let neon = vm["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .find(|r| r["id"] == "neon")
            .expect("neon");
        assert_eq!(neon["state"], "Unknown");
        assert_eq!(neon["color"], color::hex(color::MUTED));
        assert_eq!(neon["degraded"], false, "unknown is not degraded either");
    }

    #[test]
    fn the_trailing_count_agrees_with_the_rows_under_it() {
        let vm = view(&fixture_statuses());
        let degraded = vm["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .filter(|r| r["degraded"] == json!(true))
            .count();
        assert_eq!(
            degraded, 2,
            "the fixture carries an outage and a degradation"
        );
        assert_eq!(vm["trailing"], "2 degraded");

        let mut calm = ServiceStatuses::new();
        calm.watching(builtins());
        for &s in &ServiceId::ALL {
            calm.succeeded(
                s,
                ServiceStatus {
                    component: Some(ComponentStatus::Operational),
                    incident: None,
                },
            );
        }
        assert_eq!(view(&calm)["trailing"], "all clear", "never \"0 degraded\"");
    }

    /// A failed refresh explains why a row is not newer; it never replaces the
    /// row, because the last good reading still stands.
    #[test]
    fn a_failed_refresh_keeps_the_row_and_says_why_it_is_stale() {
        let mut statuses = fixture_statuses();
        statuses.failed(ServiceId::GitHub, "couldn't reach the status page");
        let vm = view(&statuses);
        let github = vm["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .find(|r| r["id"] == "github")
            .expect("github");
        assert_eq!(
            github["state"], "Operational",
            "the last good reading stands"
        );
        assert!(
            github["detail"]
                .as_str()
                .expect("detail")
                .contains("couldn't reach"),
            "…and the row explains why it is not newer: {github}"
        );
    }

    /// …and that same failure makes the *watch* see nothing, so a page that
    /// comes back saying something new still fires. The panel and the notifier
    /// want different things from one state.
    #[test]
    fn a_failed_refresh_reports_no_reading_to_the_watch() {
        let mut statuses = fixture_statuses();
        statuses.failed(ServiceId::GitHub, "couldn't reach the status page");
        let reading = statuses
            .readings()
            .into_iter()
            .find(|r| r.service == ServiceId::GitHub)
            .expect("github");
        assert_eq!(
            reading.status, None,
            "a retained value is not a fresh observation"
        );
    }

    // MARK: - an operator-added vendor, all the way through a pass (#375)

    /// One pass, as `poll_service_status` runs it: declare the watched set,
    /// fold in what each page said, then hand the readings to the watch.
    /// Nothing here is the shell's own arithmetic, which is why the shell is
    /// not needed to exercise it.
    fn pass(
        statuses: &mut ServiceStatuses,
        watch: &mut StatusWatch,
        vendors: Vec<ActiveVendor>,
        reads: &[(ServiceId, ComponentStatus)],
    ) -> Vec<StatusNotice> {
        statuses.watching(vendors);
        for &(service, component) in reads {
            statuses.succeeded(
                service,
                ServiceStatus {
                    component: Some(component),
                    incident: None,
                },
            );
        }
        watch.observe(&statuses.readings(), true)
    }

    fn railway() -> StatusVendor {
        StatusVendor::new("Railway", "https://status.railway.app", "abc123", "API")
    }

    /// The bug this slice fixes. Adding a status page in Settings stored it,
    /// probed it and validated it — and the panel then ignored it forever,
    /// because the poll pass walked [`ServiceId::ALL`] and both renderers
    /// filtered on [`ServiceId::label`], which answers `None` for a
    /// [`ServiceId::Custom`] by design.
    #[test]
    fn an_operator_added_vendor_reaches_both_the_panel_and_the_watch() {
        let vendor = railway();
        let id = vendor.id;
        let mut statuses = fixture_statuses();
        let mut watched = builtins();
        watched.push(ActiveVendor::from_record(&vendor));
        statuses.watching(watched);
        statuses.succeeded(
            ServiceId::Custom(id),
            ServiceStatus {
                component: Some(ComponentStatus::PartialOutage),
                incident: None,
            },
        );

        let vm = view(&statuses);
        let rows = vm["rows"].as_array().expect("rows");
        let row = rows
            .iter()
            .find(|r| r["id"] == format!("vendor-{id}"))
            .expect("the operator's vendor has a row");
        // Named by its record, never by this build and never by its bare id.
        assert_eq!(row["label"], "Railway");
        assert_eq!(row["state"], "Partial Outage");
        assert_eq!(row["degraded"], true);
        assert_eq!(
            row["detail"], "Railway API",
            "the component is qualified by the vendor, like every built-in subject"
        );
        assert_eq!(vm["trailing"], "3 degraded", "and it is counted");

        // …and the watch sees it under the same name, so a banner can be worded.
        let reading = statuses
            .readings()
            .into_iter()
            .find(|r| r.service == ServiceId::Custom(id))
            .expect("the watch is handed the operator's vendor too");
        assert_eq!(reading.label, "Railway");
        assert_eq!(reading.subject, "Railway API");
    }

    /// The other half of the acceptance: removing it in Settings removes the
    /// row on the next pass, and takes its retained reading with it.
    #[test]
    fn removing_an_operator_added_vendor_removes_its_row() {
        let vendor = railway();
        let id = vendor.id;
        let mut statuses = fixture_statuses();
        let mut watched = builtins();
        watched.push(ActiveVendor::from_record(&vendor));
        statuses.watching(watched);
        statuses.succeeded(
            ServiceId::Custom(id),
            ServiceStatus {
                component: Some(ComponentStatus::MajorOutage),
                incident: None,
            },
        );
        assert!(view(&statuses)["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .any(|r| r["id"] == format!("vendor-{id}")));

        // Removed in Settings: the next pass derives the built-ins alone.
        statuses.watching(builtins());
        let vm = view(&statuses);
        assert!(
            !vm["rows"]
                .as_array()
                .expect("rows")
                .iter()
                .any(|r| r["id"] == format!("vendor-{id}")),
            "a removed vendor keeps no row: {vm}"
        );
        assert!(
            statuses.get(ServiceId::Custom(id)).is_none(),
            "…and no retained reading either, or re-adding it would render a \
             stale outage as though it had just been read"
        );
    }

    /// A re-added vendor is read afresh rather than from the value it carried
    /// when it left — the panel's counterpart to
    /// [`StatusWatch`]'s seeding rule, and the reason
    /// [`ServiceStatuses::watching`] forgets rather than merely hides.
    #[test]
    fn a_re_added_vendor_does_not_render_the_status_it_had_when_it_left() {
        let vendor = railway();
        let id = vendor.id;
        let active = ActiveVendor::from_record(&vendor);
        let mut statuses = ServiceStatuses::new();
        statuses.watching(vec![active.clone()]);
        statuses.succeeded(
            ServiceId::Custom(id),
            ServiceStatus {
                component: Some(ComponentStatus::MajorOutage),
                incident: None,
            },
        );

        statuses.watching(Vec::new());
        statuses.watching(vec![active]);
        let row = view(&statuses)["rows"].as_array().expect("rows")[0].clone();
        assert_eq!(
            row["state"], "Unknown",
            "what a page said while nobody was watching is unknown, not the last value we held"
        );
    }

    /// #297's rule, now that a vendor can genuinely join a pass mid-session:
    /// a vendor added while its page is *already* amber is **seeded**, not
    /// alerted on. Nothing changed on our watch, so there is nothing to say.
    #[test]
    fn a_vendor_entering_the_watched_set_seeds_rather_than_alerting() {
        let vendor = railway();
        let id = vendor.id;
        let custom = ActiveVendor::from_record(&vendor);
        let mut statuses = ServiceStatuses::new();
        let mut watch = StatusWatch::new();

        // A settled pass over the built-ins alone.
        pass(
            &mut statuses,
            &mut watch,
            builtins(),
            &[(ServiceId::GitHub, ComponentStatus::Operational)],
        );

        // The operator adds a vendor whose page is already degraded.
        let mut watched = builtins();
        watched.push(custom.clone());
        let notices = pass(
            &mut statuses,
            &mut watch,
            watched.clone(),
            &[(ServiceId::Custom(id), ComponentStatus::DegradedPerformance)],
        );
        assert!(
            notices.is_empty(),
            "first sight of a vendor is a baseline, not an event: {notices:?}"
        );

        // A real change *after* that baseline does alert, worded from the record.
        let notices = pass(
            &mut statuses,
            &mut watch,
            watched,
            &[(ServiceId::Custom(id), ComponentStatus::MajorOutage)],
        );
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(notices[0].title, "Railway · major outage");
        assert_eq!(notices[0].body, "Railway API: major outage.");

        // Removed, then added back while still in a major outage: seeded again,
        // because its state while nobody was looking is unknown.
        pass(&mut statuses, &mut watch, builtins(), &[]);
        let notices = pass(
            &mut statuses,
            &mut watch,
            vec![custom],
            &[(ServiceId::Custom(id), ComponentStatus::MajorOutage)],
        );
        assert!(
            notices.is_empty(),
            "re-adding a vendor re-seeds rather than re-announcing: {notices:?}"
        );
    }

    /// The regression guard on the built-ins: adding a vendor appends, and
    /// changes neither the content nor the order of the five rows that were
    /// already there.
    #[test]
    fn the_five_built_ins_are_unchanged_by_an_operator_adding_a_vendor() {
        let before = view(&fixture_statuses());

        let vendor = railway();
        let mut statuses = fixture_statuses();
        let mut watched = builtins();
        watched.push(ActiveVendor::from_record(&vendor));
        statuses.watching(watched);
        let after = view(&statuses);

        let rows = |vm: &Value| vm["rows"].as_array().expect("rows").clone();
        assert_eq!(
            rows(&after)[..5],
            rows(&before)[..],
            "the built-ins keep their content and their order"
        );
        assert_eq!(rows(&after).len(), 6, "the operator's vendor is appended");
        assert_eq!(after["rows"][5]["id"], format!("vendor-{}", vendor.id));
    }

    // MARK: - the panel's three states

    /// Before a pass has derived the watched set the panel has genuinely not
    /// looked, and must say so. "all clear" over an empty body is the empty
    /// green panel this area exists to make impossible — and a defaulted state
    /// is as much a fabrication as a defaulted number.
    #[test]
    fn before_the_first_pass_the_panel_says_it_is_reading_rather_than_all_clear() {
        let vm = view(&ServiceStatuses::new());
        assert_eq!(vm["rows"].as_array().expect("rows").len(), 0);
        assert_eq!(vm["trailing"], "");
        assert_eq!(vm["empty"]["message"], LOADING_MESSAGE);
        assert_eq!(vm["loading"], true);
    }

    /// …and a pass that *has* looked and found nothing to watch says a
    /// different thing again: the watched set is derived from configuration
    /// (#284), so an unconfigured cockpit watches nothing, and that is not the
    /// same claim as "everything we watch is fine".
    #[test]
    fn a_pass_that_watches_nothing_says_so_rather_than_all_clear() {
        let mut statuses = ServiceStatuses::new();
        statuses.watching(Vec::new());
        let vm = view(&statuses);
        assert_eq!(vm["rows"].as_array().expect("rows").len(), 0);
        assert_eq!(vm["trailing"], "");
        assert_eq!(vm["empty"]["message"], NOTHING_WATCHED_MESSAGE);
        assert_ne!(
            vm["empty"]["message"], LOADING_MESSAGE,
            "\"we have not looked\" and \"there is nothing to look at\" are different facts"
        );
        assert_eq!(vm["loading"], false);
    }

    /// A populated panel carries no empty sentence and is not loading — the
    /// third of the three, pinned so the pair above cannot start firing on a
    /// working cockpit.
    #[test]
    fn a_populated_panel_carries_no_empty_sentence() {
        let vm = view(&fixture_statuses());
        assert!(vm["empty"].is_null(), "{vm}");
        assert_eq!(vm["loading"], false);
        assert_eq!(vm["trailing"], "2 degraded");
    }

    // MARK: - hosts

    fn host(id: &str, state: Option<Reachability>) -> HostReading<'_> {
        HostReading {
            id,
            name: id,
            state,
        }
    }

    #[test]
    fn a_host_going_quiet_and_coming_back_fires_once_each_way() {
        let mut watch = HostWatch::new();
        assert!(
            watch
                .observe(&[host("ubu-01", Some(Reachability::Reachable))], true)
                .is_empty(),
            "the first verdict only seeds"
        );

        let down = watch.observe(&[host("ubu-01", Some(Reachability::Unreachable))], true);
        assert_eq!(down.len(), 1);
        assert_eq!(down[0].title, "ubu-01 · unreachable");

        assert!(
            watch
                .observe(&[host("ubu-01", Some(Reachability::Unreachable))], true)
                .is_empty(),
            "…and does not repeat every sixty seconds"
        );

        let up = watch.observe(&[host("ubu-01", Some(Reachability::Reachable))], true);
        assert_eq!(up.len(), 1);
        assert_eq!(up[0].title, "ubu-01 · back online");
    }

    /// Before a host's first poll settles there is no verdict, and an absent
    /// verdict must not read as either state.
    #[test]
    fn a_host_with_no_verdict_yet_is_not_a_transition() {
        let mut watch = HostWatch::new();
        watch.observe(&[host("ubu-01", Some(Reachability::Reachable))], true);
        assert!(watch.observe(&[host("ubu-01", None)], true).is_empty());
        assert!(
            watch
                .observe(&[host("ubu-01", Some(Reachability::Reachable))], true)
                .is_empty(),
            "and the state it returns to is the one it left"
        );
    }

    /// A host removed in Settings has not gone down. Forgetting it is what
    /// stops a re-add from firing a "back online" for an event nobody had.
    #[test]
    fn a_host_that_leaves_the_payload_is_forgotten_not_reported() {
        let mut watch = HostWatch::new();
        watch.observe(&[host("ubu-01", Some(Reachability::Unreachable))], true);
        assert!(
            watch.observe(&[], true).is_empty(),
            "removal is not an event"
        );

        assert!(
            watch
                .observe(&[host("ubu-01", Some(Reachability::Reachable))], true)
                .is_empty(),
            "re-adding it seeds again rather than announcing a recovery"
        );
    }

    #[test]
    fn each_host_is_tracked_on_its_own() {
        let mut watch = HostWatch::new();
        watch.observe(
            &[
                host("mac-w26h", Some(Reachability::Reachable)),
                host("ubu-01", Some(Reachability::Reachable)),
            ],
            true,
        );
        let notices = watch.observe(
            &[
                host("mac-w26h", Some(Reachability::Reachable)),
                host("ubu-01", Some(Reachability::Unreachable)),
            ],
            true,
        );
        assert_eq!(notices.len(), 1, "one host's trouble is not the other's");
        assert_eq!(notices[0].title, "ubu-01 · unreachable");
    }

    /// Opening the lid must not banner. The tailnet takes a few seconds to
    /// come back, so the first polls after a resume fail — and without the
    /// re-seed that is an "unreachable" followed by a "back online" for every
    /// host, every time.
    #[test]
    fn a_reset_makes_the_next_reading_seed_rather_than_fire() {
        let mut watch = HostWatch::new();
        watch.observe(&[host("ubu-01", Some(Reachability::Reachable))], true);

        watch.reset();
        assert!(
            watch
                .observe(&[host("ubu-01", Some(Reachability::Unreachable))], true)
                .is_empty(),
            "the first reading after a resume is a seed, not a transition"
        );
        // …and the watch is live again straight afterwards, so a host that
        // really did die during the nap is still reported once it settles.
        let notices = watch.observe(&[host("ubu-01", Some(Reachability::Reachable))], true);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "ubu-01 · back online");
    }

    #[test]
    fn disabled_host_passes_still_advance_the_baseline() {
        let mut watch = HostWatch::new();
        watch.observe(&[host("ubu-01", Some(Reachability::Reachable))], true);
        assert!(watch
            .observe(&[host("ubu-01", Some(Reachability::Unreachable))], false)
            .is_empty());
        assert!(
            watch
                .observe(&[host("ubu-01", Some(Reachability::Unreachable))], true)
                .is_empty(),
            "re-enabling must not replay an outage that began while it was off"
        );
    }

    // MARK: - the derived vendor list

    /// A store and a credential store, held together with the temp dir they
    /// live in. `Store` reads a real file rather than a mock, the way
    /// `crates/store`'s own tests do.
    struct Fixture {
        _dir: tempfile::TempDir,
        store: Store,
        credentials: MemoryCredentialStore,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            // `false`: nothing is stored yet, so `migrate_v1_to_v2` has no
            // token to mint an account for. Each fixture below adds what it
            // needs afterwards.
            let store = Store::open_in(dir.path(), false).expect("open");
            Fixture {
                _dir: dir,
                store,
                credentials: MemoryCredentialStore::new(),
            }
        }

        fn vendors(&self, claude_usage_present: bool) -> Vec<ActiveVendor> {
            active_vendors(&self.store, &self.credentials, claude_usage_present)
        }

        fn labels(&self, claude_usage_present: bool) -> Vec<String> {
            self.vendors(claude_usage_present)
                .into_iter()
                .map(|vendor| vendor.label)
                .collect()
        }

        fn store_secret(&self, key: SecretKey, value: &str) {
            self.credentials.set_secret(key, value).expect("write");
        }
    }

    fn empty_store() -> Fixture {
        Fixture::new()
    }

    /// `n` GitHub accounts, each with the credential it owns — the v2 shape
    /// (#288), one `VendorAccount` per identity rather than one token per
    /// vendor.
    fn store_with_github_accounts(n: usize) -> Fixture {
        let mut fixture = Fixture::new();
        for index in 0..n {
            let mut account =
                VendorAccount::new(VendorKind::GitHub, format!("account-{index}"), "");
            account.secret_account = SecretKey::VendorToken(account.id).account();
            fixture.store_secret(SecretKey::VendorToken(account.id), "github-pat");
            fixture.store.upsert_account(account);
        }
        fixture
    }

    /// Everything the five shipped vendors are derived from, all configured —
    /// the machine the hardcoded list was written on.
    fn store_with_all_credentials() -> Fixture {
        let mut fixture = store_with_github_accounts(1);
        fixture.store_secret(SecretKey::VercelApiToken, "vercel-token");
        fixture.store_secret(SecretKey::NeonApiKey, "neon-key");
        let mut settings = fixture.store.settings().clone();
        settings.azure_storage_account = "stexample".to_owned();
        settings.azure_cost_container = "cost-exports".to_owned();
        fixture.store.set_settings(settings);
        fixture
    }

    /// The rule this list exists for. A store nobody has configured watches
    /// **nothing**, because the five vendors this file used to ship were one
    /// operator's stack — the same error `CLAUDE.md` records for container
    /// grouping, where a shipped example rule groups a stranger's containers by
    /// a rule they never wrote.
    #[test]
    fn a_store_with_no_credentials_derives_no_vendors() {
        assert!(
            empty_store().vendors(false).is_empty(),
            "a first-run store must not watch one operator's vendors on everyone's behalf"
        );
    }

    #[test]
    fn one_github_credential_derives_exactly_one_github_tile() {
        let fixture = store_with_github_accounts(1);
        let vendors = fixture.vendors(false);
        assert_eq!(vendors.len(), 1);
        assert_eq!(vendors[0].service, ServiceId::GitHub);
        assert_eq!(vendors[0].label, "GitHub");
    }

    /// Derivation is per **vendor**, not per account: a status page is one page
    /// however many credentials point at it.
    #[test]
    fn three_github_accounts_still_derive_one_tile() {
        assert_eq!(
            store_with_github_accounts(3).vendors(false).len(),
            1,
            "three accounts are three credentials and one status page"
        );
    }

    /// The regression guard. Removing the hardcoded list changes what the
    /// current operator sees after upgrading, so the derived rule reproducing
    /// today's five is asserted rather than assumed.
    #[test]
    fn a_fully_configured_store_derives_the_five_vendors_shipped_today() {
        let labels = store_with_all_credentials().labels(true);
        for expected in ["GitHub", "Anthropic", "Vercel", "Neon", "Azure"] {
            assert!(labels.contains(&expected.to_owned()), "missing {expected}");
        }
        assert_eq!(labels.len(), 5, "…and nothing beyond them: {labels:?}");
    }

    /// …in the order the panel lists them, so upgrading does not reshuffle the
    /// rows either.
    #[test]
    fn the_derived_order_is_the_one_the_panel_lists_today() {
        let services: Vec<ServiceId> = store_with_all_credentials()
            .vendors(true)
            .into_iter()
            .map(|vendor| vendor.service)
            .collect();
        assert_eq!(services, ServiceId::ALL.to_vec());
    }

    /// A disabled account *"stays configured but is not polled"*, so a vendor
    /// whose every account is switched off has nothing in the cockpit. The
    /// credential is still sitting in the keychain, and must not resurrect the
    /// tile behind the operator's back.
    #[test]
    fn a_vendor_whose_every_account_is_disabled_is_not_watched() {
        let mut fixture = store_with_github_accounts(2);
        let ids: Vec<Uuid> = fixture
            .store
            .accounts()
            .iter()
            .map(|account| account.id)
            .collect();
        for id in &ids {
            fixture.store.account_mut(*id).expect("account").enabled = false;
        }
        assert!(fixture.vendors(false).is_empty());

        // …and one of them coming back is enough, because one page is one page.
        fixture.store.account_mut(ids[0]).expect("account").enabled = true;
        assert_eq!(fixture.labels(false), vec!["GitHub".to_owned()]);
    }

    /// A v1 store that has not migrated yet has no account, so the derivation
    /// falls back to the credential the migration is waiting on. Without that
    /// fallback an operator upgrading mid-flight loses their GitHub tile until
    /// the next launch.
    #[test]
    fn a_v1_store_still_derives_github_from_the_token_its_migration_has_not_read() {
        let fixture = empty_store();
        fixture.store_secret(SecretKey::GitHubAccessToken, "github-pat");
        assert!(
            fixture.store.accounts().is_empty(),
            "the fixture opens with no token in sight, so no account is minted"
        );
        assert_eq!(fixture.labels(false), vec!["GitHub".to_owned()]);
    }

    /// Anthropic is the named exception: it has no credential, so its trigger
    /// is the presence of Claude usage data.
    #[test]
    fn anthropic_is_derived_from_usage_data_rather_than_a_credential() {
        let fixture = empty_store();
        assert!(fixture.vendors(false).is_empty());
        assert_eq!(fixture.labels(true), vec!["Anthropic".to_owned()]);
    }

    /// Azure stores no credential — the panel mints a container-scoped SAS per
    /// poll from the operator's own `az` session — so its trigger is the
    /// export's address, and **both halves** of it: `poll_azure` gates on the
    /// pair, so an account without a container is a panel with no data in it.
    #[test]
    fn azure_is_derived_from_the_export_address_and_needs_both_halves() {
        let mut fixture = empty_store();
        let mut settings = fixture.store.settings().clone();
        settings.azure_storage_account = "stexample".to_owned();
        fixture.store.set_settings(settings.clone());
        assert!(
            fixture.vendors(false).is_empty(),
            "half an address reads no export, so there is nothing to watch yet"
        );

        settings.azure_cost_container = "cost-exports".to_owned();
        fixture.store.set_settings(settings);
        assert_eq!(fixture.labels(false), vec!["Azure".to_owned()]);
    }

    /// Operator-added vendors (#294) are appended, carrying the page and the
    /// component **they** picked — this build ships no adapter for them and
    /// invents no name for them.
    #[test]
    fn an_operator_added_vendor_is_appended_with_the_page_it_was_given() {
        let mut fixture = store_with_github_accounts(1);
        let vendor = StatusVendor::new("Railway", "https://status.railway.app", "abc123", "API");
        let id = vendor.id;
        fixture.store.upsert_status_vendor(vendor);

        let vendors = fixture.vendors(false);
        assert_eq!(vendors.len(), 2, "appended, not merged: {vendors:?}");
        assert_eq!(vendors[0].service, ServiceId::GitHub, "derived ones lead");
        assert_eq!(vendors[1].service, ServiceId::Custom(id));
        assert_eq!(vendors[1].label, "Railway");
        assert_eq!(
            vendors[1].source,
            VendorSource::Statuspage {
                base_url: "https://status.railway.app".to_owned(),
                component_id: "abc123".to_owned(),
            }
        );
    }

    /// A disabled vendor stays configured and is not polled — the same rule
    /// `StatusVendor::enabled` states, applied where the pass is assembled.
    #[test]
    fn a_disabled_operator_added_vendor_is_not_watched() {
        let mut fixture = empty_store();
        let mut vendor =
            StatusVendor::new("Railway", "https://status.railway.app", "abc123", "API");
        vendor.enabled = false;
        fixture.store.upsert_status_vendor(vendor);
        assert!(fixture.vendors(false).is_empty());
    }

    /// A credential store that fails every read — a keychain that will not
    /// unlock. Value-free by construction, like `SecretError` itself.
    struct UnreadableCredentialStore;

    impl UnreadableCredentialStore {
        fn refusal() -> SecretError {
            SecretError::CorruptBlob {
                account: "solador-secrets".to_owned(),
            }
        }
    }

    impl CredentialStore for UnreadableCredentialStore {
        fn secret(&self, _key: SecretKey) -> Result<Option<String>, SecretError> {
            Err(Self::refusal())
        }

        fn set_secret(&self, _key: SecretKey, _value: &str) -> Result<(), SecretError> {
            unreachable!("no path under test writes a credential")
        }

        fn delete_secret(&self, _key: SecretKey) -> Result<(), SecretError> {
            unreachable!("no path under test deletes a credential")
        }

        fn secret_bytes(&self, _key: SecretKey) -> Result<Option<Vec<u8>>, SecretError> {
            Err(Self::refusal())
        }

        fn set_secret_bytes(&self, _key: SecretKey, _value: &[u8]) -> Result<(), SecretError> {
            unreachable!("no path under test writes a credential")
        }
    }

    /// `Unknown` is not `Absent`. A locked credential store has not told us
    /// there is no GitHub token, so no GitHub tile is invented — and, equally,
    /// the vendors that need no credential are still derived, because that
    /// answer never depended on the keychain.
    ///
    /// A store with no GitHub account at all is exactly the case that has to
    /// ask, because a keychain that refuses is also what stops
    /// `migrate_v1_to_v2` from minting one: no account here means "we do not
    /// know yet", never "there is no GitHub".
    #[test]
    fn an_unreadable_credential_store_derives_only_what_it_did_not_need_to_ask_about() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open_in(dir.path(), false).expect("open");
        assert_eq!(
            active_vendors(&store, &UnreadableCredentialStore, true),
            vec![ActiveVendor {
                service: ServiceId::Anthropic,
                label: "Anthropic".to_owned(),
                subject: "the Claude API".to_owned(),
                source: VendorSource::Builtin,
            }],
            "a credential we could not read is unknown, and unknown is not a tile"
        );
    }

    /// Each service seeds on its own first reading. A vendor added to the map
    /// three passes late must not have that first reading read as a change —
    /// which is why there is no global `seeded` flag.
    #[test]
    fn each_service_seeds_independently() {
        let mut watch = StatusWatch::new();
        watch.observe(&[reading(Some(ComponentStatus::Operational))], true);
        // A second service appearing for the first time, alongside a settled one.
        let notices = watch.observe(
            &[
                reading(Some(ComponentStatus::Operational)),
                reading(Some(ComponentStatus::MajorOutage)),
            ],
            true,
        );
        // Same service twice in one pass is the closest this can get until more
        // variants exist: the second entry is a genuine change, the first is not.
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "GitHub · major outage");
    }
}
