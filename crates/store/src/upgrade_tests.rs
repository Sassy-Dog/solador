//! The one-time upgrade copy of #647: stores written before the view settings
//! moved onto tiles. Each store is raw JSON on purpose: this crate can no longer
//! produce the old shape, and a migration tested against a value the current
//! code emits is tested against nothing.

use super::*;
use serde_json::{json, Value};
use tempfile::TempDir;

fn tile(id: &str, source: &str) -> Value {
    json!({
        "id": id, "source": source, "title": id, "scope": "all",
        "presentation": "summary", "width": "small"
    })
}

/// A pre-change store with an overview of two Machines tiles, a Repos tile and
/// two GitHub Runners tiles, and every old global set off its default.
fn old_store(extra_settings: Value, layout: Option<Value>, detail_views: Value) -> Value {
    let mut settings = json!({"core_row_span": 4, "row_gap_px": 7});
    settings
        .as_object_mut()
        .unwrap()
        .extend(extra_settings.as_object().unwrap().clone());
    let mut store = json!({
        "version": 3,
        "settings": settings,
        "dashboard": {
            "revision": 5,
            "tiles": [
                tile("m1", "hosts"), tile("m2", "hosts"),
                tile("r1", "ghWorkflows"),
                tile("u1", "ghRunners"), tile("u2", "ghRunners"),
                tile("s1", "services"),
            ],
            "detailViews": detail_views,
        }
    });
    if let Some(layout) = layout {
        store["layout"] = layout;
    }
    store
}

fn open(store: &Value) -> (TempDir, Store) {
    let dir = TempDir::new().expect("temp dir");
    fs::write(dir.path().join(STORE_FILE_NAME), store.to_string()).expect("write");
    let opened = Store::open_in(dir.path(), false).expect("open");
    (dir, opened)
}

fn tiles(store: &Store) -> &[DashboardTile] {
    &store.dashboard().expect("a dashboard").tiles
}

fn by_id<'a>(store: &'a Store, id: &str) -> &'a DashboardTile {
    tiles(store).iter().find(|t| t.id == id).expect("tile")
}

fn by_id_in<'a>(data: &'a StoreData, id: &str) -> &'a DashboardTile {
    data.dashboard
        .as_ref()
        .expect("a dashboard")
        .tiles
        .iter()
        .find(|t| t.id == id)
        .expect("tile")
}

/// Saves `store` and returns the file's text, parsed. Every dropped-value test
/// asserts on what the next save writes, not on what is in memory.
fn saved(dir: &TempDir, store: &Store) -> (String, Value) {
    store.save().expect("save");
    let text = fs::read_to_string(dir.path().join(STORE_FILE_NAME)).expect("read");
    let on_disk = serde_json::from_str(&text).expect("json");
    (text, on_disk)
}

fn tabs_layout() -> Value {
    json!([
        {"min_width": 0.0, "host_overflow": "tabs", "slots": [{"panel": "hosts", "span": "full"}]},
        {"min_width": 1400.0, "host_overflow": "stack", "slots": [{"panel": "hosts", "span": "half"}]}
    ])
}

#[test]
fn core_row_span_lands_on_every_machines_tile_and_only_those() {
    let (_dir, store) = open(&old_store(json!({}), None, json!({})));
    for id in ["m1", "m2"] {
        assert_eq!(by_id(&store, id).core_row_span, 4, "{id}");
    }
    for id in ["r1", "u1", "s1"] {
        assert_eq!(
            by_id(&store, id).core_row_span,
            settings::DEFAULT_CORE_ROW_SPAN,
            "{id} is not a Machines tile"
        );
    }
}

#[test]
fn row_gap_lands_on_every_tile() {
    let (_dir, store) = open(&old_store(json!({}), None, json!({})));
    assert_eq!(tiles(&store).len(), 6);
    for t in tiles(&store) {
        assert_eq!(t.row_gap_px, 7, "{}", t.id);
    }
}

