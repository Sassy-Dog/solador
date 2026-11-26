//! Narrows one source's panel payload to what a tile's scope keeps, so a tile's
//! Full view shows the tile's rows and nothing else.
//!
//! The kept ids come from `dashboard::scoped_row_ids`, which derives them from
//! `dashboard::source_rows`; each filter here rebuilds the same id `source_rows`
//! gives that row. Only the rows are filtered. Every other key (`trailing`,
//! `stats`, `chips`, `footer`, `freshness`, `message`, `health`, section header
//! counts) is the source-wide total the scope note refers to and is left as is.
//! The exceptions: AI Usage's `trailing` and `claudeToday`, which speak for the
//! rollups and go when the tile does not keep `claude-today`; its `freshness`,
//! `loading` and `message`, which a narrowed tile (any scope but `all` and
//! `attention`) has rebuilt from its own rows by `ai_header`, with `lines` cut to
//! those rows and `rollupsMessage` removed. **A scope picks figures only:** its
//! `footer`, every configured source's `\u{26a0}` warning, is left whole, and
//! shows on every AI Usage tile; and the
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
    // The scope picks figures only. `footer` (every row's warning) is left as the
    // whole tile has it; `freshness`, `loading` and the body `message` are the
    // scope's rows' alone, and so is what the payload carries per row: `lines` is
    // cut to those rows and `rollupsMessage` is gone.
    let narrowed = if let Some(header) = ai_header(payload, scope) {
        let rows = crate::dashboard::ai_scope_rows(scope);
        if let Some(map) = payload.as_object_mut() {
            map.insert("freshness".into(), header.freshness);
            map.insert("loading".into(), Value::Bool(header.loading));
            map.insert("message".into(), header.message);
            map.remove("rollupsMessage");
        }
        if let Some(lines) = array_mut(payload, "lines") {
            lines.retain(|l| rows.contains(&str_of(l, "id")));
        }
        true
    } else {
        false
    };
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
    // A narrowed tile's `message` was rebuilt above. An `attention` tile keeps the
    // whole tile's, which speaks for the rollups this tile left out: with nothing
    // else on the panel it stays, or a Full view that keeps no row is blank.
    if narrowed {
        return;
    }
    let shows_a_block = ["plans", "apis"]
        .into_iter()
        .any(|key| array_mut(payload, key).is_some_and(|rows| !rows.is_empty()));
    if shows_a_block {
        clear(payload, "message");
    }
}

/// What a narrowed AI Usage tile rebuilds from its own rows. `None` for `all`
/// and `attention`, which keep the whole-tile values, and for a payload carrying
/// no `lines` (a setup or error state).
///
/// **A tile's scope picks only which figures it shows.** Every `\u{26a0}` warning
/// from a configured AI source (the three plans, Claude's, Codex's and Copilot's
/// `copilot-plan`; both Admin keys; both Copilot billing rows; the Claude Code logs,
/// and a key the credential store would not read) shows on every AI Usage tile,
/// labelled with its row id: the payload's `footer` and an overview card's `\u{26a0}`
/// entry are the whole tile's, whatever the scope. Only three things follow
/// the scope's **relevant rows** (`ai_scope_rows`), never another vendor's:
/// `freshness` (the kept rows' clocks, merged as the whole tile's are),
/// `loading` (a relevant row loading, or a relevant key unread), and the body
/// `message`, which is, in order: (1) the loading line if a relevant key is
/// unread, else (2) the rollups' own line (`rollupsMessage`, such as "reading
/// logs\u{2026}") when the Claude Code logs are relevant and on but have no data yet
/// (both are said even beside a kept plan with content), else, only when no
/// relevant part is on, (3) `settings::not_set_up_message` for each vendor with
/// a relevant part off (every relevant key read and absent), else (4) none. A
/// key's failure is never the message: it is said once, as its warning. The
/// whole-tile "No AI usage yet" sentence is never used here. An overview card
/// takes its clock entry from `freshness`, so the card and the Full view match.
pub(crate) struct AiHeader {
    pub freshness: Value,
    pub loading: bool,
    pub message: Value,
}

