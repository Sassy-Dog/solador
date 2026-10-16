//! Hidden volumes: the mounts an operator chose not to see on a machine.
//!
//! Persisted in two places that predate this module — `Host.hidden_volume_mounts`
//! for a remote host, `Settings.local_hidden_volume_mounts` for this machine —
//! and keyed here the way cards are, `"local"` or the host's UUID, so the one
//! lookup serves both. A hidden volume is still measured; it is only left off
//! the cards, which is what keeps it out of the Disk meter, the status line and
//! attention (see [`viewmodel::card::hide_volumes`]).

use std::collections::HashMap;

use serde_json::Value;
use store::Store;
use uuid::Uuid;

/// The id the local machine's card carries.
pub const LOCAL: &str = "local";

/// Every machine's hidden mounts, read once per frame under the store's lock.
#[derive(Debug, Default)]
pub struct Hidden(HashMap<String, Vec<String>>);

impl Hidden {
    pub fn from_store(store: &Store) -> Self {
        let mut by_id: HashMap<String, Vec<String>> = store
            .hosts()
            .iter()
            .map(|host| (host.id.to_string(), host.hidden_volume_mounts.clone()))
            .collect();
        by_id.insert(
            LOCAL.to_owned(),
            store.settings().local_hidden_volume_mounts.clone(),
        );
        Self(by_id)
    }

    pub fn for_id(&self, id: &str) -> &[String] {
        self.0.get(id).map_or(&[], Vec::as_slice)
    }

    /// Filters every card by its own machine's list.
    pub fn apply(&self, cards: &mut [Value]) {
        for card in cards {
            let id = card["id"].as_str().unwrap_or_default().to_owned();
            viewmodel::card::hide_volumes(card, self.for_id(&id));
        }
    }
}

/// Hides or unhides one mount on one machine and saves, restoring the previous
/// list if the save fails so memory never claims what disk does not hold.
pub fn set_hidden(
    store: &mut Store,
    host_id: &str,
    mount: &str,
    hidden: bool,
) -> Result<(), String> {
    if mount.is_empty() {
        return Err("No volume was named.".into());
    }
    let list = list_mut(store, host_id)?;
    let previous = list.clone();
    if hidden {
        if !list.iter().any(|m| m == mount) {
            list.push(mount.to_owned());
        }
    } else {
        list.retain(|m| m != mount);
    }
    if let Err(error) = store.save() {
        *list_mut(store, host_id)? = previous;
        return Err(error.to_string());
    }
    Ok(())
}

fn list_mut<'a>(store: &'a mut Store, host_id: &str) -> Result<&'a mut Vec<String>, String> {
    if host_id == LOCAL {
        return Ok(&mut store.settings_mut().local_hidden_volume_mounts);
    }
    Uuid::parse_str(host_id)
        .ok()
        .and_then(|id| store.host_mut(id))
        .map(|host| &mut host.hidden_volume_mounts)
        .ok_or_else(|| "Unknown machine. Reopen the dashboard and try again.".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use store::Host;

    fn temp_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = Store::open_in(dir.path(), false).expect("open");
        (dir, store)
    }

    #[test]
    fn hiding_and_unhiding_round_trips_through_the_saved_store() {
        let (dir, mut store) = temp_store();
        let host = Host::new("nas", "100.64.0.9");
        let id = host.id;
        store.upsert_host(host);

        set_hidden(&mut store, &id.to_string(), "/srv", true).unwrap();
        set_hidden(&mut store, &id.to_string(), "/srv", true).unwrap();
        set_hidden(&mut store, LOCAL, "/Volumes/Archive", true).unwrap();

        let reopened = Store::open_in(dir.path(), false).unwrap();
        assert_eq!(
            reopened.host(id).unwrap().hidden_volume_mounts,
            ["/srv"],
            "no duplicates"
        );
        assert_eq!(
            reopened.settings().local_hidden_volume_mounts,
            ["/Volumes/Archive"]
        );

        set_hidden(&mut store, &id.to_string(), "/srv", false).unwrap();
        let reopened = Store::open_in(dir.path(), false).unwrap();
        assert!(reopened.host(id).unwrap().hidden_volume_mounts.is_empty());
    }

    #[test]
    fn an_unknown_machine_or_an_empty_mount_changes_nothing() {
        let (_dir, mut store) = temp_store();
        assert!(set_hidden(&mut store, &Uuid::new_v4().to_string(), "/srv", true).is_err());
        assert!(set_hidden(&mut store, "not-a-uuid", "/srv", true).is_err());
        assert!(set_hidden(&mut store, LOCAL, "", true).is_err());
        assert!(store.settings().local_hidden_volume_mounts.is_empty());
    }

    #[test]
    fn a_failed_save_restores_the_previous_list() {
        let (dir, mut store) = temp_store();
        set_hidden(&mut store, LOCAL, "/a", true).unwrap();
        // A directory where the store file should be makes the atomic rename fail.
        let path = store.path().to_path_buf();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();

        assert!(set_hidden(&mut store, LOCAL, "/b", true).is_err());
        assert_eq!(store.settings().local_hidden_volume_mounts, ["/a"]);
        drop(dir);
    }

    #[test]
    fn each_card_is_filtered_by_its_own_machines_list() {
        let (_dir, mut store) = temp_store();
        let host = Host::new("nas", "100.64.0.9");
        let id = host.id.to_string();
        store.upsert_host(host);
        set_hidden(&mut store, &id, "/srv", true).unwrap();
        set_hidden(&mut store, LOCAL, "/", true).unwrap();

        let volume = |mount: &str| json!({"mount": mount});
        let mut cards = vec![
            json!({"id": LOCAL, "volumes": [volume("/"), volume("/srv")]}),
            json!({"id": id, "volumes": [volume("/"), volume("/srv")]}),
            json!({"id": "pending-host", "error": {"message": "Connecting"}}),
        ];
        Hidden::from_store(&store).apply(&mut cards);

        assert_eq!(cards[0]["volumes"], json!([volume("/srv")]));
        assert_eq!(cards[1]["volumes"], json!([volume("/")]));
        assert_eq!(cards[1]["hiddenVolumes"], json!(["/srv"]));
        assert!(
            cards[2]["volumes"].is_null(),
            "a card with no volumes gains none"
        );
    }
}
