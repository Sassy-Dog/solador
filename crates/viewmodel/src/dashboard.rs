//! Dashboard ordering and runner aggregation over the cached source rows.

use crate::color;
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

/// Sort raw numeric values, never formatted cells. Unknowns stay last in
/// either direction; identity breaks ties to avoid movement between polls.
pub fn sort_repos(rows: &mut [&Value], column: &str, descending: bool) {
    rows.sort_by(|a, b| {
        let order = if column == "name" {
            text(a, "label")
                .to_lowercase()
                .cmp(&text(b, "label").to_lowercase())
        } else {
            match (
                a["sortValues"][column].as_u64(),
                b["sortValues"][column].as_u64(),
            ) {
                (Some(a), Some(b)) => a.cmp(&b),
                (Some(_), None) => return Ordering::Less,
                (None, Some(_)) => return Ordering::Greater,
                (None, None) => Ordering::Equal,
            }
        };
        (if descending { order.reverse() } else { order })
            .then_with(|| text(a, "id").cmp(text(b, "id")))
    });
}

fn text<'a>(row: &'a Value, key: &str) -> &'a str {
    row[key].as_str().unwrap_or_default()
}

/// The one place a runner's group is decided. Every runner row carries the
/// answer (`groupId`, `groupLabel`, `groupRank`, `groupMatch`, `groupSplit`)
/// and every view — a grouped tile, its inline Detail, the Full panel — reads
/// those fields rather than re-deriving them, so a group always opens onto the
/// runners it counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerGroup {
    pub id: String,
    pub label: String,
    /// Order within one OS · architecture: the operator's list order, then
    /// `other`.
    pub rank: usize,
    /// The listed label that claimed the runner, in the operator's spelling;
    /// `None` for a plain group and for `other`.
    pub matched: Option<String>,
    /// Whether the runner's OS · architecture is split by a listed label.
    pub split: bool,
}

/// One OS · architecture, as the group identity spells it: `LINUX:X64`.
#[must_use]
pub fn platform_id(os: &str, architecture: Option<&str>) -> String {
    format!("{os}:{}", architecture.unwrap_or("Unknown architecture"))
}

/// The platforms ([`platform_id`]) in which at least one runner carries a
/// listed label: the only ones a split applies to, so a platform the label
/// never appears in keeps its plain group rather than becoming `· other`.
///
/// Decided over every runner Solador knows — all organizations, registered and
/// remembered — never one tile's scope, because every view reads the group
/// stamped on the row and a group has to mean the same thing in all of them.
/// Each item is `(OS uppercased, architecture, labels)`.
#[must_use]
pub fn split_platforms<'a, I>(runners: I, group_labels: &[String]) -> BTreeSet<String>
where
    I: IntoIterator<Item = (String, Option<&'a str>, Option<&'a [String]>)>,
{
    if group_labels.is_empty() {
        return BTreeSet::new();
    }
    runners
        .into_iter()
        .filter(|(_, _, labels)| label_split(*labels, group_labels).is_some())
        .map(|(os, architecture, _)| platform_id(&os, architecture))
        .collect()
}

/// The runner's group under the operator's group labels (`store.json`'s
/// `runner_group_labels`). A platform outside `split` — every platform, when
/// the list is empty — is exactly the OS · architecture grouping that
/// predates the labels, identity included. Inside it, a runner joins its first
/// listed label's group, and anything else is `other`: a runner carrying none
/// of them, and one whose labels were never recorded, which the operator reads
/// as "the rest" and which keeps its alarm there.
#[must_use]
pub fn runner_group(
    os: &str,
    architecture: Option<&str>,
    labels: Option<&[String]>,
    group_labels: &[String],
    split: &BTreeSet<String>,
) -> RunnerGroup {
    let platform = platform_id(os, architecture);
    let id = format!("group:{platform}");
    let label = format!("{os} · {}", architecture.unwrap_or("Unknown architecture"));
    if group_labels.is_empty() || !split.contains(&platform) {
        return RunnerGroup {
            id,
            label,
            rank: 0,
            matched: None,
            split: false,
        };
    }
    match label_split(labels, group_labels) {
        Some(index) => RunnerGroup {
            id: format!("{id}:label:{}", group_labels[index].to_ascii_lowercase()),
            label: format!("{label} · {}", group_labels[index]),
            rank: index,
            matched: Some(group_labels[index].clone()),
            split: true,
        },
        None => RunnerGroup {
            id: format!("{id}:other"),
            label: format!("{label} · other"),
            rank: group_labels.len(),
            matched: None,
            split: true,
        },
    }
}