pub(crate) fn ai_header(inputs: &Value, scope: &str) -> Option<AiHeader> {
    if matches!(scope, "all" | "attention") {
        return None;
    }
    let lines = inputs["lines"].as_array()?;
    let rows = crate::dashboard::ai_scope_rows(scope);
    let mine: Vec<&Value> = lines
        .iter()
        .filter(|l| rows.contains(&str_of(l, "id")))
        .collect();
    let segments = |key: &str| -> Vec<Value> { mine.iter().map(|l| l[key].clone()).collect() };
    let rollups_on = mine
        .iter()
        .any(|l| l["on"] == true && str_of(l, "id") == CLAUDE_TODAY_ID);
    let message = if mine.is_empty() {
        Value::Null
    } else if mine.iter().any(|l| l["undecided"] == true) {
        crate::usage::state_message(crate::usage::PROVIDERS_LOADING_MESSAGE)
    } else if rollups_on && !inputs["rollupsMessage"].is_null() {
        inputs["rollupsMessage"].clone()
    } else if mine
        .iter()
        .any(|l| l["on"] == true || l["unreadable"] == true)
    {
        // Something to show, or a key that is not known to be absent: its
        // failure is its warning, not a sentence here.
        Value::Null
    } else {
        // Every relevant part is off and its key, if it has one, was read and is
        // absent: name only the vendors of those parts, in body order.
        let mut vendors: Vec<&'static str> = Vec::new();
        for l in &mine {
            if let Some(v) = crate::dashboard::vendor_of(str_of(l, "id")) {
                if !vendors.contains(&v) {
                    vendors.push(v);
                }
            }
        }
        let sentences: Vec<String> = vendors
            .iter()
            .map(|v| {
                let title = match *v {
                    "anthropic" => crate::settings::ANTHROPIC_TITLE,
                    "copilot" => crate::settings::COPILOT_TITLE,
                    _ => crate::settings::OPENAI_TITLE,
                };
                crate::settings::not_set_up_message(title)
            })
            .collect();
        crate::usage::state_message(&sentences.join(" "))
    };
    Some(AiHeader {
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

    /// The ids of a payload's plans and spend blocks, in order, then the
    /// billing halves the Copilot card carries (they are the card's, not
    /// blocks of their own, but each is a source a scope speaks for).
    fn blocks(v: &Value) -> Vec<String> {
        let id = |b: &Value| b["id"].as_str().unwrap().to_owned();
        let mut ids: Vec<String> = ["plans", "apis"]
            .iter()
            .flat_map(|key| v[*key].as_array().into_iter().flatten())
            .map(id)
            .collect();
        ids.extend(
            v["plans"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|plan| plan["halves"].as_array().into_iter().flatten())
                .map(id),
        );
        ids
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

    /// On a payload with no per-row `lines` (a setup or error state, or an
    /// `attention` tile, which keeps the whole tile's message), `message` speaks
    /// for the rollups: a tile showing another block with the rollups scoped
    /// out drops it, and a tile left with nothing keeps it, or its Full view
    /// would be blank. A narrowed tile rebuilds its own message instead.
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

    /// A scope picks figures only: every row's warning is on every tile, with
    /// the whole tile's wording and colour, and only the clocks are the scope's
    /// rows'. `all` keeps everything.
    #[test]
    fn a_scoped_tile_carries_every_warning_but_only_its_own_clocks() {
        let p = every_line_firing();
        let all_ids = [
            "claude-plan",
            "codex-plan",
            "copilot-plan",
            "anthropic-api",
            "openai-api",
            "copilot-org",
            "copilot-personal",
            "claude-today",
        ];
        // (scope, the ids whose clock it may carry)
        let cases: [(&str, &[&str]); 6] = [
            (
                "copilot",
                &["copilot-org", "copilot-personal", "copilot-plan"],
            ),
            ("openai", &["codex-plan", "openai-api"]),
            ("item:openai-api", &["openai-api"]),
            ("item:codex-plan", &["codex-plan"]),
            (
                "anthropic",
                &["claude-plan", "anthropic-api", "claude-today"],
            ),
            ("claude", &["claude-today"]),
        ];
        for (scope, clocks) in cases {
            let out = scoped(&p, scope);
            assert_eq!(out["footer"], p["footer"], "{scope}");
            let footer = header_text(&out, "footer");
            for id in all_ids {
                assert!(footer.contains(id), "{scope} footer names {id}");
            }
            let t = header_text(&out, "freshness");
            for id in all_ids.iter().filter(|id| !clocks.contains(id)) {
                assert!(!t.contains(id), "{scope} freshness must not name {id}");
            }
        }
        let all = scoped(&p, "all");
        assert_eq!(all["footer"], p["footer"]);
        assert_eq!(all["freshness"], p["freshness"]);
    }

    /// Another vendor's failing request still warns on this tile, and a tile with
    /// no clock of its own has no freshness line.
    #[test]
    fn a_failing_vendor_warns_on_every_tile_but_dates_only_its_own() {
        use crate::usage::{fixture_state, Fixture};
        let mut state = fixture_state(Fixture::Measured, AI_NOW, AI_NOW);
        state
            .anthropic_api_mut()
            .failed("Anthropic key rejected".into());
        let p = crate::usage::ai_usage_view(&state, crate::usage::FIXTURE_PLANS, AI_NOW);
        for scope in ["openai", "item:codex-plan", "plans", "item:openai-api"] {
            let out = scoped(&p, scope);
            assert!(
                header_text(&out, "footer")
                    .contains("\u{26a0} anthropic-api: Anthropic key rejected"),
                "{scope}"
            );
            assert!(out["freshness"].is_null(), "{scope} has no stale clock");
        }
    }

    /// A narrowed tile's `freshness` is its own rows' clocks, and it is not
    /// empty: each scope names the kept rows whose clock fires.
    #[test]
    fn a_narrowed_tiles_freshness_names_the_kept_rows_that_are_stale() {
        use crate::usage::{fixture_state, Fixture};
        let state = fixture_state(Fixture::Measured, AI_NOW, AI_NOW - 7 * 3_600);
        let p = crate::usage::ai_usage_view(&state, crate::usage::FIXTURE_PLANS, AI_NOW);
        let all_ids = ["claude-plan", "codex-plan", "anthropic-api", "openai-api"];
        let cases: [(&str, &[&str]); 7] = [
            ("openai", &["codex-plan", "openai-api"]),
            ("anthropic", &["claude-plan", "anthropic-api"]),
            ("plans", &["claude-plan", "codex-plan"]),
            ("item:claude-plan", &["claude-plan"]),
            ("item:codex-plan", &["codex-plan"]),
            ("item:anthropic-api", &["anthropic-api"]),
            ("item:openai-api", &["openai-api"]),
        ];
        for (scope, clocks) in cases {
            let t = header_text(&scoped(&p, scope), "freshness");
            assert!(!t.is_empty(), "{scope} has a clock to show");
            for id in all_ids {
                assert_eq!(t.contains(id), clocks.contains(&id), "{scope} {id}");
            }
        }
        // The Claude Code row has no clock of its own.
        assert!(scoped(&p, "claude")["freshness"].is_null());
    }

    /// Stale Claude Code logs warn on an OpenAI tile too.
    #[test]
    fn stale_claude_code_logs_warn_on_an_openai_tile() {
        let p = every_line_firing();
        let out = scoped(&p, "openai");
        assert!(header_text(&out, "footer").contains("claude-today"));
        assert!(!header_text(&out, "freshness").contains("claude"));
    }

    /// A key the credential store would not read has no section and so no
    /// warning of its own; it warns under its row id on every tile, and its
    /// reason is said exactly once: not again as the message or `tileEmpty`.
    #[test]
    fn an_unreadable_key_warns_on_every_tile_and_says_its_reason_once() {
        let reason = "keychain locked";
        let warn = "\u{26a0} anthropic-api: keychain locked";
        for (p, label) in [
            (
                ai_state(true, false, false, Key::Unreadable, Key::Absent),
                "a Claude plan showing",
            ),
            (
                ai_state(false, false, false, Key::Unreadable, Key::Absent),
                "nothing showing",
            ),
        ] {
            for scope in [
                "all",
                "anthropic",
                "openai",
                "item:codex-plan",
                "plans",
                "item:anthropic-api",
            ] {
                let out = scoped(&p, scope);
                assert!(
                    header_text(&out, "footer").contains(warn),
                    "{label} {scope}: footer"
                );
                let said = header_text(&out, "footer").matches(reason).count()
                    + text(&out).matches(reason).count()
                    + out["tileEmpty"]
                        .as_str()
                        .unwrap_or_default()
                        .matches(reason)
                        .count();
                assert_eq!(said, 1, "{label} {scope}: said {said} times");
            }
        }
        // A key merely unread warns of nothing: it is loading.
        let p = ai_state(false, false, false, Key::Unread, Key::Absent);
        assert!(!header_text(&scoped(&p, "all"), "footer").contains("anthropic-api"));
        assert!(!header_text(&scoped(&p, "openai"), "footer").contains("anthropic-api"));
    }

    /// The scoped parts stay scoped: an OpenAI tile is not held by Anthropic's
    /// unread key, never says Anthropic is not set up, and dates only OpenAI.
    #[test]
    fn an_openai_tile_stays_scoped_for_loading_message_and_clocks() {
        let p = ai_state(false, false, false, Key::Unread, Key::Absent);
        let out = scoped(&p, "openai");
        assert_eq!(out["loading"], false);
        assert_eq!(text(&out), OPENAI_OFF);
        assert!(!text(&out).contains("Anthropic"));
        let p = every_line_firing();
        let t = header_text(&scoped(&p, "openai"), "freshness");
        assert!(!t.contains("claude") && !t.contains("anthropic"), "{t}");
    }

    /// How a test sets one vendor's Admin key before a pass has read it.
    #[derive(Clone, Copy)]
    enum Key {
        Unread,
        Absent,
        Unreadable,
        Stored,
    }

    /// A store state from the five things an operator can have on or off: the
    /// two plans, the Claude Code logs and each vendor's Admin key.
    fn ai_state(claude: bool, codex: bool, logs: bool, anthropic: Key, openai: Key) -> Value {
        use crate::plan_usage::PlanInputs;
        let mut state = crate::usage::UsageState::new();
        let set =
            |key: &mut crate::usage::ProviderState<::usage::ApiSpendSummary>, how: Key| match how {
                Key::Unread => {}
                Key::Absent => key.unconfigure(),
                Key::Unreadable => key.unreadable("keychain locked".into()),
                Key::Stored => {
                    key.begin();
                    key.succeeded(
                        ::usage::ApiSpendSummary::Measured {
                            usd: 1.0,
                            refreshed_at: None,
                        },
                        AI_NOW,
                        None,
                    );
                }
            };
        set(state.anthropic_api_mut(), anthropic);
        set(state.openai_api_mut(), openai);
        // Copilot's token has been read and is absent, as on a store that never
        // added it: these tests are about the other vendors.
        state.copilot_personal_unconfigure();
        let inputs = PlanInputs {
            claude,
            codex,
            claude_local_logs: logs,
            ..PlanInputs::OFF
        };
        crate::usage::ai_usage_view(&state, inputs, AI_NOW)
    }

    fn scoped(p: &Value, scope: &str) -> Value {
        scope_payload(&tile("aiUsage", scope), p.clone())
    }

    fn text(v: &Value) -> String {
        v["message"]["text"].as_str().unwrap_or_default().to_owned()
    }

    const ANTHROPIC_OFF: &str = "Anthropic isn\u{2019}t set up \u{2014} add it in Settings \u{2192} Sources \u{2192} Add source";
    const OPENAI_OFF: &str = "OpenAI isn\u{2019}t set up \u{2014} add it in Settings \u{2192} Sources \u{2192} Add source";
    const COPILOT_OFF: &str = "GitHub Copilot isn\u{2019}t set up \u{2014} add it in Settings \u{2192} Sources \u{2192} Add source";

    /// Every scope: the ids it keeps, and what stays scoped (clocks, loading,
    /// message, lines) beside what does not (every row's warning). The state has
    /// every part on and every line firing.
    #[test]
    fn every_scope_keeps_its_rows_and_carries_only_their_header() {
        let p = every_line_firing();
        let all_ids = [
            "claude-plan",
            "codex-plan",
            "copilot-plan",
            "anthropic-api",
            "openai-api",
            "copilot-org",
            "copilot-personal",
            "claude-today",
        ];
        // (scope, the ids it speaks for)
        let matrix: [(&str, &[&str]); 16] = [
            (
                "copilot",
                &["copilot-org", "copilot-personal", "copilot-plan"],
            ),
            // Each was a row of its own once; all three now show the one card,
            // so all three sources speak for it.
            (
                "item:copilot-org",
                &["copilot-org", "copilot-personal", "copilot-plan"],
            ),
            (
                "item:copilot-personal",
                &["copilot-org", "copilot-personal", "copilot-plan"],
            ),
            (
                "item:copilot-plan",
                &["copilot-org", "copilot-personal", "copilot-plan"],
            ),
            ("all", &all_ids),
            ("attention", &all_ids),
            (
                "anthropic",
                &["claude-plan", "anthropic-api", "claude-today"],
            ),
            ("openai", &["codex-plan", "openai-api"]),
            ("claude", &["claude-today"]),
            (
                "plans",
                &[
                    "claude-plan",
                    "codex-plan",
                    "copilot-plan",
                    "copilot-org",
                    "copilot-personal",
                ],
            ),
            ("item:claude-plan", &["claude-plan"]),
            ("item:codex-plan", &["codex-plan"]),
            ("item:anthropic-api", &["anthropic-api"]),
            ("item:openai-api", &["openai-api"]),
            ("item:claude-today", &["claude-today"]),
            ("item:gone", &[]),
        ];
        for (scope, ids) in matrix {
            let out = scoped(&p, scope);
            if matches!(scope, "all" | "attention") {
                // Unchanged: the whole-tile header, lines and sentence.
                for key in [
                    "footer",
                    "freshness",
                    "loading",
                    "message",
                    "lines",
                    "rollupsMessage",
                ] {
                    assert_eq!(out[key], p[key], "{scope} {key}");
                }
                continue;
            }
            // The rows kept.
            let mut kept = blocks(&out);
            if !out["windows"].as_array().unwrap().is_empty() {
                kept.push("claude-today".into());
            }
            kept.sort();
            let mut want: Vec<String> = ids.iter().map(|s| (*s).to_owned()).collect();
            want.sort();
            assert_eq!(kept, want, "{scope} keeps");
            // The scope picks figures only: every row's warning is on every
            // tile, as the whole tile has it. Only the clocks are the rows'.
            assert_eq!(out["footer"], p["footer"], "{scope} footer");
            let footer = header_text(&out, "footer");
            for id in all_ids {
                assert!(footer.contains(id), "{scope} footer names {id}");
            }
            let clocks = header_text(&out, "freshness");
            for id in all_ids.iter().filter(|id| !ids.contains(id)) {
                assert!(!clocks.contains(id), "{scope} freshness must not name {id}");
            }
            let line_ids: Vec<&str> = out["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| l["id"].as_str().unwrap())
                .collect();
            assert_eq!(
                line_ids,
                all_ids
                    .iter()
                    .copied()
                    .filter(|id| ids.contains(id))
                    .collect::<Vec<_>>(),
                "{scope} lines"
            );
            assert!(out.get("rollupsMessage").is_none(), "{scope}");
            assert!(out.get("vendors").is_none(), "{scope}");
            let carried = out["lines"].to_string();
            for id in all_ids.iter().filter(|id| !ids.contains(id)) {
                assert!(!carried.contains(id), "{scope} lines must not carry {id}");
            }
            assert_eq!(out["loading"], false, "{scope} loading");
            assert!(
                out["message"].is_null(),
                "{scope} has content, so no message"
            );
        }
    }

    /// One vendor on, the other off: the off vendor's tiles say that vendor is
    /// not set up, never the whole-tile sentence, and the on vendor's show rows.
    #[test]
    fn an_off_vendor_names_only_itself_whichever_vendor_is_on() {
        let whole = crate::settings::none_added_message();
        // Anthropic on (its plan), OpenAI's key absent and Codex off.
        let p = ai_state(true, false, false, Key::Absent, Key::Absent);
        for scope in ["openai", "item:codex-plan", "item:openai-api"] {
            let out = scoped(&p, scope);
            assert_eq!(text(&out), OPENAI_OFF, "{scope}");
            assert_ne!(text(&out), whole, "{scope}");
            assert_eq!(out["loading"], false, "{scope}");
        }
        let both_plans = scoped(&p, "plans");
        assert!(both_plans["message"].is_null(), "one plan is on");
        let anthropic = scoped(&p, "anthropic");
        assert_eq!(blocks(&anthropic), ["claude-plan"]);
        assert!(anthropic["message"].is_null());

        // Mirror: OpenAI on (Codex), Anthropic's key absent, Claude and logs off.
        let p = ai_state(false, true, false, Key::Absent, Key::Absent);
        for scope in [
            "anthropic",
            "claude",
            "item:claude-today",
            "item:claude-plan",
            "item:anthropic-api",
        ] {
            let out = scoped(&p, scope);
            assert_eq!(text(&out), ANTHROPIC_OFF, "{scope}");
            assert_ne!(text(&out), whole, "{scope}");
        }
        let openai = scoped(&p, "openai");
        assert_eq!(blocks(&openai), ["codex-plan"]);
        assert!(openai["message"].is_null());

        // `plans` with both plan parts off names each vendor with a part off,
        // in body order, and nothing of the whole-tile sentence. Copilot's plan
        // row is one of the plans, so it is named too.
        let p = ai_state(false, false, false, Key::Absent, Key::Absent);
        let out = scoped(&p, "plans");
        assert_eq!(
            text(&out),
            format!("{ANTHROPIC_OFF} {OPENAI_OFF} {COPILOT_OFF}")
        );
        // The whole tile keeps its own sentence.
        assert_eq!(scoped(&p, "all")["message"]["text"], whole);
    }

    /// The other vendor's key not being read yet neither holds a tile loading nor
    /// hides its own sentence, in both directions.
    #[test]
    fn an_unread_key_holds_only_its_own_vendors_tiles() {
        // Anthropic unread, OpenAI read and absent.
        let p = ai_state(false, false, false, Key::Unread, Key::Absent);
        for scope in ["openai", "item:codex-plan", "item:openai-api"] {
            let out = scoped(&p, scope);
            assert_eq!(out["loading"], false, "{scope}");
            assert_eq!(text(&out), OPENAI_OFF, "{scope}");
        }
        for scope in ["anthropic", "item:anthropic-api"] {
            let out = scoped(&p, scope);
            assert_eq!(out["loading"], true, "{scope}");
            assert_eq!(
                text(&out),
                crate::usage::PROVIDERS_LOADING_MESSAGE,
                "{scope}"
            );
        }
        // Parts with no key of their own are not held by it.
        for scope in ["claude", "item:claude-plan", "item:claude-today"] {
            assert_eq!(scoped(&p, scope)["loading"], false, "{scope}");
        }
        // Mirror.
        let p = ai_state(false, false, false, Key::Absent, Key::Unread);
        for scope in [
            "anthropic",
            "claude",
            "item:claude-plan",
            "item:anthropic-api",
        ] {
            let out = scoped(&p, scope);
            assert_eq!(out["loading"], false, "{scope}");
            assert_eq!(text(&out), ANTHROPIC_OFF, "{scope}");
        }
        for scope in ["openai", "item:openai-api"] {
            let out = scoped(&p, scope);
            assert_eq!(out["loading"], true, "{scope}");
            assert_eq!(
                text(&out),
                crate::usage::PROVIDERS_LOADING_MESSAGE,
                "{scope}"
            );
        }
        // The whole tile is held by either.
        assert_eq!(scoped(&p, "all")["loading"], true);
    }

    /// A Copilot tile keeps only Copilot: the one card with both halves and
    /// their clocks, none of another vendor's, and not the Claude Code rollups.
    #[test]
    fn a_copilot_tile_keeps_only_copilot() {
        let p = every_line_firing();
        let out = scoped(&p, "copilot");
        // One card, in the `plans` block, and its halves inside it.
        assert_eq!(
            blocks(&out),
            ["copilot-plan", "copilot-org", "copilot-personal"]
        );
        assert_eq!(out["windows"], json!([]));
        assert!(out.get("projects").is_none());
        assert_eq!(out["trailing"], "");
        assert!(out.get("copilot").is_none(), "no list beside the card");
        // A saved selection of one half shows the same card.
        let one = scoped(&p, "item:copilot-personal");
        assert_eq!(
            blocks(&one),
            ["copilot-plan", "copilot-org", "copilot-personal"]
        );
        // The scope picks figures only: every other row's warning is still here.
        assert_eq!(out["footer"], p["footer"]);
        assert!(header_text(&out, "footer").contains("anthropic-api"));
        // The card is also the `plans` tile's: `plans` means every plan limit.
        // No other vendor's tile carries it, or its halves.
        let has_plan =
            |scope: &str| blocks(&scoped(&p, scope)).contains(&"copilot-plan".to_owned());
        assert!(has_plan("plans"));
        for scope in ["anthropic", "openai", "claude"] {
            assert!(!has_plan(scope), "{scope}");
            assert!(
                !blocks(&scoped(&p, scope)).contains(&"copilot-org".to_owned()),
                "{scope}"
            );
        }
    }

    /// A Copilot half that is on and not yet read holds only a Copilot tile
    /// loading, and one that is off and read absent says only its own sentence.
    #[test]
    fn a_copilot_tile_is_held_by_its_own_key_only() {
        let mut state = crate::usage::UsageState::new();
        state.anthropic_api_mut().unconfigure();
        state.openai_api_mut().unconfigure();
        let p = crate::usage::ai_usage_view(&state, crate::plan_usage::PlanInputs::OFF, AI_NOW);
        let out = scoped(&p, "copilot");
        assert_eq!(out["loading"], true);
        assert_eq!(text(&out), crate::usage::PROVIDERS_LOADING_MESSAGE);
        for scope in ["anthropic", "openai"] {
            assert_eq!(scoped(&p, scope)["loading"], false, "{scope}");
        }
        state.copilot_personal_unconfigure();
        let p = crate::usage::ai_usage_view(&state, crate::plan_usage::PlanInputs::OFF, AI_NOW);
        let out = scoped(&p, "copilot");
        assert_eq!(out["loading"], false);
        assert_eq!(
            text(&out),
            crate::settings::not_set_up_message(crate::settings::COPILOT_TITLE)
        );
    }

    /// A key failure is never a tile's message; it is its warning.
    #[test]
    fn a_key_failure_is_never_the_tiles_message() {
        let p = ai_state(false, false, false, Key::Unreadable, Key::Absent);
        for scope in ["anthropic", "item:anthropic-api"] {
            assert!(scoped(&p, scope)["message"].is_null(), "{scope}");
        }
        assert_eq!(text(&scoped(&p, "openai")), OPENAI_OFF);
        let p = ai_state(false, false, false, Key::Absent, Key::Unreadable);
        assert!(scoped(&p, "openai")["message"].is_null());
        assert_eq!(text(&scoped(&p, "anthropic")), ANTHROPIC_OFF);
        // The whole tile: nothing on and a key unreadable says nothing either.
        assert!(scoped(&p, "all")["message"].is_null());
        // A stored key is content, not a message.
        let p = ai_state(false, false, false, Key::Stored, Key::Absent);
        assert!(scoped(&p, "anthropic")["message"].is_null());
    }

    /// The Full view says a narrowed tile's sentence once: in `message`, which the
    /// panel paints, and not again as `tileEmpty` (#656). Whatever the sentence
    /// is: not set up, a read still in flight, or a key that could not be read.
    #[test]
    fn a_narrowed_tiles_sentence_is_said_once_never_as_tile_empty_too() {
        let cases: [(Value, &[&str], String); 6] = [
            // Anthropic on, OpenAI off.
            (
                ai_state(true, false, false, Key::Absent, Key::Absent),
                &["openai", "item:openai-api", "item:codex-plan"],
                OPENAI_OFF.into(),
            ),
            // Mirror: OpenAI on, Anthropic off.
            (
                ai_state(false, true, false, Key::Absent, Key::Absent),
                &[
                    "anthropic",
                    "claude",
                    "item:claude-plan",
                    "item:anthropic-api",
                ],
                ANTHROPIC_OFF.into(),
            ),
            // The other vendor on and this key unread.
            (
                ai_state(true, false, false, Key::Absent, Key::Unread),
                &["openai", "item:openai-api"],
                crate::usage::PROVIDERS_LOADING_MESSAGE.into(),
            ),
            (
                ai_state(false, true, false, Key::Unread, Key::Absent),
                &["anthropic", "item:anthropic-api"],
                crate::usage::PROVIDERS_LOADING_MESSAGE.into(),
            ),
            // This key unreadable: no message, its warning says why (once).
            (
                ai_state(true, false, false, Key::Absent, Key::Unreadable),
                &["openai", "item:openai-api"],
                String::new(),
            ),
            (
                ai_state(false, true, false, Key::Unreadable, Key::Absent),
                &["anthropic", "item:anthropic-api"],
                String::new(),
            ),
        ];
        for (p, scopes, sentence) in cases {
            for scope in scopes {
                let out = scoped(&p, scope);
                assert_eq!(text(&out), sentence, "{scope} message");
                if sentence.is_empty() {
                    // No sentence: the tile's stock empty text is allowed, but
                    // not the key's reason, which is its warning.
                    let empty = out["tileEmpty"].as_str().unwrap_or_default();
                    assert!(!empty.contains("keychain locked"), "{scope}");
                } else {
                    assert!(
                        out.get("tileEmpty").is_none(),
                        "{scope}: the sentence is also painted as tileEmpty"
                    );
                }
            }
        }
    }

    /// Parts that are on but have nothing to say yet, and the order the message
    /// checks run in: an unread key of the tile leads even when a part is on, and
    /// the rollups' own line comes after it.
    #[test]
    fn a_narrowed_tiles_message_checks_run_in_order() {
        // (a) Claude Code logs on, no summary yet: the rollups' own line.
        let p = ai_state(false, false, true, Key::Absent, Key::Absent);
        for scope in ["anthropic", "claude", "item:claude-today"] {
            assert_eq!(
                text(&scoped(&p, scope)),
                crate::usage::LOADING_MESSAGE,
                "{scope}"
            );
        }
        // Not about the other vendor: its tiles say it is not set up.
        assert_eq!(text(&scoped(&p, "openai")), OPENAI_OFF);
        // (b) A part on with its key unread: the key leads. Unreadable is a
        // warning, not a message.
        let p = ai_state(true, false, false, Key::Unread, Key::Absent);
        assert_eq!(
            text(&scoped(&p, "anthropic")),
            crate::usage::PROVIDERS_LOADING_MESSAGE
        );
        assert_eq!(scoped(&p, "anthropic")["loading"], true);
        let p = ai_state(true, false, false, Key::Unreadable, Key::Absent);
        assert!(scoped(&p, "anthropic")["message"].is_null());
        // The plan row alone has no key, so it is not held by it.
        assert!(scoped(&p, "item:claude-plan")["message"].is_null());
        // (c) Logs on with no summary and the key unreadable: the rollups' line.
        let p = ai_state(false, false, true, Key::Unreadable, Key::Absent);
        assert_eq!(
            text(&scoped(&p, "anthropic")),
            crate::usage::LOADING_MESSAGE
        );
        let p = ai_state(false, false, true, Key::Unread, Key::Absent);
        assert_eq!(
            text(&scoped(&p, "anthropic")),
            crate::usage::PROVIDERS_LOADING_MESSAGE
        );
        // A scope naming nothing says nothing and waits for nothing, however
        // many keys are unread.
        let p = ai_state(false, false, false, Key::Unread, Key::Unread);
        let gone = scoped(&p, "item:gone");
        assert!(gone["message"].is_null());
        assert_eq!(gone["loading"], false);
    }

    /// A stored key whose read failed is a warning in the header, not the
    /// tile's message: the row is on and shows its figure.
    #[test]
    fn a_failed_read_of_a_stored_key_is_not_the_tiles_message() {
        use crate::plan_usage::PlanInputs;
        let mut state = crate::usage::UsageState::new();
        state.anthropic_api_mut().begin();
        state
            .anthropic_api_mut()
            .failed("Anthropic rejected it".into());
        state.openai_api_mut().unconfigure();
        let p = crate::usage::ai_usage_view(&state, PlanInputs::OFF, AI_NOW);
        let out = scoped(&p, "anthropic");
        assert!(out["message"].is_null());
        assert!(header_text(&out, "footer").contains("anthropic-api"));
    }

    /// The header figure on real narrowed payloads: only a tile that keeps the
    /// Claude Code row names it.
    #[test]
    fn a_narrowed_payload_names_claude_code_only_when_it_keeps_the_row() {
        let p = every_line_firing();
        for (scope, names) in [
            ("anthropic", true),
            ("claude", true),
            ("item:claude-today", true),
            ("openai", false),
            ("copilot", false),
            ("item:copilot-org", false),
            ("plans", false),
            ("item:claude-plan", false),
            ("item:anthropic-api", false),
            ("item:codex-plan", false),
            ("item:openai-api", false),
            ("item:gone", false),
        ] {
            let out = scoped(&p, scope);
            let (trailing, today) = (
                out["trailing"].as_str().unwrap(),
                out["claudeToday"].as_str().unwrap(),
            );
            if names {
                assert_eq!(trailing, "Claude Code \u{b7} 1.2M today", "{scope}");
                assert_eq!(today, "1.2M today", "{scope}");
            } else {
                assert_eq!((trailing, today), ("", ""), "{scope}");
            }
        }
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
            "copilot",
            "item:copilot-plan",
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
