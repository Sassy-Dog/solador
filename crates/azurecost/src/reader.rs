//! List an export's month folder, pick the newest run, sum its partitions, and
//! fold month-to-date plus best-effort prior-month into a summary. Port of
//! `AzureCostReader` in `AzureCostService`.
//!
//! Everything here is generic over [`BlobFetcher`] and takes `now` as an
//! argument, so the whole decision surface — newest-run selection, the
//! rollover fallback, the projection, the cache short-circuit — is testable
//! against an in-memory map at a pinned instant. No service, no credential
//! store, no polling loop: this is the read, and nothing else.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use crate::blob::BlobFetcher;
use crate::csv::{aggregate_cost_csv, sorted_costs};
use crate::error::AzureCostError;
use crate::models::{CostAggregate, CostSummary, ExportFingerprint, FetchResult};
use crate::month::{month_range_folder, prior_month_date, project_monthly_spend};

/// Container-relative root prefix of the current-month (daily) export.
pub const MTD_PREFIX: &str = "daily/mc-platform-daily-actualcost";
/// Container-relative root prefix of the last-completed-month export.
pub const PRIOR_PREFIX: &str = "last-month/mc-platform-lastmonth-actualcost";
/// How many rows each breakdown tile shows.
pub const TOP_N: usize = 5;

/// Which exports to read and how much of each breakdown to keep.
///
/// [`Default`] is the platform's own layout; the fields exist so a test — or a
/// second export — can point elsewhere without the prefixes becoming
/// hard-coded call-site constants.
#[derive(Debug, Clone, Copy)]
pub struct FetchOptions<'a> {
    pub mtd_prefix: &'a str,
    pub prior_prefix: &'a str,
    pub top_n: usize,
}

impl Default for FetchOptions<'_> {
    fn default() -> Self {
        FetchOptions {
            mtd_prefix: MTD_PREFIX,
            prior_prefix: PRIOR_PREFIX,
            top_n: TOP_N,
        }
    }
}

/// List one export's month folder and return the sorted CSV partition paths of
/// its newest run — the *cheap* half of a read: a blob listing of a few KB, no
/// partition bodies.
///
/// Layout: `{root_prefix}/{month_range}/{run_timestamp}/{run_guid}/000001.csv`,
/// alongside a `_manifest.json`. The returned paths double as the run's cache
/// fingerprint.
///
/// # Errors
///
/// [`AzureCostError::NoBlobs`] when the month folder is empty, and
/// [`AzureCostError::NoCsv`] when the newest run holds no `.csv` partitions —
/// two different states, kept apart because only the first one means "try the
/// previous month instead".
pub async fn select_latest_run<F>(
    fetcher: &F,
    root_prefix: &str,
    month: DateTime<Utc>,
) -> Result<Vec<String>, AzureCostError>
where
    F: BlobFetcher + Sync,
{
    let prefix = format!("{root_prefix}/{}/", month_range_folder(month));
    let names = fetcher.list_blobs(&prefix).await?;
    if names.is_empty() {
        return Err(AzureCostError::NoBlobs { prefix });
    }

    // The path segment right after the month folder is the run timestamp
    // (`YYYYMMDDHHMM`, so lexical order is chronological order) — the greatest
    // one is the newest run.
    let latest_run = names
        .iter()
        .map(|name| run_of(name, &prefix))
        .max()
        .unwrap_or_default()
        .to_owned();
    let mut csv_blobs: Vec<String> = names
        .into_iter()
        .filter(|name| name.ends_with(".csv") && run_of(name, &prefix) == latest_run)
        .collect();
    if csv_blobs.is_empty() {
        return Err(AzureCostError::NoCsv {
            run: latest_run,
            prefix,
        });
    }
    // Sorted so the fingerprint is order-stable across polls — blob listing
    // order is not guaranteed, and an order flip would read as new data. The
    // aggregation below is order-independent either way.
    csv_blobs.sort();
    Ok(csv_blobs)
}

/// The run folder a blob belongs to, or `""` for a name outside the prefix.
fn run_of<'a>(name: &'a str, prefix: &str) -> &'a str {
    name.strip_prefix(prefix)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or_default()
}