#[test]
fn the_first_breakpoints_overflow_lands_on_every_machines_tile_only() {
    // The settings value disagrees, so only the layout can be where tabs came from.
    let (_dir, store) = open(&old_store(
        json!({"host_overflow_mode": "stack"}),
        Some(tabs_layout()),
        json!({}),
    ));
    for id in ["m1", "m2"] {
        assert_eq!(by_id(&store, id).host_overflow, HostOverflowMode::Tabs);
    }
    for id in ["r1", "u1", "s1"] {
        assert_eq!(by_id(&store, id).host_overflow, HostOverflowMode::Stack);
    }
}

#[test]
fn the_first_breakpoint_is_the_narrowest_not_the_first_listed() {
    let layout = json!([
        {"min_width": 1400.0, "host_overflow": "stack", "slots": []},
        {"min_width": 0.0, "host_overflow": "tabs", "slots": []}
    ]);
    let (_dir, store) = open(&old_store(json!({}), Some(layout), json!({})));
    assert_eq!(by_id(&store, "m1").host_overflow, HostOverflowMode::Tabs);
}

#[test]
fn without_a_stored_layout_overflow_comes_from_the_setting() {
    let (_dir, store) = open(&old_store(
        json!({"host_overflow_mode": "tabs"}),
        None,
        json!({}),
    ));
    assert_eq!(by_id(&store, "m2").host_overflow, HostOverflowMode::Tabs);
    // A bare slot array predates breakpoints and names no mode: same fallback.
    let bare = json!([{"panel": "hosts", "span": "full"}]);
    let (_dir, store) = open(&old_store(
        json!({"host_overflow_mode": "tabs"}),
        Some(bare),
        json!({}),
    ));
    assert_eq!(by_id(&store, "m2").host_overflow, HostOverflowMode::Tabs);
    // So does a profile whose mode was left empty.
    let empty = json!([{"min_width": 0.0, "host_overflow": "", "slots": []}]);
    let (_dir, store) = open(&old_store(
        json!({"host_overflow_mode": "tabs"}),
        Some(empty),
        json!({}),
    ));
    assert_eq!(by_id(&store, "m2").host_overflow, HostOverflowMode::Tabs);
}

#[test]
fn a_detail_view_lands_on_every_tile_of_its_source_only() {
    let (_dir, store) = open(&old_store(
        json!({}),
        None,
        json!({"ghWorkflows": "list", "hosts": "list"}),
    ));
    assert_eq!(by_id(&store, "r1").detail_view, "list");
    assert_eq!(by_id(&store, "m1").detail_view, "list");
    assert_eq!(by_id(&store, "m2").detail_view, "list");
    assert_eq!(
        by_id(&store, "s1").detail_view,
        "table",
        "no entry, no copy"
    );
    assert_eq!(
        by_id(&store, "r1").runner_full_view,
        "table",
        "only Runners have a Full view field"
    );
}

#[test]
fn a_runners_detail_view_lands_in_both_fields_of_every_runners_tile() {
    let (_dir, store) = open(&old_store(json!({}), None, json!({"ghRunners": "list"})));
    for id in ["u1", "u2"] {
        assert_eq!(by_id(&store, id).detail_view, "list", "{id} Detail");
        assert_eq!(by_id(&store, id).runner_full_view, "list", "{id} Full view");
    }
    assert_eq!(by_id(&store, "r1").detail_view, "table");
}

#[test]
fn a_store_with_neither_loads_with_defaults_and_drops_nothing() {
    let store_json = json!({
        "version": 3,
        "settings": {"github_org": "acme"},
        "dashboard": {"revision": 2, "tiles": [tile("m1", "hosts")]}
    });
    let (_dir, store) = open(&store_json);
    let t = by_id(&store, "m1");
    assert_eq!(t.core_row_span, settings::DEFAULT_CORE_ROW_SPAN);
    assert_eq!(t.row_gap_px, settings::DEFAULT_ROW_GAP_PX);
    assert_eq!(t.host_overflow, HostOverflowMode::Stack);
    assert_eq!(t.detail_view, "table");
    assert_eq!(store.settings().github_org, "acme");
    assert_eq!(store.dashboard().expect("dashboard").revision, 2);

    // And a store with no overview and nothing to carry stays without one.
    let (_dir, bare) = open(&json!({"version": 3}));
    assert_eq!(bare.dashboard(), None);
}

