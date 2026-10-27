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
/// two Runners tiles, and every old global set off its default.
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
