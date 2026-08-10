//! Pure aggregation of [`UsageRecord`]s into a [`UsageSummary`]. Port of
//! `DevCanopy/Services/ClaudeUsage/ClaudeUsageAggregator.swift` plus the
//! totals/breakdown half of `UsageModels.swift`.
//!
//! No file I/O and no clock access: `now` is an argument, so results are
//! deterministic and testable — the same rule `crates/github` runs on.
//!
//! Pricing is applied **per record by its own model** before summing, so a
//! window mixing models (opus + sonnet in one hour) costs out correctly.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, FixedOffset, NaiveTime, TimeDelta, Utc};

use super::log::UsageRecord;
use super::pricing::ModelPricing;

/// The rolling windows the panel renders, alongside the local calendar day.
const FIVE_HOURS: TimeDelta = TimeDelta::hours(5);
const SEVEN_DAYS: TimeDelta = TimeDelta::days(7);

/// Summed token counts plus computed cost for a window or breakdown bucket.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UsageTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    /// Computed from [`ModelPricing`] and **never displayed** — the account is
    /// subscription-based. See `pricing.rs`.
    pub cost_usd: f64,
}

impl UsageTotals {
    /// All token kinds combined (input + output + cache write + cache read).
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cache_creation_tokens)
            .saturating_add(self.cache_read_tokens)
    }

    /// Folds one already-priced record into the running totals.
    fn add(&mut self, record: &UsageRecord, cost: f64) {
        self.input_tokens = self.input_tokens.saturating_add(record.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(record.output_tokens);
        self.cache_creation_tokens = self
            .cache_creation_tokens
            .saturating_add(record.cache_creation_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(record.cache_read_tokens);
        self.cost_usd += cost;
    }
}

/// A named breakdown bucket (project or model) with its totals.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageBreakdown {
    pub name: String,
    pub totals: UsageTotals,
}

/// The aggregate view the panel renders.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageSummary {
    /// The local calendar day containing `now`.
    pub today: UsageTotals,
    pub last_5h: UsageTotals,
    pub last_7d: UsageTotals,
    /// Sorted by total tokens descending, name ascending as the tiebreak.
    pub projects_last_7d: Vec<UsageBreakdown>,
    /// Same ordering as [`UsageSummary::projects_last_7d`].
    pub models_last_7d: Vec<UsageBreakdown>,
}

/// First instant of the local calendar day containing `now`, as a UTC instant.
///
/// The offset is a parameter rather than read from the machine because nothing
/// in this crate touches the wall clock — `chrono`'s `clock` feature is off on
/// purpose. The shell passes the local offset; tests pass whatever boundary
/// they need to exercise.
#[must_use]
pub fn local_day_start(now: DateTime<Utc>, offset: FixedOffset) -> DateTime<Utc> {
    let shift = TimeDelta::seconds(i64::from(offset.local_minus_utc()));
    let local_midnight = (now.naive_utc() + shift).date().and_time(NaiveTime::MIN);
    DateTime::from_naive_utc_and_offset(local_midnight - shift, Utc)
}

/// Folds records into the panel's summary.
///
/// Records are deduped by `request_id`, first occurrence wins — the same call
/// can appear in more than one session file. A record whose timestamp is
/// unknown still consumes its `request_id` but lands in no window: it cannot be
/// dated, so counting it anywhere would be a guess.
#[must_use]
pub fn summarize(records: &[UsageRecord], now: DateTime<Utc>, offset: FixedOffset) -> UsageSummary {
    let midnight = local_day_start(now, offset);
    let five_hours_ago = now - FIVE_HOURS;
    let seven_days_ago = now - SEVEN_DAYS;

    let mut summary = UsageSummary::default();
    let mut project_totals: HashMap<&str, UsageTotals> = HashMap::new();
    let mut model_totals: HashMap<&str, UsageTotals> = HashMap::new();

    // First-wins dedup. Note this keys on the *raw* `request_id`, so records
    // that carried none share the empty key and collapse into one — matching
    // the Swift, where an absent id is simply not a distinguishing id.
    let mut seen: HashSet<&str> = HashSet::new();

    for record in records {
        if !seen.insert(record.request_id.as_str()) {
            continue;
        }
        let Some(timestamp) = record.timestamp else {
            continue;
        };
        let cost = ModelPricing::for_model(&record.model).cost(
            record.input_tokens,
            record.output_tokens,
            record.cache_creation_tokens,
            record.cache_read_tokens,
        );

        if timestamp >= midnight {
            summary.today.add(record, cost);
        }
        if timestamp >= five_hours_ago {
            summary.last_5h.add(record, cost);
        }
        if timestamp >= seven_days_ago {
            summary.last_7d.add(record, cost);
            project_totals
                .entry(record.project.as_str())
                .or_default()
                .add(record, cost);
            model_totals
                .entry(record.model.as_str())
                .or_default()
                .add(record, cost);
        }
    }

    summary.projects_last_7d = sorted_breakdowns(project_totals);
    summary.models_last_7d = sorted_breakdowns(model_totals);
    summary
}