#[test]
fn values_with_no_overview_materialise_the_shipped_one_rather_than_vanish() {
    let (_dir, store) = open(&json!({
        "version": 3,
        "settings": {"core_row_span": 3, "row_gap_px": 9, "host_overflow_mode": "tabs"}
    }));
    let dashboard = store.dashboard().expect("the shipped overview");
    assert_eq!(dashboard.revision, 0);
    assert_eq!(
        dashboard.tiles,
        DashboardLayout::shipped()
            .tiles
            .iter()
            .cloned()
            .map(|mut t| {
                t.row_gap_px = 9;
                if t.source == "hosts" {
                    t.core_row_span = 3;
                    t.host_overflow = HostOverflowMode::Tabs;
                }
                t
            })
            .collect::<Vec<_>>()
    );
}

#[test]
fn default_valued_globals_with_no_overview_do_not_invent_one() {
    let (_dir, store) = open(&json!({
        "version": 3,
        "settings": {"core_row_span": 2, "row_gap_px": 0, "host_overflow_mode": "stack"}
    }));
    assert_eq!(store.dashboard(), None);
}

#[test]
fn a_save_after_loading_drops_the_retired_keys_and_a_reload_keeps_the_values() {
    let (dir, store) = open(&old_store(
        json!({"host_overflow_mode": "tabs"}),
        Some(tabs_layout()),
        json!({"ghRunners": "list"}),
    ));
    store.save().expect("save");
    let text = fs::read_to_string(dir.path().join(STORE_FILE_NAME)).expect("read");
    let on_disk: Value = serde_json::from_str(&text).expect("json");
    assert!(on_disk.get("layout").is_none(), "layout dropped: {text}");
    for key in ["core_row_span", "row_gap_px", "host_overflow_mode"] {
        assert!(on_disk["settings"].get(key).is_none(), "{key} dropped");
    }
    assert!(on_disk["dashboard"].get("detailViews").is_none());

    let reopened = Store::open_in(dir.path(), false).expect("reopen");
    assert_eq!(
        reopened.data(),
        store.data(),
        "a second load changes nothing"
    );
    assert_eq!(by_id(&reopened, "m1").core_row_span, 4);
    assert_eq!(by_id(&reopened, "u1").runner_full_view, "list");
}

#[test]
fn a_second_load_of_an_unsaved_store_carries_the_same_values() {
    let stored = old_store(json!({}), None, json!({"ghRunners": "list"}));
    let dir = TempDir::new().expect("temp dir");
    fs::write(dir.path().join(STORE_FILE_NAME), stored.to_string()).expect("write");
    let first = Store::open_in(dir.path(), false).expect("open");
    let second = Store::open_in(dir.path(), false).expect("open again");
    assert_eq!(first.data(), second.data());
    assert_eq!(
        fs::read_to_string(dir.path().join(STORE_FILE_NAME)).expect("read"),
        stored.to_string(),
        "loading never writes the store: the old keys leave only on the next save"
    );
}

#[test]
fn the_copy_is_spent_by_its_first_pass() {
    let stored = old_store(
        json!({"host_overflow_mode": "tabs"}),
        Some(tabs_layout()),
        json!({"ghRunners": "list"}),
    );
    let mut data: StoreData = serde_json::from_value(stored).expect("parse");
    data.carry_view_settings_onto_tiles();
    let after_first = data.clone();
    assert_eq!(
        by_id_in(&data, "m1").core_row_span,
        4,
        "the first pass carries"
    );
    // The pass consumes its input: a copy that left the legacy values in place
    // would still compare equal to itself after a second pass.
    assert_eq!(data.settings.legacy_core_row_span, None);
    assert_eq!(data.settings.legacy_row_gap_px, None);
    assert_eq!(data.settings.legacy_host_overflow_mode, None);
    assert_eq!(data.legacy_layout_overflow, None);
    assert!(data
        .dashboard
        .as_ref()
        .unwrap()
        .legacy_detail_views
        .is_empty());
    data.carry_view_settings_onto_tiles();
    assert_eq!(data, after_first, "a second pass has nothing left to carry");
}

