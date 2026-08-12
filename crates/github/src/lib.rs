//! GitHub REST client plus the pure mappings behind the cockpit's **Repos** and
//! **GitHub Runners** panels. Rust port of the original app.
//!
//! Layering, outermost first:
//! - [`client`] — the authenticated GETs and their error classification.
//! - [`workflows`] — runs to per-repo health (main / last-PR / running /
//!   needs-approval / stuck), plus the branch, issue and PR counts.
//! - [`runners`] / [`roster`] / [`presence`] — the registered runner list, the
//!   auto-learned roster that remembers ephemeral runners between
//!   registrations, and the absence state machine they share.
//! - [`link`] — the `Link`-header page arithmetic the counts are built on.
//! - [`status`] — GitHub's own availability, read from the unauthenticated
//!   statuspage and folded with the fleet into one "is it us?" verdict.
//!
//! Two rules run through the whole crate:
//!
//! **Unknown is not zero.** Every count is an `Option`, and `None` means the
//! fetch failed or the total was unknowable — the panel renders "—". A zero is
//! a positive claim that there are none. The cursor-pagination guard in
//! [`link`] exists solely to keep those apart: reading "no last page" as "one
//! page" silently reported every repo as having a single open issue.
//!
//! **Clocks advance only on success.** Nothing here reads the wall clock;
//! `now` is always an argument, and callers pass the last *successful* poll
//! time. An outage must not age a healthy runner into a red "missing 40m" —
//! the runner never went anywhere, our view of it did.
//!
//! There is no app wiring here: no persistence, no polling loop, no UI. The
//! roster's storage lives in `crates/store`, and the org and repo lists are
//! parameters, never constants.

pub mod client;
pub mod link;
pub mod presence;
pub mod roster;
pub mod runners;
pub mod status;
pub mod workflows;

pub use client::{GitHubClient, GitHubError};
pub use presence::PresenceState;
pub use roster::{GhRunnerAbsence, GhRunnerDisplayRow, RosterUpdate, RunnerRosterEntry};
pub use runners::{GhRunner, RunnerOs, RunnerState, RunnerSummary};
pub use status::{Conjunction, Verdict};
pub use workflows::{
    RepoCounts, RepoWorkflowHealth, RunConclusion, RunRef, RunStatus, WorkflowRun,
};
