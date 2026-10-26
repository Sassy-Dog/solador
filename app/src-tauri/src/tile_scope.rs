//! Narrows one source's panel payload to what a tile's scope keeps, so a tile's
//! Full view shows the tile's rows and nothing else.
//!
//! The kept ids come from `dashboard::scoped_row_ids`, which derives them from
//! `dashboard::source_rows`; each filter here rebuilds the same id `source_rows`
//! gives that row. Only the rows are filtered. Every other key (`trailing`,
//! `stats`, `chips`, `footer`, `freshness`, `message`, `health`, section header
//! counts) is the source-wide total the scope note refers to and is left as is.

use crate::dashboard::scoped_row_ids;
use serde_json::Value;
use std::collections::BTreeSet;
use store::{DashboardLayout, DashboardTile};

pub(crate) const TILE_GONE: &str = "That tile is no longer on the overview.";
pub(crate) const TILE_HIDDEN: &str = "That tile is hidden, so it has no views.";
pub(crate) const TILE_WRONG_SOURCE: &str = "That tile shows a different source.";

/// The tile a Full view was asked for, or why it has none. Checked in order:
/// missing, hidden, then a different source.
pub(crate) fn find_tile<'a>(
    layout: &'a DashboardLayout,
    id: &str,
    source: &str,
) -> Result<&'a DashboardTile, &'static str> {
    let tile = layout.tiles.iter().find(|t| t.id == id).ok_or(TILE_GONE)?;
    if tile.hidden {
        return Err(TILE_HIDDEN);
    }
    if tile.source != source {
        return Err(TILE_WRONG_SOURCE);
    }
    Ok(tile)
}

/// `payload` with every item the tile's scope does not keep removed. A tile
/// that keeps everything gets the payload back unchanged; one that keeps
/// nothing gets an empty panel, never the whole list.
pub(crate) fn scope_payload(tile: &DashboardTile, mut payload: Value) -> Value {
    let Some(kept) = scoped_row_ids(tile, &payload) else {
        return payload;
    };
    match tile.source.as_str() {
        "ghWorkflows" => retain_rows(&mut payload, |v| str_of(v, "repo").to_owned(), &kept),
        "ghRunners" => retain_rows(
            &mut payload,
            |v| format!("{}/{}", str_of(v, "org"), str_of(v, "name")),
            &kept,
        ),
        "services" | "sentryCrons" => {
            retain_rows(&mut payload, |v| str_of(v, "id").to_owned(), &kept);
        }
        "containers" => scope_containers(&mut payload, &kept),
        "claudeUsage" => scope_claude(&mut payload, &kept, tile.scope == "plans"),
        "azureCost" => scope_azure(&mut payload, &kept),
        "openclawAgents" => scope_openclaw(&mut payload, &kept),
        // Hosts are filtered before layout (they are cards, not rows).
        _ => {}
    }
    payload
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_default()
}

/// Keeps the entries of `container[key]` whose id is in `kept`. An absent or
/// non-array key is left alone.
fn retain_in(
    container: &mut Value,
    key: &str,
    id: impl Fn(&Value) -> String,
    kept: &BTreeSet<String>,
) {
    if let Some(items) = container[key].as_array_mut() {
        items.retain(|v| kept.contains(&id(v)));
    }
}

fn retain_rows(payload: &mut Value, id: impl Fn(&Value) -> String, kept: &BTreeSet<String>) {
    retain_in(payload, "rows", id, kept);
}

fn has_rows(v: &Value) -> bool {
    v["rows"].as_array().is_some_and(|r| !r.is_empty())
}

fn scope_containers(payload: &mut Value, kept: &BTreeSet<String>) {
    let Some(sections) = payload["sections"].as_array_mut() else {
        return;
    };
    for section in sections.iter_mut() {
        let host = str_of(section, "host").to_owned();
        retain_in(
            section,
            "rows",
            |v| format!("{host}|{}", str_of(v, "name")),
            kept,
        );
    }
    sections.retain(has_rows);
}