#[test]
fn a_tile_value_set_after_the_upgrade_is_not_overwritten_by_a_later_load() {
    let (dir, mut store) = open(&old_store(json!({}), None, json!({})));
    let mut layout = store.dashboard().expect("dashboard").clone();
    layout.tiles[0].core_row_span = 1;
    store.save_dashboard(layout).expect("save");
    let reopened = Store::open_in(dir.path(), false).expect("reopen");
    assert_eq!(by_id(&reopened, "m1").core_row_span, 1);
}

#[test]
fn out_of_range_tile_values_are_clamped_on_read() {
    let mut t = tile("m1", "hosts");
    t["coreRowSpan"] = json!(9);
    t["rowGapPx"] = json!(99);
    let (_dir, store) = open(&json!({"version": 3, "dashboard": {"tiles": [t]}}));
    assert_eq!(by_id(&store, "m1").core_row_span, 4);
    assert_eq!(by_id(&store, "m1").row_gap_px, 16);
}

// A value with no tile to apply to is dropped, and its legacy key is not kept:
// Solador's Full view, the only other place it applied, is gone (#647). Each case
// below is one way to have no tile.

fn assert_no_legacy_keys(text: &str, on_disk: &Value) {
    assert!(on_disk.get("layout").is_none(), "layout kept: {text}");
    for key in ["core_row_span", "row_gap_px", "host_overflow_mode"] {
        assert!(on_disk["settings"].get(key).is_none(), "{key} kept: {text}");
    }
    assert!(
        on_disk["dashboard"].get("detailViews").is_none(),
        "detailViews kept: {text}"
    );
}

#[test]
fn core_rows_and_overflow_with_no_machines_tile_are_dropped_not_kept() {
    let mut stored = old_store(
        json!({"host_overflow_mode": "tabs"}),
        Some(tabs_layout()),
        json!({}),
    );
    stored["dashboard"]["tiles"] = json!([tile("r1", "ghWorkflows"), tile("s1", "services")]);
    let (dir, store) = open(&stored);

    let defaults = DashboardTile::new("x", "hosts", "x", "all");
    for t in tiles(&store) {
        assert_eq!(t.core_row_span, defaults.core_row_span, "{}", t.id);
        assert_eq!(t.host_overflow, defaults.host_overflow, "{}", t.id);
        assert_eq!(t.row_gap_px, 7, "row spacing still lands on every tile");
    }
    assert_eq!(tiles(&store).len(), 2, "no Machines tile is invented");

    let (text, on_disk) = saved(&dir, &store);
    assert_no_legacy_keys(&text, &on_disk);
    let reopened = Store::open_in(dir.path(), false).expect("reopen");
    assert_eq!(
        reopened.data(),
        store.data(),
        "nothing resurfaces on reload"
    );
}

#[test]
fn an_overview_with_no_tiles_drops_every_value_and_stays_empty() {
    let mut stored = old_store(
        json!({"host_overflow_mode": "tabs"}),
        Some(tabs_layout()),
        json!({"ghRunners": "list"}),
    );
    stored["dashboard"]["tiles"] = json!([]);
    let (dir, store) = open(&stored);

    assert!(
        tiles(&store).is_empty(),
        "an intentionally empty overview stays empty: the shipped one is not brought back"
    );
    let (text, on_disk) = saved(&dir, &store);
    assert_no_legacy_keys(&text, &on_disk);
    assert!(on_disk["dashboard"]["tiles"].as_array().unwrap().is_empty());
}

#[test]
fn a_detail_view_for_a_source_with_no_tile_is_dropped_not_kept() {
    let (dir, store) = open(&old_store(
        json!({}),
        None,
        json!({"containers": "list", "ghRunners": "list"}),
    ));
    let defaults = DashboardTile::new("x", "containers", "x", "all");
    for t in tiles(&store).iter().filter(|t| t.source != "ghRunners") {
        assert_eq!(t.detail_view, defaults.detail_view, "{}", t.id);
    }
    assert_eq!(
        by_id(&store, "u1").detail_view,
        "list",
        "a source with tiles still carries"
    );

    let (text, on_disk) = saved(&dir, &store);
    assert_no_legacy_keys(&text, &on_disk);
}

