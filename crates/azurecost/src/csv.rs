//! Network-free parsing and aggregation of the platform-owned Azure Cost
//! Management export. Port of
//! `DevCanopy/Services/AzureCost/AzureCostCSV.swift`, which is itself a port of
//! mission-control's `azure-cost/query.ts` — three consumers, one reading of
//! the same file.
//!
//! Columns read (the Cost Management "ActualCost" export schema): the panel
//! shows USD, so the sum is over `costInUsd`; one top-N tile groups by
//! `resourceGroupName`, the other by `meterCategory` (the human-readable
//! service type — "Virtual Network", "NAT Gateway", "SQL Database", "Storage").

use std::collections::BTreeMap;

use crate::error::{AzureCostError, COST_COLUMN};
use crate::models::{CostAggregate, ResourceCost};

const RESOURCE_GROUP_COLUMN: &str = "resourceGroupName";
const METER_CATEGORY_COLUMN: &str = "meterCategory";

/// Where a row with no resource group lands (subscription-level charges).
const UNASSIGNED_GROUP: &str = "(unassigned)";
/// Where a row with no meter category lands (reservation/capacity charges).
const OTHER_CATEGORY: &str = "(other)";

/// Lenient numeric parse matching JS `Number(v)`: blanks, non-numbers and
/// non-finite values fold to 0 rather than failing, so one stray short row
/// never poisons the whole month's sum.
fn num(value: &str) -> f64 {
    match value.trim().parse::<f64>() {
        Ok(parsed) if parsed.is_finite() => parsed,
        _ => 0.0,
    }
}

