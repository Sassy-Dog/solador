//! Applies the current machine preferences to cached readings on every render.

use serde_json::{json, Value};
use store::machine_alerts::{MachineAlerts, MachineThresholds};
use viewmodel::color::{self, MemoryPressure};

pub fn apply(payload: &mut Value, alerts: &MachineAlerts) {
    let Some(hosts) = payload["hosts"].as_array_mut() else {
        return;
    };
    for host in hosts {
        // Pending/unreachable cards carry no readings. Their connection owns the alert.
        if host.get("cpuFraction").is_none() {
            continue;
        }
        let limits = alerts.for_host(host["id"].as_str().unwrap_or_default());
        for (fraction, tint, warning, critical) in [
            (
                "cpuFraction",
                "cpuValueColor",
                limits.cpu_warning,
                limits.cpu_critical,
            ),
            (
                "memFraction",
                "memValueColor",
                limits.ram_warning,
                limits.ram_critical,
            ),
        ] {
            // Where the host reports the kernel's memory-pressure level (#544),
            // it decides the RAM colour and the RAM thresholds do not apply:
            // a Mac keeps RAM full on purpose, so `used ÷ total` is not the
            // question. The meter's fill is untouched.
            if fraction == "memFraction" {
                if let Some(level) = host["memPressureLevel"]
                    .as_i64()
                    .and_then(MemoryPressure::from_wire)
                {
                    host[tint] = json!(color::hex(level.color()));
                    continue;
                }
            }
            let value = host[fraction].as_f64();
            host[tint] = json!(color::hex(value.map_or(color::MUTED, |v| {
                color::usage_fraction_color(v, warning, critical)
            })));
        }
    }
}

pub fn save(
    store: &mut store::Store,
    host_id: Option<&str>,
    input: Option<&Value>,
) -> Result<(), String> {
    if let Some(id) = host_id.filter(|id| *id != "local") {
        let found = uuid::Uuid::parse_str(id).ok().and_then(|id| store.host(id));
        if found.is_none() {
            return Err("Unknown machine. Reopen Settings and try again.".into());
        }
    }
    let thresholds = input.map(MachineThresholds::parse).transpose()?;
    let previous = store.settings().machine_alerts.clone();
    let alerts = &mut store.settings_mut().machine_alerts;
    match (host_id, thresholds) {
        (None, value) => alerts.defaults = value.unwrap_or_default(),
        (Some(id), Some(value)) => {
            alerts.overrides.insert(id.to_owned(), value);
        }
        (Some(id), None) => {
            alerts.overrides.remove(id);
        }
    }
    if let Err(error) = store.save() {
        store.settings_mut().machine_alerts = previous;
        return Err(error.to_string());
    }
    Ok(())
}

