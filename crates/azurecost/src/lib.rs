//! Reads Azure spend from the platform-owned daily Cost Management export.
//! Rust port of `DevCanopy/Services/AzureCost/`.
//!
//! The export is a CSV dropped into blob storage once a day. There is no Azure
//! SDK here and no OAuth: a container-scoped, read+list SAS URL *is* the
//! credential, and every request is a plain GET with that query appended.
//!
//! Layering, outermost first:
//! - [`blob`] — the two Blob REST operations ([`BlobFetcher`]) and the SAS
//!   transport that performs them.
//! - [`reader`] — list the month folder, pick the newest run, download its
//!   partitions, fold MTD + prior month into a [`CostSummary`].
//! - [`csv`] — the RFC 4180 state machine and the aggregation it feeds.
//! - [`month`] / [`error`] — UTC calendar arithmetic, and the operator-facing
//!   failure strings.
//!
//! Four rules run through the whole crate:
//!
//! **The SAS is a credential in a URL.** Everything else in this workspace
//! keeps credentials in headers; this cannot. So nothing here logs, the
//! transport's `Debug` redacts the query, and every `reqwest` error is stripped
//! of its URL before it becomes a string. See [`blob`].
//!
//! **Nothing reads the wall clock.** `now` is always an argument. The
//! projection is frozen onto the summary at fetch time, so a carried-forward
//! snapshot keeps the number it was computed with instead of quietly
//! re-extrapolating against a month it does not cover.
//!
//! **A missing figure is not a zero.** A failed MTD read is an error, not $0. A
//! missing prior-month export folds to 0 deliberately and is documented as
//! best-effort; that is the one place a zero stands in for absent data, and it
//! never blanks the card.
//!
//! **An unchanged export costs nothing.** Every run lands in a fresh
//! timestamped folder, so the newest run's partition paths are an identity for
//! the data. Unchanged since the last success, the read returns the cached
//! summary and downloads not one partition body.
//!
//! There is no app wiring here: no credential store, no polling loop, no UI.
//! The caller supplies a container-scoped SAS URL — the shell mints a
//! short-lived one per poll from the operator's Azure CLI session and stores
//! nothing — and the cadence to use is [`POLL_INTERVAL`].

pub mod blob;
pub mod csv;
pub mod error;
pub mod models;
pub mod month;
pub mod reader;

#[cfg(test)]
mod test_support;

pub use blob::{parse_blob_list_xml, BlobFetcher, BlobListPage, SasBlobFetcher};
pub use csv::{aggregate_cost_csv, parse_csv};
pub use error::{AzureCostError, COST_COLUMN};
pub use models::{CostAggregate, CostSummary, ExportFingerprint, FetchResult, ResourceCost};
pub use month::{days_in_month, month_range_folder, prior_month_date, project_monthly_spend};
pub use reader::{
    aggregate, fetch_summary, fetch_summary_with, read_export, select_latest_run, FetchOptions,
    MTD_PREFIX, PRIOR_PREFIX, TOP_N,
};

/// How often a caller should poll.
///
/// The Cost Management export is published roughly once a day, so this is its
/// own fixed cadence rather than the app's shared refresh interval — polling a
/// daily file every 30 seconds moves a great deal of egress to learn nothing.
/// Paired with the fingerprint cache, an unchanged cycle downloads nothing at
/// all.
pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(4 * 60 * 60);

#[cfg(test)]
mod tests {
    use super::*;

    /// Azure cost runs on its own cadence, not the shared refresh interval.
    #[test]
    fn the_poll_interval_is_four_hours() {
        assert_eq!(POLL_INTERVAL.as_secs(), 4 * 60 * 60);
    }
}