/// Retains the `{group.id}:{label}` rows of each group under `key`, then drops
/// the groups left with none.
fn scope_groups(payload: &mut Value, key: &str, kept: &BTreeSet<String>) {
    let Some(groups) = payload[key].as_array_mut() else {
        return;
    };
    for group in groups.iter_mut() {
        let gid = str_of(group, "id").to_owned();
        retain_in(
            group,
            "rows",
            |v| format!("{gid}:{}", str_of(v, "label")),
            kept,
        );
    }
    groups.retain(has_rows);
}

fn scope_claude(payload: &mut Value, kept: &BTreeSet<String>, plans_scope: bool) {
    retain_in(
        payload,
        "windows",
        |v| format!("claude-{}", str_of(v, "label")),
        kept,
    );
    scope_groups(payload, "plans", kept);
    scope_groups(payload, "providers", kept);
    if let Some(map) = payload.as_object_mut() {
        if !kept.iter().any(|id| id.starts_with("claude-")) {
            map.remove("projects");
        }
        if !plans_scope {
            map.remove("plansMessage");
        }
    }
}

fn scope_azure(payload: &mut Value, kept: &BTreeSet<String>) {
    if !kept.contains("mtd") {
        payload["headline"] = Value::Null;
    }
    retain_in(
        payload,
        "stats",
        |v| format!("cost-{}", str_of(v, "label")),
        kept,
    );
    if !kept.contains("budget") {
        payload["budget"] = Value::Null;
    }
}

