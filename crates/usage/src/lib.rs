//! The three data sources behind the cockpit's **Usage** panel. Rust port of
//! `DevCanopy/Services/ClaudeUsage/`, `NeonUsage/` and `SentryUsage/`.
//!
//! - [`claude`] — token rollups walked out of Claude Code's local JSONL logs.
//! - [`neon`] — month-to-date Neon compute + branch storage for one org.
//! - [`sentry`] — accepted error events for one org over a rolling window.
//!
//! Three rules run through the whole crate:
//!
//! **Unknown is not zero.** Every figure a provider might not have measured is
//! an `Option`, and the summaries make that structural: [`neon::NeonUsageSummary`]
//! and [`sentry::SentryUsageSummary`] are enums whose unmeasured variant carries
//! no numbers at all, so a `—` can never be typed into a 0 by mistake. The
//! distinction is real on both wires — Neon omits a metric that was zero for a
//! timeframe it *did* return, and Sentry omits an outcome group that had no
//! data, but neither returns anything at all when nothing was measured.
//!
//! **No wall clock.** `now` is always an argument, and `chrono`'s `clock`
//! feature is off, so every window is reproducible from a fixture. The local
//! calendar day the Claude summary needs arrives as a [`chrono::FixedOffset`]
//! rather than being read from the machine.
//!
//! **No app wiring.** No persistence, no polling loop, no UI, no credential
//! storage. Keys and org identifiers are parameters; the cadences, the Keychain
//! reads and the published state stay in the shell.

pub mod claude;
pub mod neon;
pub mod sentry;

pub use claude::{summarize_logs, UsageSummary, UsageTotals};
pub use neon::{NeonClient, NeonUsageError, NeonUsageSummary};
pub use sentry::{SentryClient, SentryUsageError, SentryUsageSummary};
