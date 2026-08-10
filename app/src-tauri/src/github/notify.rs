//! Needs-approval notifications: which runs have *just* entered the
//! deployment-protection gate, and the two sentences that describe them.
//!
//! Port of the transition half of
//! `DevCanopy/Services/GitHub/GHWorkflowsService.swift`
//! (`notifyApprovalTransitions(in:)`) and the wording half of
//! `DevCanopy/Services/Notifications/WorkflowNotificationService.swift`
//! (`notifyNeedsApproval(repo:title:context:htmlURL:)`).
//!
//! Everything here is pure: [`ApprovalWatch::observe`] takes a completed Repos
//! pass and answers with the notices it produced. Delivery — the one part that
//! needs a live `AppHandle` and an OS that will show a banner — lives in
//! `main.rs`, so the rule that decides *whether* to alert is testable without a
//! notification centre.
//!
//! Three rules, all of them Swift's:
//!
//! **Transition, not state.** A run parked at a gate stays parked for as long
//! as a human takes to notice it, which is many poll passes. The alert fires on
//! the pass where the run *enters* [`needs_approval`](RunRef::needs_approval)
//! and never again — a re-alert every 60 seconds is how a signal becomes noise.
//!
//! **The first pass only seeds.** Launching the app must not alert for every
//! gate that was already open before it started. The first `observe` records
//! the baseline and returns nothing.
//!
//! **The baseline advances even when the preference is off.** Swift updates
//! `knownApprovalRunIDs` in a `defer`, *outside* the `notifyOnApprovalNeeded`
//! guard, so turning the alert off and back on does not fire a backlog for
//! everything that parked in between. Same here — see
//! `disabled_passes_still_advance_the_baseline`.

use github::workflows::{RepoWorkflowHealth, RunRef};
use std::collections::HashSet;

/// One needs-approval notification, already worded.
///
/// Two fields, not three: Swift also carries the run's `htmlURL` so tapping the
/// banner opens the run, and `tauri-plugin-notification`'s desktop path has no
/// tap callback at all to hang that on (it is fire-and-forget through
/// `notify-rust`). A URL nothing can act on would be a field that reads as a
/// feature — the same reasoning that kept it out of the Repos payload until
/// there was a click to spend it on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalNotice {
    /// `"widget · needs approval"`.
    pub title: String,
    /// `"Release · main is parked at an approval gate."`.
    pub body: String,
}

impl ApprovalNotice {
    /// `repo` is the **short** name (`short_name()`), matching Swift's
    /// `h.shortName` — the banner is glanced at, and `acme/` on every line
    /// is the part that is always the same.
    fn new(repo: &str, run: &RunRef) -> Self {
        Self {
            title: format!("{repo} · needs approval"),
            body: format!(
                "{} · {} is parked at an approval gate.",
                run.title, run.context
            ),
        }
    }
}

/// The run ids already alerted on, and whether the baseline has been seeded.
///
/// Lives for the process, not for a poll: it *is* the memory that turns a
/// repeated state into a one-off event.
#[derive(Debug, Default)]
pub struct ApprovalWatch {
    /// Every run seen at a gate on the previous pass. Not "every run ever
    /// alerted on": a run that leaves the gate and comes back is a second
    /// approval to grant, and re-alerting for it is right.
    seen: HashSet<i64>,
    /// False until the first [`observe`](Self::observe) has run.
    seeded: bool,
}

