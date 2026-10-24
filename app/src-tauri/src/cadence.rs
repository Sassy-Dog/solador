//! The poll cadence controls in a tile's Configure (#614).
//!
//! A cadence belongs to the source, not to a tile, so the control is shared by
//! every tile of that source and the Full view. It says so, and saves on the
//! click through `panel_interval_save` / `panel_interval_clear`, outside the
//! tile's Apply and the dashboard's undo history. The floor and its reason are
//! Rust's sentences; the webview paints them and decides nothing.
//!
//! A source carries a **list** of them, `cadences`, because one tile can be fed
//! by more than one poll: the Usage tile shows the metered providers and the
//! plans' limit windows, which are separate requests to separate vendors with
//! separate floors. Folding them into one number would make one vendor's floor
//! the other's.
//!
//! There is deliberately no control for the 1 Hz host poll or the 10s
//! agent-health poll. Neither has a [`PanelInterval`] to name it, because the
//! first is the sparklines' time axis and the second is liveness rather than a
//! panel.

use serde_json::{json, Value};
use store::settings::{interval_label, PanelInterval, Settings};

/// The dashboard source a cadence feeds. Exhaustive, so a new settable panel
/// cannot be added without stating which tile's Configure hosts its control.
pub fn source_id(panel: PanelInterval) -> &'static str {
    match panel {
        PanelInterval::Containers => "containers",
        PanelInterval::UsageProviders => "claudeUsage",
        PanelInterval::AzureCost => "azureCost",
        PanelInterval::Crons => "sentryCrons",
        PanelInterval::PlanUsage => "claudeUsage",
    }
}

/// Attaches each source's cadence sections to its payload (matched by `id`),
/// as `cadences` in [`PanelInterval::ALL`]'s order. A source no cadence feeds
/// gets no key at all. Used by both `dashboard_view` and the `--dump-dashboard`
/// fixture, so the two cannot differ.
pub fn attach(payloads: &mut [Value], settings: &Settings) {
    for panel in PanelInterval::ALL {
        let id = source_id(panel);
        if let Some(payload) = payloads.iter_mut().find(|p| p["id"] == id) {
            let section = section(settings, panel, source_title(id));
            match payload["cadences"].as_array_mut() {
                Some(list) => list.push(section),
                None => payload["cadences"] = json!([section]),
            }
        }
    }
}

fn source_title(id: &str) -> &'static str {
    crate::dashboard::SOURCES
        .iter()
        .find(|(source, _)| *source == id)
        .map_or("", |(_, title)| title)
}

