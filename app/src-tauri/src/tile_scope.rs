//! Narrows one source's panel payload to what a tile's scope keeps, so a tile's
//! Full view shows the tile's rows and nothing else.
//!
//! The kept ids come from `dashboard::scoped_row_ids`, which derives them from
//! `dashboard::source_rows`; each filter here rebuilds the same id `source_rows`
//! gives that row. Only the rows are filtered. Every other key (`trailing`,
//! `stats`, `chips`, `footer`, `freshness`, `message`, `health`, section header
//! counts) is the source-wide total the scope note refers to and is left as is.
//! The exceptions: AI Usage's `message`, `trailing` and `claudeToday`, which
//! speak for the rollups, and its `footer`, `freshness`, `loading` and setup
//! `message`, which are rebuilt for the tile's scope from the payload's per-row
//! `lines` and per-vendor `vendors` (`ai_header`), so a vendor-scoped tile never
//! carries another vendor's warning, clock or wait; and the
//! Repos owner groups (`owners`, `cardOrder`), rebuilt from the kept rows so
//! each owner's header counts exactly the repos under it.
//!
//! **Nothing here writes through `IndexMut`.** `value["key"]` on a `&mut Value`
//! inserts a missing key and turns a `null` into an object, so a runtime whose
//! `agents` was `null` came back as `{"rows": null}` and the OpenClaw panel
//! painted empty AGENTS and CHANNELS sections under its pairing banner. Every
//! write goes through [`field_mut`], and a key that is absent or `null` stays
//! exactly as it was.

use crate::dashboard::{scoped_empty, scoped_row_ids, CLAUDE_TODAY_ID};
use serde_json::Value;
use std::collections::BTreeSet;
use store::{DashboardLayout, DashboardTile};

pub(crate) const TILE_GONE: &str = "That tile is no longer on the overview.";
pub(crate) const TILE_HIDDEN: &str = "That tile is hidden, so it has no views.";
pub(crate) const TILE_WRONG_SOURCE: &str = "That tile shows something else.";

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
    // Before the early return below: a tile that keeps every repo still lays
    // its owners out its own way. Replaced, never added: `repos_view` always
    // carries the key, and nothing here invents one (see the module note).
    if tile.source == "ghWorkflows" {
        if let Some(layout) = field_mut(&mut payload, "layout") {
            *layout = Value::String(tile.repo_layout.clone());
        }
    }
    let Some(kept) = scoped_row_ids(tile, &payload) else {
        return payload;
    };
    // Hosts are cards, filtered (and stamped) by `cockpit_for_tile`.
    let empty = scoped_empty(tile, &payload).filter(|_| tile.source != "hosts");
    match tile.source.as_str() {
        "ghWorkflows" => {
            retain_rows(&mut payload, |v| str_of(v, "repo").to_owned(), &kept);
            if payload.get("owners").is_some() {
                crate::github::attach_owners(&mut payload);
            }
        }
        "ghRunners" => retain_rows(
            &mut payload,
            |v| format!("{}/{}", str_of(v, "org"), str_of(v, "name")),
            &kept,
        ),
        "services" | "sentryCrons" => {
            retain_rows(&mut payload, |v| str_of(v, "id").to_owned(), &kept);
        }
        "containers" => scope_containers(&mut payload, &kept),
        "aiUsage" => scope_ai_usage(&mut payload, &kept, &tile.scope),
        "usage" => scope_groups(&mut payload, "providers", &kept),
        "azureCost" => scope_azure(&mut payload, &kept),
        "actionsBilling" => scope_actions_billing(&mut payload, &kept),
        "openclawAgents" => scope_openclaw(&mut payload, &kept),
        // Hosts are filtered before layout (they are cards, not rows).
        _ => {}
    }
    stamp_empty(&mut payload, empty);
    payload
}

/// Adds the tile's own sentence for a scope that keeps nothing, as `tileEmpty`.
/// Only a payload that is an object takes it, and an absent sentence adds no
/// key (see the module note on writes).
pub(crate) fn stamp_empty(payload: &mut Value, empty: Option<String>) {
    if let (Some(map), Some(sentence)) = (payload.as_object_mut(), empty) {
        map.insert("tileEmpty".into(), Value::String(sentence));
    }
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_default()
}

/// `container[key]` when `container` is an object holding `key`, never
/// inserting either (see the module note).
fn field_mut<'a>(container: &'a mut Value, key: &str) -> Option<&'a mut Value> {
    container.as_object_mut()?.get_mut(key)
}

/// The array at `container[key]`, if there is one.
fn array_mut<'a>(container: &'a mut Value, key: &str) -> Option<&'a mut Vec<Value>> {
    field_mut(container, key)?.as_array_mut()
}

/// Sets `container[key]` to `null` when the key is there. An absent key stays
/// absent.
fn clear(container: &mut Value, key: &str) {
    if let Some(v) = field_mut(container, key) {
        *v = Value::Null;
    }
}

