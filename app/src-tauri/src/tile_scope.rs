//! Narrows one source's panel payload to what a tile's scope keeps, so a tile's
//! Full view shows the tile's rows and nothing else.
//!
//! The kept ids come from `dashboard::scoped_row_ids`, which derives them from
//! `dashboard::source_rows`; each filter here rebuilds the same id `source_rows`
//! gives that row. Only the rows are filtered. Every other key (`trailing`,
//! `stats`, `chips`, `footer`, `freshness`, `message`, `health`, section header
//! counts) is the source-wide total the scope note refers to and is left as is.
//!
//! **Nothing here writes through `IndexMut`.** `value["key"]` on a `&mut Value`
//! inserts a missing key and turns a `null` into an object, so a runtime whose
//! `agents` was `null` came back as `{"rows": null}` and the OpenClaw panel
//! painted empty AGENTS and CHANNELS sections under its pairing banner. Every
//! write goes through [`field_mut`], and a key that is absent or `null` stays
//! exactly as it was.

use crate::dashboard::{scoped_empty, scoped_row_ids};
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
    // Hosts are cards, filtered (and stamped) by `cockpit_for_tile`.
    let empty = scoped_empty(tile, &payload).filter(|_| tile.source != "hosts");
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
            ("claudeUsage", "plans"),
            ("azureCost", "item:budget"),
            ("openclawAgents", "attention"),
        ] {
            // Rows to filter are absent, so the scope keeps nothing. A scope on
            // one `item:` is the tile's own claim and says so (`tileEmpty`); any
            // other scope leaves the source's setup or error state to its
            // panel, and the payload comes back exactly as it went in, with no
            // `tileEmpty` key (#656). Both are compared whole.
            let sentence = scope.starts_with("item:").then(|| {
                json!("This resource has no current reading. Review its connection or choose another scope.")
            });
            let expect = |p: &Value| {
                let mut p = p.clone();
                if let Some(sentence) = &sentence {
                    p.as_object_mut()
                        .unwrap()
                        .insert("tileEmpty".into(), sentence.clone());
                }
                p
            };
            let absent = json!({"message":"Add a token in Settings","footer":{"text":"f"}});
            let scoped = scope_payload(&tile(source, scope), absent.clone());
            assert_eq!(scoped, expect(&absent), "{source} {scope}: absent");
            let null = json!({"rows":null,"sections":null,"runtimes":null,"windows":null,
                "plans":null,"providers":null,"stats":null});
            let scoped = scope_payload(&tile(source, scope), null.clone());
            assert_eq!(scoped, expect(&null), "{source} {scope}: null");
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
                "claudeUsage",
                json!({"trailing":"1M","projects":[{"name":"p"}],
                    "windows":[{"label":"Today"},{"label":"7 days"}],
                    "plans":[{"id":"claude","rows":[{"label":"5H"},{"label":"7D"}]}],
                    "providers":[
                        {"id":"neon","rows":[{"label":"Compute"}]},
                        {"id":"sentry","rows":[{"label":"Compute"}]}]}),
                Box::new(move |v| item(v, "windows") + nested(v, "plans") + nested(v, "providers")),
                // The trailing total is a line of its own, not an entry in any
                // list this module filters.
                &["claude-today"],
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
