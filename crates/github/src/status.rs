//! GitHub's own availability, and the conjunction verdict that makes it useful.
//!
//! A bare "GitHub is down" chip is the less interesting half. The question an
//! operator actually asks during an incident is *is it us?*, and answering it
//! needs both halves: GitHub's published `Actions` status **and** our fleet's
//! state. Only one of the four combinations is a page — GitHub operational
//! while our runners are offline — and the rest are reassurance that used to
//! cost an SSH and three commands to obtain.
//!
//! ```text
//! Actions          fleet     chip                 colour
//! operational      dark      Fleet Down           red     — investigate us
//! major_outage     any       Major Outage         red     — runs are failing
//! degraded/partial any       Services Degraded    amber   — GitHub is slow
//! operational      online    Operational          green
//! (unreadable)     any       Status Unknown       muted
//! ```
//!
//! When GitHub admits to a problem it takes the headline, at *its* severity —
//! how bad GitHub says it is comes first, and whether that explains our dark
//! runners is the sentence behind it. The fleet only takes the headline in the
//! case GitHub is not explaining: an operational Actions with a platform dark
//! anyway, which is the one row that is a page rather than reassurance.
//!
//! The transport is GitHub's Atlassian Statuspage: unauthenticated, CDN-backed,
//! and a different host from the REST API — so it stays up when the API does
//! not, which is exactly when it is needed.
//!
//! The crate's two rules apply here as everywhere:
//!
//! **Unknown is not zero**, restated as *unreachable is not operational*. A
//! statuspage fetch that fails or times out yields [`Verdict::Unknown`], never
//! a green "Operational". A check that cannot answer must not report the happy
//! path — and it must not suppress the fleet reading either, which is why the
//! verdict annotates the panel rather than gating it.
//!
//! **Nothing here reads the wall clock**, and nothing here polls. This module
//! is a client plus pure mappings; the cadence and the retained state live in
//! the app.

use crate::runners::RunnerSummary;
use servicestatus::{ComponentStatus, ServiceStatus};

/// The Actions component's Statuspage id, on `https://www.githubstatus.com`.
///
/// Matched by id, not by name: `components[]` carries a non-component entry
/// called *"Visit www.githubstatus.com for more information"*, and names are
/// display strings Atlassian is free to re-word.
pub const ACTIONS_COMPONENT_ID: &str = "br0l2tvcx85d";

/// `https://www.githubstatus.com`.
pub const GITHUB_STATUS_URL: &str = "https://www.githubstatus.com";

/// A client pointed at GitHub's Actions component.
#[must_use]
pub fn client() -> servicestatus::StatusPageClient {
    servicestatus::StatusPageClient::new(GITHUB_STATUS_URL, ACTIONS_COMPONENT_ID)
}

/// The three platforms the runner panel tracks by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Linux,
    Windows,
}

impl Platform {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Platform::MacOs => "macOS",
            Platform::Linux => "Linux",
            Platform::Windows => "Windows",
        }
    }
}

/// Which tracked platforms have runners registered but **none** online.
///
/// The `_total > 0` guard is the whole point: an org with no Windows runners
/// has `windows_online == 0` forever, and reading that as an outage would pin a
/// permanent red "it's us" on a fleet that is exactly as intended. `os_chips`
/// in the app makes the same distinction for the same reason.
///
/// Per platform rather than in aggregate because that is what the 2026-08-06
/// incident looked like: all ten Linux runners offline while both macs stayed
/// up, because the macs churn far less and their sessions were not being
/// invalidated. An aggregate "some runners are online" would have reported all
/// clear through it.
#[must_use]
pub fn offline_platforms(summary: &RunnerSummary) -> Vec<Platform> {
    let mut offline = Vec::new();
    for (platform, total, online) in [
        (Platform::MacOs, summary.macos_total, summary.macos_online),
        (Platform::Linux, summary.linux_total, summary.linux_online),
        (
            Platform::Windows,
            summary.windows_total,
            summary.windows_online,
        ),
    ] {
        if total > 0 && online == 0 {
            offline.push(platform);
        }
    }
    offline
}

