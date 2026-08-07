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

use servicestatus::ComponentStatus;
use std::collections::BTreeMap;

/// A service whose availability the cockpit watches.
///
/// `BTreeMap`-keyed rather than `HashMap`, so the notices a pass emits come out
/// in a stable order — three banners in a different sequence on every launch
/// would be a diff nobody asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ServiceId {
    GitHub,
}

impl ServiceId {
    /// The vendor, for the banner's title. Short: a notification is glanced at.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ServiceId::GitHub => "GitHub",
        }
    }

    /// The specific thing we watch, for the banner's body. Not the vendor —
    /// "GitHub is operational again" would overclaim from one component.
    #[must_use]
    pub fn subject(self) -> &'static str {
        match self {
            ServiceId::GitHub => "GitHub Actions",
        }
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
    pub fn observe(&mut self, readings: &[Reading<'_>], enabled: bool) -> Vec<StatusNotice> {
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
/// variant: hosts come and go with Settings, and a key set that changes at
/// runtime needs [`observe`](Self::observe)'s `retain` — which would be wrong
/// for the vendor list, where a service missing from a pass means the poll
/// failed, not that it was deleted.
#[derive(Debug, Default)]
pub struct HostWatch {
    seen: BTreeMap<String, Reachability>,
}

impl HostWatch {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
                .observe(&[host("ubu-3xdv", Some(Reachability::Reachable))], true)
                .is_empty(),
            "the first verdict only seeds"
        );

        let down = watch.observe(&[host("ubu-3xdv", Some(Reachability::Unreachable))], true);
        assert_eq!(down.len(), 1);
        assert_eq!(down[0].title, "ubu-3xdv · unreachable");

        assert!(
            watch
                .observe(&[host("ubu-3xdv", Some(Reachability::Unreachable))], true)
                .is_empty(),
            "…and does not repeat every sixty seconds"
        );

        let up = watch.observe(&[host("ubu-3xdv", Some(Reachability::Reachable))], true);
        assert_eq!(up.len(), 1);
        assert_eq!(up[0].title, "ubu-3xdv · back online");
    }

    /// Before a host's first poll settles there is no verdict, and an absent
    /// verdict must not read as either state.
    #[test]
    fn a_host_with_no_verdict_yet_is_not_a_transition() {
        let mut watch = HostWatch::new();
        watch.observe(&[host("ubu-3xdv", Some(Reachability::Reachable))], true);
        assert!(watch.observe(&[host("ubu-3xdv", None)], true).is_empty());
        assert!(
            watch
                .observe(&[host("ubu-3xdv", Some(Reachability::Reachable))], true)
                .is_empty(),
            "and the state it returns to is the one it left"
        );
    }

    /// A host removed in Settings has not gone down. Forgetting it is what
    /// stops a re-add from firing a "back online" for an event nobody had.
    #[test]
    fn a_host_that_leaves_the_payload_is_forgotten_not_reported() {
        let mut watch = HostWatch::new();
        watch.observe(&[host("ubu-3xdv", Some(Reachability::Unreachable))], true);
        assert!(
            watch.observe(&[], true).is_empty(),
            "removal is not an event"
        );

        assert!(
            watch
                .observe(&[host("ubu-3xdv", Some(Reachability::Reachable))], true)
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
                host("ubu-3xdv", Some(Reachability::Reachable)),
            ],
            true,
        );
        let notices = watch.observe(
            &[
                host("mac-w26h", Some(Reachability::Reachable)),
                host("ubu-3xdv", Some(Reachability::Unreachable)),
            ],
            true,
        );
        assert_eq!(notices.len(), 1, "one host's trouble is not the other's");
        assert_eq!(notices[0].title, "ubu-3xdv · unreachable");
    }

    #[test]
    fn disabled_host_passes_still_advance_the_baseline() {
        let mut watch = HostWatch::new();
        watch.observe(&[host("ubu-3xdv", Some(Reachability::Reachable))], true);
        assert!(watch
            .observe(&[host("ubu-3xdv", Some(Reachability::Unreachable))], false)
            .is_empty());
        assert!(
            watch
                .observe(&[host("ubu-3xdv", Some(Reachability::Unreachable))], true)
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
