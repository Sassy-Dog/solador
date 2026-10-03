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

pub fn runner_group_id(row: &Value) -> String {
    let os = row["os"].as_str().unwrap_or("Unknown OS");
    let architecture = row["architecture"]
        .as_str()
        .unwrap_or("Unknown architecture");
    format!("group:{os}:{architecture}")
}

/// Each absent runner remains in its remembered type, but cannot contribute
/// to busy/idle/online. Group only the rows selected by this tile's scope.
pub fn group_runners(rows: &[&Value]) -> Vec<Value> {
    let mut groups: BTreeMap<(String, String), Vec<&Value>> = BTreeMap::new();
    for row in rows {
        let os = row["os"].as_str().unwrap_or("Unknown OS");
        let architecture = row["architecture"]
            .as_str()
            .unwrap_or("Unknown architecture");
        groups
            .entry((os.into(), architecture.into()))
            .or_default()
            .push(row);
    }
    groups.into_iter().map(|((os, architecture), members)| {
        let count = |state| members.iter().filter(|r| r["runnerState"] == state).count();
        let (busy, idle, offline, missing, recycling) = (count("busy"), count("idle"), count("offline"), count("missing"), count("recycling"));
        let total = members.len();
        let unknown = total - busy - idle - offline - missing - recycling;
        let attention_count = members.iter().filter(|r| r["attention"] == true).count();
        let attention = attention_count > 0;
        let tint = color::hex(if attention { color::RED } else if busy > 0 || recycling > 0 { color::AMBER } else if idle > 0 { color::GREEN } else { color::MUTED });
        let mut detail = format!("{busy} busy · {idle} idle · {offline} offline");
        if recycling > 0 { detail.push_str(&format!(" · {recycling} recycling")); }
        if missing > 0 { detail.push_str(&format!(" · {missing} missing")); }
        if unknown > 0 { detail.push_str(&format!(" · {unknown} unknown")); }
        json!({"id":runner_group_id(members[0]),"label":format!("{os} · {architecture}"),
            "value":format!("{} online / {total}",busy+idle),"color":tint,"valueColor":tint,
            "attention":attention,"attentionCount":attention_count,"detail":detail,"compactDetail":detail,"grouped":true,
            "summary":{"total":total,"busy":busy,"idle":idle,"offline":offline,"missing":missing,"recycling":recycling,"unknown":unknown}})
    }).collect()
}