/// Our fleet's half of the conjunction, carrying its own credibility.
///
/// Not a bare `Option<&RunnerSummary>`, because "we have never fetched the
/// runners" and "the runners we fetched are too old to speak for" reach the
/// same conclusion and a caller holding a retained summary would otherwise pass
/// it in regardless. On 2026-08-07 a laptop woke from a night's sleep and the
/// chip read a red **Fleet Down** off pre-sleep data while every runner was in
/// fact online — the summary was real, it was simply hours old, and nothing in
/// this signature could tell.
///
/// [`Unknown`](Self::Unknown) makes [`Verdict::ItsUs`] unreachable. That is the
/// point: blaming our own fleet is the one verdict that sends someone to go and
/// look at something, and it may only be reached from a reading we can vouch
/// for.
#[derive(Debug, Clone, Copy)]
pub enum Fleet<'a> {
    /// No successful runners fetch yet, or the last one is older than the
    /// Runners panel's own staleness window.
    Unknown,
    Known(&'a RunnerSummary),
}

/// The conjunction: what to tell an operator looking at the panel.
///
/// When GitHub admits to a problem, the verdict takes **GitHub's severity** —
/// `Degraded` or `MajorOutage` — rather than folding both into one amber
/// "something is up". Whether one of our platforms is also dark then shapes the
/// *detail*, not the headline: during an outage the first thing worth knowing
/// is how bad GitHub says it is, and the second is whether that explains what
/// we are seeing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// GitHub is fine and so are we.
    AllGood,
    /// `degraded_performance` or `partial_outage`. Amber.
    Degraded,
    /// `major_outage`. Red — GitHub is not merely slow.
    MajorOutage,
    /// GitHub says it is operational and a platform is dark anyway. The one
    /// row that is a page rather than reassurance.
    ItsUs,
    /// The statuspage could not be read. Never green: a check that cannot
    /// answer must not report the happy path.
    Unknown,
}

/// The verdict, its short chip label, and the sentence behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conjunction {
    pub verdict: Verdict,
    /// Header-sized. The header already holds a title, a staleness warning and
    /// a count, so this stays short and the detail goes in `detail`.
    pub label: String,
    /// The full explanation, for the chip's `title` — including the platforms
    /// involved and the incident name when there is one.
    pub detail: String,
}

/// The chip's words, in one place because they are the panel's whole vocabulary.
///
/// Title Case throughout: the two an operator most needs to recognise at a
/// glance are Statuspage's own severities, and a chip that mixed
/// "Major Outage" with "gh ok" would read as two different controls.
///
/// The healthy label is **"Operational"**, not "GitHub OK", so it matches the
/// word the [Services panel] paints for the same condition in the same dim
/// green. These are two renderings of one underlying `ComponentStatus`, and one
/// screen calling it *OK* while the row beneath calls it *Operational* invites
/// the reading that they measure different things. The subject is already in
/// the panel title the chip sits beside, so naming GitHub again bought nothing.
///
/// [Services panel]: ../../../app/src-tauri/src/services.rs
pub const ALL_GOOD_LABEL: &str = "Operational";
pub const DEGRADED_LABEL: &str = "Services Degraded";
pub const MAJOR_OUTAGE_LABEL: &str = "Major Outage";
pub const ITS_US_LABEL: &str = "Fleet Down";
pub const UNKNOWN_LABEL: &str = "Status Unknown";

/// Statuspage's four states collapsed onto the two an operator acts on.
///
/// `degraded_performance` and `partial_outage` are both "GitHub is having a
/// bad time and jobs may be slow"; `major_outage` is "workflow runs are
/// failing", which is a different afternoon. They are coloured apart for the
/// same reason.
#[must_use]
fn severity_verdict(actions: ComponentStatus) -> Verdict {
    match actions {
        ComponentStatus::MajorOutage => Verdict::MajorOutage,
        _ => Verdict::Degraded,
    }
}

#[must_use]
fn severity_label(actions: ComponentStatus) -> &'static str {
    match severity_verdict(actions) {
        Verdict::MajorOutage => MAJOR_OUTAGE_LABEL,
        _ => DEGRADED_LABEL,
    }
}