pub fn settings(alerts: &MachineAlerts, host_id: Option<&str>) -> Value {
    let values = host_id.map_or(alerts.defaults, |id| alerts.for_host(id));
    json!({
        "heading": "Machine alerts",
        "help": if host_id.is_none() {
            "Warning (amber) and critical (red) percentages for overall CPU and used RAM. Values at or above either threshold need attention. A machine that reports the kernel's memory-pressure level (a Mac) is coloured by that level instead, so the RAM thresholds apply only to machines that do not report one. Changes apply on the next refresh. Override these in Connections for individual machines."
        } else {
            "Choose this machine's CPU and RAM limits, or follow the shared defaults in Preferences. Values at or above a warning or critical threshold need attention. The RAM limits apply only if this machine does not report memory pressure; a Mac does, and is coloured by that level."
        },
        "rangeHelp": "Use whole percentages from 1 to 100. Each warning must be lower than its critical threshold.",
        "hostId": host_id,
        "shared": host_id.is_none(),
        "inherited": host_id.is_some_and(|id| !alerts.overrides.contains_key(id)),
        "defaults": alerts.defaults,
        "useDefaultsLabel": "Use shared defaults",
        "saveLabel": "Apply thresholds",
        "resetLabel": "Reset defaults",
        "min": 1, "max": 100,
        "fields": [
            {"id":"cpuWarning", "label":"CPU warning (%)", "value":values.cpu_warning},
            {"id":"cpuCritical", "label":"CPU critical (%)", "value":values.cpu_critical},
            {"id":"ramWarning", "label":"RAM warning (%)", "value":values.ram_warning},
            {"id":"ramCritical", "label":"RAM critical (%)", "value":values.ram_critical},
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use store::machine_alerts::{MachineAlerts, MachineThresholds};
    use viewmodel::color;

    fn readings() -> serde_json::Value {
        let mut payload = crate::dump_cockpit(1400.0, 1, store::HostOverflowMode::Stack);
        payload["id"] = json!("hosts");
        for host in payload["hosts"].as_array_mut().unwrap() {
            host["cpuFraction"] = json!(0.45);
            host["memFraction"] = json!(0.83);
            host["volumes"] = json!([]);
            host["thermalColor"] = json!(color::hex(color::GREEN));
        }
        payload
    }

    #[test]
    fn settings_edits_persist_inherit_and_reject_without_changing_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store::Store::open_in(dir.path(), false).unwrap();
        let host = store::Host::new("mac", "mac.local");
        let id = host.id.to_string();
        store.upsert_host(host);
        let limits = json!({"cpuWarning":80,"cpuCritical":95,"ramWarning":90,"ramCritical":98});
        save(&mut store, None, Some(&limits)).unwrap();
        let local = json!({"cpuWarning":70,"cpuCritical":90,"ramWarning":85,"ramCritical":95});
        save(&mut store, Some("local"), Some(&local)).unwrap();
        save(&mut store, Some(&id), Some(&local)).unwrap();
        let reopened = store::Store::open_in(dir.path(), false).unwrap();
        assert_eq!(
            reopened
                .settings()
                .machine_alerts
                .for_host("other")
                .ram_warning,
            90
        );
        assert_eq!(
            reopened
                .settings()
                .machine_alerts
                .for_host("local")
                .ram_warning,
            85
        );
        assert_eq!(
            reopened.settings().machine_alerts.for_host(&id).ram_warning,
            85
        );
        let before = store.settings().machine_alerts.clone();
        assert!(save(&mut store, Some("missing-host"), Some(&limits)).is_err());
        assert!(save(&mut store, None, Some(&json!({}))).is_err());
        assert_eq!(store.settings().machine_alerts, before);
        save(&mut store, Some(&id), None).unwrap();
        assert_eq!(
            store.settings().machine_alerts.for_host(&id).ram_warning,
            90
        );
        save(&mut store, None, None).unwrap();
        assert_eq!(
            store.settings().machine_alerts.for_host(&id).ram_warning,
            70
        );
        assert_eq!(
            store
                .settings()
                .machine_alerts
                .for_host("local")
                .ram_warning,
            85
        );
    }

    #[test]
    fn a_failed_save_restores_the_effective_thresholds() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store::Store::open_in(dir.path(), false).unwrap();
        // A directory where the destination file should be makes the atomic rename fail.
        std::fs::remove_file(dir.path().join("store.json")).unwrap();
        std::fs::create_dir(dir.path().join("store.json")).unwrap();
        let limits = json!({"cpuWarning":80,"cpuCritical":95,"ramWarning":90,"ramCritical":98});
        assert!(save(&mut store, None, Some(&limits)).is_err());
        assert_eq!(store.settings().machine_alerts, MachineAlerts::default());
    }

    fn machine_rows(payload: serde_json::Value) -> Vec<serde_json::Value> {
        let view = crate::dashboard::view(&crate::dashboard::default_layout(), &[payload]);
        view["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == "hosts")
            .unwrap()["rows"]
            .as_array()
            .unwrap()
            .clone()
    }

    #[test]
    fn a_ram_override_clears_attention_and_only_changes_the_named_machine() {
        let mut payload = readings();
        let mut alerts = MachineAlerts::default();
        apply(&mut payload, &alerts);
        assert!(machine_rows(payload.clone())
            .iter()
            .all(|r| r["attention"] == true));
        alerts.overrides.insert(
            "local".into(),
            MachineThresholds {
                ram_warning: 90,
                ram_critical: 98,
                ..MachineThresholds::default()
            },
        );
        apply(&mut payload, &alerts);
        let rows = machine_rows(payload);
        assert_eq!(rows[0]["attention"], false);
        assert_eq!(rows[0]["value"], "Connected");
        assert_eq!(rows[0]["metrics"][1]["color"], color::hex(color::GREEN));
        assert_eq!(rows[1]["attention"], true);
        assert_eq!(rows[1]["value"], "Connected");
        assert_eq!(rows[1]["color"], color::hex(color::AMBER));
    }

    #[test]
    fn custom_limits_are_inclusive_and_unknown_metrics_remain_muted() {
        let mut payload = readings();
        let mut alerts = MachineAlerts::default();
        alerts.defaults.cpu_warning = 80;
        alerts.defaults.cpu_critical = 95;
        for (fraction, expected) in [
            (Some(0.799), color::GREEN),
            (Some(0.8), color::AMBER),
            (Some(0.95), color::RED),
            (None, color::MUTED),
        ] {
            payload["hosts"][0]["cpuFraction"] = json!(fraction);
            apply(&mut payload, &alerts);
            assert_eq!(payload["hosts"][0]["cpuValueColor"], color::hex(expected));
        }
    }

    /// #544: on a host reporting the kernel's level, the level colours the RAM
    /// meter and raises (or does not raise) attention, whatever `used ÷ total`
    /// and the RAM thresholds say; the fill stays the fraction.
    #[test]
    fn a_reported_memory_pressure_level_replaces_the_ram_thresholds() {
        let alerts = MachineAlerts::default();
        for (level, expected, attention) in [
            (json!(0), color::GREEN, false),
            (json!(1), color::AMBER, true),
            (json!(2), color::RED, true),
        ] {
            let mut payload = readings();
            payload["hosts"][0]["memPressureLevel"] = level.clone();
            // 83% used is amber on the thresholds alone; at 20% used a
            // critical level must still be red.
            if level == json!(2) {
                payload["hosts"][0]["memFraction"] = json!(0.2);
            }
            apply(&mut payload, &alerts);
            assert_eq!(payload["hosts"][0]["memValueColor"], color::hex(expected));
            let rows = machine_rows(payload);
            assert_eq!(rows[0]["attention"], attention, "level {level}");
            assert_eq!(rows[0]["metrics"][1]["color"], color::hex(expected));
        }
    }

    /// The operator's RAM limits do not matter on a host that reports a level:
    /// strict limits cannot redden a normal Mac, lax limits cannot hide a
    /// critical one.
    #[test]
    fn operator_ram_limits_do_not_override_a_reported_level() {
        let mut strict = MachineAlerts::default();
        strict.defaults.ram_warning = 10;
        strict.defaults.ram_critical = 20;
        let mut payload = readings();
        payload["hosts"][0]["memPressureLevel"] = json!(0);
        apply(&mut payload, &strict);
        assert_eq!(
            payload["hosts"][0]["memValueColor"],
            color::hex(color::GREEN)
        );
        assert_eq!(machine_rows(payload)[0]["attention"], false);

        let mut lax = MachineAlerts::default();
        lax.defaults.ram_warning = 99;
        lax.defaults.ram_critical = 100;
        let mut payload = readings();
        payload["hosts"][0]["memPressureLevel"] = json!(2);
        apply(&mut payload, &lax);
        assert_eq!(payload["hosts"][0]["memValueColor"], color::hex(color::RED));
        assert_eq!(machine_rows(payload)[0]["attention"], true);
    }

    #[test]
    fn an_absent_or_out_of_contract_level_leaves_the_thresholds_in_charge() {
        let alerts = MachineAlerts::default();
        for level in [json!(null), json!(3), json!("normal")] {
            let mut payload = readings();
            payload["hosts"][0]["memPressureLevel"] = level;
            apply(&mut payload, &alerts);
            assert_eq!(
                payload["hosts"][0]["memValueColor"],
                color::hex(color::AMBER)
            );
        }
    }

    #[test]
    fn higher_ram_limits_do_not_hide_disk_thermal_or_connection_problems() {
        let mut alerts = MachineAlerts::default();
        alerts.defaults.ram_warning = 90;
        alerts.defaults.ram_critical = 98;
        for problem in ["disk", "thermal", "connection"] {
            let mut payload = readings();
            match problem {
                "disk" => {
                    payload["hosts"][0]["volumes"] =
                        json!([{"mount":"/","fraction":0.96,"tint":color::hex(color::RED)}])
                }
                "thermal" => payload["hosts"][0]["thermalColor"] = json!(color::hex(color::RED)),
                _ => payload["hosts"][0]["connection"]["state"] = json!("stale"),
            }
            apply(&mut payload, &alerts);
            assert_eq!(machine_rows(payload)[0]["attention"], true, "{problem}");
        }
    }
}