/// The index of the listed label that claims a runner carrying `labels`, or
/// `None` when it carries none of them or its labels were never recorded.
///
/// `group_labels` is the operator's list, in their priority order. The list's
/// order decides, not the order GitHub reports the runner's own labels in.
/// Matching is ASCII case-insensitive, the way GitHub matches a `runs-on`
/// label to a runner's.
fn label_split(labels: Option<&[String]>, group_labels: &[String]) -> Option<usize> {
    let labels = labels?;
    group_labels
        .iter()
        .position(|wanted| carries_label(labels, wanted))
}

/// What a runner whose labels were never recorded shows where its labels go.
pub const LABELS_NOT_RECORDED: &str = "— not recorded";

/// Whether `label` is one GitHub assigns every self-hosted runner: the
/// `self-hosted` marker, its OS and its architecture. They are dimmed, never
/// hidden, and never offered as a group label.
#[must_use]
pub fn is_standard_label(label: &str) -> bool {
    [
        "self-hosted",
        "linux",
        "macos",
        "windows",
        "x64",
        "arm",
        "arm64",
    ]
    .iter()
    .any(|standard| label.eq_ignore_ascii_case(standard))
}

/// A runner's labels as chips, in GitHub's order: the one that put it in its
/// group is `match`, GitHub's own are `standard`, the rest `custom`. `null`
/// when the labels were never recorded, which a view renders as
/// [`LABELS_NOT_RECORDED`] rather than an empty list.
#[must_use]
pub fn label_chips(labels: Option<&[String]>, matched: Option<&str>) -> Value {
    let Some(labels) = labels else {
        return Value::Null;
    };
    labels
        .iter()
        .map(|label| {
            let kind = if matched.is_some_and(|m| label.eq_ignore_ascii_case(m)) {
                "match"
            } else if is_standard_label(label) {
                "standard"
            } else {
                "custom"
            };
            json!({"text": label, "kind": kind})
        })
        .collect()
}

/// Whether a runner carrying `labels` carries `wanted` — ASCII
/// case-insensitive, as GitHub matches `runs-on`. The one matching rule:
/// Settings' "carried by N runners" count asks it too, so the count can never
/// disagree with the grouping it predicts.
#[must_use]
pub fn carries_label(labels: &[String], wanted: &str) -> bool {
    labels
        .iter()
        .any(|label| label.eq_ignore_ascii_case(wanted))
}

/// Each absent runner remains in its remembered type, but cannot contribute
/// to busy/idle/online. Group only the rows selected by this tile's scope.
///
/// Each row's group is the one [`runner_group`] stamped on it; a row built
/// without one (no `groupId`) falls into its plain OS · architecture group.
/// A runner group's colour is its load: busy ÷ online, where online is busy +
/// idle — the runners that could take a job right now. Offline, recycling and
/// missing runners are not capacity, so they are in neither number.
///
/// Gray while nothing is busy, green, amber from half busy, red once no
/// runner is free: every online runner busy, or none online at all, which a
/// queued job cannot tell apart. Saturation is colour only — it never raises
/// attention, because CI bursts would flap it. A missing runner (`attention`)
/// is the alarm and wins over every load reading.
fn group_load_color(attention: bool, busy: usize, idle: usize) -> u32 {
    let online = busy + idle;
    // `busy == online` is also none online (0 == 0), which is why it sits
    // above `busy == 0`: a group no job can reach must never read as idle.
    if attention || busy == online {
        color::RED
    } else if busy == 0 {
        color::MUTED
    } else if busy * 2 >= online {
        color::AMBER
    } else {
        color::GREEN
    }
}

