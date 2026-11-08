//! GitHub repos grouped by owner: the part of `owner/name` before the slash.
//!
//! The one place an owner's group, totals and card position are decided. The
//! Repos panel payload, the overview tile and its Detail all read these, so a
//! group's header always counts the repos listed under it.
//!
//! Unknown is not zero: a total that includes a repo whose count is unknown is
//! itself unknown, and the sentence prints `—` for it.

use serde_json::Value;
use std::collections::BTreeMap;

/// An owner whose card would show more rows than this spans the full width
/// and splits its rows into columns, so no card is left mostly empty beside
/// it. Eight sits between the operator's small owners (2 and 4 repos) and the
/// large one (18), and above the overview's default per-owner limit of 5.
pub const CARD_SPAN_ROWS: usize = 8;

/// A status rank for a row that carries none: the panel's Healthy rank (0 is
/// Unreadable, 4 Healthy), so a row without one never darkens its owner's dot.
const UNRANKED: u8 = 4;

/// The part of a slug before the first `/`; the whole slug if there is none.
/// Never defaulted: a repo with no account still has an owner from its slug.
pub fn owner_of(slug: &str) -> &str {
    slug.split_once('/').map_or(slug, |(owner, _)| owner)
}

/// What grouping reads from one repo row. Built by [`owner_row`] from either
/// payload's `sortValues`, which carry raw counts rather than formatted cells.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnerRow<'a> {
    pub slug: &'a str,
    pub rank: u8,
    pub issues: Option<u64>,
    pub prs: Option<u64>,
    pub dependabot: Option<u64>,
    pub running: u64,
    pub attention: bool,
}

/// Reads `sortValues{status, issues, prs, dependabot, running}` and
/// `attention` off a repo row. A missing or null count is unknown; a missing
/// running count is zero, because the row lists its running jobs whenever it
/// has any; a missing status rank reads as Healthy.
pub fn owner_row<'a>(row: &'a Value, slug: &'a str) -> OwnerRow<'a> {
    let values = &row["sortValues"];
    OwnerRow {
        slug,
        rank: values["status"]
            .as_u64()
            .and_then(|r| u8::try_from(r).ok())
            .unwrap_or(UNRANKED),
        issues: values["issues"].as_u64(),
        prs: values["prs"].as_u64(),
        dependabot: values["dependabot"].as_u64(),
        running: values["running"].as_u64().unwrap_or(0),
        attention: row["attention"].as_bool().unwrap_or(false),
    }
}

/// An owner's totals over its member rows.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnerTotals {
    pub issues: Option<u64>,
    pub prs: Option<u64>,
    pub dependabot: Option<u64>,
    pub running: u64,
}

/// One owner and the rows under it.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnerGroup {
    /// As first seen: owners compare case-insensitively, and the first
    /// spelling labels the group.
    pub owner: String,
    /// Indices into the input rows, in input order.
    pub members: Vec<usize>,
    pub totals: OwnerTotals,
    /// The member with the lowest status rank, the first on ties: its colour
    /// is the owner's dot.
    pub worst: usize,
    pub attention: usize,
}

fn add(total: Option<u64>, value: Option<u64>) -> Option<u64> {
    Some(total? + value?)
}