/// A Usage tile scoped to one of the rows #624 shipped keeps that row after
/// plans became one row each and the token windows folded into `claude-today`.
#[test]
fn a_usage_tile_scoped_to_a_retired_row_keeps_the_row_that_replaced_it() {
    let usage = |id: &str, scope: &str| {
        let mut t = tile(id, "claudeUsage");
        t["scope"] = json!(scope);
        t
    };
    let mut store = old_store(json!({}), None, json!({}));
    let tiles_json = store["dashboard"]["tiles"].as_array_mut().unwrap();
    tiles_json.extend([
        usage("c5", "item:claude-plan:CLAUDE PLAN (5H)"),
        usage("cw", "item:claude-plan:CLAUDE PLAN (WEEK · FABLE)"),
        usage("x5", "item:codex-plan:CODEX PLAN (5H)"),
        usage("t5", "item:claude-5H"),
        usage("tw", "item:claude-WEEK"),
        usage("today", "item:claude-today"),
        usage("neon", "item:neon:COMPUTE"),
        usage("plans", "plans"),
        // Another source's look-alike id is not a Usage row.
        {
            let mut t = tile("gh", "ghWorkflows");
            t["scope"] = json!("item:claude-plan:x");
            t
        },
    ]);
    let (_dir, opened) = open(&store);
    for (id, scope) in [
        ("c5", "item:claude-plan"),
        ("cw", "item:claude-plan"),
        ("x5", "item:codex-plan"),
        ("t5", "item:claude-today"),
        ("tw", "item:claude-today"),
        ("today", "item:claude-today"),
        ("neon", "item:neon:COMPUTE"),
        ("plans", "plans"),
        ("gh", "item:claude-plan:x"),
    ] {
        assert_eq!(by_id(&opened, id).scope, scope, "{id}");
    }

    let mut data = opened.data().clone();
    let once = data.clone();
    data.carry_usage_item_scopes_onto_plan_rows();
    assert_eq!(data, once, "a second pass changes nothing");
}

/// A saved tile of the one Usage source that held Claude, Codex and cloud
/// billing together.
fn old_usage(id: &str, scope: &str, title: &str) -> Value {
    let mut t = tile(id, "claudeUsage");
    t["scope"] = json!(scope);
    t["title"] = json!(title);
    t
}

fn store_of(tiles: Vec<Value>, settings: Value) -> Value {
    json!({
        "version": 3,
        "settings": settings,
        "dashboard": {"revision": 5, "tiles": tiles},
    })
}

const CODEX_ON: &str = r#"{"codex_plan_usage_enabled":true}"#;

fn settings(json_text: &str) -> Value {
    serde_json::from_str(json_text).expect("settings json")
}

/// `(id, source, scope, title)` of every tile, in overview order.
fn tiles_of(store: &Store) -> Vec<(String, String, String, String)> {
    store
        .dashboard()
        .expect("dashboard")
        .tiles
        .iter()
        .map(|t| {
            (
                t.id.clone(),
                t.source.clone(),
                t.scope.clone(),
                t.title.clone(),
            )
        })
        .collect()
}

fn row(id: &str, source: &str, scope: &str, title: &str) -> (String, String, String, String) {
    (id.into(), source.into(), scope.into(), title.into())
}