/// One panel's cadence control.
///
/// `status` distinguishes the two states a bare number cannot: an override that
/// happens to equal today's constant, and a panel nobody has ever configured.
/// They render identically in the input and behave differently forever after —
/// the second follows the constant if it moves, the first pins the panel to
/// today's number — so the control says which one it is.
pub fn section(settings: &Settings, panel: PanelInterval, title: &str) -> Value {
    let spec = panel.spec();
    let secs = settings.panel_interval_secs(panel);
    let configured = settings.panel_interval_is_configured(panel);
    json!({
        "id": spec.key,
        "heading": "Poll cadence",
        "shared": format!("Shared \u{b7} saved immediately \u{b7} applies to every {title} tile and the Full view."),
        "label": format!("{} (seconds)", spec.label),
        "value": secs,
        // The input's own floor, so the browser marks a refusable value. It is a
        // courtesy and not the enforcement point: `PanelInterval::check_secs`
        // is, and it does not trust this number.
        "min": spec.floor_secs,
        "configured": configured,
        "status": if configured {
            format!("Set to {}. The default is {}.", interval_label(secs), interval_label(spec.default_secs))
        } else {
            format!("Using the default, {}.", interval_label(secs))
        },
        "help": format!(
            "No faster than {} \u{2014} {}. A cadence on a metered source is a spend decision, so a value below the floor is refused rather than rounded up.",
            interval_label(spec.floor_secs),
            spec.floor_reason,
        ),
        "applyLabel": "Apply cadence",
        "resetLabel": "Use default",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payloads() -> Vec<Value> {
        crate::dashboard::SOURCES
            .iter()
            .map(|(id, _)| json!({ "id": id }))
            .collect()
    }

    /// Every settable cadence lands on exactly one source, and no other source
    /// (the host poll above all) is offered one.
    #[test]
    fn every_settable_cadence_is_attached_to_its_source_and_nothing_else_is() {
        let mut all = payloads();
        attach(&mut all, &Settings::default());
        let mut got: Vec<(&str, &str)> = all
            .iter()
            .flat_map(|p| {
                let source = p["id"].as_str().unwrap();
                p["cadences"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(move |c| (source, c["id"].as_str().unwrap()))
            })
            .collect();
        let mut wanted: Vec<(&str, &str)> = PanelInterval::ALL
            .into_iter()
            .map(|panel| (source_id(panel), panel.spec().key))
            .collect();
        wanted.sort_unstable();
        got.sort_unstable();
        assert_eq!(got, wanted);
        assert!(
            all.iter()
                .filter(|p| p["cadences"].is_null())
                .any(|p| p["id"] == "hosts"),
            "the host poll must not be settable"
        );
    }

    /// The Usage tile is fed by two polls, so its Configure carries both, the
    /// metered providers first, in [`PanelInterval::ALL`]'s order. Neither
    /// overwrites the other, which is what a single `cadence` key did.
    #[test]
    fn the_usage_tile_carries_the_providers_and_the_plans_cadences() {
        let mut all = payloads();
        attach(&mut all, &Settings::default());
        let usage = all.iter().find(|p| p["id"] == "claudeUsage").unwrap();
        let ids: Vec<&str> = usage["cadences"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["usage_providers", "plan_usage"]);
    }

    /// An unconfigured store shows today's constant and says it is the default,
    /// with the floor and its reason before anybody trips over it.
    #[test]
    fn an_unconfigured_cadence_shows_the_default_the_floor_and_the_reason() {
        for panel in PanelInterval::ALL {
            let spec = panel.spec();
            let s = section(&Settings::default(), panel, "Tile");
            assert_eq!(s["value"], json!(spec.default_secs), "{panel:?}");
            assert_eq!(s["configured"], json!(false), "{panel:?}");
            assert_eq!(s["min"], json!(spec.floor_secs), "{panel:?}");
            assert_eq!(
                s["status"],
                json!(format!(
                    "Using the default, {}.",
                    interval_label(spec.default_secs)
                ))
            );
            let help = s["help"].as_str().unwrap();
            assert!(help.contains(&interval_label(spec.floor_secs)), "{help}");
            assert!(help.contains(spec.floor_reason), "{help}");
            assert!(s["shared"].as_str().unwrap().contains("every Tile tile"));
        }
    }

    /// An explicit choice that equals the default is still a choice.
    #[test]
    fn choosing_the_default_value_is_not_the_same_as_never_choosing() {
        let mut settings = Settings::default();
        let spec = PanelInterval::Containers.spec();
        settings.set_panel_interval_secs(PanelInterval::Containers, spec.default_secs);
        let s = section(&settings, PanelInterval::Containers, "Containers / VMs");
        assert_eq!(s["configured"], json!(true));
        assert_ne!(
            s["status"],
            json!(format!(
                "Using the default, {}.",
                interval_label(spec.default_secs)
            ))
        );
    }

    #[test]
    fn a_configured_cadence_says_it_is_a_choice_and_names_the_default() {
        let mut settings = Settings::default();
        settings.set_panel_interval_secs(PanelInterval::AzureCost, 6 * 3600);
        let s = section(&settings, PanelInterval::AzureCost, "Azure Cost");
        assert_eq!(s["id"], json!("azure_cost"));
        assert_eq!(s["value"], json!(6 * 3600));
        assert_eq!(
            s["status"],
            json!("Set to 6 hours. The default is 4 hours.")
        );
    }

    /// A cadence hand-edited below the floor is shown floored: the loop polls
    /// at the floor, and a control showing the file's value would describe a
    /// cadence nothing runs on.
    #[test]
    fn a_hand_edited_below_floor_cadence_renders_as_the_floor() {
        let mut settings = Settings::default();
        settings.panel_intervals.insert("azure_cost".to_owned(), 30);
        let s = section(&settings, PanelInterval::AzureCost, "Azure Cost");
        assert_eq!(s["value"], json!(3600));
        assert_eq!(s["configured"], json!(true));
    }
}