pub fn group_runners(rows: &[&Value]) -> Vec<Value> {
    type Key = (String, String, u64, String);
    let mut groups: BTreeMap<Key, (String, Vec<&Value>)> = BTreeMap::new();
    for row in rows {
        let os = row["os"].as_str().unwrap_or("Unknown OS");
        let architecture = row["architecture"].as_str();
        let plain = runner_group(os, architecture, None, &[], &BTreeSet::new());
        let id = row["groupId"].as_str().map_or(plain.id, str::to_owned);
        let label = row["groupLabel"]
            .as_str()
            .map_or(plain.label, str::to_owned);
        let rank = row["groupRank"].as_u64().unwrap_or(0);
        let architecture = architecture.unwrap_or("Unknown architecture");
        groups
            .entry((os.into(), architecture.into(), rank, id))
            .or_insert_with(|| (label, Vec::new()))
            .1
            .push(row);
    }
    groups.into_iter().map(|((_, _, _, id), (label, members))| {
        let count = |state| members.iter().filter(|r| r["runnerState"] == state).count();
        let (busy, idle, offline, missing, recycling) = (count("busy"), count("idle"), count("offline"), count("missing"), count("recycling"));
        let total = members.len();
        let unknown = total - busy - idle - offline - missing - recycling;
        let attention_count = members.iter().filter(|r| r["attention"] == true).count();
        let attention = attention_count > 0;
        let tint = color::hex(group_load_color(attention, busy, idle));
        let online = busy + idle;
        // A missing runner is the alarm, so the value says so rather than
        // pairing a load figure with the alarm's colour; the load stays in
        // the line under it.
        let value = if missing > 0 {
            format!("{missing} missing")
        } else if online == 0 {
            "none online".to_owned()
        } else {
            format!("{busy}/{online} busy")
        };
        let mut detail = format!("{busy} busy · {idle} idle · {offline} offline");
        if recycling > 0 { detail.push_str(&format!(" · {recycling} recycling")); }
        if missing > 0 { detail.push_str(&format!(" · {missing} missing")); }
        if unknown > 0 { detail.push_str(&format!(" · {unknown} unknown")); }
        // Only an explicit `null` is "not recorded": a row with no `labels`
        // key made no claim about the roster either way.
        let not_recorded = members
            .iter()
            .filter(|r| r.get("labels") == Some(&Value::Null))
            .count();
        let matched = members.first().and_then(|r| r["groupMatch"].as_str());
        let split = members.first().is_some_and(|r| r["groupSplit"] == true);
        let match_line = group_match_line(matched, split, total, not_recorded);
        json!({"id":id,"label":label,"match":match_line,
            "value":value,"color":tint,"valueColor":tint,
            "attention":attention,"attentionCount":attention_count,"detail":detail,"compactDetail":detail,"grouped":true,
            "summary":{"total":total,"busy":busy,"idle":idle,"offline":offline,"missing":missing,"recycling":recycling,"unknown":unknown,"notRecorded":not_recorded}})
    }).collect()
}