fn scope_openclaw(payload: &mut Value, kept: &BTreeSet<String>) {
    let Some(runtimes) = payload["runtimes"].as_array_mut() else {
        return;
    };
    for runtime in runtimes.iter_mut() {
        let rid = str_of(runtime, "id").to_owned();
        retain_in(
            &mut runtime["agents"],
            "rows",
            |v| format!("{rid}:agent:{}", str_of(v, "id")),
            kept,
        );
        retain_in(
            &mut runtime["channels"],
            "rows",
            |v| format!("{rid}:channel:{}", str_of(v, "id")),
            kept,
        );
        for key in ["connection", "hint", "pairing", "cron"] {
            if !kept.contains(&format!("{rid}-{key}")) {
                runtime[key] = Value::Null;
            }
        }
    }
    runtimes.retain(|r| {
        has_rows(&r["agents"])
            || has_rows(&r["channels"])
            || ["connection", "hint", "pairing", "cron"]
                .iter()
                .any(|k| !r[*k].is_null())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::default_layout;
    use serde_json::json;

    fn tile(source: &str, scope: &str) -> DashboardTile {
        let mut t = default_layout().tiles.remove(0);
        t.id = format!("t-{source}");
        t.source = source.into();
        t.scope = scope.into();
        t
    }

    fn rows(p: &Value, key: &str) -> Vec<String> {
        p["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v[key].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn find_tile_refuses_a_missing_hidden_or_foreign_tile() {
        let mut layout = default_layout();
        let mut hidden = tile("services", "all");
        hidden.id = "h".into();
        hidden.hidden = true;
        let mut shown = tile("services", "all");
        shown.id = "s".into();
        layout.tiles = vec![hidden, shown];
        assert_eq!(
            find_tile(&layout, "nope", "services").unwrap_err(),
            TILE_GONE
        );
        assert_eq!(
            find_tile(&layout, "h", "services").unwrap_err(),
            TILE_HIDDEN
        );
        assert_eq!(
            find_tile(&layout, "s", "ghRunners").unwrap_err(),
            TILE_WRONG_SOURCE
        );
        assert_eq!(find_tile(&layout, "s", "services").unwrap().id, "s");
        // Order: a hidden tile of another source is hidden, not foreign.
        assert_eq!(
            find_tile(&layout, "h", "ghRunners").unwrap_err(),
            TILE_HIDDEN
        );
    }

    #[test]
    fn an_unscoped_tile_gets_the_payload_back_unchanged() {
        let p = json!({"rows":[{"id":"a","label":"A"}],"footer":{"text":"x"}});
        assert_eq!(scope_payload(&tile("services", "all"), p.clone()), p);
    }

    #[test]
    fn repos_keep_only_the_scoped_repos() {
        let repo = |name: &str, status: &str| {
            json!({"repo":format!("o/{name}"),"name":name,"status":status,
                "statusLabel":status,"attention":status != "healthy","cells":[]})
        };
        let p = json!({"columns":[],"health":{"text":"2 of 3"},
            "rows":[repo("a","healthy"),repo("b","failing"),repo("c","healthy")]});
        let out = scope_payload(&tile("ghWorkflows", "healthy"), p.clone());
        assert_eq!(rows(&out, "repo"), ["o/a", "o/c"]);
        assert_eq!(out["health"], p["health"]);
    }

    #[test]
    fn runners_keep_rows_by_org_and_name() {
        let r = |org: &str, name: &str, os: &str| json!({"org":org,"name":name,"os":os,"status":"online","cells":[]});
        let p = json!({"stats":[{"label":"online","value":"3"}],"chips":[{"text":"c"}],
            "rows":[r("o","a","LINUX"),r("o","b","MACOS"),r("p","a","LINUX")]});
        let out = scope_payload(&tile("ghRunners", "LINUX"), p.clone());
        assert_eq!(rows(&out, "org"), ["o", "p"]);
        assert_eq!(rows(&out, "name"), ["a", "a"]);
        assert_eq!(out["stats"], p["stats"]);
        assert_eq!(out["chips"], p["chips"]);
    }

    #[test]
    fn services_and_crons_keep_rows_by_id() {
        let p = json!({"footer":{"text":"f"},"rows":[
            {"id":"m1","label":"one","age":"1d","suppressed":false},
            {"id":"m2","label":"two","age":"2d","suppressed":true}]});
        let out = scope_payload(&tile("sentryCrons", "active"), p.clone());
        assert_eq!(rows(&out, "id"), ["m1"]);
        assert_eq!(out["footer"], p["footer"]);
        let out = scope_payload(&tile("services", "item:m2"), p);
        assert_eq!(rows(&out, "id"), ["m2"]);
    }

    #[test]
    fn containers_drop_a_section_left_empty() {
        let p = json!({"sections":[
            {"host":"hostA","label":"A","count":2,"rows":[{"name":"db","status":"up"},{"name":"web","status":"up"}]},
            {"host":"hostB","label":"B","count":1,"rows":[{"name":"db","status":"up"}]}]});
        let out = scope_payload(&tile("containers", "item:hostA|db"), p);
        let sections = out["sections"].as_array().unwrap();
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0]["host"], "hostA");
        assert_eq!(sections[0]["rows"].as_array().unwrap().len(), 1);
        assert_eq!(sections[0]["rows"][0]["name"], "db");
        assert_eq!(sections[0]["count"], 2);
    }

    fn usage_payload() -> Value {
        json!({"trailing":"1M","projects":[{"name":"p"}],"plansMessage":"off",
            "windows":[{"label":"Today","value":"1"}],
            "plans":[{"id":"claude","rows":[{"label":"5H","value":"10%"}]}],
            "providers":[{"id":"neon","rows":[{"label":"Compute","value":"1"}]}]})
    }

    #[test]
    fn usage_plans_scope_drops_claude_and_providers() {
        let p = usage_payload();
        let out = scope_payload(&tile("claudeUsage", "plans"), p.clone());
        assert_eq!(out["windows"], json!([]));
        assert_eq!(out["providers"], json!([]));
        assert!(out.get("projects").is_none());
        assert_eq!(out["plans"], p["plans"]);
        assert_eq!(out["plansMessage"], "off");
    }

    #[test]
    fn usage_claude_scope_keeps_projects_and_drops_plans_message() {
        let out = scope_payload(&tile("claudeUsage", "claude"), usage_payload());
        assert_eq!(out["windows"].as_array().unwrap().len(), 1);
        assert_eq!(out["plans"], json!([]));
        assert_eq!(out["projects"], json!([{"name":"p"}]));
        assert!(out.get("plansMessage").is_none());
        let out = scope_payload(&tile("claudeUsage", "providers"), usage_payload());
        assert_eq!(out["providers"].as_array().unwrap().len(), 1);
        assert_eq!(out["plans"], json!([]));
    }

    fn azure_payload() -> Value {
        json!({"headline":{"value":"$10","caption":"c"},
            "stats":[{"label":"Today","value":"$1"}],
            "budget":{"label":"Budget","value":"50%","bar":{"color":"#fff"}},
            "breakdowns":[{"title":"b"}]})
    }

    #[test]
    fn azure_budget_scope_keeps_only_the_budget() {
        let p = azure_payload();
        let out = scope_payload(&tile("azureCost", "item:budget"), p.clone());
        assert_eq!(out["headline"], Value::Null);
        assert_eq!(out["stats"], json!([]));
        assert_eq!(out["budget"], p["budget"]);
        assert_eq!(out["breakdowns"], p["breakdowns"]);
    }

    #[test]
    fn azure_headline_and_stat_scopes_drop_the_budget() {
        let p = azure_payload();
        let out = scope_payload(&tile("azureCost", "item:mtd"), p.clone());
        assert_eq!(out["headline"], p["headline"]);
        assert_eq!(out["budget"], Value::Null);
        let out = scope_payload(&tile("azureCost", "item:cost-Today"), p.clone());
        assert_eq!(out["stats"], p["stats"]);
        assert_eq!(out["headline"], Value::Null);
    }

    fn openclaw_payload() -> Value {
        let rt = |id: &str| {
            json!({"id":id,
                "agents":{"rows":[{"id":"a1","name":"A1","status":"ok"},{"id":"a2","name":"A2","status":"ok"}]},
                "channels":{"rows":[{"id":"c1","name":"C1","status":"ok"}]},
                "connection":{"text":"up"},"hint":{"text":"h"},"cron":{"title":"c"}})
        };
        json!({"runtimes":[rt("r1"),rt("r2")]})
    }

    #[test]
    fn openclaw_keeps_one_agent_and_drops_other_runtimes() {
        let out = scope_payload(
            &tile("openclawAgents", "item:r1:agent:a1"),
            openclaw_payload(),
        );
        let runtimes = out["runtimes"].as_array().unwrap();
        assert_eq!(runtimes.len(), 1);
        let r = &runtimes[0];
        assert_eq!(r["id"], "r1");
        assert_eq!(r["agents"]["rows"].as_array().unwrap().len(), 1);
        assert_eq!(r["agents"]["rows"][0]["id"], "a1");
        assert_eq!(r["channels"]["rows"], json!([]));
        assert_eq!(r["connection"], Value::Null);
        assert_eq!(r["hint"], Value::Null);
        assert_eq!(r["cron"], Value::Null);
    }

    #[test]
    fn openclaw_keeps_a_channel_and_a_status_object() {
        let out = scope_payload(
            &tile("openclawAgents", "item:r2:channel:c1"),
            openclaw_payload(),
        );
        assert_eq!(out["runtimes"].as_array().unwrap().len(), 1);
        assert_eq!(out["runtimes"][0]["channels"]["rows"][0]["id"], "c1");
        let out = scope_payload(
            &tile("openclawAgents", "item:r2-connection"),
            openclaw_payload(),
        );
        let r = &out["runtimes"][0];
        assert_eq!(r["connection"], json!({"text":"up"}));
        assert_eq!(r["agents"]["rows"], json!([]));
        assert_eq!(r["cron"], Value::Null);
    }

    #[test]
    fn a_scope_that_keeps_nothing_empties_the_panel() {
        let p = json!({"rows":[{"id":"a","label":"A"}]});
        let out = scope_payload(&tile("services", "item:gone"), p);
        assert_eq!(out["rows"], json!([]));
    }

    #[test]
    fn hosts_come_back_unchanged() {
        let p = json!({"hosts":[{"hostName":"a"}]});
        assert_eq!(scope_payload(&tile("hosts", "item:gone"), p.clone()), p);
    }
}