/// Breakdowns sorted by total tokens descending, with name as a stable
/// tiebreaker — matching the token figures the panel displays (cost is computed
/// but not surfaced).
fn sorted_breakdowns(map: HashMap<&str, UsageTotals>) -> Vec<UsageBreakdown> {
    let mut rows: Vec<UsageBreakdown> = map
        .into_iter()
        .map(|(name, totals)| UsageBreakdown {
            name: name.to_string(),
            totals,
        })
        .collect();
    rows.sort_by(|a, b| {
        b.totals
            .total_tokens()
            .cmp(&a.totals.total_tokens())
            .then_with(|| a.name.cmp(&b.name))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixed reference "now" the Swift twins use — the same epoch second,
    /// which is 2026-05-30T12:00:00Z. (The Swift's comment says the 29th; every
    /// assertion there is relative, so the slip never showed up. The absolute
    /// assertions below would catch it, so the value is named correctly here.)
    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_780_142_400, 0).expect("valid reference instant")
    }

    fn utc() -> FixedOffset {
        FixedOffset::east_opt(0).expect("UTC is a valid offset")
    }

    /// Mirrors the Swift test helper: everything defaulted, offset in hours
    /// *back* from `now`.
    struct Rec {
        offset_hours: f64,
        request_id: &'static str,
        model: &'static str,
        project: &'static str,
        input: u64,
        output: u64,
        cache_write: u64,
        cache_read: u64,
    }

    impl Default for Rec {
        fn default() -> Self {
            Rec {
                offset_hours: 0.0,
                request_id: "req_1",
                model: "claude-sonnet-4-5",
                project: "gadget",
                input: 0,
                output: 0,
                cache_write: 0,
                cache_read: 0,
            }
        }
    }

    impl Rec {
        fn build(self) -> UsageRecord {
            let seconds = (self.offset_hours * 3600.0).round() as i64;
            UsageRecord {
                timestamp: Some(now() - TimeDelta::seconds(seconds)),
                request_id: self.request_id.to_string(),
                model: self.model.to_string(),
                project: self.project.to_string(),
                input_tokens: self.input,
                output_tokens: self.output,
                cache_creation_tokens: self.cache_write,
                cache_read_tokens: self.cache_read,
            }
        }
    }

    fn summarize_at_utc(records: &[UsageRecord]) -> UsageSummary {
        summarize(records, now(), utc())
    }

    // MARK: - Dedup

    /// Twin of `testDedupDropsDuplicateRequestId`.
    #[test]
    fn dedup_drops_a_duplicate_request_id() {
        let records = [
            Rec {
                request_id: "req_a",
                input: 100,
                output: 10,
                ..Rec::default()
            }
            .build(),
            // The same call seen in a second session file: must be ignored.
            Rec {
                request_id: "req_a",
                input: 100,
                output: 10,
                ..Rec::default()
            }
            .build(),
            Rec {
                request_id: "req_b",
                input: 50,
                output: 5,
                ..Rec::default()
            }
            .build(),
        ];
        let summary = summarize_at_utc(&records);
        assert_eq!(summary.last_7d.input_tokens, 150);
        assert_eq!(summary.last_7d.output_tokens, 15);
    }

    /// First occurrence wins, so a duplicate id carrying different numbers
    /// cannot inflate the totals by arriving second.
    #[test]
    fn the_first_occurrence_of_a_request_id_is_the_one_that_counts() {
        let records = [
            Rec {
                request_id: "dup",
                input: 10,
                ..Rec::default()
            }
            .build(),
            Rec {
                request_id: "dup",
                input: 999_999,
                ..Rec::default()
            }
            .build(),
        ];
        assert_eq!(summarize_at_utc(&records).last_7d.input_tokens, 10);
    }

    /// Records with no `requestId` share the empty key, so they collapse the
    /// same way the Swift collapses them. Pinned so a change here is a decision
    /// rather than a drift.
    #[test]
    fn records_without_a_request_id_collapse_into_one() {
        let records = [
            Rec {
                request_id: "",
                input: 10,
                ..Rec::default()
            }
            .build(),
            Rec {
                request_id: "",
                input: 10,
                ..Rec::default()
            }
            .build(),
        ];
        assert_eq!(summarize_at_utc(&records).last_7d.input_tokens, 10);
    }

    // MARK: - Windows

    /// Twin of `testFiveHourWindowExcludesOldIncludesRecent`.
    #[test]
    fn the_five_hour_window_excludes_older_calls() {
        let records = [
            Rec {
                offset_hours: 6.0,
                request_id: "old",
                input: 1000,
                ..Rec::default()
            }
            .build(),
            Rec {
                offset_hours: 1.0,
                request_id: "recent",
                input: 200,
                ..Rec::default()
            }
            .build(),
        ];
        let summary = summarize_at_utc(&records);
        assert_eq!(summary.last_5h.input_tokens, 200);
        assert_eq!(summary.last_7d.input_tokens, 1200, "both are within 7d");
    }

    /// Twin of `testSevenDayWindowExcludesOlderThanSevenDays`.
    #[test]
    fn the_seven_day_window_excludes_anything_older() {
        let records = [
            Rec {
                offset_hours: 24.0 * 8.0,
                request_id: "ancient",
                input: 999,
                ..Rec::default()
            }
            .build(),
            Rec {
                offset_hours: 24.0 * 3.0,
                request_id: "midweek",
                input: 300,
                ..Rec::default()
            }
            .build(),
        ];
        assert_eq!(summarize_at_utc(&records).last_7d.input_tokens, 300);
    }

    /// The boundary is inclusive on both windows: a record landing exactly on
    /// the cutoff is inside it.
    #[test]
    fn a_record_exactly_on_a_window_edge_is_inside_it() {
        let on_5h = Rec {
            offset_hours: 5.0,
            request_id: "edge5",
            input: 1,
            ..Rec::default()
        }
        .build();
        let on_7d = Rec {
            offset_hours: 24.0 * 7.0,
            request_id: "edge7",
            input: 1,
            ..Rec::default()
        }
        .build();
        let summary = summarize_at_utc(&[on_5h, on_7d]);
        assert_eq!(summary.last_5h.input_tokens, 1);
        assert_eq!(summary.last_7d.input_tokens, 2);
    }

    /// "Today" is a calendar day, not a rolling 24h: at 12:00 UTC a call from
    /// 13 hours earlier is yesterday, while the 7d window still holds it.
    #[test]
    fn today_is_the_local_calendar_day_not_a_rolling_day() {
        let records = [
            Rec {
                offset_hours: 13.0,
                request_id: "yesterday",
                input: 500,
                ..Rec::default()
            }
            .build(),
            Rec {
                offset_hours: 2.0,
                request_id: "this_morning",
                input: 20,
                ..Rec::default()
            }
            .build(),
        ];
        let summary = summarize_at_utc(&records);
        assert_eq!(summary.today.input_tokens, 20);
        assert_eq!(summary.last_7d.input_tokens, 520);
    }

    /// The day boundary follows the offset it is given, so the *same* call can
    /// be yesterday in one zone and today in another. At UTC the day starts
    /// 2026-05-30T00:00Z; at UTC-13 it starts 2026-05-29T13:00Z, which is why a
    /// call from 16 hours ago (2026-05-29T20:00Z) falls on opposite sides. This
    /// is the reason the offset is a parameter rather than an assumption.
    #[test]
    fn the_day_boundary_moves_with_the_offset() {
        let record = Rec {
            offset_hours: 16.0,
            request_id: "late_yesterday_utc",
            input: 42,
            ..Rec::default()
        }
        .build();
        let far_west = FixedOffset::west_opt(13 * 3600).expect("valid offset");

        let records = std::slice::from_ref(&record);
        assert_eq!(
            summarize_at_utc(records).today.input_tokens,
            0,
            "2026-05-29T20:00Z is yesterday in UTC"
        );
        assert_eq!(
            summarize(records, now(), far_west).today.input_tokens,
            42,
            "the same instant is 07:00 today at UTC-13"
        );
    }

    #[test]
    fn local_day_start_lands_on_midnight_in_the_given_offset() {
        let plus_two = FixedOffset::east_opt(2 * 3600).expect("valid offset");
        assert_eq!(
            local_day_start(now(), utc()).to_rfc3339(),
            "2026-05-30T00:00:00+00:00"
        );
        assert_eq!(
            local_day_start(now(), plus_two).to_rfc3339(),
            "2026-05-29T22:00:00+00:00",
            "midnight on the 30th at UTC+2 is 22:00Z on the 29th"
        );
    }

    /// An undatable record cannot be filed under any window — the alternative
    /// (an epoch sentinel) would quietly park it outside every window by luck
    /// rather than by rule, and a *future* sentinel would park it inside all of
    /// them.
    #[test]
    fn a_record_without_a_timestamp_lands_in_no_window() {
        let mut record = Rec {
            request_id: "undated",
            input: 100,
            ..Rec::default()
        }
        .build();
        record.timestamp = None;

        let summary = summarize_at_utc(&[record]);
        assert_eq!(summary.today, UsageTotals::default());
        assert_eq!(summary.last_5h, UsageTotals::default());
        assert_eq!(summary.last_7d, UsageTotals::default());
        assert!(summary.projects_last_7d.is_empty());
    }

    // MARK: - Cost (computed, never displayed)

    /// Twin of `testCostComputedFromPricingTablePerModel`.
    #[test]
    fn cost_comes_from_the_pricing_table() {
        let records = [Rec {
            request_id: "opus",
            model: "claude-opus-4-7",
            input: 1_000_000,
            output: 1_000_000,
            cache_write: 1_000_000,
            cache_read: 1_000_000,
            ..Rec::default()
        }
        .build()];
        let cost = summarize_at_utc(&records).last_7d.cost_usd;
        assert!((cost - 110.25).abs() < 1e-9, "got {cost}");
    }

    /// Twin of `testMixedModelCostSumsPerRecordPricing`: each record prices
    /// under its own model, so a mixed window is not priced at one rate.
    #[test]
    fn a_mixed_model_window_prices_each_record_separately() {
        let records = [
            Rec {
                request_id: "o",
                model: "claude-opus-4-7",
                input: 1_000_000,
                ..Rec::default()
            }
            .build(),
            Rec {
                request_id: "s",
                model: "claude-sonnet-4-5",
                input: 1_000_000,
                ..Rec::default()
            }
            .build(),
        ];
        let cost = summarize_at_utc(&records).last_7d.cost_usd;
        assert!((cost - 18.0).abs() < 1e-9, "15 opus + 3 sonnet, got {cost}");
    }

    /// Twin of `testUnknownModelPricedAsSonnet`.
    #[test]
    fn an_unknown_model_prices_as_sonnet() {
        let records = [Rec {
            request_id: "u",
            model: "some-future-model",
            input: 1_000_000,
            ..Rec::default()
        }
        .build()];
        let cost = summarize_at_utc(&records).last_7d.cost_usd;
        assert!((cost - 3.0).abs() < 1e-9, "got {cost}");
    }

    // MARK: - Breakdowns

    /// Twin of `testPerProjectAttributionSortedByTokensDesc`.
    #[test]
    fn per_project_totals_sort_by_tokens_then_name() {
        let records = [
            Rec {
                request_id: "a",
                model: "claude-sonnet-4-5",
                project: "gadget",
                input: 1_000_000,
                ..Rec::default()
            }
            .build(),
            Rec {
                request_id: "b",
                model: "claude-opus-4-7",
                project: "pipe-fitting",
                input: 1_000_000,
                ..Rec::default()
            }
            .build(),
        ];
        let summary = summarize_at_utc(&records);
        assert_eq!(summary.projects_last_7d.len(), 2);
        // Equal tokens (1M each), so the name tiebreak decides — and it is
        // alphabetical, not by cost: `gadget` leads despite the opus record
        // under `pipe-fitting` being the more expensive of the two. That
        // separation is the point of the test.
        assert_eq!(summary.projects_last_7d[0].name, "gadget");
        assert_eq!(summary.projects_last_7d[1].name, "pipe-fitting");
        assert!((summary.projects_last_7d[1].totals.cost_usd - 15.0).abs() < 1e-9);
    }

    /// Twin of `testPerModelBreakdownGroupsByModel`.
    #[test]
    fn per_model_totals_group_and_sort_by_tokens() {
        let records = [
            Rec {
                request_id: "a",
                model: "claude-opus-4-7",
                input: 1_000_000,
                ..Rec::default()
            }
            .build(),
            Rec {
                request_id: "b",
                model: "claude-opus-4-7",
                input: 1_000_000,
                ..Rec::default()
            }
            .build(),
            Rec {
                request_id: "c",
                model: "claude-sonnet-4-5",
                input: 1_000_000,
                ..Rec::default()
            }
            .build(),
        ];
        let summary = summarize_at_utc(&records);
        assert_eq!(summary.models_last_7d.len(), 2);
        assert_eq!(summary.models_last_7d[0].name, "claude-opus-4-7");
        assert_eq!(summary.models_last_7d[0].totals.input_tokens, 2_000_000);
        assert_eq!(summary.models_last_7d[1].name, "claude-sonnet-4-5");
    }

    /// Breakdowns cover the 7d window only, so a record outside it contributes
    /// no bucket at all — not an empty one.
    #[test]
    fn breakdowns_only_cover_the_seven_day_window() {
        let records = [Rec {
            offset_hours: 24.0 * 9.0,
            request_id: "ancient",
            project: "forgotten",
            input: 5,
            ..Rec::default()
        }
        .build()];
        let summary = summarize_at_utc(&records);
        assert!(summary.projects_last_7d.is_empty());
        assert!(summary.models_last_7d.is_empty());
    }

    /// Ordering must not depend on `HashMap` iteration order.
    #[test]
    fn breakdown_ordering_is_stable_across_runs() {
        let records: Vec<UsageRecord> = ["alpha", "bravo", "charlie", "delta", "echo"]
            .iter()
            .enumerate()
            .map(|(i, project)| UsageRecord {
                timestamp: Some(now()),
                request_id: format!("r{i}"),
                model: "claude-sonnet-4-5".to_string(),
                project: (*project).to_string(),
                input_tokens: 100,
                output_tokens: 0,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
            })
            .collect();

        let names: Vec<String> = summarize_at_utc(&records)
            .projects_last_7d
            .into_iter()
            .map(|b| b.name)
            .collect();
        assert_eq!(names, ["alpha", "bravo", "charlie", "delta", "echo"]);
        for _ in 0..8 {
            let again: Vec<String> = summarize_at_utc(&records)
                .projects_last_7d
                .into_iter()
                .map(|b| b.name)
                .collect();
            assert_eq!(again, names);
        }
    }

    // MARK: - Totals

    /// Twin of `testTotalsAggregateAllTokenKinds`.
    #[test]
    fn totals_aggregate_every_token_kind() {
        let records = [Rec {
            request_id: "a",
            input: 10,
            output: 20,
            cache_write: 30,
            cache_read: 40,
            ..Rec::default()
        }
        .build()];
        assert_eq!(summarize_at_utc(&records).last_7d.total_tokens(), 100);
    }

    #[test]
    fn no_records_summarize_to_zeroes_and_no_buckets() {
        let summary = summarize_at_utc(&[]);
        assert_eq!(summary, UsageSummary::default());
        assert_eq!(summary.last_7d.total_tokens(), 0);
    }
}