/// What put a group's runners in it, for the headers that show one group:
/// `None` for a plain group. `other` cannot claim "carries none" of labels
/// nobody has seen, so it counts the runners whose labels were never recorded.
fn group_match_line(
    matched: Option<&str>,
    split: bool,
    total: usize,
    not_recorded: usize,
) -> Option<String> {
    if let Some(label) = matched {
        return Some(format!("matched by {label}"));
    }
    if !split {
        return None;
    }
    Some(match not_recorded {
        0 => "carries none of your group labels".to_owned(),
        n if n == total => "labels not recorded".to_owned(),
        n => format!("carries none of your group labels · {n} not recorded"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn group(id: &str, label: &str, rank: usize) -> RunnerGroup {
        RunnerGroup {
            id: id.into(),
            label: label.into(),
            rank,
            matched: None,
            split: false,
        }
    }

    /// A group of a split platform: `matched` is the listed label that claimed
    /// it, `None` for `other`.
    fn split_group(id: &str, label: &str, rank: usize, matched: Option<&str>) -> RunnerGroup {
        RunnerGroup {
            matched: matched.map(str::to_owned),
            split: true,
            ..group(id, label, rank)
        }
    }

    fn split(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    /// With no group labels the identity is byte-for-byte the one that
    /// predates them, so a Detail selection saved before the upgrade opens.
    #[test]
    fn no_group_labels_keep_the_os_and_architecture_identity() {
        let large = strings(&["self-hosted", "runner-large"]);
        let everywhere = split(&["LINUX:X64", "LINUX:Unknown architecture"]);
        assert_eq!(
            runner_group("LINUX", Some("X64"), Some(&large), &[], &everywhere),
            group("group:LINUX:X64", "LINUX · X64", 0)
        );
        assert_eq!(
            runner_group("LINUX", None, None, &[], &everywhere),
            group(
                "group:LINUX:Unknown architecture",
                "LINUX · Unknown architecture",
                0
            )
        );
    }

    /// GitHub matches `runs-on` labels case-insensitively, so the split does
    /// too; the group is titled with the operator's own spelling.
    #[test]
    fn a_runner_joins_the_group_of_a_listed_label_it_carries_in_any_case() {
        let carried = strings(&["self-hosted", "Linux", "X64", "runner-large"]);
        assert_eq!(
            runner_group(
                "LINUX",
                Some("X64"),
                Some(&carried),
                &strings(&["Runner-Large"]),
                &split(&["LINUX:X64"])
            ),
            split_group(
                "group:LINUX:X64:label:runner-large",
                "LINUX · X64 · Runner-Large",
                0,
                Some("Runner-Large")
            )
        );
    }

    /// The operator's list order is the priority, not the order GitHub
    /// happens to report the runner's own labels in.
    #[test]
    fn the_first_listed_label_claims_a_runner_carrying_several() {
        let carried = strings(&["runner-large", "gpu"]);
        assert_eq!(
            runner_group(
                "LINUX",
                Some("X64"),
                Some(&carried),
                &strings(&["gpu", "runner-large"]),
                &split(&["LINUX:X64"])
            ),
            split_group(
                "group:LINUX:X64:label:gpu",
                "LINUX · X64 · gpu",
                0,
                Some("gpu")
            )
        );
    }

    /// A runner carrying none of the listed labels — the small runners, which
    /// carry no size label at all — is `other`, after every listed label.
    #[test]
    fn a_runner_carrying_none_of_the_listed_labels_is_other() {
        let carried = strings(&["self-hosted", "Linux", "X64"]);
        assert_eq!(
            runner_group(
                "LINUX",
                Some("X64"),
                Some(&carried),
                &strings(&["runner-large", "gpu"]),
                &split(&["LINUX:X64"])
            ),
            split_group("group:LINUX:X64:other", "LINUX · X64 · other", 2, None)
        );
    }

    /// A platform no listed label appears in is not split: no `· other`
    /// suffix, and the plain identity.
    #[test]
    fn a_platform_no_listed_label_appears_in_stays_plain() {
        let carried = strings(&["self-hosted", "macOS", "ARM64"]);
        let g = runner_group(
            "MACOS",
            Some("ARM64"),
            Some(&carried),
            &strings(&["runner-large"]),
            &split(&["LINUX:X64"]),
        );
        assert_eq!(
            (g.id.as_str(), g.label.as_str(), g.matched, g.split),
            ("group:MACOS:ARM64", "MACOS · ARM64", None, false)
        );
    }

    /// The operator's "the rest": a runner whose labels were never recorded
    /// joins `other` rather than a group of its own.
    #[test]
    fn unrecorded_labels_join_other_where_the_platform_is_split() {
        let g = runner_group(
            "LINUX",
            Some("X64"),
            None,
            &strings(&["runner-large"]),
            &split(&["LINUX:X64"]),
        );
        assert_eq!(
            (g.id.as_str(), g.label.as_str(), g.rank, g.split),
            ("group:LINUX:X64:other", "LINUX · X64 · other", 1, true)
        );
    }

    #[test]
    fn unrecorded_labels_stay_plain_where_the_platform_is_not_split() {
        let g = runner_group(
            "LINUX",
            Some("X64"),
            None,
            &strings(&["runner-large"]),
            &BTreeSet::new(),
        );
        assert_eq!(g.id, "group:LINUX:X64");
    }

    #[test]
    fn split_platforms_name_only_platforms_carrying_a_listed_label() {
        let large = strings(&["self-hosted", "Runner-Large"]);
        let plain = strings(&["self-hosted"]);
        let got = split_platforms(
            [
                ("LINUX".to_owned(), Some("X64"), Some(&large[..])),
                ("MACOS".to_owned(), Some("ARM64"), Some(&plain[..])),
                ("LINUX".to_owned(), Some("ARM64"), None),
            ],
            &strings(&["runner-large"]),
        );
        assert_eq!(got, split(&["LINUX:X64"]));
        assert!(
            split_platforms([("LINUX".to_owned(), Some("X64"), Some(&large[..]))], &[]).is_empty()
        );
    }

    #[test]
    fn label_chips_mark_the_match_ignoring_case() {
        let carried = strings(&["self-hosted", "Linux", "X64", "Runner-Large", "gpu"]);
        assert_eq!(
            label_chips(Some(&carried), Some("runner-large")),
            json!([
                {"text":"self-hosted","kind":"standard"}, {"text":"Linux","kind":"standard"},
                {"text":"X64","kind":"standard"}, {"text":"Runner-Large","kind":"match"},
                {"text":"gpu","kind":"custom"}])
        );
        assert_eq!(label_chips(None, None), Value::Null);
    }

    fn row(name: &str, state: &str, group: &RunnerGroup) -> Value {
        json!({"name":name,"os":"LINUX","architecture":"X64","runnerState":state,"attention":false,
            "labels":["self-hosted"],
            "groupId":group.id,"groupLabel":group.label,"groupRank":group.rank,
            "groupMatch":group.matched,"groupSplit":group.split})
    }

    /// Groups follow the rows' stamped identity, ordered by OS, architecture
    /// and then the operator's list — `other` after every listed label even
    /// though "other" sorts before "runner-large" as text.
    #[test]
    fn grouping_follows_each_rows_stamped_group_in_list_order() {
        let large = split_group(
            "group:LINUX:X64:label:runner-large",
            "LINUX · X64 · runner-large",
            0,
            Some("runner-large"),
        );
        let other = split_group("group:LINUX:X64:other", "LINUX · X64 · other", 1, None);
        let rows = [
            row("ubu-1", "idle", &other),
            row("ubu-2", "busy", &large),
            row("ubu-3", "idle", &other),
        ];
        let groups = group_runners(&rows.iter().collect::<Vec<_>>());
        let summary: Vec<_> = groups
            .iter()
            .map(|g| {
                (
                    g["id"].as_str().unwrap_or(""),
                    g["value"].as_str().unwrap_or(""),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("group:LINUX:X64:label:runner-large", "1/1 busy"),
                ("group:LINUX:X64:other", "0/2 busy"),
            ]
        );
        assert_eq!(groups[0]["label"], "LINUX · X64 · runner-large");
    }

    /// A row built without a stamped group still lands in its plain
    /// OS · architecture group, as every row did before group labels.
    #[test]
    fn a_row_without_a_stamped_group_falls_into_its_plain_group() {
        let unstamped = json!({"name":"ubu-1","os":"LINUX","architecture":"X64","runnerState":"idle","attention":false});
        let groups = group_runners(&[&unstamped]);
        assert_eq!(groups[0]["id"], "group:LINUX:X64");
        assert_eq!(groups[0]["label"], "LINUX · X64");
    }

    /// One plain group built from `states`, as its colour and value. A
    /// missing runner raises attention, exactly as `runner_row` stamps it.
    fn load(states: &[&str]) -> (String, String) {
        let plain = group("group:LINUX:X64", "LINUX · X64", 0);
        let rows: Vec<Value> = states
            .iter()
            .enumerate()
            .map(|(i, state)| {
                let mut runner = row(&format!("ubu-{i}"), state, &plain);
                runner["attention"] = json!(*state == "missing");
                runner
            })
            .collect();
        let groups = group_runners(&rows.iter().collect::<Vec<_>>());
        (
            groups[0]["color"].as_str().unwrap_or("").to_owned(),
            groups[0]["value"].as_str().unwrap_or("").to_owned(),
        )
    }

    /// A group's colour is its load, busy ÷ online: gray while nothing is
    /// busy, green, amber from half, red once no online runner is free. An
    /// offline runner is not capacity, so it is in neither number.
    #[test]
    fn a_groups_colour_scales_with_busy_over_online() {
        let (gray, green, amber, red) = (
            color::hex(color::MUTED),
            color::hex(color::GREEN),
            color::hex(color::AMBER),
            color::hex(color::RED),
        );
        for (states, colour, value) in [
            (&["idle", "idle", "idle"][..], &gray, "0/3 busy"),
            (&["busy", "idle", "idle"][..], &green, "1/3 busy"),
            (&["busy", "busy", "idle"][..], &amber, "2/3 busy"),
            (&["busy", "idle"][..], &amber, "1/2 busy"),
            (&["busy", "busy", "busy"][..], &red, "3/3 busy"),
            (&["busy", "busy", "offline"][..], &red, "2/2 busy"),
        ] {
            assert_eq!(
                load(states),
                (colour.clone(), value.to_owned()),
                "{states:?}"
            );
        }
    }

    /// No runner online means no runner can take a job: for a queued job
    /// that is the same as every runner busy.
    #[test]
    fn a_group_with_no_runner_online_is_red() {
        assert_eq!(
            load(&["offline", "recycling"]),
            (color::hex(color::RED), "none online".to_owned())
        );
    }

    /// Missing is the alarm, and it wins over an otherwise idle group.
    #[test]
    fn a_missing_runner_still_turns_an_idle_group_red() {
        assert_eq!(
            load(&["idle", "missing"]),
            (color::hex(color::RED), "1 missing".to_owned())
        );
    }

    /// A group holding a missing runner leads with the alarm, not a load
    /// figure in an alarm colour; the load stays in the line under it.
    #[test]
    fn a_group_with_a_missing_runner_reads_its_missing_count() {
        let plain = group("group:LINUX:X64", "LINUX · X64", 0);
        let rows: Vec<Value> = ["busy", "idle", "missing"]
            .iter()
            .enumerate()
            .map(|(i, state)| {
                let mut runner = row(&format!("ubu-{i}"), state, &plain);
                runner["attention"] = json!(*state == "missing");
                runner
            })
            .collect();
        let groups = group_runners(&rows.iter().collect::<Vec<_>>());
        assert_eq!(groups[0]["value"], "1 missing");
        assert_eq!(groups[0]["color"], color::hex(color::RED));
        assert_eq!(
            groups[0]["detail"],
            "1 busy · 1 idle · 0 offline · 1 missing"
        );
    }

    #[test]
    fn a_label_groups_match_names_the_label() {
        let large = split_group(
            "group:LINUX:X64:label:runner-large",
            "LINUX · X64 · runner-large",
            0,
            Some("runner-large"),
        );
        let groups = group_runners(&[&row("ubu-1", "idle", &large)]);
        assert_eq!(groups[0]["match"], "matched by runner-large");
    }

    /// `other` cannot claim "carries none" of labels nobody has seen, so it
    /// counts the runners whose labels were never recorded.
    #[test]
    fn other_says_how_many_labels_were_not_recorded() {
        let other = split_group("group:LINUX:X64:other", "LINUX · X64 · other", 1, None);
        let unrecorded = |name: &str| {
            let mut runner = row(name, "idle", &other);
            runner["labels"] = Value::Null;
            runner
        };
        let mixed = [
            row("a", "idle", &other),
            row("b", "busy", &other),
            unrecorded("c"),
        ];
        let groups = group_runners(&mixed.iter().collect::<Vec<_>>());
        assert_eq!(
            groups[0]["match"],
            "carries none of your group labels · 1 not recorded"
        );
        assert_eq!(groups[0]["summary"]["notRecorded"], 1);

        let none = [unrecorded("a"), unrecorded("b")];
        let groups = group_runners(&none.iter().collect::<Vec<_>>());
        assert_eq!(groups[0]["match"], "labels not recorded");

        let all = [row("a", "idle", &other)];
        let groups = group_runners(&all.iter().collect::<Vec<_>>());
        assert_eq!(groups[0]["match"], "carries none of your group labels");
    }

    #[test]
    fn a_plain_group_has_no_match_line() {
        let plain = group("group:LINUX:X64", "LINUX · X64", 0);
        let groups = group_runners(&[&row("ubu-1", "idle", &plain)]);
        assert_eq!(groups[0]["match"], Value::Null);
    }

    /// Recycling is presence churn, not load: it no longer tints a group.
    #[test]
    fn a_recycling_runner_is_not_load() {
        assert_eq!(
            load(&["idle", "idle", "recycling"]),
            (color::hex(color::MUTED), "0/2 busy".to_owned())
        );
    }
}
