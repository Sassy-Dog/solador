//! CPU and RAM alert preferences, shared by default and overridable by host id.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineThresholds {
    pub cpu_warning: u8,
    pub cpu_critical: u8,
    pub ram_warning: u8,
    pub ram_critical: u8,
}

impl Default for MachineThresholds {
    fn default() -> Self {
        Self {
            cpu_warning: 70,
            cpu_critical: 90,
            ram_warning: 70,
            ram_critical: 90,
        }
    }
}

impl MachineThresholds {
    /// Strict for Settings submissions; invalid edits never become saved defaults.
    pub fn parse(value: &Value) -> Result<Self, &'static str> {
        let percent = |key: &str| {
            value[key]
                .as_u64()
                .filter(|n| (1..=100).contains(n))
                .map(|n| n as u8)
                .ok_or("Enter whole percentages from 1 to 100 for all four thresholds.")
        };
        let limits = Self {
            cpu_warning: percent("cpuWarning")?,
            cpu_critical: percent("cpuCritical")?,
            ram_warning: percent("ramWarning")?,
            ram_critical: percent("ramCritical")?,
        };
        if limits.cpu_warning >= limits.cpu_critical || limits.ram_warning >= limits.ram_critical {
            return Err("Each warning threshold must be lower than its critical threshold.");
        }
        Ok(limits)
    }
}

impl<'de> Deserialize<'de> for MachineThresholds {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // A bad hand-edited threshold must not prevent the rest of the store loading.
        Ok(Self::parse(&Value::deserialize(deserializer)?).unwrap_or_default())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MachineAlerts {
    pub defaults: MachineThresholds,
    /// Stable machine ids, including "local"; names and addresses can change.
    pub overrides: BTreeMap<String, MachineThresholds>,
}

impl MachineAlerts {
    pub fn for_host(&self, id: &str) -> MachineThresholds {
        self.overrides.get(id).copied().unwrap_or(self.defaults)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn old_settings_keep_the_existing_thresholds() {
        let settings: crate::Settings = serde_json::from_str("{}").unwrap();
        let limits = settings.machine_alerts.for_host("local");
        assert_eq!((limits.cpu_warning, limits.cpu_critical), (70, 90));
        assert_eq!((limits.ram_warning, limits.ram_critical), (70, 90));
    }

    #[test]
    fn overrides_follow_identity_and_reset_to_the_current_defaults() {
        let mut alerts = MachineAlerts::default();
        let mac = MachineThresholds::parse(&json!({
            "cpuWarning": 80, "cpuCritical": 95, "ramWarning": 90, "ramCritical": 98
        }))
        .unwrap();
        alerts.overrides.insert("mac-id".into(), mac);
        alerts.defaults.ram_warning = 75;
        assert_eq!(alerts.for_host("mac-id"), mac);
        assert_eq!(alerts.for_host("other-id").ram_warning, 75);
        alerts.overrides.remove("mac-id");
        assert_eq!(alerts.for_host("mac-id").ram_warning, 75);
    }

    #[test]
    fn input_rejects_inverted_out_of_range_fractional_and_missing_limits() {
        let valid = json!({"cpuWarning":70,"cpuCritical":90,"ramWarning":90,"ramCritical":98});
        for (key, value) in [
            ("cpuWarning", json!(90)),
            ("ramCritical", json!(89)),
            ("cpuWarning", json!(0)),
            ("ramCritical", json!(101)),
            ("ramWarning", json!(85.5)),
            ("cpuWarning", json!(-1)),
            ("ramCritical", json!(null)),
        ] {
            let mut input = valid.clone();
            input[key] = value;
            assert!(MachineThresholds::parse(&input).is_err(), "{input}");
        }
        assert!(MachineThresholds::parse(&json!({})).is_err());
    }

    #[test]
    fn malformed_stored_limits_fall_back_without_losing_other_settings() {
        let settings: crate::Settings = serde_json::from_value(json!({
            "core_row_span": 4,
            "machine_alerts": {"defaults": {"ramWarning": -1}}
        }))
        .unwrap();
        assert_eq!(settings.core_row_span, 4);
        assert_eq!(
            settings.machine_alerts.defaults,
            MachineThresholds::default()
        );
    }

    #[test]
    fn deleting_a_host_removes_only_its_override() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = crate::Store::open_in(dir.path(), false).unwrap();
        let host = crate::Host::new("mac", "mac.local");
        let id = host.id;
        store.upsert_host(host);
        store
            .settings_mut()
            .machine_alerts
            .overrides
            .insert(id.to_string(), MachineThresholds::default());
        store
            .settings_mut()
            .machine_alerts
            .overrides
            .insert("local".into(), MachineThresholds::default());
        store.remove_host(id).unwrap();
        store.save().unwrap();
        let reopened = crate::Store::open_in(dir.path(), false).unwrap();
        assert!(!reopened
            .settings()
            .machine_alerts
            .overrides
            .contains_key(&id.to_string()));
        assert!(reopened
            .settings()
            .machine_alerts
            .overrides
            .contains_key("local"));
    }
}