/// Keeps the entries of `container[key]` whose id is in `kept`. An absent,
/// `null` or non-array key, or a container that is not an object, is left
/// alone.
fn retain_in(
    container: &mut Value,
    key: &str,
    id: impl Fn(&Value) -> String,
    kept: &BTreeSet<String>,
) {
    if let Some(items) = array_mut(container, key) {
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
    let Some(sections) = array_mut(payload, "sections") else {
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
    let Some(groups) = array_mut(payload, key) else {
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

/// A plan is kept by its section id, the id `source_rows` gives its row.
fn plan_id(plan: &Value) -> String {
    str_of(plan, "id").to_owned()
}

/// AI Usage keeps each plan and each API spend block by its row id, and the
/// rollups by `claude-today`. The vendor scopes (`anthropic`, `openai`) and the
/// block scopes (`plans`, `claude`) were already turned into ids by
/// `source_rows`, so this never reads a scope.
fn scope_ai_usage(payload: &mut Value, kept: &BTreeSet<String>, scope: &str) {
    retain_in(payload, "plans", plan_id, kept);
    retain_in(payload, "apis", plan_id, kept);
    // The header lines, loading and setup sentence are the scope's rows' alone.
    if let Some(header) = ai_header(payload, scope) {
        if let Some(map) = payload.as_object_mut() {
            map.insert("footer".into(), header.footer);
            map.insert("freshness".into(), header.freshness);
            map.insert("loading".into(), Value::Bool(header.loading));
            map.insert("message".into(), header.message);
        }
    }
    // The rollups, their projects and their heading are the `claude-today`
    // row's, kept or dropped together. Matched exactly: `claude-plan` also
    // starts with `claude-`, and a plans-only tile once kept the projects.
    if kept.contains(CLAUDE_TODAY_ID) {
        return;
    }
    if let Some(windows) = array_mut(payload, "windows") {
        windows.clear();
    }
    if let Some(map) = payload.as_object_mut() {
        map.remove("projects");
        map.remove("machineHeading");
    }
    // The header's Claude Code figure is the row's too: a tile that does not
    // keep the row must not still name this machine's Claude usage.
    clear_text(payload, "trailing");
    clear_text(payload, "claudeToday");
    // `message` speaks for the rollups, which this tile left out. With nothing
    // else on the panel it stays, or a Full view that keeps no row is blank.
    let shows_a_block = ["plans", "apis"]
        .into_iter()
        .any(|key| array_mut(payload, key).is_some_and(|rows| !rows.is_empty()));
    if shows_a_block {
        clear(payload, "message");
    }
}

/// An AI Usage tile's header for its scope: the merged warning and clock lines
/// of the scope's rows only, whether any of them is still loading, and the
/// message. Built from the payload's per-row `lines`, each vendor's key state
/// (`vendors`) and `rollupsMessage`, so wording, order and colour are the
/// merge helpers' own and a vendor's segment never reaches another vendor's
/// tile. `None` when the payload carries none of them (a setup or error state).
pub(crate) struct AiHeader {
    pub footer: Value,
    pub freshness: Value,
    pub loading: bool,
    pub message: Value,
}

pub(crate) fn ai_header(inputs: &Value, scope: &str) -> Option<AiHeader> {
    let lines = inputs["lines"].as_array()?;
    let vendors = inputs["vendors"].as_array()?;
    let rows = crate::dashboard::ai_scope_rows(scope);
    let mine: Vec<&Value> = lines
        .iter()
        .filter(|l| rows.contains(&str_of(l, "id")))
        .collect();
    let segments = |key: &str| -> Vec<Value> { mine.iter().map(|l| l[key].clone()).collect() };
    // The vendors the scope's rows belong to, in body order.
    let scoped_vendors: Vec<&Value> = vendors
        .iter()
        .filter(|v| {
            rows.iter()
                .any(|r| crate::dashboard::vendor_of(r) == v["id"].as_str())
        })
        .collect();
    let on = scoped_vendors.iter().any(|v| v["on"] == true);
    let message = if scoped_vendors.is_empty() {
        Value::Null
    } else if !on {
        // No part of the tile's vendors is on: the setup sentence, once every
        // one of *their* keys has been read, and the reason for one that
        // could not be.
        let reads: Vec<crate::usage::KeyRead> = scoped_vendors
            .iter()
            .map(|v| crate::usage::KeyRead {
                undecided: v["undecided"] == true,
                error: v["error"].as_str().map(str::to_owned),
            })
            .collect();
        crate::usage::nothing_added_message(&reads)
    } else if rows.contains(&CLAUDE_TODAY_ID) {
        inputs["rollupsMessage"].clone()
    } else {
        Value::Null
    };
    Some(AiHeader {
        footer: crate::panel::merged_footer(&segments("warning")),
        freshness: crate::panel::merged_freshness(&segments("clock")),
        loading: mine.iter().any(|l| l["loading"] == true),
        message,
    })
}

/// Sets `container[key]` to `""` when the key is a string. Anything else, and
/// an absent key, stays as it was (see the module note).
fn clear_text(container: &mut Value, key: &str) {
    if let Some(v) = field_mut(container, key).filter(|v| v.is_string()) {
        *v = Value::String(String::new());
    }
}

fn scope_azure(payload: &mut Value, kept: &BTreeSet<String>) {
    if !kept.contains("mtd") {
        clear(payload, "headline");
    }
    retain_in(
        payload,
        "stats",
        |v| format!("cost-{}", str_of(v, "label")),
        kept,
    );
    if !kept.contains("budget") {
        clear(payload, "budget");
    }
}

/// The rows `source_rows` builds for Actions billing are `mtd`, one `stat-…` per
/// stat, `budget` (the bar, or the note that there is no budget) and one per
/// tripped or unjudged Billing alerts rule. The
/// breakdowns, the report's lag and the message stay: they describe the whole
/// panel and have no row of their own.
fn scope_actions_billing(payload: &mut Value, kept: &BTreeSet<String>) {
    if !kept.contains("mtd") {
        clear(payload, "headline");
    }
    retain_in(
        payload,
        "stats",
        |v| format!("stat-{}", str_of(v, "label")),
        kept,
    );
    // The rule rows (#770) are each a row of their own, by their payload id.
    retain_in(payload, "alerts", |v| str_of(v, "id").to_owned(), kept);
    if !kept.contains("budget") {
        clear(payload, "budget");
        clear(payload, "budgetNote");
    }
    // The trend (#771) is not a row anyone can pick, so a tile that picked
    // rows does not show it. Only a tile that keeps everything does.
    clear(payload, "trend");
    clear(payload, "trendNote");
}

fn scope_openclaw(payload: &mut Value, kept: &BTreeSet<String>) {
    let Some(runtimes) = array_mut(payload, "runtimes") else {
        return;
    };
    for runtime in runtimes.iter_mut() {
        let rid = str_of(runtime, "id").to_owned();
        // A runtime that is pairing or erroring has `agents: null` and
        // `channels: null`, and `openclaw.js` reads a non-null one as a section
        // to paint: they must stay `null`.
        for (key, kind) in [("agents", "agent"), ("channels", "channel")] {
            if let Some(section) = field_mut(runtime, key) {
                retain_in(
                    section,
                    "rows",
                    |v| format!("{rid}:{kind}:{}", str_of(v, "id")),
                    kept,
                );
            }
        }
        for key in ["connection", "hint", "pairing", "cron"] {
            if !kept.contains(&format!("{rid}-{key}")) {
                clear(runtime, key);
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

    /// `store` carries saved Usage scopes onto the rows that replaced them by
    /// literal id, because it cannot see this crate. This keeps the two in
    /// step: renaming a row here fails this test instead of leaving the
    /// migration to write an id no row has, which empties a scoped tile.
    #[test]
    fn the_store_migration_lands_saved_scopes_on_the_rows_this_crate_emits() {
        let mut data = store::StoreData {
            dashboard: Some(DashboardLayout {
                tiles: vec![
                    tile("claudeUsage", "item:claude-plan:CLAUDE PLAN (5H)"),
                    tile("claudeUsage", "item:codex-plan:CODEX PLAN (5H)"),
                    tile("claudeUsage", "item:claude-WEEK"),
                ],
                ..default_layout()
            }),
            ..store::StoreData::default()
        };
        data.carry_usage_item_scopes_onto_plan_rows();
        let scopes: Vec<String> = data
            .dashboard
            .expect("layout")
            .tiles
            .into_iter()
            .map(|t| t.scope)
            .collect();
        assert_eq!(
            scopes,
            [
                format!("item:{}", crate::plan_usage::CLAUDE_ID),
                format!("item:{}", crate::plan_usage::CODEX_ID),
                format!("item:{CLAUDE_TODAY_ID}"),
            ]
        );
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

    fn repo(slug: &str, status: &str, issues: u64) -> Value {
        json!({"repo":slug,"name":slug.split('/').nth(1),"status":status,
            "statusLabel":status,"attention":status != "healthy","cells":[],
            "dotColor":if status == "healthy" { "#0f0" } else { "#f00" },
            "sortValues":{"status":if status == "healthy" { 4 } else { 1 },
                "issues":issues,"prs":0,"dependabot":0,"running":0}})
    }

    /// The rows are scoped, and so are the owner groups built over them: a
    /// header counts the repos under it, never the owner's whole portfolio.
    /// Everything else stays source-wide.
    #[test]
    fn repos_keep_only_the_scoped_repos() {
        let mut p = json!({"columns":[],"health":{"text":"2 of 3"},"trailing":"1 stuck",
            "rows":[repo("acme/a","healthy",2),repo("acme/b","failing",3),
                repo("globex/c","failing",1),repo("acme/d","healthy",0)]});
        crate::github::attach_owners(&mut p);
        assert_eq!(p["owners"].as_array().unwrap().len(), 2);
        let out = scope_payload(&tile("ghWorkflows", "healthy"), p.clone());
        assert_eq!(rows(&out, "repo"), ["acme/a", "acme/d"]);
        assert_eq!(out["health"], p["health"]);
        assert_eq!(out["trailing"], p["trailing"]);
        let owners = out["owners"].as_array().unwrap();
        assert_eq!(owners.len(), 1, "globex kept no row, so it has no header");
        assert_eq!(owners[0]["owner"], "acme");
        assert_eq!(owners[0]["totalsText"], "2 issues · 0 PRs · 0 Dependabot");
        assert_eq!(
            owners[0]["dotColor"], "#0f0",
            "the failed repo was scoped out"
        );
        assert_eq!(out["cardOrder"], json!([0]));
    }

    /// A scoped Full view keeps the source's owner label even when the scope
    /// drops the repo that spelled it first, so the tile and its Full view
    /// never name one owner two ways.
    #[test]
    fn a_scoped_repos_payload_keeps_the_sources_owner_label() {
        let mut p =
            json!({"rows":[repo("Acme/anvil","failing",1),repo("acme/zephyr","healthy",9)]});
        crate::github::attach_owners(&mut p);
        let out = scope_payload(&tile("ghWorkflows", "healthy"), p);
        assert_eq!(rows(&out, "repo"), ["acme/zephyr"]);
        assert_eq!(out["owners"][0]["owner"], "Acme");
        assert_eq!(out["rows"][0]["owner"], "Acme");
    }

    /// A tile's Full view lays its owners out the way the tile does — on the
    /// unscoped path too, where every row is kept.
    #[test]
    fn a_scoped_repos_payload_carries_the_tiles_layout() {
        let mut p = json!({"layout":"sections","rows":[repo("acme/a","healthy",1)]});
        crate::github::attach_owners(&mut p);
        let mut sections = tile("ghWorkflows", "all");
        sections.repo_layout = "sections".into();
        assert_eq!(scope_payload(&sections, p.clone())["layout"], "sections");
        assert_eq!(
            scope_payload(&tile("ghWorkflows", "all"), p.clone())["layout"],
            "cards"
        );
        assert_eq!(
            scope_payload(&tile("ghWorkflows", "healthy"), p.clone())["layout"],
            "cards"
        );
        let payload_without_rows = json!({"layout":"sections","message":{"text":"loading…"}});
        assert_eq!(
            scope_payload(&tile("ghWorkflows", "all"), payload_without_rows)["layout"],
            "cards"
        );
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
    fn a_scope_that_keeps_nothing_carries_the_tiles_sentence() {
        let p = json!({"rows":[{"id":"m1","label":"one","age":"1d","suppressed":false}]});
        let out = scope_payload(&tile("services", "item:gone"), p.clone());
        assert_eq!(out["rows"], json!([]));
        assert!(out["tileEmpty"]
            .as_str()
            .unwrap()
            .contains("no current reading"));
        // A scope that keeps a row, and one that filters nothing, add no key.
        assert!(scope_payload(&tile("services", "item:m1"), p.clone())
            .get("tileEmpty")
            .is_none());
        assert!(scope_payload(&tile("services", "all"), p)
            .get("tileEmpty")
            .is_none());
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

    fn ai_usage_payload() -> Value {
        json!({"trailing":"Claude Code · 1M today","claudeToday":"1M today",
            "projects":[{"name":"p"}],
            "machineHeading":"THIS MACHINE",
            "apis":[
                {"id":"anthropic-api","label":"Anthropic API spend (MTD)","value":"$1.00"},
                {"id":"openai-api","label":"OpenAI API spend (MTD)","value":"$2.00"}],
            "windows":[{"label":"LAST 5H","value":"1"}],
            "plans":[
                {"id":"claude-plan","label":"Claude · Ultra","value":"resets in 3d",
                    "metrics":[{"label":"Week","value":"10%","fraction":0.1}]},
                {"id":"codex-plan","label":"Codex · Plus","value":"ok","metrics":[]}]})
    }

    /// The ids of a payload's plans and spend blocks, in order.
    fn blocks(v: &Value) -> Vec<String> {
        ["plans", "apis"]
            .iter()
            .flat_map(|key| v[*key].as_array().into_iter().flatten())
            .map(|b| b["id"].as_str().unwrap().to_owned())
            .collect()
    }

    /// The plan's real id, `claude-plan`, starts with `claude-`. Matching the
    /// rollups by that prefix once kept a plans-only tile's projects.
    #[test]
    fn plans_scope_keeps_both_plans_and_drops_the_rollups() {
        let p = ai_usage_payload();
        let out = scope_payload(&tile("aiUsage", "plans"), p.clone());
        assert_eq!(out["windows"], json!([]));
        assert!(out.get("projects").is_none());
        assert!(out.get("machineHeading").is_none());
        assert_eq!(out["plans"], p["plans"]);
        assert_eq!(out["apis"], json!([]));
        let one = scope_payload(&tile("aiUsage", "item:claude-plan"), p.clone());
        assert_eq!(blocks(&one), ["claude-plan"]);
        assert!(one.get("projects").is_none());
    }

    #[test]
    fn claude_scope_keeps_projects_and_drops_the_rest() {
        let out = scope_payload(&tile("aiUsage", "claude"), ai_usage_payload());
        assert_eq!(out["windows"].as_array().unwrap().len(), 1);
        assert_eq!(out["plans"], json!([]));
        assert_eq!(out["apis"], json!([]));
        assert_eq!(out["projects"], json!([{"name":"p"}]));
        assert_eq!(out["machineHeading"], "THIS MACHINE");
        // The row is kept, so the header still names Claude Code.
        assert_eq!(out["trailing"], "Claude Code · 1M today");
        assert_eq!(out["claudeToday"], "1M today");
    }

    #[test]
    fn one_api_row_keeps_its_block_and_nothing_else() {
        let p = ai_usage_payload();
        let out = scope_payload(&tile("aiUsage", "item:anthropic-api"), p.clone());
        assert_eq!(out["apis"], json!([p["apis"][0].clone()]));
        assert_eq!(out["plans"], json!([]));
        assert_eq!(out["windows"], json!([]));
        let out = scope_payload(&tile("aiUsage", "item:openai-api"), p.clone());
        assert_eq!(out["apis"], json!([p["apis"][1].clone()]));
    }

    /// A vendor-scoped tile keeps only that vendor's blocks: its plan, its
    /// spend, and for Anthropic the Claude Code rollups.
    #[test]
    fn a_vendor_scoped_tile_keeps_only_that_vendors_blocks() {
        let p = ai_usage_payload();
        let anthropic = scope_payload(&tile("aiUsage", "anthropic"), p.clone());
        assert_eq!(blocks(&anthropic), ["claude-plan", "anthropic-api"]);
        assert_eq!(anthropic["windows"].as_array().unwrap().len(), 1);
        assert_eq!(anthropic["projects"], json!([{"name":"p"}]));
        assert_eq!(anthropic["trailing"], "Claude Code · 1M today");

        let openai = scope_payload(&tile("aiUsage", "openai"), p.clone());
        assert_eq!(blocks(&openai), ["codex-plan", "openai-api"]);
        assert_eq!(openai["windows"], json!([]));
        assert!(openai.get("projects").is_none());
        assert!(openai.get("machineHeading").is_none());
    }

    /// A tile that does not keep `claude-today` must not still name this
    /// machine's Claude usage in its header.
    #[test]
    fn a_tile_without_claude_today_drops_the_claude_header_figure() {
        for scope in [
            "plans",
            "openai",
            "item:claude-plan",
            "item:anthropic-api",
            "item:codex-plan",
        ] {
            let out = scope_payload(&tile("aiUsage", scope), ai_usage_payload());
            assert_eq!(out["trailing"], "", "{scope}");
            assert_eq!(out["claudeToday"], "", "{scope}");
        }
        for scope in ["claude", "anthropic", "item:claude-today"] {
            let out = scope_payload(&tile("aiUsage", scope), ai_usage_payload());
            assert_eq!(out["trailing"], "Claude Code · 1M today", "{scope}");
        }
    }

    /// `message` speaks for the rollups, so a tile showing another block with
    /// the rollups scoped out drops it. A tile left with nothing keeps it, or
    /// its Full view would be blank.
    #[test]
    fn an_ai_usage_tile_keeps_the_rollups_line_only_when_it_has_nothing_else() {
        let p = json!({"message":{"text":"no usage data"},"windows":[],"plans":[],"apis":[]});
        for scope in ["claude", "item:claude-today"] {
            let out = scope_payload(&tile("aiUsage", scope), p.clone());
            assert_eq!(out["message"]["text"], "no usage data", "{scope}");
        }
        let with_plan = json!({"message":{"text":"no usage data"},"windows":[],
            "plans":[{"id":"claude-plan","label":"Claude"}],"apis":[]});
        let plans = scope_payload(&tile("aiUsage", "plans"), with_plan);
        assert!(plans["message"].is_null());
        // A spend block is a block too.
        let with_spend = json!({"message":{"text":"no usage data"},"windows":[],
            "plans":[],"apis":[{"id":"openai-api","label":"OpenAI"}]});
        let spend = scope_payload(&tile("aiUsage", "openai"), with_spend);
        assert!(spend["message"].is_null());
        // Clearing the line invents no key on a payload that has none.
        let bare = scope_payload(&tile("aiUsage", "plans"), json!({"plans": null}));
        assert!(bare.get("message").is_none());
    }

    /// One plan row picks its plan out of the payload, and leaves the source's
    /// own setup line where it is. A tile scoped to one provider row keeps that
    /// row and its siblings go.
    #[test]
    fn an_ai_usage_tile_scopes_to_the_codex_plan_row() {
        let p = ai_usage_payload();
        let out = scope_payload(&tile("aiUsage", "item:codex-plan"), p.clone());
        assert_eq!(blocks(&out), ["codex-plan"]);
        let gone = scope_payload(&tile("aiUsage", "item:gone"), p);
        assert_eq!(gone["plans"], json!([]));
        assert_eq!(gone["apis"], json!([]));
    }

    const AI_NOW: u64 = 1_700_000_000;

    /// Both vendors' plans, both vendors' spend and the Claude Code logs, every
    /// one of them failing or stale, so each header segment exists and a leak
    /// from one vendor to another's tile shows up as the wrong id in the text.
    fn every_line_firing() -> Value {
        use crate::usage::{fixture_state, Fixture};
        let mut state = fixture_state(Fixture::Measured, AI_NOW - 3_600, AI_NOW - 7 * 3_600);
        state
            .plan_mut(subscription::Provider::Claude)
            .state
            .failed("Claude sign-in expired".into());
        state
            .plan_mut(subscription::Provider::Codex)
            .state
            .failed("Codex sign-in expired".into());
        state
            .anthropic_api_mut()
            .failed("Anthropic key rejected".into());
        state.openai_api_mut().failed("OpenAI key rejected".into());
        crate::usage::ai_usage_view(&state, crate::usage::FIXTURE_PLANS, AI_NOW)
    }

    fn header_text(v: &Value, key: &str) -> String {
        v[key]["text"].as_str().unwrap_or_default().to_owned()
    }

    /// A vendor-scoped tile's header lines are its own rows': never another
    /// vendor's plan, spend or the Claude Code logs. `all` keeps every line.
    #[test]
    fn a_scoped_tile_carries_only_its_rows_header_lines() {
        let p = every_line_firing();
        let all = scope_payload(&tile("aiUsage", "all"), p.clone());
        assert_eq!(all["footer"], p["footer"]);
        assert_eq!(all["freshness"], p["freshness"]);
        let text = header_text(&all, "footer");
        for id in [
            "claude-plan",
            "codex-plan",
            "anthropic-api",
            "openai-api",
            "claude-today",
        ] {
            assert!(text.contains(id), "all keeps {id}");
        }

        // (scope, ids its header may name, ids it must not)
        let cases: [(&str, &[&str], &[&str]); 5] = [
            (
                "openai",
                &["codex-plan", "openai-api"],
                &["claude-plan", "anthropic-api", "claude-today"],
            ),
            (
                "item:openai-api",
                &["openai-api"],
                &["codex-plan", "claude-plan", "anthropic-api", "claude-today"],
            ),
            (
                "item:codex-plan",
                &["codex-plan"],
                &["openai-api", "claude-plan", "anthropic-api", "claude-today"],
            ),
            (
                "anthropic",
                &["claude-plan", "anthropic-api", "claude-today"],
                &["codex-plan", "openai-api"],
            ),
            (
                "claude",
                &["claude-today"],
                &["claude-plan", "codex-plan", "anthropic-api", "openai-api"],
            ),
        ];
        for (scope, named, absent) in cases {
            let out = scope_payload(&tile("aiUsage", scope), p.clone());
            for key in ["footer", "freshness"] {
                let text = header_text(&out, key);
                for id in absent {
                    assert!(!text.contains(id), "{scope} {key} must not name {id}");
                }
            }
            let footer = header_text(&out, "footer");
            for id in named {
                assert!(footer.contains(id), "{scope} footer names {id}");
            }
        }
    }

    /// A tile whose rows have no warning and no clock has no line at all, never
    /// another vendor's text.
    #[test]
    fn a_scoped_tile_with_nothing_to_say_has_no_header_line() {
        use crate::usage::{fixture_state, Fixture};
        let mut state = fixture_state(Fixture::Measured, AI_NOW, AI_NOW);
        state
            .anthropic_api_mut()
            .failed("Anthropic key rejected".into());
        let p = crate::usage::ai_usage_view(&state, crate::usage::FIXTURE_PLANS, AI_NOW);
        assert!(!header_text(&p, "footer").is_empty());
        let out = scope_payload(&tile("aiUsage", "openai"), p);
        assert!(out["footer"].is_null(), "{}", header_text(&out, "footer"));
        assert!(out["freshness"].is_null());
    }

    /// `loading` and the setup sentence belong to the vendors the tile keeps. An
    /// unread Anthropic key holds only tiles that include Anthropic, and its
    /// failure is never an OpenAI tile's message.
    #[test]
    fn a_scoped_tile_waits_and_explains_only_for_its_own_vendors() {
        use crate::plan_usage::PlanInputs;
        let mut state = crate::usage::UsageState::new();
        state.openai_api_mut().unconfigure();
        let off = PlanInputs::OFF;
        let p = crate::usage::ai_usage_view(&state, off, AI_NOW);
        let openai = scope_payload(&tile("aiUsage", "openai"), p.clone());
        assert_eq!(openai["loading"], false, "OpenAI's key is read and absent");
        assert_eq!(
            openai["message"]["text"],
            crate::settings::none_added_message()
        );
        let anthropic = scope_payload(&tile("aiUsage", "anthropic"), p.clone());
        assert_eq!(
            anthropic["loading"], true,
            "Anthropic's key is still unread"
        );
        assert_eq!(
            anthropic["message"]["text"],
            crate::usage::PROVIDERS_LOADING_MESSAGE
        );
        let item = scope_payload(&tile("aiUsage", "item:codex-plan"), p);
        assert_eq!(item["loading"], false);

        // An Anthropic failure to read speaks on Anthropic's tiles only.
        let mut state = crate::usage::UsageState::new();
        state
            .anthropic_api_mut()
            .unreadable("keychain locked".into());
        state.openai_api_mut().unconfigure();
        let p = crate::usage::ai_usage_view(&state, off, AI_NOW);
        let openai = scope_payload(&tile("aiUsage", "openai"), p.clone());
        assert_eq!(
            openai["message"]["text"],
            crate::settings::none_added_message()
        );
        let anthropic = scope_payload(&tile("aiUsage", "anthropic"), p);
        assert_eq!(anthropic["message"]["text"], "keychain locked");
    }

    /// `store` writes the AI Usage kind and title by literal, because it cannot
    /// see this crate. They must be the ones `SOURCES` holds.
    #[test]
    fn the_store_spells_the_ai_usage_kind_and_title_as_sources_does() {
        let mut data = store::StoreData {
            dashboard: Some(DashboardLayout {
                tiles: vec![
                    DashboardTile {
                        title: "Anthropic".into(),
                        ..tile("anthropic", "all")
                    },
                    tile("claudeUsage", "all"),
                ],
                ..default_layout()
            }),
            ..store::StoreData::default()
        };
        data.split_usage_tiles();
        data.merge_ai_tiles();
        let ai: Vec<DashboardTile> = data
            .dashboard
            .expect("layout")
            .tiles
            .into_iter()
            .filter(|t| t.source == "aiUsage")
            .collect();
        assert!(!ai.is_empty());
        let (source, title) = crate::dashboard::SOURCES
            .iter()
            .find(|(id, _)| *id == "aiUsage")
            .copied()
            .expect("aiUsage is a source");
        for tile in ai {
            assert_eq!((tile.source.as_str(), tile.title.as_str()), (source, title));
        }
    }

    #[test]
    fn a_usage_tile_scopes_to_its_provider_rows() {
        let p = json!({"providers":[
            {"id":"neon","rows":[{"label":"Compute"},{"label":"Storage"}]},
            {"id":"sentry","rows":[{"label":"Errors"}]}]});
        let out = scope_payload(&tile("usage", "item:neon:Storage"), p);
        let sections = out["providers"].as_array().unwrap();
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0]["rows"], json!([{"label":"Storage"}]));
    }

    /// `store` writes the new tiles by literal source id, title and scope,
    /// because it cannot see this crate. Every shape of saved tile must land on
    /// names the app knows, and pass the check every later save runs: a bad
    /// scope or title here would refuse each edit to the dashboard from then on,
    /// and no test in `store` could see it. Covers the old one-source Usage
    /// tile and the Anthropic and OpenAI tiles that replaced it.
    #[test]
    fn every_migrated_layout_passes_the_check_each_save_runs() {
        let usage_scopes = [
            "all",
            "attention",
            "providers",
            "plans",
            "claude",
            "item:claude-plan",
            "item:claude-today",
            "item:codex-plan",
            "item:neon:Compute",
            "item:vercel:Bandwidth",
        ];
        let vendor_scopes = [
            "all",
            "attention",
            "plans",
            "claude",
            "item:claude-plan",
            "item:claude-today",
            "item:anthropic-api",
            "item:codex-plan",
            "item:openai-api",
        ];
        let mut tiles: Vec<DashboardTile> = usage_scopes
            .iter()
            .enumerate()
            .map(|(i, scope)| DashboardTile {
                id: format!("u{i}"),
                title: "Usage".into(),
                ..tile("claudeUsage", scope)
            })
            .collect();
        for (source, title) in [("anthropic", "Anthropic"), ("openai", "OpenAI")] {
            for (i, scope) in vendor_scopes.iter().enumerate() {
                tiles.push(DashboardTile {
                    id: format!("{source}{i}"),
                    title: title.into(),
                    ..tile(source, scope)
                });
            }
        }
        let mut data = store::StoreData {
            dashboard: Some(DashboardLayout {
                tiles,
                ..default_layout()
            }),
            ..store::StoreData::default()
        };
        data.split_usage_tiles();
        data.merge_ai_tiles();
        let layout = data.dashboard.unwrap();
        crate::dashboard::validate(&layout).unwrap();
        for tile in &layout.tiles {
            let title = crate::dashboard::SOURCES
                .iter()
                .find(|(id, _)| *id == tile.source)
                .unwrap_or_else(|| panic!("{} is not a source", tile.source))
                .1;
            // A name the operator chose, or a narrowed tile's, stays; every
            // other tile takes its source's.
            if tile.title == "Usage" || tile.title == "AI Usage" {
                assert_eq!(tile.title, title, "{}", tile.id);
            }
            assert!(
                !matches!(tile.source.as_str(), "anthropic" | "openai"),
                "{} was left on a retired source",
                tile.id
            );
        }
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

    fn actions_billing_payload() -> Value {
        json!({"headline":{"value":"$10","caption":"c"},
            "stats":[{"label":"HOSTED MINUTES","value":"1 min"}],
            "budget":{"label":"BUDGET","value":"50%","bar":{"color":"#fff"}},
            "budgetNote":null,
            "dataThrough":{"text":"usage through Nov 14 19:00 UTC"},
            "breakdowns":[{"title":"b"}]})
    }

    #[test]
    fn actions_billing_scopes_keep_only_the_rows_they_name() {
        let p = actions_billing_payload();
        let out = scope_payload(&tile("actionsBilling", "item:budget"), p.clone());
        assert_eq!(out["headline"], Value::Null);
        assert_eq!(out["stats"], json!([]));
        assert_eq!(out["budget"], p["budget"]);
        assert_eq!(out["dataThrough"], p["dataThrough"]);
        let out = scope_payload(&tile("actionsBilling", "item:mtd"), p.clone());
        assert_eq!(out["headline"], p["headline"]);
        assert_eq!(out["budget"], Value::Null);
        let out = scope_payload(
            &tile("actionsBilling", "item:stat-HOSTED MINUTES"),
            p.clone(),
        );
        assert_eq!(out["stats"], p["stats"]);
        assert_eq!(out["headline"], Value::Null);
    }

    #[test]
    fn actions_billing_scopes_keep_only_the_rule_rows_they_name() {
        let mut p = actions_billing_payload();
        p["alerts"] = json!([
            {"id":"spike:repo-a:2026-11-21","label":"SPIKE"},
            {"id":"macos:repo-b:2026-11-21","label":"MACOS"},
        ]);
        let out = scope_payload(
            &tile("actionsBilling", "item:macos:repo-b:2026-11-21"),
            p.clone(),
        );
        assert_eq!(out["alerts"], json!([p["alerts"][1].clone()]));
        let out = scope_payload(&tile("actionsBilling", "item:budget"), p);
        assert_eq!(out["alerts"], json!([]));
    }

    #[test]
    fn actions_billing_trend_belongs_to_a_tile_that_keeps_every_row_only() {
        let mut p = actions_billing_payload();
        p["trend"] = json!({"label":"HOSTED MINUTES PER DAY","values":[1.0,2.0]});
        p["trendNote"] = json!({"text":"n"});
        let out = scope_payload(&tile("actionsBilling", "item:budget"), p.clone());
        assert_eq!(out["trend"], Value::Null);
        assert_eq!(out["trendNote"], Value::Null);
        let out = scope_payload(&tile("actionsBilling", "all"), p.clone());
        assert_eq!(out["trend"], p["trend"]);
        assert_eq!(out["trendNote"], p["trendNote"]);
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

    /// The two states an attention-scoped OpenClaw tile keeps while a runtime
    /// is not answering: a pairing request, and a red hint. Such a runtime has
    /// no agents or channels (`null`, from `sample-openclaw-pairing.json`'s
    /// shape, or the key absent), and the scope must not invent either.
    #[test]
    fn openclaw_attention_keeps_pairing_and_hint_without_inventing_sections() {
        let red = viewmodel::color::hex(viewmodel::color::RED);
        let p = json!({"runtimes":[
            {"id":"r1","agents":null,"channels":null,"connection":null,"hint":null,"cron":null,
                "pairing":{"title":"device pairing required","command":"openclaw devices approve req-7f31",
                    "dotColor":"#e0a03a"}},
            {"id":"r2","agents":null,"connection":null,"pairing":null,
                "hint":{"text":"gateway refused","color":red}},
            {"id":"r3","agents":{"rows":[{"id":"a1","name":"A1","status":"ok"}]},
                "channels":{"rows":[]},"connection":{"text":"up"},"hint":null,"pairing":null,"cron":null}]});
        let out = scope_payload(&tile("openclawAgents", "attention"), p);
        let runtimes = out["runtimes"].as_array().unwrap();
        let ids: Vec<&str> = runtimes.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(
            ids,
            ["r1", "r2"],
            "the healthy runtime has nothing in scope"
        );
        let (r1, r2) = (&runtimes[0], &runtimes[1]);
        assert_eq!(r1.get("agents"), Some(&Value::Null), "pairing: {r1}");
        assert_eq!(r1.get("channels"), Some(&Value::Null), "pairing: {r1}");
        assert_eq!(
            r1["pairing"]["command"],
            "openclaw devices approve req-7f31"
        );
        assert_eq!(r2.get("agents"), Some(&Value::Null), "hint: {r2}");
        assert_eq!(
            r2.get("channels"),
            None,
            "hint: an absent key stays absent: {r2}"
        );
        assert_eq!(r2["hint"]["text"], "gateway refused");
        assert_eq!(r2.get("hint").map(Value::is_null), Some(false));
        // A status key the scope drops is nulled where it exists, never added.
        assert_eq!(r1.get("cron"), Some(&Value::Null), "{r1}");
        assert_eq!(
            r2.get("cron"),
            None,
            "an absent status key stays absent: {r2}"
        );
    }

    /// A payload without the list a source's rows live in (a setup or error
    /// state) comes back exactly as it went in: no `rows`, `sections`,
    /// `runtimes`, `headline` or `budget` key is invented, and a `null` one
    /// stays `null`.
    #[test]
    fn a_payload_without_rows_gets_no_rows_invented() {
        for (source, scope) in [
            ("services", "item:gone"),
            ("sentryCrons", "active"),
            ("ghWorkflows", "failing"),
            ("ghRunners", "LINUX"),
            ("containers", "item:hostA|db"),
            ("aiUsage", "claude"),
            ("aiUsage", "item:gone"),
            ("usage", "item:gone"),
            ("azureCost", "item:budget"),
            ("openclawAgents", "attention"),
        ] {
            // Rows to filter are absent, so the scope keeps nothing. What the
            // tile adds is `tileEmpty`, compared whole: a scope on one `item:`
            // is the tile's own claim and says so; any other scope leaves a
            // source's own message to its panel (key absent), and carries the
            // stock sentence only when there is no message to paint (#656).
            let item =
                "This resource has no current reading. Review its source or choose another scope.";
            let stock = "No readings are available yet.";
            let expect = |p: &Value, has_message: bool| {
                let mut p = p.clone();
                let sentence = match (scope.starts_with("item:"), has_message) {
                    (true, _) => Some(item),
                    (false, true) => None,
                    (false, false) => Some(stock),
                };
                if let Some(sentence) = sentence {
                    p.as_object_mut()
                        .unwrap()
                        .insert("tileEmpty".into(), json!(sentence));
                }
                p
            };
            let absent =
                json!({"message":{"text":"Add a token in Settings"},"footer":{"text":"f"}});
            let scoped = scope_payload(&tile(source, scope), absent.clone());
            assert_eq!(scoped, expect(&absent, true), "{source} {scope}: absent");
            let null = json!({"rows":null,"sections":null,"runtimes":null,"windows":null,
                "plans":null,"apis":null,"providers":null,"stats":null,
                "trailing":null,"claudeToday":null});
            let scoped = scope_payload(&tile(source, scope), null.clone());
            assert_eq!(scoped, expect(&null, false), "{source} {scope}: null");
        }
    }

    #[test]
    fn a_scope_that_keeps_nothing_empties_the_panel() {
        let p = json!({"rows":[{"id":"a","label":"A"}]});
        let out = scope_payload(&tile("services", "item:gone"), p);
        assert_eq!(out["rows"], json!([]));
    }

    /// Row ids are built twice: by `dashboard::source_rows`, which names every
    /// row a scope can pick, and by this module's filters, which must rebuild
    /// the same id to keep that row (#641). For each scoped source this runs one
    /// sample payload through both. Every id `source_rows` gives is used as an
    /// `item:` scope, and the scoped payload must hold exactly one entry: none
    /// means this module derives a different id for it, more means two entries
    /// share one. The last assertion is that the sample holds nothing
    /// `source_rows` does not name, so a new kind of entry cannot slip past.
    #[test]
    fn each_scoped_source_rebuilds_the_ids_the_dashboard_gives_its_rows() {
        fn item(v: &Value, key: &str) -> usize {
            v[key].as_array().map_or(0, Vec::len)
        }
        fn nested(v: &Value, key: &str) -> usize {
            v[key]
                .as_array()
                .map_or(0, |groups| groups.iter().map(|g| item(g, "rows")).sum())
        }
        fn present(v: &Value) -> usize {
            usize::from(!v.is_null())
        }
        type Held = Box<dyn Fn(&Value) -> usize>;
        let sources: Vec<(&str, Value, Held, &[&str])> = vec![
            (
                "ghWorkflows",
                json!({"rows":[
                    {"repo":"o/a","name":"a"},{"repo":"o/b","name":"b"}]}),
                Box::new(move |v| item(v, "rows")),
                &[],
            ),
            (
                "ghRunners",
                json!({"rows":[
                    {"org":"o","name":"a"},{"org":"o","name":"b"},{"org":"p","name":"a"}]}),
                Box::new(move |v| item(v, "rows")),
                &[],
            ),
            (
                "services",
                json!({"rows":[{"id":"s1","label":"One"},{"id":"s2","label":"Two"}]}),
                Box::new(move |v| item(v, "rows")),
                &[],
            ),
            (
                "sentryCrons",
                json!({"rows":[{"id":"m1","label":"One"},{"id":"m2","label":"Two"}]}),
                Box::new(move |v| item(v, "rows")),
                &[],
            ),
            (
                "containers",
                json!({"sections":[
                    {"host":"hostA","label":"A","rows":[{"name":"db"},{"name":"web"}]},
                    {"host":"hostB","label":"B","rows":[{"name":"db"}]}]}),
                Box::new(move |v| nested(v, "sections")),
                &[],
            ),
            (
                "aiUsage",
                json!({"trailing":"Claude Code · 1M","claudeToday":"1M",
                    "projects":[{"name":"p"}],
                    "windows":[{"label":"LAST 5H"},{"label":"LAST 7D"}],
                    "plans":[
                        {"id":"claude-plan","label":"Claude · Ultra","metrics":[{"label":"Week"}]},
                        {"id":"codex-plan","label":"Codex · Plus","metrics":[]}],
                    "apis":[
                        {"id":"anthropic-api","label":"Anthropic API spend (MTD)","value":"$1"},
                        {"id":"openai-api","label":"OpenAI API spend (MTD)","value":"$2"}]}),
                // The `claude-today` row stands for the whole rollup block, so
                // its windows count as one entry.
                Box::new(move |v| {
                    usize::from(item(v, "windows") > 0) + item(v, "plans") + item(v, "apis")
                }),
                &[],
            ),
            (
                "usage",
                json!({"providers":[
                    {"id":"neon","rows":[{"label":"Compute"}]},
                    {"id":"sentry","rows":[{"label":"Compute"}]}]}),
                Box::new(move |v| nested(v, "providers")),
                &[],
            ),
            (
                "azureCost",
                json!({"headline":{"value":"$10"},
                    "stats":[{"label":"Today","value":"$1"},{"label":"Yesterday","value":"$2"}],
                    "budget":{"label":"Budget","value":"50%"}}),
                Box::new(move |v| {
                    present(&v["headline"]) + item(v, "stats") + present(&v["budget"])
                }),
                &[],
            ),
            (
                "openclawAgents",
                json!({"runtimes":[
                    {"id":"r1",
                        "agents":{"rows":[{"id":"a1"},{"id":"a2"}]},
                        "channels":{"rows":[{"id":"c1"}]},
                        "connection":{"text":"up"},"hint":{"text":"h"},
                        "pairing":{"title":"p"},"cron":{"title":"c"}},
                    {"id":"r2",
                        "agents":{"rows":[{"id":"a1"}]},
                        "channels":{"rows":[{"id":"c1"}]},
                        "connection":{"text":"up"},"hint":null,"pairing":null,"cron":null}]}),
                Box::new(move |v| {
                    v["runtimes"].as_array().map_or(0, |runtimes| {
                        runtimes
                            .iter()
                            .map(|r| {
                                nested_one(r, "agents")
                                    + nested_one(r, "channels")
                                    + ["connection", "hint", "pairing", "cron"]
                                        .iter()
                                        .filter(|k| !r[**k].is_null())
                                        .count()
                            })
                            .sum()
                    })
                }),
                &[],
            ),
        ];
        fn nested_one(runtime: &Value, key: &str) -> usize {
            runtime[key]["rows"].as_array().map_or(0, Vec::len)
        }
        for (source, payload, held, unfiltered) in &sources {
            let ids = crate::dashboard::all_row_ids(source, payload);
            let distinct: BTreeSet<&String> = ids.iter().collect();
            assert_eq!(distinct.len(), ids.len(), "{source}: ids are distinct");
            assert_eq!(
                held(payload),
                ids.len() - unfiltered.len(),
                "{source}: the sample holds exactly what source_rows names: {ids:?}"
            );
            for id in ids.iter().filter(|id| !unfiltered.contains(&id.as_str())) {
                let scoped = scope_payload(&tile(source, &format!("item:{id}")), payload.clone());
                assert_eq!(
                    held(&scoped),
                    1,
                    "{source}: scoping to {id:?} must keep that entry and no other: {scoped}"
                );
            }
        }
    }

    /// Machines are cards, and a Machines tile keeps them by the card's `id`
    /// (`CockpitFrame::payload`); `source_rows` must name each by the same key.
    #[test]
    fn machines_are_named_by_their_card_id() {
        let cards = json!({"hosts":[
            {"id":"local","hostName":"Studio","connection":{"state":"live"}},
            {"id":"2b1c","error":{"hostName":"nuc"},"connection":{"state":"unreachable"}}]});
        assert_eq!(
            crate::dashboard::all_row_ids("hosts", &cards),
            ["local", "2b1c"]
        );
    }

    #[test]
    fn hosts_come_back_unchanged() {
        let p = json!({"hosts":[{"hostName":"a"}]});
        assert_eq!(scope_payload(&tile("hosts", "item:gone"), p.clone()), p);
    }
}
