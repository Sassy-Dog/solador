//! Dashboard ordering and runner aggregation over the cached source rows.

use crate::color;
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::collections::BTreeMap;

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
/// answer (`groupId`, `groupLabel`, `groupRank`) and every view — a grouped
/// tile, its inline Detail, the Full panel — reads those fields rather than
/// re-deriving them, so a group always opens onto the runners it counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerGroup {
    pub id: String,
    pub label: String,
    /// Order within one OS · architecture: the operator's list order, then
    /// `other`, then `labels unknown`.
    pub rank: usize,
}

/// Which of the operator's group labels claims a runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LabelSplit {
    /// The index into the operator's list.
    Label(usize),
    /// Labels known, none of them listed.
    Other,
    /// No labels to read (a roster entry from before they were remembered).
    Unknown,
}

/// The runner's group under the operator's group labels (`store.json`'s
/// `runner_group_labels`). An empty list is exactly the OS · architecture
/// grouping that predates it, identity included, so a stored Detail selection
/// still opens.
#[must_use]
pub fn runner_group(
    os: &str,
    architecture: Option<&str>,
    labels: Option<&[String]>,
    group_labels: &[String],
) -> RunnerGroup {
    let architecture = architecture.unwrap_or("Unknown architecture");
    let id = format!("group:{os}:{architecture}");
    let label = format!("{os} · {architecture}");
    if group_labels.is_empty() {
        return RunnerGroup { id, label, rank: 0 };
    }
    match label_split(labels, group_labels) {
        LabelSplit::Label(index) => RunnerGroup {
            id: format!("{id}:label:{}", group_labels[index].to_ascii_lowercase()),
            label: format!("{label} · {}", group_labels[index]),
            rank: index,
        },
        LabelSplit::Other => RunnerGroup {
            id: format!("{id}:other"),
            label: format!("{label} · other"),
            rank: group_labels.len(),
        },
        LabelSplit::Unknown => RunnerGroup {
            id: format!("{id}:unknown"),
            label: format!("{label} · labels unknown"),
            rank: group_labels.len() + 1,
        },
    }
}

/// Which group label claims a runner carrying `labels`.
///
/// `group_labels` is the operator's list, in their priority order, and is
/// never empty here. The list's order decides, not the order GitHub reports
/// the runner's own labels in. Matching is ASCII case-insensitive, the way
/// GitHub matches a `runs-on` label to a runner's.
fn label_split(labels: Option<&[String]>, group_labels: &[String]) -> LabelSplit {
    let Some(labels) = labels else {
        return LabelSplit::Unknown;
    };
    group_labels
        .iter()
        .position(|wanted| carries_label(labels, wanted))
        .map_or(LabelSplit::Other, LabelSplit::Label)
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
        let plain = runner_group(os, architecture, None, &[]);
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
        let value = if online == 0 { "none online".to_owned() } else { format!("{busy}/{online} busy") };
        let mut detail = format!("{busy} busy · {idle} idle · {offline} offline");
        if recycling > 0 { detail.push_str(&format!(" · {recycling} recycling")); }
        if missing > 0 { detail.push_str(&format!(" · {missing} missing")); }
        if unknown > 0 { detail.push_str(&format!(" · {unknown} unknown")); }
        json!({"id":id,"label":label,
            "value":value,"color":tint,"valueColor":tint,
            "attention":attention,"attentionCount":attention_count,"detail":detail,"compactDetail":detail,"grouped":true,
            "summary":{"total":total,"busy":busy,"idle":idle,"offline":offline,"missing":missing,"recycling":recycling,"unknown":unknown}})
    }).collect()
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
        }
    }

    /// With no group labels the identity is byte-for-byte the one that
    /// predates them, so a Detail selection saved before the upgrade opens.
    #[test]
    fn no_group_labels_keep_the_os_and_architecture_identity() {
        let large = strings(&["self-hosted", "runner-large"]);
        assert_eq!(
            runner_group("LINUX", Some("X64"), Some(&large), &[]),
            group("group:LINUX:X64", "LINUX · X64", 0)
        );
        assert_eq!(
            runner_group("LINUX", None, None, &[]),
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
                &strings(&["Runner-Large"])
            ),
            group(
                "group:LINUX:X64:label:runner-large",
                "LINUX · X64 · Runner-Large",
                0
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
                &strings(&["gpu", "runner-large"])
            ),
            group("group:LINUX:X64:label:gpu", "LINUX · X64 · gpu", 0)
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
                &strings(&["runner-large", "gpu"])
            ),
            group("group:LINUX:X64:other", "LINUX · X64 · other", 2)
        );
    }

    /// No labels to read is not "carries none of them": that runner is
    /// `labels unknown`, never counted as `other`.
    #[test]
    fn a_runner_with_unknown_labels_is_never_other() {
        assert_eq!(
            runner_group("LINUX", Some("X64"), None, &strings(&["runner-large"])),
            group("group:LINUX:X64:unknown", "LINUX · X64 · labels unknown", 2)
        );
    }

    fn row(name: &str, state: &str, group: &RunnerGroup) -> Value {
        json!({"name":name,"os":"LINUX","architecture":"X64","runnerState":state,"attention":false,
            "groupId":group.id,"groupLabel":group.label,"groupRank":group.rank})
    }

    /// Groups follow the rows' stamped identity, ordered by OS, architecture
    /// and then the operator's list — `other` after every listed label even
    /// though "other" sorts before "runner-large" as text.
    #[test]
    fn grouping_follows_each_rows_stamped_group_in_list_order() {
        let large = group(
            "group:LINUX:X64:label:runner-large",
            "LINUX · X64 · runner-large",
            0,
        );
        let other = group("group:LINUX:X64:other", "LINUX · X64 · other", 1);
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
            (color::hex(color::RED), "0/1 busy".to_owned())
        );
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