impl ApprovalWatch {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Diff one completed Repos pass against the last, and answer with the
    /// notices to deliver.
    ///
    /// `enabled` is the store's `notify_on_approval_needed`, re-read every pass
    /// so a change applies without a relaunch. It suppresses the *notices*, not
    /// the bookkeeping.
    ///
    /// A repo that went unreachable this pass reports no gates at all, so its
    /// parked run drops out of the baseline and re-alerts when the repo comes
    /// back. That is Swift's behaviour too (`needsApproval` is empty on an
    /// unreachable health), and it is the safe direction to be wrong in: an
    /// extra banner for a gate that is genuinely still open beats silence.
    pub fn observe(&mut self, health: &[RepoWorkflowHealth], enabled: bool) -> Vec<ApprovalNotice> {
        let waiting: Vec<(&RunRef, &str)> = health
            .iter()
            .flat_map(|h| h.needs_approval.iter().map(|run| (run, h.short_name())))
            .collect();

        let notices = if self.seeded && enabled {
            waiting
                .iter()
                .filter(|(run, _)| !self.seen.contains(&run.run_id))
                .map(|(run, repo)| ApprovalNotice::new(repo, run))
                .collect()
        } else {
            Vec::new()
        };

        self.seen = waiting.iter().map(|(run, _)| run.run_id).collect();
        self.seeded = true;
        notices
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, TimeZone, Utc};
    use github::workflows::{RepoCounts, WorkflowRun};