/// Parse RFC 4180 CSV into rows of fields.
///
/// A full character state machine, not a line split: the export's `tags` column
/// embeds quoted JSON with commas, and a quoted field may in principle contain
/// newlines. A leading UTF-8 BOM is stripped (the export's header carries one).
///
/// The Swift original had to scan Unicode *scalars* rather than `Character`s,
/// because Swift folds a CRLF into a single grapheme cluster that equals
/// neither `"\r"` nor `"\n"` — which once collapsed a whole CRLF export into
/// one row and summed the month to $0. Rust's `char` is already a scalar, so
/// the same scan is correct here by construction; the CRLF test below is the
/// guard that keeps it that way.
#[must_use]
pub fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut field = String::new();
    let mut row: Vec<String> = Vec::new();
    let mut in_quotes = false;

    let chars: Vec<char> = text
        .strip_prefix('\u{FEFF}')
        .unwrap_or(text)
        .chars()
        .collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_quotes {
            if c == '"' {
                if chars.get(i + 1) == Some(&'"') {
                    field.push('"'); // an escaped quote
                    i += 1;
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == ',' {
            row.push(std::mem::take(&mut field));
        } else if c == '\n' || c == '\r' {
            // End the row on either; swallow the `\n` of a `\r\n` pair.
            if c == '\r' && chars.get(i + 1) == Some(&'\n') {
                i += 1;
            }
            row.push(std::mem::take(&mut field));
            rows.push(std::mem::take(&mut row));
        } else {
            field.push(c);
        }
        i += 1;
    }
    // Flush a trailing field/row with no terminating newline.
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

/// Sum one export CSV: total `costInUsd`, plus the two breakdowns (each sorted
/// descending) — by resource group and by resource type (`meterCategory`).
///
/// **Resource groups** are grouped by, and displayed as, the *lowercased* name:
/// Azure resource-group names are case-insensitive and the export can report
/// the same group as both `rg-sassydog` and `RG-SASSYDOG` across rows. Rows
/// with no group fold into `(unassigned)`.
///
/// **Resource types** keep their original casing — `meterCategory` holds
/// human-readable display names, so lowercasing them would be a rendering bug.
/// Blank rows fold into `(other)`.
///
/// # Errors
///
/// [`AzureCostError::MissingColumn`] when the header has no `costInUsd` column.
/// An entirely empty export is *not* an error: it aggregates to zero.
pub fn aggregate_cost_csv(csv: &str) -> Result<CostAggregate, AzureCostError> {
    let rows = parse_csv(csv);
    let Some(header) = rows.first() else {
        return Ok(CostAggregate::default());
    };

    let cost_idx = header
        .iter()
        .position(|column| column == COST_COLUMN)
        .ok_or_else(|| AzureCostError::MissingColumn(COST_COLUMN.to_owned()))?;
    let rg_idx = header
        .iter()
        .position(|column| column == RESOURCE_GROUP_COLUMN);
    let type_idx = header
        .iter()
        .position(|column| column == METER_CATEGORY_COLUMN);

    let mut total = 0.0;
    let mut by_resource_group: BTreeMap<String, f64> = BTreeMap::new();
    let mut by_meter_category: BTreeMap<String, f64> = BTreeMap::new();
    for row in rows.iter().skip(1) {
        // Skip blank/short trailing lines rather than reading them as $0 rows.
        let Some(cost_field) = row.get(cost_idx) else {
            continue;
        };
        let cost = num(cost_field);
        total += cost;

        let group = cell(row, rg_idx);
        let group = if group.is_empty() {
            UNASSIGNED_GROUP.to_owned()
        } else {
            group.to_lowercase()
        };
        *by_resource_group.entry(group).or_default() += cost;

        let category = cell(row, type_idx);
        let category = if category.is_empty() {
            OTHER_CATEGORY
        } else {
            category
        };
        *by_meter_category.entry(category.to_owned()).or_default() += cost;
    }

    Ok(CostAggregate {
        total,
        by_resource: sorted_costs(by_resource_group),
        by_type: sorted_costs(by_meter_category),
    })
}

/// A trimmed cell, or `""` when the column is absent from the header or the row
/// is short — a missing optional column is not a parse failure.
fn cell(row: &[String], index: Option<usize>) -> &str {
    index
        .and_then(|i| row.get(i))
        .map_or("", |value| value.trim())
}

/// Fold a `name → cost` map into [`ResourceCost`]s sorted by cost descending.
///
/// The input is a `BTreeMap` and `sort_by` is stable, so equal costs come out
/// in name order. A `HashMap` would have made the tie order depend on hash
/// seeding — different every run, and the panel's top-5 list would shuffle.
pub(crate) fn sorted_costs(totals: BTreeMap<String, f64>) -> Vec<ResourceCost> {
    let mut costs: Vec<ResourceCost> = totals
        .into_iter()
        .map(|(name, cost)| ResourceCost { name, cost })
        .collect();
    costs.sort_by(|a, b| b.cost.total_cmp(&a.cost));
    costs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::assert_resources;

    // MARK: parse_csv (RFC 4180)

    #[test]
    fn parse_csv_keeps_commas_and_escaped_quotes_inside_quoted_fields() {
        // The export's `tags` column embeds quoted JSON with commas — a naive
        // split would shift every later column. `""` is an escaped quote.
        let rows = parse_csv("a,b,c\n1,\"x,y\",\"he said \"\"hi\"\"\"\n");
        assert_eq!(rows[0], ["a", "b", "c"]);
        assert_eq!(rows[1], ["1", "x,y", "he said \"hi\""]);
    }

    /// The real shape of the `tags` cell: a JSON object whose commas and quotes
    /// must not disturb the column positions of anything after it.
    #[test]
    fn parse_csv_survives_a_quoted_json_tags_column() {
        let rows = parse_csv(
            "tags,costInUsd\n\"{\"\"env\"\":\"\"prod\"\",\"\"team\"\":\"\"x\"\"}\",12.5\n",
        );
        assert_eq!(
            rows[1],
            [
                "{\"env\":\"prod\",\"team\":\"x\"}".to_owned(),
                "12.5".to_owned()
            ]
        );
    }

    #[test]
    fn parse_csv_strips_a_leading_utf8_bom_from_the_header() {
        let rows = parse_csv("\u{FEFF}col\nval\n");
        assert_eq!(rows[0], ["col"]);
    }

    #[test]
    fn parse_csv_splits_rows_on_crlf_line_endings() {
        let rows = parse_csv("a,b\r\n1,2\r\n3,4\r\n");
        assert_eq!(rows, [["a", "b"], ["1", "2"], ["3", "4"]]);
    }

    /// A quoted field may legally span a newline; the state machine must carry
    /// the row across it instead of breaking mid-record.
    #[test]
    fn parse_csv_keeps_a_newline_inside_a_quoted_field() {
        let rows = parse_csv("a,b\n\"line1\nline2\",2\n");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1], ["line1\nline2", "2"]);
    }

    /// A final row with no terminating newline still counts.
    #[test]
    fn parse_csv_flushes_a_row_with_no_trailing_newline() {
        assert_eq!(parse_csv("a,b\n1,2"), [["a", "b"], ["1", "2"]]);
    }

    #[test]
    fn parse_csv_of_an_empty_string_has_no_rows() {
        assert!(parse_csv("").is_empty());
    }

    // MARK: aggregate_cost_csv

    /// Mirrors the production failure: a CRLF export summed to $0 because every
    /// data row folded into the header row.
    #[test]
    fn aggregate_sums_cost_across_crlf_rows() {
        let agg = aggregate_cost_csv("resourceGroupName,costInUsd\r\nrg-a,10\r\nrg-b,5\r\n")
            .expect("cost column present");
        assert!((agg.total - 15.0).abs() < 1e-6);
        assert_resources(&agg.by_resource, &[("rg-a", 10.0), ("rg-b", 5.0)]);
    }

    #[test]
    fn aggregate_sums_cost_in_usd_and_groups_by_resource_group_desc() {
        // A miniature export with a tags column carrying an embedded comma, to
        // prove the cost column is still found by name after a quoted field.
        let csv = [
            "date,resourceGroupName,costInUsd,tags",
            "2026-06-01,rg-a,12.5,\"{\"\"env\"\":\"\"prod\"\",\"\"team\"\":\"\"x\"\"}\"",
            "2026-06-02,rg-b,7.5,",
            "2026-06-03,,3,", // no resource group → (unassigned)
            "2026-06-04,rg-a,2.5,",
            "", // trailing blank line
        ]
        .join("\n");

        let agg = aggregate_cost_csv(&csv).expect("cost column present");
        assert!((agg.total - 25.5).abs() < 1e-6);
        assert_resources(
            &agg.by_resource,
            &[("rg-a", 15.0), ("rg-b", 7.5), ("(unassigned)", 3.0)],
        );
    }

    #[test]
    fn aggregate_folds_case_variant_resource_groups_into_one() {
        let csv = [
            "resourceGroupName,costInUsd",
            "rg-sassydog,175.45",
            "RG-SASSYDOG,22.12", // same group, reported uppercase in some rows
            "rg-velovate-prd,321.65",
        ]
        .join("\n");

        let agg = aggregate_cost_csv(&csv).expect("cost column present");
        assert_resources(
            &agg.by_resource,
            &[("rg-velovate-prd", 321.65), ("rg-sassydog", 197.57)],
        );
    }

    #[test]
    fn aggregate_normalizes_display_casing_to_lowercase() {
        let agg = aggregate_cost_csv("resourceGroupName,costInUsd\nRG-PACKER-BUILD,0.03")
            .expect("cost column present");
        assert_resources(&agg.by_resource, &[("rg-packer-build", 0.03)]);
    }

    /// `meterCategory` is the resource-TYPE column. Unlike resource groups it
    /// holds human-readable display names, so casing is preserved; the same
    /// type across groups merges; blank rows fold into `(other)`.
    #[test]
    fn aggregate_groups_by_meter_category_preserving_casing_and_folding_blanks() {
        let csv = [
            "resourceGroupName,meterCategory,costInUsd",
            "rg-a,Virtual Network,40.96",
            "rg-a,NAT Gateway,28.67",
            "rg-b,Virtual Network,9.04",
            "rg-c,,72.27",
        ]
        .join("\n");

        let agg = aggregate_cost_csv(&csv).expect("cost column present");
        assert!((agg.total - 150.94).abs() < 1e-6);
        assert_resources(
            &agg.by_type,
            &[
                ("(other)", 72.27),
                ("Virtual Network", 50.0),
                ("NAT Gateway", 28.67),
            ],
        );
    }

    #[test]
    fn aggregate_errors_when_the_cost_column_is_absent() {
        let err = aggregate_cost_csv("date,resourceGroupName\n2026-06-01,rg-a").unwrap_err();
        assert_eq!(err, AzureCostError::MissingColumn("costInUsd".to_owned()));
    }

    #[test]
    fn aggregate_handles_an_empty_export_gracefully() {
        let agg = aggregate_cost_csv("").expect("an empty export is not an error");
        assert_eq!(agg, CostAggregate::default());
    }

    /// A header with no rows is a real state between export runs — zero, not an
    /// error, and not a fabricated figure either.
    #[test]
    fn aggregate_of_a_header_only_export_is_zero() {
        let agg = aggregate_cost_csv("resourceGroupName,costInUsd\n").expect("cost column present");
        assert_eq!(agg, CostAggregate::default());
    }

    /// A non-numeric or blank cost folds to 0 instead of failing the read; a
    /// short row is skipped entirely rather than counted as an `(unassigned)`
    /// zero.
    #[test]
    fn aggregate_tolerates_unparseable_costs_and_short_rows() {
        let csv = [
            "resourceGroupName,costInUsd",
            "rg-a,10",
            "rg-b,not-a-number",
            "rg-c,",
            "rg-d", // short row: no cost cell at all
            "rg-e,NaN",
            "rg-f,inf",
        ]
        .join("\n");

        let agg = aggregate_cost_csv(&csv).expect("cost column present");
        assert!((agg.total - 10.0).abs() < 1e-6);
        assert_eq!(agg.by_resource.len(), 5, "rg-d's short row is skipped");
        assert_eq!(agg.by_resource[0].name, "rg-a");
    }

    /// Equal costs must not shuffle between polls — the top-5 tile would
    /// reorder for no reason.
    #[test]
    fn ties_are_broken_by_name_not_by_hash_order() {
        let csv = "resourceGroupName,costInUsd\nrg-c,5\nrg-a,5\nrg-b,5";
        let names: Vec<String> = aggregate_cost_csv(csv)
            .expect("cost column present")
            .by_resource
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(names, ["rg-a", "rg-b", "rg-c"]);
    }
}