/// One saved scope, split with Codex's plan off and then on.
#[test]
fn every_saved_usage_scope_lands_on_its_new_source() {
    type Want = &'static [(&'static str, &'static str, &'static str, &'static str)];
    let cases: [(&str, Want, Want); 10] = [
        (
            "all",
            &[
                ("u", "usage", "all", "Usage"),
                ("u-anthropic", "anthropic", "all", "Anthropic"),
            ],
            &[
                ("u", "usage", "all", "Usage"),
                ("u-anthropic", "anthropic", "all", "Anthropic"),
                ("u-openai", "openai", "all", "OpenAI"),
            ],
        ),
        (
            "attention",
            &[
                ("u", "usage", "attention", "Usage"),
                ("u-anthropic", "anthropic", "attention", "Anthropic"),
            ],
            &[
                ("u", "usage", "attention", "Usage"),
                ("u-anthropic", "anthropic", "attention", "Anthropic"),
                ("u-openai", "openai", "attention", "OpenAI"),
            ],
        ),
        (
            "providers",
            &[("u", "usage", "all", "Usage")],
            &[("u", "usage", "all", "Usage")],
        ),
        (
            "item:neon:COMPUTE",
            &[("u", "usage", "item:neon:COMPUTE", "Usage")],
            &[("u", "usage", "item:neon:COMPUTE", "Usage")],
        ),
        (
            "item:sentry:EVENTS",
            &[("u", "usage", "item:sentry:EVENTS", "Usage")],
            &[("u", "usage", "item:sentry:EVENTS", "Usage")],
        ),
        (
            "item:gone",
            &[("u", "usage", "item:gone", "Usage")],
            &[("u", "usage", "item:gone", "Usage")],
        ),
        (
            "claude",
            &[("u", "anthropic", "claude", "Anthropic")],
            &[("u", "anthropic", "claude", "Anthropic")],
        ),
        (
            "plans",
            &[("u", "anthropic", "plans", "Anthropic")],
            // OpenAI has no `plans` scope, and its only row is the plan, so
            // `all` shows what the old scope showed of Codex.
            &[
                ("u", "anthropic", "plans", "Anthropic"),
                ("u-openai", "openai", "all", "OpenAI"),
            ],
        ),
        (
            "item:claude-plan",
            &[("u", "anthropic", "item:claude-plan", "Anthropic")],
            &[("u", "anthropic", "item:claude-plan", "Anthropic")],
        ),
        (
            "item:codex-plan",
            &[("u", "openai", "item:codex-plan", "OpenAI")],
            &[("u", "openai", "item:codex-plan", "OpenAI")],
        ),
    ];
    for (scope, off, on) in cases {
        for (codex, want) in [(json!({}), off), (settings(CODEX_ON), on)] {
            let (_dir, opened) = open(&store_of(vec![old_usage("u", scope, "Usage")], codex));
            let want: Vec<_> = want.iter().map(|w| row(w.0, w.1, w.2, w.3)).collect();
            assert_eq!(tiles_of(&opened), want, "{scope}");
        }
    }
}

#[test]
fn a_saved_scope_from_the_oldest_rows_still_reaches_its_new_source() {
    let (_dir, opened) = open(&store_of(
        vec![
            old_usage("p", "item:claude-plan:CLAUDE PLAN (5H)", "Usage"),
            old_usage("t", "item:claude-WEEK", "Usage"),
            old_usage("x", "item:codex-plan:CODEX PLAN (5H)", "Usage"),
        ],
        json!({}),
    ));
    assert_eq!(
        tiles_of(&opened),
        [
            row("p", "anthropic", "item:claude-plan", "Anthropic"),
            row("t", "anthropic", "item:claude-today", "Anthropic"),
            row("x", "openai", "item:codex-plan", "OpenAI"),
        ]
    );
}

#[test]
fn a_split_tile_keeps_its_place_and_its_additions_follow_it() {
    let (_dir, opened) = open(&store_of(
        vec![
            tile("m", "hosts"),
            old_usage("u", "all", "Usage"),
            tile("s", "services"),
        ],
        settings(CODEX_ON),
    ));
    let ids: Vec<String> = tiles_of(&opened).into_iter().map(|t| t.0).collect();
    assert_eq!(ids, ["m", "u", "u-anthropic", "u-openai", "s"]);
}

/// Hidden stays hidden, so unhiding gives back everything the one tile showed.
#[test]
fn an_added_tile_copies_every_field_of_the_tile_it_came_from() {
    let mut saved = old_usage("u", "all", "Usage");
    saved["hidden"] = json!(true);
    saved["width"] = json!("wide");
    saved["presentation"] = json!("detailed");
    saved["rowLimit"] = json!("10");
    saved["rowGapPx"] = json!(7);
    saved["detailView"] = json!("list");
    let (_dir, opened) = open(&store_of(vec![saved], json!({})));
    let original = by_id(&opened, "u");
    let added = by_id(&opened, "u-anthropic");
    assert!(added.hidden && original.hidden);
    assert_eq!(
        DashboardTile {
            id: original.id.clone(),
            source: original.source.clone(),
            title: original.title.clone(),
            ..added.clone()
        },
        *original,
    );
}