    fn now() -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000, 0).unwrap()
    }

    /// One run, at whatever status the case needs. `name` is the workflow name
    /// (the notification body's first half) and `branch` its context.
    fn run(id: i64, name: &str, branch: &str, status: &str) -> WorkflowRun {
        WorkflowRun {
            id,
            name: name.to_owned(),
            event: "push".to_owned(),
            status: status.to_owned(),
            html_url: format!("https://github.com/acme/x/actions/runs/{id}"),
            created_at: now().to_rfc3339(),
            head_branch: Some(branch.to_owned()),
            conclusion: None,
            run_started_at: None,
            display_title: Some("a commit".to_owned()),
        }
    }

    fn health(slug: &str, runs: &[WorkflowRun]) -> RepoWorkflowHealth {
        github::workflows::health(slug, runs, None, RepoCounts::default(), now())
    }

    /// A gate that was already open when the app started must not alert. This
    /// is the whole point of the seeding pass: without it, every launch fires a
    /// banner for every parked run in the portfolio.
    #[test]
    fn the_first_pass_only_seeds() {
        let mut watch = ApprovalWatch::new();
        let pass = [health(
            "acme/widget",
            &[run(1, "Release", "main", "waiting")],
        )];

        assert_eq!(watch.observe(&pass, true), Vec::new());
        // …and the same run on the next pass is still not new.
        assert_eq!(watch.observe(&pass, true), Vec::new());
    }

    /// The transition itself, with the exact strings Swift builds.
    #[test]
    fn a_run_entering_the_gate_alerts_once_with_the_swift_wording() {
        let mut watch = ApprovalWatch::new();
        watch.observe(
            &[health("acme/widget", &[run(1, "CI", "main", "completed")])],
            true,
        );

        let parked = [health(
            "acme/widget",
            &[run(2, "Release", "main", "waiting")],
        )];
        assert_eq!(
            watch.observe(&parked, true),
            vec![ApprovalNotice {
                title: "widget · needs approval".to_owned(),
                body: "Release · main is parked at an approval gate.".to_owned(),
            }]
        );

        // Still parked on the next pass — the gate has not moved, so neither
        // has the signal.
        assert_eq!(watch.observe(&parked, true), Vec::new());
    }

    /// Two repos, two gates, one pass: one notice each, and each names its own
    /// repo. The short name is what the banner carries, never the full slug.
    #[test]
    fn every_repo_at_a_gate_gets_its_own_notice() {
        let mut watch = ApprovalWatch::new();
        watch.observe(&[], true);

        let notices = watch.observe(
            &[
                health("acme/widget", &[run(1, "Release", "main", "waiting")]),
                health(
                    "acme/pipe-fitting",
                    &[run(2, "Deploy", "release/2.0", "waiting")],
                ),
            ],
            true,
        );

        assert_eq!(
            notices,
            vec![
                ApprovalNotice {
                    title: "widget · needs approval".to_owned(),
                    body: "Release · main is parked at an approval gate.".to_owned(),
                },
                ApprovalNotice {
                    title: "pipe-fitting · needs approval".to_owned(),
                    body: "Deploy · release/2.0 is parked at an approval gate.".to_owned(),
                },
            ]
        );
    }

    /// Approving one run must not re-alert the other. The baseline is a set of
    /// run ids, not a count.
    #[test]
    fn approving_one_of_two_leaves_the_other_quiet() {
        let mut watch = ApprovalWatch::new();
        watch.observe(&[], true);
        watch.observe(
            &[health(
                "acme/widget",
                &[
                    run(1, "Release", "main", "waiting"),
                    run(2, "Deploy", "main", "waiting"),
                ],
            )],
            true,
        );

        // Run 1 was approved and is now executing; run 2 is still parked.
        let notices = watch.observe(
            &[health(
                "acme/widget",
                &[
                    run(1, "Release", "main", "in_progress"),
                    run(2, "Deploy", "main", "waiting"),
                ],
            )],
            true,
        );
        assert_eq!(notices, Vec::new());
    }

    /// A second gate in the *same* run id is not a thing, but a run that clears
    /// one gate and hits the next one is: it leaves `waiting`, so it leaves the
    /// baseline, and coming back is a new approval to grant.
    #[test]
    fn a_run_that_re_enters_the_gate_alerts_again() {
        let mut watch = ApprovalWatch::new();
        watch.observe(&[], true);

        let parked = [health(
            "acme/widget",
            &[run(7, "Release", "main", "waiting")],
        )];
        let running = [health(
            "acme/widget",
            &[run(7, "Release", "main", "in_progress")],
        )];

        assert_eq!(watch.observe(&parked, true).len(), 1);
        assert_eq!(watch.observe(&running, true), Vec::new());
        assert_eq!(watch.observe(&parked, true).len(), 1);
    }

    /// Off means silent, **and** it means the baseline keeps moving. Swift
    /// updates `knownApprovalRunIDs` in a `defer` outside the preference guard;
    /// skipping that here would make re-enabling the alert fire a backlog for
    /// every gate that opened while it was off.
    #[test]
    fn disabled_passes_still_advance_the_baseline() {
        let mut watch = ApprovalWatch::new();
        watch.observe(&[], false);

        let parked = [health(
            "acme/widget",
            &[run(1, "Release", "main", "waiting")],
        )];
        assert_eq!(watch.observe(&parked, false), Vec::new());
        // Re-enabled, same gate, still open: not a transition, so still quiet.
        assert_eq!(watch.observe(&parked, true), Vec::new());
    }

    /// Only `waiting` is an approval gate. A queued, running or failed run is
    /// somebody else's signal — the dot's, the JOBS column's, the trailing
    /// label's — and none of them is worth a banner.
    #[test]
    fn no_other_status_is_an_approval_gate() {
        let mut watch = ApprovalWatch::new();
        watch.observe(&[], true);

        for status in ["queued", "in_progress", "completed", "pending", "requested"] {
            assert_eq!(
                watch.observe(
                    &[health("acme/widget", &[run(1, "CI", "main", status)])],
                    true
                ),
                Vec::new(),
                "status {status}"
            );
        }
    }

    /// An unreachable repo reports no gates, so a parked run drops out of the
    /// baseline and re-alerts when the fetch recovers. Documented rather than
    /// fixed: it is Swift's behaviour, and an extra banner for a gate that is
    /// genuinely still open is the right direction to be wrong in.
    #[test]
    fn a_repo_going_unreachable_re_alerts_when_it_returns() {
        let mut watch = ApprovalWatch::new();
        watch.observe(&[], true);

        let parked = [health(
            "acme/widget",
            &[run(1, "Release", "main", "waiting")],
        )];
        assert_eq!(watch.observe(&parked, true).len(), 1);
        assert_eq!(
            watch.observe(&[RepoWorkflowHealth::unreachable("acme/widget")], true),
            Vec::new()
        );
        assert_eq!(watch.observe(&parked, true).len(), 1);
    }
}