/// Fold GitHub's status and our fleet into one verdict.
///
/// `status` is `None` when the last statuspage read failed. Note the asymmetry
/// that buys: an unreadable statuspage still reports the *fleet* half, because
/// suppressing a real outage because we could not reach a status page would be
/// the worst of both.
#[must_use]
pub fn conjunction(status: Option<&ServiceStatus>, fleet: Fleet<'_>) -> Conjunction {
    // An unknown fleet reports nothing dark — not because everything is up, but
    // because we cannot say. The `fleet_known` flag below is what keeps that
    // silence from being read as good news.
    let dark = match fleet {
        Fleet::Known(summary) => platform_list(&offline_platforms(summary)),
        Fleet::Unknown => None,
    };
    let fleet_known = matches!(fleet, Fleet::Known(_));

    let Some(status) = status else {
        return Conjunction {
            verdict: Verdict::Unknown,
            label: UNKNOWN_LABEL.to_owned(),
            detail: match dark {
                Some(dark) => format!(
                    "Couldn't read GitHub's status page, so we can't say whether {dark} being \
                     offline is GitHub or us."
                ),
                None => "Couldn't read GitHub's status page.".to_owned(),
            },
        };
    };

    // Same rule one layer in: a status word we could not classify is not a
    // promise that Actions is healthy.
    let Some(actions) = status.component else {
        return Conjunction {
            verdict: Verdict::Unknown,
            label: UNKNOWN_LABEL.to_owned(),
            detail: "GitHub's status page did not report an Actions status.".to_owned(),
        };
    };

    let incident = status
        .incident
        .as_ref()
        .map(|i| format!(" Incident: {} ({}).", i.name, i.impact))
        .unwrap_or_default();

    // When GitHub admits to a problem the headline is *its* severity; whether
    // one of our platforms is also dark shapes the sentence behind it. The one
    // case where the fleet takes the headline is the one GitHub is not
    // explaining — an operational Actions with a platform dark anyway.
    let severity = severity_verdict(actions);
    match (actions.is_degraded(), dark) {
        (false, Some(dark)) => Conjunction {
            verdict: Verdict::ItsUs,
            label: ITS_US_LABEL.to_owned(),
            detail: format!(
                "GitHub Actions is operational, so {dark} being offline is ours to \
                 investigate.{incident}"
            ),
        },
        (true, Some(dark)) => Conjunction {
            verdict: severity,
            label: severity_label(actions).to_owned(),
            detail: format!(
                "GitHub Actions: {}. {dark} being offline is expected while this lasts, and \
                 self-heals.{incident}",
                actions.label()
            ),
        },
        (true, None) => Conjunction {
            verdict: severity,
            label: severity_label(actions).to_owned(),
            detail: format!(
                "GitHub Actions: {}.{}{incident}",
                actions.label(),
                if fleet_known {
                    " Our runners are online regardless."
                } else {
                    ""
                }
            ),
        },
        // GitHub is fine. Whether *we* are is a second claim, and it needs a
        // runner list recent enough to make it — otherwise the chip says only
        // what it knows, which is that GitHub is up.
        (false, None) if fleet_known => Conjunction {
            verdict: Verdict::AllGood,
            label: ALL_GOOD_LABEL.to_owned(),
            detail: "GitHub Actions is operational and every registered platform has runners \
                     online."
                .to_owned(),
        },
        (false, None) => Conjunction {
            verdict: Verdict::AllGood,
            label: ALL_GOOD_LABEL.to_owned(),
            detail: "GitHub Actions is operational. The runner list is older than its refresh \
                     window, so this says nothing about our own fleet."
                .to_owned(),
        },
    }
}