#[test]
fn each_saved_usage_tile_splits_on_its_own() {
    let (_dir, opened) = open(&store_of(
        vec![
            old_usage("a", "all", "Usage"),
            old_usage("b", "attention", "Usage"),
        ],
        json!({}),
    ));
    assert_eq!(
        tiles_of(&opened),
        [
            row("a", "usage", "all", "Usage"),
            row("a-anthropic", "anthropic", "all", "Anthropic"),
            row("b", "usage", "attention", "Usage"),
            row("b-anthropic", "anthropic", "attention", "Anthropic"),
        ]
    );
}

/// Only the default name follows the tile to its new source. A name the
/// operator chose stays, and an added tile is new, so it takes the source's.
#[test]
fn a_chosen_title_stays_on_a_moved_tile() {
    let (_dir, opened) = open(&store_of(
        vec![
            old_usage("c", "claude", "My Claude"),
            old_usage("k", "all", "Everything"),
        ],
        json!({}),
    ));
    assert_eq!(
        tiles_of(&opened),
        [
            row("c", "anthropic", "claude", "My Claude"),
            row("k", "usage", "all", "Everything"),
            row("k-anthropic", "anthropic", "all", "Anthropic"),
        ]
    );
}

/// `dashboard::validate` refuses an id over 80 characters or a duplicate, and
/// it runs on every save, so one bad id would refuse every later edit.
#[test]
fn an_added_id_stays_short_enough_and_unique() {
    let long = "x".repeat(80);
    let (_dir, opened) = open(&store_of(
        vec![
            old_usage(&long, "all", "Usage"),
            tile("u-anthropic", "hosts"),
            old_usage("u", "all", "Usage"),
        ],
        json!({}),
    ));
    let ids: Vec<String> = tiles_of(&opened).into_iter().map(|t| t.0).collect();
    assert_eq!(ids.len(), 5);
    assert!(ids.iter().all(|id| id.len() <= 80), "{ids:?}");
    let distinct: std::collections::BTreeSet<&String> = ids.iter().collect();
    assert_eq!(distinct.len(), ids.len(), "{ids:?}");
    assert!(ids[1].ends_with("-anthropic"), "{ids:?}");
    assert!(ids[4].starts_with("u-anthropic"), "{ids:?}");
}

/// Dropping a tile to fit would lose something it showed.
#[test]
fn a_full_dashboard_still_splits() {
    let mut tiles: Vec<Value> = (0..47).map(|i| tile(&format!("t{i}"), "hosts")).collect();
    tiles.push(old_usage("u", "all", "Usage"));
    let (_dir, opened) = open(&store_of(tiles, settings(CODEX_ON)));
    assert_eq!(opened.dashboard().expect("dashboard").tiles.len(), 50);
}

#[test]
fn a_store_with_no_saved_dashboard_has_nothing_to_split() {
    let (_dir, opened) = open(&json!({"version": 3, "settings": {}}));
    assert!(opened.dashboard().is_none());
}

/// Nothing the split writes matches what it reads, which is what makes a
/// removed tile stay removed. A store marker would not survive an older build's
/// save, and a version bump would stop older builds opening the file at all.
#[test]
fn the_split_runs_once_so_a_removed_tile_stays_removed() {
    let (dir, mut opened) = open(&store_of(
        vec![old_usage("u", "all", "Usage")],
        settings(CODEX_ON),
    ));
    let once = opened.data().clone();
    let mut again = once.clone();
    again.split_usage_tiles();
    assert_eq!(again, once, "a second pass changes nothing");

    let mut layout = opened.dashboard().expect("dashboard").clone();
    layout.tiles.retain(|t| t.source != "anthropic");
    opened.save_dashboard(layout).expect("save");
    let reopened = Store::open_in(dir.path(), false).expect("reopen");
    let sources: Vec<String> = tiles_of(&reopened).into_iter().map(|t| t.1).collect();
    assert_eq!(sources, ["usage", "openai"]);
}
