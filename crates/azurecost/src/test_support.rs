//! Test-only fixtures shared across the crate's module tests. Twins of the
//! helpers and stubs at the bottom of `DevCanopyTests/AzureCostCSVTests.swift`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::{DateTime, NaiveDate, TimeZone, Utc};

use crate::blob::BlobFetcher;
use crate::error::AzureCostError;
use crate::models::ResourceCost;

/// A UTC instant, spelled the way the Swift tests spell one.
pub(crate) fn utc(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
    let naive = NaiveDate::from_ymd_opt(year, month, day)
        .and_then(|d| d.and_hms_opt(hour, minute, 0))
        .expect("valid UTC instant");
    Utc.from_utc_datetime(&naive)
}

/// Assert names, order and costs together: the order *is* part of the contract
/// (the panel renders a top-N list), so checking the set would pass a shuffle.
pub(crate) fn assert_resources(actual: &[ResourceCost], expected: &[(&str, f64)]) {
    let names: Vec<&str> = actual.iter().map(|r| r.name.as_str()).collect();
    let want: Vec<&str> = expected.iter().map(|(name, _)| *name).collect();
    assert_eq!(names, want, "resource order/names");
    for (resource, (_, want_cost)) in actual.iter().zip(expected) {
        assert!(
            (resource.cost - want_cost).abs() < 1e-6,
            "cost for {}: got {}, want {want_cost}",
            resource.name,
            resource.cost
        );
    }
}

/// In-memory [`BlobFetcher`]: `list_blobs` returns the keys under a prefix,
/// `get_blob_text` returns the stored body. No network.
#[derive(Debug, Clone, Default)]
pub(crate) struct StubBlobFetcher {
    blobs: BTreeMap<String, String>,
}

impl StubBlobFetcher {
    pub(crate) fn new(blobs: &[(&str, &str)]) -> Self {
        StubBlobFetcher {
            blobs: blobs
                .iter()
                .map(|(path, body)| ((*path).to_owned(), (*body).to_owned()))
                .collect(),
        }
    }
}

impl BlobFetcher for StubBlobFetcher {
    async fn list_blobs(&self, prefix: &str) -> Result<Vec<String>, AzureCostError> {
        Ok(self
            .blobs
            .keys()
            .filter(|name| name.starts_with(prefix))
            .cloned()
            .collect())
    }

    async fn get_blob_text(&self, path: &str) -> Result<String, AzureCostError> {
        Ok(self.blobs.get(path).cloned().unwrap_or_default())
    }
}

/// Wraps a [`StubBlobFetcher`] and counts downloads — the deterministic proof
/// that a fingerprint cache hit performs zero partition downloads.
#[derive(Debug, Default)]
pub(crate) struct CountingBlobFetcher {
    inner: StubBlobFetcher,
    downloads: AtomicUsize,
    lists: AtomicUsize,
}

impl CountingBlobFetcher {
    pub(crate) fn new(blobs: &[(&str, &str)]) -> Self {
        CountingBlobFetcher {
            inner: StubBlobFetcher::new(blobs),
            downloads: AtomicUsize::new(0),
            lists: AtomicUsize::new(0),
        }
    }

    /// How many partition bodies have been downloaded — the expensive half of a
    /// read, and the number the cache exists to hold at zero.
    pub(crate) fn downloads(&self) -> usize {
        self.downloads.load(Ordering::Relaxed)
    }

    /// How many blob listings have been performed — the cheap half, which runs
    /// on every poll including a cache hit.
    pub(crate) fn lists(&self) -> usize {
        self.lists.load(Ordering::Relaxed)
    }
}

impl BlobFetcher for CountingBlobFetcher {
    async fn list_blobs(&self, prefix: &str) -> Result<Vec<String>, AzureCostError> {
        self.lists.fetch_add(1, Ordering::Relaxed);
        self.inner.list_blobs(prefix).await
    }

    async fn get_blob_text(&self, path: &str) -> Result<String, AzureCostError> {
        self.downloads.fetch_add(1, Ordering::Relaxed);
        self.inner.get_blob_text(path).await
    }
}