/// `"Linux"`, `"Linux and Windows"`, `"macOS, Linux and Windows"` — or `None`
/// when nothing is dark, which is what the match above branches on.
fn platform_list(offline: &[Platform]) -> Option<String> {
    let names: Vec<&str> = offline.iter().map(|p| p.label()).collect();
    match names.as_slice() {
        [] => None,
        [one] => Some((*one).to_owned()),
        [rest @ .., last] => Some(format!("{} and {last}", rest.join(", "))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runners::RunnerSummary;
    use servicestatus::Incident;
    use servicestatus::{ComponentStatus, ServiceStatus};

    fn fleet(
        macos: (usize, usize),
        linux: (usize, usize),
        windows: (usize, usize),
    ) -> RunnerSummary {
        RunnerSummary {
            total: macos.1 + linux.1 + windows.1,
            online: macos.0 + linux.0 + windows.0,
            busy: 0,
            idle: macos.0 + linux.0 + windows.0,
            macos_online: macos.0,
            macos_total: macos.1,
            linux_online: linux.0,
            linux_total: linux.1,
            windows_online: windows.0,
            windows_total: windows.1,
            other_online: 0,
            other_total: 0,
        }
    }

    // MARK: offline_platforms

    /// The `_total > 0` guard. An org with no Windows runners has
    /// `windows_online == 0` forever; reading that as an outage would pin a
    /// permanent red "it's us" on a fleet that is exactly as configured.
    #[test]
    fn a_platform_with_no_runners_at_all_is_not_offline() {
        let summary = fleet((2, 2), (10, 10), (0, 0));
        assert!(offline_platforms(&summary).is_empty());
    }

    /// The 2026-08-06 shape: every Linux runner dark, both macs up.
    #[test]
    fn a_platform_with_runners_but_none_online_is_offline() {
        let summary = fleet((2, 2), (0, 10), (0, 0));
        assert_eq!(offline_platforms(&summary), vec![Platform::Linux]);
    }

    #[test]
    fn several_dark_platforms_are_all_reported() {
        let summary = fleet((0, 2), (0, 10), (0, 3));
        assert_eq!(
            offline_platforms(&summary),
            vec![Platform::MacOs, Platform::Linux, Platform::Windows]
        );
    }

    // MARK: the matrix

    fn status(actions: ComponentStatus, incident: Option<&str>) -> ServiceStatus {
        ServiceStatus {
            component: Some(actions),
            incident: incident.map(|name| Incident {
                name: name.to_owned(),
                impact: "critical".to_owned(),
            }),
        }
    }

    /// The row that earns the feature: GitHub says it is fine, and a platform
    /// is dark anyway. This is the only red one.
    #[test]
    fn operational_github_plus_a_dark_platform_is_our_problem() {
        let c = conjunction(
            Some(&status(ComponentStatus::Operational, None)),
            Fleet::Known(&fleet((2, 2), (0, 10), (0, 0))),
        );
        assert_eq!(c.verdict, Verdict::ItsUs);
        assert_eq!(c.label, ITS_US_LABEL);
        assert!(
            c.detail.contains("Linux"),
            "names the dark platform: {}",
            c.detail
        );
        assert!(c.detail.contains("ours to investigate"));
    }

    /// The 2026-08-06 reading. Same dark fleet, but GitHub is admitting to it,
    /// so nobody needs to SSH anywhere — and the headline is GitHub's severity,
    /// not the fleet, because how bad GitHub says it is comes first.
    #[test]
    fn a_major_outage_takes_the_headline_even_when_a_platform_is_dark() {
        let c = conjunction(
            Some(&status(
                ComponentStatus::MajorOutage,
                Some("Incident with Actions"),
            )),
            Fleet::Known(&fleet((2, 2), (0, 10), (0, 0))),
        );
        assert_eq!(c.verdict, Verdict::MajorOutage);
        assert_eq!(c.label, MAJOR_OUTAGE_LABEL);
        // …and the fleet half is still the sentence behind it.
        assert!(c.detail.contains("major outage"));
        assert!(c.detail.contains("Linux"), "{}", c.detail);
        assert!(c.detail.contains("expected"));
        assert!(c.detail.contains("Incident with Actions"), "{}", c.detail);
    }

    /// Statuspage's two middle states are both "GitHub is having a bad time";
    /// only `major_outage` means runs are failing outright.
    #[test]
    fn degraded_and_partial_outage_share_a_label_but_major_outage_does_not() {
        for actions in [
            ComponentStatus::DegradedPerformance,
            ComponentStatus::PartialOutage,
        ] {
            let c = conjunction(
                Some(&status(actions, None)),
                Fleet::Known(&fleet((2, 2), (10, 10), (0, 0))),
            );
            assert_eq!(c.verdict, Verdict::Degraded, "{actions:?}");
            assert_eq!(c.label, DEGRADED_LABEL, "{actions:?}");
            assert!(c.detail.contains("online regardless"));
        }

        let major = conjunction(
            Some(&status(ComponentStatus::MajorOutage, None)),
            Fleet::Known(&fleet((2, 2), (10, 10), (0, 0))),
        );
        assert_eq!(major.verdict, Verdict::MajorOutage);
        assert_eq!(major.label, MAJOR_OUTAGE_LABEL);
    }

    #[test]
    fn operational_github_with_a_healthy_fleet_is_all_good() {
        let c = conjunction(
            Some(&status(ComponentStatus::Operational, None)),
            Fleet::Known(&fleet((2, 2), (10, 10), (0, 0))),
        );
        assert_eq!(c.verdict, Verdict::AllGood);
        assert_eq!(c.label, ALL_GOOD_LABEL);
    }

    /// Unreachable is not operational. The fleet half survives, because
    /// suppressing a real outage over an unreachable status page would be the
    /// worst of both.
    #[test]
    fn an_unreadable_status_page_is_unknown_and_still_reports_the_fleet() {
        let c = conjunction(None, Fleet::Known(&fleet((2, 2), (0, 10), (0, 0))));
        assert_eq!(c.verdict, Verdict::Unknown);
        assert_ne!(c.verdict, Verdict::AllGood);
        assert_eq!(c.label, UNKNOWN_LABEL);
        assert!(c.detail.contains("Linux"), "{}", c.detail);
        assert!(c.detail.contains("can't say"), "{}", c.detail);
    }

    /// …and with nothing dark it simply admits it does not know, rather than
    /// inventing a fleet claim to go with it.
    #[test]
    fn an_unreadable_status_page_with_a_healthy_fleet_claims_nothing() {
        let c = conjunction(None, Fleet::Known(&fleet((2, 2), (10, 10), (0, 0))));
        assert_eq!(c.verdict, Verdict::Unknown);
        assert_eq!(c.detail, "Couldn't read GitHub's status page.");
    }

    /// The 2026-08-07 defect, pinned. A laptop woke from a night's sleep and
    /// the chip read a red "Fleet Down" off pre-sleep data while every runner
    /// was in fact online. The summary was real; it was hours old, and nothing
    /// in `conjunction`'s signature could tell.
    ///
    /// One summary, two credibilities, two verdicts — which is the whole reason
    /// [`Fleet`] exists rather than an `Option`.
    #[test]
    fn the_same_dark_fleet_is_only_our_problem_when_the_reading_is_fresh() {
        let dark = fleet((2, 2), (0, 10), (0, 0));
        let github_fine = status(ComponentStatus::Operational, None);

        let fresh = conjunction(Some(&github_fine), Fleet::Known(&dark));
        assert_eq!(fresh.verdict, Verdict::ItsUs, "a reading we can vouch for");

        let stale = conjunction(Some(&github_fine), Fleet::Unknown);
        assert_ne!(
            stale.verdict,
            Verdict::ItsUs,
            "the same runners, too old to blame anyone for"
        );
        assert_eq!(stale.verdict, Verdict::AllGood);
        assert!(
            stale.detail.contains("says nothing about our own fleet"),
            "…and it must not quietly claim the fleet is healthy either: {}",
            stale.detail
        );
        assert!(!stale.detail.contains("online."), "{}", stale.detail);
    }

    /// GitHub's half is fresh even when ours is not, so its severity still
    /// leads — a stale runner list must not cost us the outage warning.
    #[test]
    fn a_stale_fleet_still_reports_githubs_own_severity() {
        let c = conjunction(
            Some(&status(ComponentStatus::MajorOutage, None)),
            Fleet::Unknown,
        );
        assert_eq!(c.verdict, Verdict::MajorOutage);
        assert_eq!(c.label, MAJOR_OUTAGE_LABEL);
        assert!(
            !c.detail.contains("runners are online"),
            "it may not vouch for a fleet it cannot see: {}",
            c.detail
        );
    }

    /// Before the first runners fetch there is no fleet half either. The
    /// verdict must not read that as "every platform is online".
    #[test]
    fn no_runner_summary_yet_never_reads_as_a_healthy_fleet() {
        let c = conjunction(
            Some(&status(ComponentStatus::Operational, None)),
            Fleet::Unknown,
        );
        assert_eq!(c.verdict, Verdict::AllGood);
        assert!(
            !c.detail.contains("offline"),
            "nothing is claimed dark without a summary: {}",
            c.detail
        );
    }

    #[test]
    fn the_platform_list_reads_as_a_sentence() {
        assert_eq!(platform_list(&[]), None);
        assert_eq!(platform_list(&[Platform::Linux]).as_deref(), Some("Linux"));
        assert_eq!(
            platform_list(&[Platform::Linux, Platform::Windows]).as_deref(),
            Some("Linux and Windows")
        );
        assert_eq!(
            platform_list(&[Platform::MacOs, Platform::Linux, Platform::Windows]).as_deref(),
            Some("macOS, Linux and Windows")
        );
    }
}