/// Download and sum the given CSV partitions — the *expensive* half of a read.
///
/// A large month splits into `000001.csv`, `000002.csv`, …; the already-keyed
/// group and type breakdowns merge across partitions, so a resource group named
/// in two partitions lands as one row.
///
/// # Errors
///
/// Any download failure, or a partition whose header lacks `costInUsd`.
pub async fn aggregate<F>(
    fetcher: &F,
    csv_blobs: &[String],
) -> Result<CostAggregate, AzureCostError>
where
    F: BlobFetcher + Sync,
{
    let mut total = 0.0;
    let mut by_resource_group: BTreeMap<String, f64> = BTreeMap::new();
    let mut by_meter_category: BTreeMap<String, f64> = BTreeMap::new();
    for blob in csv_blobs {
        let part = aggregate_cost_csv(&fetcher.get_blob_text(blob).await?)?;
        total += part.total;
        for resource in part.by_resource {
            *by_resource_group.entry(resource.name).or_default() += resource.cost;
        }
        for category in part.by_type {
            *by_meter_category.entry(category.name).or_default() += category.cost;
        }
    }
    Ok(CostAggregate {
        total,
        by_resource: sorted_costs(by_resource_group),
        by_type: sorted_costs(by_meter_category),
    })
}

/// Read one export's newest run for `month` and aggregate it (list, then
/// download).
///
/// # Errors
///
/// Whatever [`select_latest_run`] or [`aggregate`] returns.
pub async fn read_export<F>(
    fetcher: &F,
    root_prefix: &str,
    month: DateTime<Utc>,
) -> Result<CostAggregate, AzureCostError>
where
    F: BlobFetcher + Sync,
{
    let csv_blobs = select_latest_run(fetcher, root_prefix, month).await?;
    aggregate(fetcher, &csv_blobs).await
}

/// [`fetch_summary_with`] against the platform's own export layout.
///
/// # Errors
///
/// See [`fetch_summary_with`].
pub async fn fetch_summary<F>(
    fetcher: &F,
    now: DateTime<Utc>,
    previous: Option<&FetchResult>,
) -> Result<FetchResult, AzureCostError>
where
    F: BlobFetcher + Sync,
{
    fetch_summary_with(fetcher, &FetchOptions::default(), now, previous).await
}

