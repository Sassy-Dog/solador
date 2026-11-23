//! Where an attention chip leads (`chip_target`): one visible tile that keeps
//! every row in trouble, or a trouble panel. Never a hidden tile.

use super::*;

fn tile(id: &str, source: &str, scope: &str) -> DashboardTile {
    let mut t = default_layout()
        .tiles
        .into_iter()
        .find(|t| t.source == source)
        .unwrap();
    t.id = id.into();
    t.title = id.into();
    t.scope = scope.into();
    t
}

/// `b` is failing; `a` and `c` are healthy.
fn repos_payload() -> Value {
    let repo = |name: &str, status: &str| {
        json!({"id":"ghWorkflows","repo":format!("o/{name}"),"name":name,"status":status,
            "statusLabel":status,"attention":status != "healthy","cells":[]})
    };
    json!({"id":"ghWorkflows","columns":[],"rows":[repo("a","healthy"),repo("b","failing"),repo("c","healthy")]})
}

fn repos() -> Value {
    source_view("ghWorkflows", "GitHub Repos", &repos_payload())
}

#[test]
fn one_tile_that_keeps_every_row_in_trouble_is_the_target() {
    let all = tile("all", "ghWorkflows", "all");
    assert_eq!(chip_target(&repos(), &[&all]), json!({"tile":"all"}));
    let attention = tile("att", "ghWorkflows", "attention");
    assert_eq!(chip_target(&repos(), &[&attention]), json!({"tile":"att"}));
}

#[test]
fn a_tile_whose_scope_misses_a_row_in_trouble_is_not_the_target() {
    let healthy = tile("healthy", "ghWorkflows", "healthy");
    let t = chip_target(&repos(), &[&healthy]);
    assert!(t["tile"].is_null(), "{t}");
    let trouble = &t["trouble"];
    assert_eq!(trouble["intro"], "Healthy does not show every item.");
    assert_eq!(trouble["tiles"][0]["id"], "healthy");
    assert!(trouble["add"].is_null());
    // The repository selection narrows what the tile keeps, too.
    let mut picked = tile("picked", "ghWorkflows", "all");
    picked.selected_repos = vec!["o/a".into()];
    assert!(chip_target(&repos(), &[&picked])["tile"].is_null());
}

#[test]
fn a_source_with_no_tile_gets_the_panel_with_add_tile_and_its_items() {
    let t = chip_target(&repos(), &[]);
    let trouble = &t["trouble"];
    assert_eq!(trouble["title"], "GitHub Repos needs attention");
    assert_eq!(trouble["add"]["source"], "ghWorkflows");
    assert_eq!(trouble["add"]["label"], "Add tile");
    assert!(list(trouble, "tiles").is_empty());
    let items = list(trouble, "items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["label"], "b");
}

#[test]
fn several_tiles_that_all_keep_the_trouble_are_each_linked() {
    let one = tile("one", "ghWorkflows", "all");
    let two = tile("two", "ghWorkflows", "attention");
    let t = chip_target(&repos(), &[&one, &two]);
    let ids: Vec<_> = list(&t["trouble"], "tiles")
        .iter()
        .map(|l| string(l, "id"))
        .collect();
    assert_eq!(ids, ["one", "two"]);
    assert!(t["trouble"]["add"].is_null());
}

#[test]
fn the_one_tile_that_keeps_the_trouble_wins_over_a_tile_that_misses_it() {
    let one = tile("one", "ghWorkflows", "all");
    let healthy = tile("healthy", "ghWorkflows", "healthy");
    assert_eq!(
        chip_target(&repos(), &[&one, &healthy]),
        json!({"tile":"one"})
    );
}

#[test]
fn warnings_alone_are_listed_and_any_single_tile_shows_them() {
    let source = source_view(
        "ghWorkflows",
        "GitHub Repos",
        &json!({"rows":[],"footer":{"text":"Rate limited","color":color::hex(color::RED)}}),
    );
    let healthy = tile("healthy", "ghWorkflows", "healthy");
    assert_eq!(chip_target(&source, &[&healthy]), json!({"tile":"healthy"}));
    let none = chip_target(&source, &[]);
    assert_eq!(none["trouble"]["items"][0]["label"], "Rate limited");
}

#[test]
fn view_never_targets_a_hidden_tile_and_the_chip_still_stands() {
    let payload = repos_payload();
    let mut layout = default_layout();
    layout.tiles.retain(|t| t.source == "ghWorkflows");
    let id = layout.tiles[0].id.clone();
    let chip = |layout: &DashboardLayout| {
        list(&view(layout, std::slice::from_ref(&payload)), "attention")
            .iter()
            .find(|a| a["source"] == "ghWorkflows")
            .cloned()
            .unwrap()
    };
    assert_eq!(chip(&layout)["target"], json!({"tile":id}));
    layout.tiles[0].hidden = true;
    let hidden = chip(&layout);
    assert!(hidden["target"]["tile"].is_null(), "{hidden}");
    assert_eq!(hidden["target"]["trouble"]["add"]["source"], "ghWorkflows");
    assert!(list(&hidden["target"]["trouble"], "tiles").is_empty());
}

/// A narrowed tile's Detail is source-level (it paints the source's whole
/// `warnings`), so a chip may point at a narrowed tile whose card shows nothing.
/// An `anthropic` tile with only the Codex plan's clock late: no `\u{26a0}`, an
/// empty card, the chip targets the tile, and the source's warnings (what Detail
/// paints) carry the codex-plan clock.
#[test]
fn a_narrowed_tile_with_an_empty_card_is_still_the_chips_target() {
    use crate::usage::{fixture_state, Fixture};
    let now = 1_700_000_000;
    let mut state = fixture_state(Fixture::Measured, now, now);
    // Late, but short of the warning's window (twice the cadence).
    state
        .plan_mut(subscription::Provider::Codex)
        .state
        .last_updated = Some(now - 1_200);
    let payload = crate::usage::ai_usage_view(&state, crate::usage::FIXTURE_PLANS, now);
    assert!(payload["footer"].is_null(), "no \u{26a0} line");
    let source = source_view("aiUsage", "AI Usage", &payload);
    let anthropic = DashboardTile::new("anth", "aiUsage", "Anthropic", "anthropic");

    // The card shows nothing of the Codex plan's clock.
    let card = tile_view(&anthropic, &source);
    assert!(list(&card, "warnings").is_empty(), "the card is empty");
    // The chip still points at the tile.
    assert_eq!(chip_target(&source, &[&anthropic]), json!({"tile": "anth"}));
    // What Detail paints, the source's warnings, says why.
    let detail: Vec<&str> = list(&source, "warnings")
        .iter()
        .map(|w| string(w, "text"))
        .collect();
    assert!(
        detail.iter().any(|t| t.contains("codex-plan")),
        "the source's warnings carry the codex-plan clock"
    );
}
