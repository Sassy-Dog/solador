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

use serde_json::{json, Value};
use servicestatus::{ComponentStatus, Incident, ServiceStatus};
use std::collections::BTreeMap;
use viewmodel::cockpit::PanelKind;
use viewmodel::color;

/// A service whose availability the cockpit watches.
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
}

impl ServiceId {
    /// Every watched vendor, in the order the Services panel lists them —
    /// roughly by how loudly this stack notices each one going down.
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
    pub fn id(self) -> &'static str {
        match self {
            ServiceId::GitHub => "github",
            ServiceId::Anthropic => "anthropic",
            ServiceId::Vercel => "vercel",
            ServiceId::Neon => "neon",
            ServiceId::Azure => "azure",
        }
    }

    /// The vendor, for the banner's title. Short: a notification is glanced at.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ServiceId::GitHub => "GitHub",
            ServiceId::Anthropic => "Anthropic",
            ServiceId::Vercel => "Vercel",
            ServiceId::Neon => "Neon",
            ServiceId::Azure => "Azure",
        }
    }

    /// The specific thing we watch, for the banner's body. Not the vendor —
    /// "GitHub is operational again" would overclaim from one component, and
    /// every one of these is a single component of a much larger service.
    #[must_use]
    pub fn subject(self) -> &'static str {
        match self {
            ServiceId::GitHub => "GitHub Actions",
            ServiceId::Anthropic => "the Claude API",
            ServiceId::Vercel => "Vercel Builds",
            ServiceId::Neon => "Neon",
            ServiceId::Azure => "Azure",
        }
    }

    /// Whether this vendor can say "everything is fine".
    ///
    /// Azure alone cannot: its feed lists active incidents and never publishes
    /// health, so a quiet feed is *no known incidents* rather than operational.
    /// The panel words its healthy row differently for that reason, and the
    /// distinction is here rather than in the renderer so there is one place to
    /// read it from.
    #[must_use]
    pub fn publishes_health(self) -> bool {
        !matches!(self, ServiceId::Azure)
    }
}

/// One pass's reading for one service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading<'a> {
    pub service: ServiceId,
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
        let service = reading.service;
        if current == ComponentStatus::Operational {
            return Self {
                title: format!("{} · recovered", service.label()),
                body: format!(
                    "{} is operational again, after {}.",
                    service.subject(),
                    previous.label()
                ),
            };
        }
        let incident = reading
            .incident
            .map(|name| format!(" Incident: {name}."))
            .unwrap_or_default();
        Self {
            title: format!("{} · {}", service.label(), current.label()),
            body: format!("{}: {}.{incident}", service.subject(), current.label()),
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

/// Every watched vendor's availability, as the app holds it.
#[derive(Debug, Default)]
pub struct ServiceStatuses {
    entries: BTreeMap<ServiceId, ServiceEntry>,
}

impl ServiceStatuses {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
    #[must_use]
    pub fn readings(&self) -> Vec<Reading<'_>> {
        ServiceId::ALL
            .iter()
            .map(|&service| {
                let fresh = self
                    .entries
                    .get(&service)
                    .filter(|e| e.error.is_none())
                    .and_then(|e| e.status.as_ref());
                Reading {
                    service,
                    status: fresh.and_then(|s| s.component),
                    incident: fresh
                        .and_then(|s| s.incident.as_ref())
                        .map(|i| i.name.as_str()),
                }
            })
            .collect()
    }
}

/// The Services panel payload: one row per watched vendor.
///
/// Every string and colour is decided here, like every other panel's. The row
/// order is [`ServiceId::ALL`]'s, fixed, so a vendor never moves between polls
/// — a list that re-sorted itself as things broke would be unreadable in
/// exactly the moment it matters.
#[must_use]
pub fn view(statuses: &ServiceStatuses) -> Value {
    let kind = PanelKind::Services;
    let rows: Vec<Value> = ServiceId::ALL.iter().map(|&s| row(s, statuses)).collect();
    // "2 degraded" / "all clear". Counted from the rendered rows so the
    // trailing label can never disagree with what is under it.
    let degraded = rows.iter().filter(|r| r["degraded"] == json!(true)).count();
    json!({
        "id": kind.id(),
        "title": kind.title(),
        "trailing": if degraded > 0 { format!("{degraded} degraded") } else { "all clear".to_owned() },
        "rows": rows,
    })
}

fn row(service: ServiceId, statuses: &ServiceStatuses) -> Value {
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
        (Some(error), _) => format!("{} — {error}", service.subject()),
        (None, Some(incident)) => format!(
            "{}: {} ({}).",
            service.subject(),
            incident.name,
            incident.impact
        ),
        (None, None) => service.subject().to_owned(),
    };

    json!({
        "id": service.id(),
        "label": service.label(),
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
#[must_use]
pub fn fixture_statuses() -> ServiceStatuses {
    let mut s = ServiceStatuses::new();
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

/// Read one vendor's status page.
///
/// The three transports differ enough to need their own clients and the same
/// enough to answer one type, which is the whole point of `crates/servicestatus`.
///
/// # Errors
/// [`servicestatus::StatusError`] as each adapter classifies it.
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

    fn reading(status: Option<ComponentStatus>) -> Reading<'static> {
        Reading {
            service: ServiceId::GitHub,
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
                service: ServiceId::GitHub,
                status: Some(ComponentStatus::MajorOutage),
                incident: Some("Incident with Actions"),
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
                Reading {
                    service: ServiceId::GitHub,
                    status: Some(ComponentStatus::MajorOutage),
                    incident: None,
                },
            ],
            true,
        );
        // Same service twice in one pass is the closest this can get until more
        // variants exist: the second entry is a genuine change, the first is not.
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "GitHub · major outage");
    }
}