/// Month-to-date spend, prior-month total, and the top breakdowns, wrapped with
/// the fingerprint of the export they came from.
///
/// Three behaviours are load-bearing:
///
/// - **MTD is required, prior month is best-effort.** A failed MTD read
///   propagates; a missing prior-month export yields `None` — unknown, not a
///   claimed $0 — and leaves `error` `None`, so half an outage never blanks the
///   card.
/// - **Month rollover falls back.** On the 1st the current month may not be
///   exported yet. Rather than showing an error, the read drops to the last
///   completed month, stamps `as_of_month`, and projects that month to itself
///   (it is over — extrapolating it would invent a number).
/// - **An unchanged export downloads nothing.** Pass the last successful
///   result as `previous`: if the newest run's partition paths are identical,
///   no new export has been published and the cached summary comes straight
///   back. Only the cheap listing runs on an unchanged cycle.
///
/// # Errors
///
/// [`AzureCostError::NoBlobs`] when neither the current month nor the last
/// completed month has an export, plus any transport, HTTP or CSV failure from
/// the MTD read.
pub async fn fetch_summary_with<F>(
    fetcher: &F,
    options: &FetchOptions<'_>,
    now: DateTime<Utc>,
    previous: Option<&FetchResult>,
) -> Result<FetchResult, AzureCostError>
where
    F: BlobFetcher + Sync,
{
    let mut covered_month = now;
    let mtd_blobs = match select_latest_run(fetcher, options.mtd_prefix, now).await {
        Ok(blobs) => blobs,
        // Only an empty month folder earns the fallback. A run that exists but
        // holds no CSV yet, or a 403, is a different problem and must surface.
        Err(AzureCostError::NoBlobs { .. }) => {
            covered_month = prior_month_date(now);
            select_latest_run(fetcher, options.mtd_prefix, covered_month).await?
        }
        Err(other) => return Err(other),
    };
    let is_fallback = month_range_folder(covered_month) != month_range_folder(now);

    // Best-effort, and taken relative to whichever month is actually on show.
    // A missing export leaves the path list empty, which leaves prior unknown —
    // stable across polls, so cache hits keep working, and it flips to a miss
    // the cycle the export first appears.
    let prior_blobs = select_latest_run(
        fetcher,
        options.prior_prefix,
        prior_month_date(covered_month),
    )
    .await
    .unwrap_or_default();

    // The identity of the data this read would build from. Unchanged against
    // the last success means nothing new landed, so the cached summary stands
    // and not one partition body moves.
    let fingerprint = ExportFingerprint {
        mtd: mtd_blobs,
        prior: prior_blobs,
    };
    if let Some(previous) = previous {
        if previous.fingerprint == fingerprint {
            return Ok(previous.clone());
        }
    }

    let mtd = aggregate(fetcher, &fingerprint.mtd).await?;
    // Stays `None` when the export is missing or unreadable, so the panel can
    // say "unknown" instead of claiming last month cost nothing.
    let mut spend_prior_month = None;
    if !fingerprint.prior.is_empty() {
        if let Ok(prior) = aggregate(fetcher, &fingerprint.prior).await {
            spend_prior_month = Some(prior.total);
        }
    }

    let summary = CostSummary {
        spend_mtd: mtd.total,
        spend_prior_month,
        // A completed month projects to itself; the current month is linearly
        // extrapolated by elapsed days and frozen here. Over a poll interval
        // the elapsed-days drift is cosmetic, and it self-heals on the next
        // real export — a cache miss, recomputed against a fresh `now`.
        spend_projected: if is_fallback {
            mtd.total
        } else {
            project_monthly_spend(mtd.total, now)
        },
        by_resource: mtd.by_resource.into_iter().take(options.top_n).collect(),
        by_type: mtd.by_type.into_iter().take(options.top_n).collect(),
        as_of_month: is_fallback.then_some(covered_month),
        error: None,
    };
    Ok(FetchResult {
        summary,
        fingerprint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{assert_resources, utc, CountingBlobFetcher, StubBlobFetcher};

    fn mtd_path(run: &str) -> String {
        format!("{MTD_PREFIX}/20260601-20260630/{run}/g/000001.csv")
    }

    fn prior_path(run: &str) -> String {
        format!("{PRIOR_PREFIX}/20260501-20260531/{run}/g/000001.csv")
    }

    // MARK: select_latest_run / read_export

    #[tokio::test]
    async fn read_export_picks_the_latest_run_and_sums_its_partitions() {
        let prefix = format!("{MTD_PREFIX}/20260601-20260630/");
        let older = format!("{prefix}202606150900/g1/000001.csv");
        let newer_1 = format!("{prefix}202606151800/g2/000001.csv");
        let newer_2 = format!("{prefix}202606151800/g2/000002.csv");
        let manifest = format!("{prefix}202606151800/g2/_manifest.json");
        let stub = StubBlobFetcher::new(&[
            // Older run — must be ignored entirely.
            (&older, "resourceGroupName,costInUsd\nrg-a,1"),
            // Newest run, split across two partitions (case-variant RG across
            // them, to prove the merge happens after the lowercasing).
            (&newer_1, "resourceGroupName,costInUsd\nrg-a,10\nrg-b,5"),
            (&newer_2, "resourceGroupName,costInUsd\nRG-A,2"),
            // In the newest run but not a .csv — must be skipped.
            (&manifest, "{}"),
        ]);

        let result = read_export(&stub, MTD_PREFIX, utc(2026, 6, 15, 0, 0))
            .await
            .expect("should read");
        assert!((result.total - 17.0).abs() < 1e-6);
        assert_resources(&result.by_resource, &[("rg-a", 12.0), ("rg-b", 5.0)]);
    }

    #[tokio::test]
    async fn an_empty_month_folder_is_no_blobs() {
        let err = select_latest_run(
            &StubBlobFetcher::default(),
            MTD_PREFIX,
            utc(2026, 6, 15, 0, 0),
        )
        .await
        .unwrap_err();
        let AzureCostError::NoBlobs { prefix } = err else {
            panic!("expected NoBlobs, got {err:?}");
        };
        assert_eq!(prefix, format!("{MTD_PREFIX}/20260601-20260630/"));
    }

    /// A run that has published its manifest but not yet its partitions is a
    /// distinct state from an empty month — reading it as "no export" would
    /// send the fallback to last month for a run that is simply mid-write.
    #[tokio::test]
    async fn a_run_with_no_csv_partitions_is_not_no_blobs() {
        let manifest = format!("{MTD_PREFIX}/20260601-20260630/202606151800/g/_manifest.json");
        let stub = StubBlobFetcher::new(&[(&manifest, "{}")]);
        let err = select_latest_run(&stub, MTD_PREFIX, utc(2026, 6, 15, 0, 0))
            .await
            .unwrap_err();
        let AzureCostError::NoCsv { run, .. } = err else {
            panic!("expected NoCsv, got {err:?}");
        };
        assert_eq!(run, "202606151800");
    }

    // MARK: fetch_summary

    #[tokio::test]
    async fn combines_month_to_date_and_prior_month() {
        let (mtd, prior) = (mtd_path("202606151800"), prior_path("202606010300"));
        let stub = StubBlobFetcher::new(&[
            (&mtd, "resourceGroupName,costInUsd\nrg-a,10\nrg-b,5"),
            (&prior, "resourceGroupName,costInUsd\nrg-a,100"),
        ]);

        let summary = fetch_summary(&stub, utc(2026, 6, 15, 0, 0), None)
            .await
            .expect("should fetch")
            .summary;
        assert!((summary.spend_mtd - 15.0).abs() < 1e-6);
        assert_eq!(summary.spend_prior_month, Some(100.0));
        assert_eq!(summary.error, None);
        assert_resources(&summary.by_resource, &[("rg-a", 10.0), ("rg-b", 5.0)]);
    }

    /// Only the MTD export exists. The prior-month read fails and must not
    /// blank the card: prior stays `None` — unknown, not a claimed $0 — and
    /// `error` stays `None`.
    #[tokio::test]
    async fn is_best_effort_about_the_prior_month() {
        let mtd = mtd_path("202606151800");
        let stub = StubBlobFetcher::new(&[(&mtd, "resourceGroupName,costInUsd\nrg-a,42")]);

        let summary = fetch_summary(&stub, utc(2026, 6, 15, 0, 0), None)
            .await
            .expect("should fetch")
            .summary;
        assert!((summary.spend_mtd - 42.0).abs() < 1e-6);
        assert_eq!(summary.spend_prior_month, None);
        assert_eq!(summary.error, None);
    }

    #[tokio::test]
    async fn computes_the_projection_and_the_type_breakdown() {
        let mtd = mtd_path("202606151800");
        let stub = StubBlobFetcher::new(&[(
            &mtd,
            "resourceGroupName,meterCategory,costInUsd\nrg-a,SQL Database,200\nrg-b,Storage,100",
        )]);

        let summary = fetch_summary(&stub, utc(2026, 6, 15, 0, 0), None)
            .await
            .expect("should fetch")
            .summary;
        assert!((summary.spend_mtd - 300.0).abs() < 1e-6);
        // 15 of 30 days elapsed → the projection doubles MTD, frozen here.
        assert!((summary.spend_projected - 600.0).abs() < 1e-6);
        assert_eq!(summary.as_of_month, None, "current month present");
        assert_resources(
            &summary.by_type,
            &[("SQL Database", 200.0), ("Storage", 100.0)],
        );
    }

    /// The 1st-of-month gap: July has no export yet, so fall back to June's
    /// still-present folder and stamp the covered month. Prior is then May, and
    /// a completed month projects to itself.
    #[tokio::test]
    async fn falls_back_to_the_last_completed_month_on_rollover() {
        let june = mtd_path("202606301508");
        let may = prior_path("202606301508");
        let stub = StubBlobFetcher::new(&[
            (
                &june,
                "resourceGroupName,meterCategory,costInUsd\nrg-a,Storage,600\nrg-b,SQL Database,88.46",
            ),
            (&may, "resourceGroupName,costInUsd\nrg-a,239.72"),
        ]);

        let summary = fetch_summary(&stub, utc(2026, 7, 1, 0, 0), None)
            .await
            .expect("should fetch")
            .summary;
        assert!((summary.spend_mtd - 688.46).abs() < 1e-6);
        assert_eq!(summary.spend_prior_month, Some(239.72));
        assert!(
            (summary.spend_projected - 688.46).abs() < 1e-6,
            "a completed month projects to itself"
        );
        assert_eq!(summary.as_of_month, Some(utc(2026, 6, 1, 0, 0)));
        assert_resources(
            &summary.by_type,
            &[("Storage", 600.0), ("SQL Database", 88.46)],
        );
        assert_eq!(summary.error, None);
    }

    /// Neither the current month nor the last completed month has an export.
    /// The fallback misses too, so `NoBlobs` propagates — and the service turns
    /// it into a calm message while keeping any summary already on screen.
    #[tokio::test]
    async fn errors_when_no_recent_month_is_available() {
        let err = fetch_summary(&StubBlobFetcher::default(), utc(2026, 7, 1, 0, 0), None)
            .await
            .unwrap_err();
        assert!(matches!(err, AzureCostError::NoBlobs { .. }), "got {err:?}");
        assert_eq!(err.user_message(), "no recent cost export found");
    }

    /// An expired SAS must not be mistaken for "this month has no export" and
    /// quietly retried against last month — it has to reach the operator.
    #[tokio::test]
    async fn a_non_empty_failure_does_not_trigger_the_rollover_fallback() {
        struct Forbidden;
        impl BlobFetcher for Forbidden {
            async fn list_blobs(&self, _prefix: &str) -> Result<Vec<String>, AzureCostError> {
                Err(AzureCostError::Http {
                    status: 403,
                    body: None,
                })
            }
            async fn get_blob_text(&self, _path: &str) -> Result<String, AzureCostError> {
                unreachable!("no listing succeeded, so nothing is downloaded")
            }
        }

        let err = fetch_summary(&Forbidden, utc(2026, 7, 1, 0, 0), None)
            .await
            .unwrap_err();
        assert!(err.is_auth_failure(), "got {err:?}");
    }

    #[tokio::test]
    async fn keeps_only_the_top_n_rows_of_each_breakdown() {
        let mtd = mtd_path("202606151800");
        let body = "resourceGroupName,costInUsd\nrg-a,7\nrg-b,6\nrg-c,5\nrg-d,4\nrg-e,3\nrg-f,2";
        let stub = StubBlobFetcher::new(&[(&mtd, body)]);

        let options = FetchOptions {
            top_n: 2,
            ..FetchOptions::default()
        };
        let summary = fetch_summary_with(&stub, &options, utc(2026, 6, 15, 0, 0), None)
            .await
            .expect("should fetch")
            .summary;
        assert_resources(&summary.by_resource, &[("rg-a", 7.0), ("rg-b", 6.0)]);
        assert!(
            (summary.spend_mtd - 27.0).abs() < 1e-6,
            "the total still counts every row"
        );
    }

    // MARK: fingerprint cache

    #[tokio::test]
    async fn a_cache_hit_skips_every_partition_download() {
        let (mtd, prior) = (mtd_path("202606151800"), prior_path("202606010300"));
        let fetcher = CountingBlobFetcher::new(&[
            (&mtd, "resourceGroupName,costInUsd\nrg-a,10"),
            (&prior, "resourceGroupName,costInUsd\nrg-a,100"),
        ]);

        let first = fetch_summary(&fetcher, utc(2026, 6, 15, 0, 0), None)
            .await
            .expect("should fetch");
        let downloads_after_first = fetcher.downloads();
        assert!(
            downloads_after_first > 0,
            "the first fetch must download partitions"
        );

        // Same blobs → same fingerprint → cache hit → not one further body.
        let second = fetch_summary(&fetcher, utc(2026, 6, 15, 0, 0), Some(&first))
            .await
            .expect("should fetch");
        assert_eq!(
            fetcher.downloads(),
            downloads_after_first,
            "a cache hit must not re-download partitions"
        );
        assert!(
            fetcher.lists() > 2,
            "the cheap listing still runs — that is what detects a new run"
        );
        assert_eq!(second.summary, first.summary);
        assert_eq!(second.fingerprint, first.fingerprint);
    }

    #[tokio::test]
    async fn a_new_run_misses_the_cache_and_re_downloads() {
        let run1 = mtd_path("202606151800");
        let first = fetch_summary(
            &CountingBlobFetcher::new(&[(&run1, "resourceGroupName,costInUsd\nrg-a,10")]),
            utc(2026, 6, 15, 0, 0),
            None,
        )
        .await
        .expect("should fetch");
        assert!((first.summary.spend_mtd - 10.0).abs() < 1e-6);

        // A newer run folder lands with different data → new fingerprint →
        // cache miss → the new run downloads and the total moves.
        let run2 = mtd_path("202606152100");
        let fetcher = CountingBlobFetcher::new(&[
            (&run1, "resourceGroupName,costInUsd\nrg-a,10"),
            (&run2, "resourceGroupName,costInUsd\nrg-a,25"),
        ]);
        let second = fetch_summary(&fetcher, utc(2026, 6, 15, 0, 0), Some(&first))
            .await
            .expect("should fetch");
        assert!(fetcher.downloads() > 0, "a cache miss must re-download");
        assert!((second.summary.spend_mtd - 25.0).abs() < 1e-6);
        assert_ne!(second.fingerprint, first.fingerprint);
    }

    /// The prior-month export appearing for the first time is new data too —
    /// the fingerprint covers both halves, so it flips to a miss and the
    /// month-over-month figure stops reading as unknown.
    #[tokio::test]
    async fn a_first_prior_month_export_misses_the_cache() {
        let mtd = mtd_path("202606151800");
        let first = fetch_summary(
            &StubBlobFetcher::new(&[(&mtd, "resourceGroupName,costInUsd\nrg-a,10")]),
            utc(2026, 6, 15, 0, 0),
            None,
        )
        .await
        .expect("should fetch");
        assert_eq!(first.summary.spend_prior_month, None);

        let prior = prior_path("202606010300");
        let stub = StubBlobFetcher::new(&[
            (&mtd, "resourceGroupName,costInUsd\nrg-a,10"),
            (&prior, "resourceGroupName,costInUsd\nrg-a,100"),
        ]);
        let second = fetch_summary(&stub, utc(2026, 6, 15, 0, 0), Some(&first))
            .await
            .expect("should fetch");
        assert_ne!(second.fingerprint, first.fingerprint);
        assert_eq!(second.summary.spend_prior_month, Some(100.0));
    }

    /// The cached summary is returned verbatim, projection included — that is
    /// what "frozen at fetch time" means, and it is why a snapshot carried
    /// across a day boundary does not silently re-extrapolate.
    #[tokio::test]
    async fn a_cache_hit_returns_the_frozen_projection_not_a_recomputed_one() {
        let mtd = mtd_path("202606151800");
        let stub = StubBlobFetcher::new(&[(&mtd, "resourceGroupName,costInUsd\nrg-a,300")]);

        let first = fetch_summary(&stub, utc(2026, 6, 15, 0, 0), None)
            .await
            .expect("should fetch");
        assert!((first.summary.spend_projected - 600.0).abs() < 1e-6);

        // Ten days later, same export: the frozen 600 stands rather than
        // becoming 300 / 25 × 30.
        let later = fetch_summary(&stub, utc(2026, 6, 25, 0, 0), Some(&first))
            .await
            .expect("should fetch");
        assert!((later.summary.spend_projected - 600.0).abs() < 1e-6);
    }
}