/// Groups rows by owner, in case-insensitive owner order. Members keep the
/// input order, so a caller's sort applies within each owner.
pub fn group_by_owner(rows: &[OwnerRow<'_>]) -> Vec<OwnerGroup> {
    let mut groups: BTreeMap<String, OwnerGroup> = BTreeMap::new();
    for (index, row) in rows.iter().enumerate() {
        let owner = owner_of(row.slug);
        let group = groups
            .entry(owner.to_lowercase())
            .or_insert_with(|| OwnerGroup {
                owner: owner.to_owned(),
                members: Vec::new(),
                totals: OwnerTotals {
                    issues: Some(0),
                    prs: Some(0),
                    dependabot: Some(0),
                    running: 0,
                },
                worst: index,
                attention: 0,
            });
        group.members.push(index);
        group.totals.issues = add(group.totals.issues, row.issues);
        group.totals.prs = add(group.totals.prs, row.prs);
        group.totals.dependabot = add(group.totals.dependabot, row.dependabot);
        group.totals.running += row.running;
        if row.rank < rows[group.worst].rank {
            group.worst = index;
        }
        if row.attention {
            group.attention += 1;
        }
    }
    groups.into_values().collect()
}

fn count(value: Option<u64>, one: &str, many: &str) -> String {
    match value {
        Some(1) => format!("1 {one}"),
        Some(n) => format!("{n} {many}"),
        None => format!("— {many}"),
    }
}

/// `17 issues · 9 PRs · 5 Dependabot · 1 running`. A zero running count is
/// left out; an unknown count prints `—` in place of its number.
pub fn totals_text(totals: &OwnerTotals) -> String {
    let mut parts = vec![
        count(totals.issues, "issue", "issues"),
        count(totals.prs, "PR", "PRs"),
        count(totals.dependabot, "Dependabot", "Dependabot"),
    ];
    if totals.running > 0 {
        parts.push(format!("{} running", totals.running));
    }
    parts.join(" · ")
}

/// The order cards are laid out in, from each group's shown row count (groups
/// in name order): owners that fit in one card first, then owners that span
/// the full width, each in name order. Strict name order would strand two
/// small owners on separate, mostly empty rows whenever a big one sorts
/// between them. Returns `(group index, spans)`.
pub fn card_order(shown: &[usize]) -> Vec<(usize, bool)> {
    let spans = |rows: usize| rows > CARD_SPAN_ROWS;
    let mut order: Vec<(usize, bool)> = shown
        .iter()
        .enumerate()
        .map(|(index, &rows)| (index, spans(rows)))
        .collect();
    // Stable: name order survives within each half.
    order.sort_by_key(|&(_, spans)| spans);
    order
}

/// How many owner cards share a row in an overview tile. Tiles span 4, 6 and
/// 12 of the overview's 12 columns, so a medium tile is half the window and a
/// wide one all of it.
pub fn owner_card_columns(tile_width: &str) -> usize {
    match tile_width {
        "medium" => 2,
        "wide" => 3,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(slug: &str) -> Value {
        json!({"id": slug, "sortValues": {"status": 4, "issues": 0, "prs": 0, "dependabot": 0, "running": 0}, "attention": false})
    }

    fn rows_of(values: &[Value]) -> Vec<OwnerRow<'_>> {
        values
            .iter()
            .map(|v| owner_row(v, v["id"].as_str().unwrap()))
            .collect()
    }

    #[test]
    fn owner_is_the_part_before_the_first_slash() {
        assert_eq!(owner_of("acme/gadget"), "acme");
        assert_eq!(owner_of("acme/a/b"), "acme");
        assert_eq!(owner_of("noslash"), "noslash");
    }

    #[test]
    fn owners_compare_case_insensitively() {
        let values = [row("Acme/x"), row("globex/y"), row("acme/z")];
        let groups = group_by_owner(&rows_of(&values));
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].owner, "Acme");
        assert_eq!(groups[0].members, vec![0, 2]);
        assert_eq!(groups[1].owner, "globex");
        assert_eq!(groups[1].members, vec![1]);
    }

    #[test]
    fn groups_follow_owner_name_order_and_keep_input_order_within() {
        let values = [row("zeta/a"), row("acme/c"), row("acme/b")];
        let groups = group_by_owner(&rows_of(&values));
        let owners: Vec<&str> = groups.iter().map(|g| g.owner.as_str()).collect();
        assert_eq!(owners, ["acme", "zeta"]);
        assert_eq!(groups[0].members, vec![1, 2]);
    }

    #[test]
    fn totals_sum_and_running_counts() {
        let mut a = row("acme/a");
        a["sortValues"] =
            json!({"status": 4, "issues": 2, "prs": 1, "dependabot": 1, "running": 0});
        let mut b = row("acme/b");
        b["sortValues"] =
            json!({"status": 4, "issues": 5, "prs": 3, "dependabot": 0, "running": 2});
        let values = [a, b];
        let groups = group_by_owner(&rows_of(&values));
        assert_eq!(
            groups[0].totals,
            OwnerTotals {
                issues: Some(7),
                prs: Some(4),
                dependabot: Some(1),
                running: 2
            }
        );
    }

    #[test]
    fn an_unknown_count_makes_the_total_unknown() {
        let mut a = row("acme/a");
        a["sortValues"] =
            json!({"status": 4, "issues": null, "prs": 1, "dependabot": 0, "running": 0});
        let mut b = row("acme/b");
        b["sortValues"] =
            json!({"status": 4, "issues": 3, "prs": 2, "dependabot": 1, "running": 0});
        let values = [a, b];
        let groups = group_by_owner(&rows_of(&values));
        assert_eq!(groups[0].totals.issues, None);
        assert_eq!(groups[0].totals.prs, Some(3));
        assert_eq!(groups[0].totals.dependabot, Some(1));
    }

    #[test]
    fn worst_is_the_lowest_rank_first_on_ties() {
        let mut values = [row("acme/a"), row("acme/b"), row("acme/c")];
        values[1]["sortValues"]["status"] = json!(1);
        values[2]["sortValues"]["status"] = json!(1);
        let groups = group_by_owner(&rows_of(&values));
        assert_eq!(groups[0].worst, 1);
    }

    #[test]
    fn attention_counts_attention_rows() {
        let mut values = [row("acme/a"), row("acme/b"), row("acme/c")];
        values[0]["attention"] = json!(true);
        values[2]["attention"] = json!(true);
        let groups = group_by_owner(&rows_of(&values));
        assert_eq!(groups[0].attention, 2);
    }

    #[test]
    fn the_totals_sentence() {
        let t = |issues, prs, dependabot, running| OwnerTotals {
            issues,
            prs,
            dependabot,
            running,
        };
        assert_eq!(
            totals_text(&t(Some(17), Some(9), Some(5), 1)),
            "17 issues · 9 PRs · 5 Dependabot · 1 running"
        );
        assert_eq!(
            totals_text(&t(Some(1), Some(1), Some(0), 0)),
            "1 issue · 1 PR · 0 Dependabot"
        );
        assert_eq!(
            totals_text(&t(None, Some(2), None, 0)),
            "— issues · 2 PRs · — Dependabot"
        );
    }

    #[test]
    fn cards_put_non_spanning_owners_first() {
        assert_eq!(
            card_order(&[2, 18, 4]),
            vec![(0, false), (2, false), (1, true)]
        );
        assert_eq!(card_order(&[8]), vec![(0, false)]);
        assert_eq!(card_order(&[9]), vec![(0, true)]);
    }

    #[test]
    fn owner_card_columns_follow_the_tile_width() {
        assert_eq!(owner_card_columns("small"), 1);
        assert_eq!(owner_card_columns("medium"), 2);
        assert_eq!(owner_card_columns("wide"), 3);
        assert_eq!(owner_card_columns("bogus"), 1);
    }

    #[test]
    fn owner_row_reads_sort_values() {
        let v = json!({"sortValues": {"status": 1, "issues": 3, "prs": null, "dependabot": 2, "running": 1}, "attention": true});
        let r = owner_row(&v, "acme/x");
        assert_eq!(r.slug, "acme/x");
        assert_eq!(r.rank, 1);
        assert_eq!(r.issues, Some(3));
        assert_eq!(r.prs, None);
        assert_eq!(r.dependabot, Some(2));
        assert_eq!(r.running, 1);
        assert!(r.attention);
    }

    #[test]
    fn a_row_without_sort_values_is_unknown_and_ranked_last() {
        let v = json!({});
        let r = owner_row(&v, "acme/x");
        assert_eq!(
            (
                r.rank,
                r.issues,
                r.prs,
                r.dependabot,
                r.running,
                r.attention
            ),
            (4, None, None, None, 0, false)
        );
    }
}
