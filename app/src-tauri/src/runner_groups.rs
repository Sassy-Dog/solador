//! The runner group labels: one list, edited from a Runners tile's Configure.
//!
//! The list is not the tile's. It decides the groups of every Runners tile, of
//! Detail and of the Full view, so the section says it is shared and every edit
//! saves on the click — never part of a tile draft's Apply or the dashboard's
//! undo history.

use serde_json::{json, Value};
use store::Store;

/// The longest runner group label accepted, in bytes. Ours, not GitHub's: a
/// generous ceiling on a value that is echoed into every group's title.
pub const GROUP_LABEL_MAX_BYTES: usize = 256;

/// The most runner group labels one cockpit lists. Ours as well: every label
/// is a group per OS · architecture, and past a few dozen the panel is a list
/// of groups rather than an answer.
pub const GROUP_LABELS_MAX: usize = 32;

/// One runner group label to append to `listed`, trimmed, or the status line
/// saying why not.
///
/// Anything a fleet might use is accepted — the gate refuses only what cannot
/// be a label (blank, a control character, past [`GROUP_LABEL_MAX_BYTES`]), a
/// second spelling of one already listed (GitHub matches labels
/// case-insensitively, so the two would be one group named twice), and a
/// list already at [`GROUP_LABELS_MAX`].
///
/// # Errors
/// The status line to show, already worded as one.
pub fn validated_label(label: &str, listed: &[String]) -> Result<String, String> {
    let label = label.trim();
    if label.is_empty() {
        return Err("Skipped — a runner label is required.".to_owned());
    }
    if label.chars().any(char::is_control) {
        return Err("Skipped — a runner label cannot contain control characters.".to_owned());
    }
    if label.len() > GROUP_LABEL_MAX_BYTES {
        return Err(format!(
            "Skipped — a runner label is at most {GROUP_LABEL_MAX_BYTES} bytes."
        ));
    }
    if let Some(existing) = listed
        .iter()
        .find(|existing| existing.eq_ignore_ascii_case(label))
    {
        return Err(format!("Skipped — {existing} is already listed."));
    }
    if listed.len() >= GROUP_LABELS_MAX {
        return Err(format!(
            "Skipped — at most {GROUP_LABELS_MAX} runner group labels can be listed."
        ));
    }
    Ok(label.to_owned())
}

/// `listed` without `label`, matched exactly as it is stored.
///
/// # Errors
/// The status line saying the label is not listed.
pub fn removed(listed: &[String], label: &str) -> Result<Vec<String>, String> {
    if !listed.iter().any(|item| item == label) {
        return Err(format!("Skipped — {label} is not listed."));
    }
    Ok(listed
        .iter()
        .filter(|item| *item != label)
        .cloned()
        .collect())
}

/// `listed` with `label` one place earlier (`up`) or later.
///
/// # Errors
/// The status line saying the label is not listed, or already at that end.
pub fn moved(listed: &[String], label: &str, up: bool) -> Result<Vec<String>, String> {
    let Some(at) = listed.iter().position(|item| item == label) else {
        return Err(format!("Skipped — {label} is not listed."));
    };
    let to = if up {
        at.checked_sub(1)
            .ok_or_else(|| format!("Skipped — {label} is already first."))?
    } else if at + 1 < listed.len() {
        at + 1
    } else {
        return Err(format!("Skipped — {label} is already last."));
    };
    let mut next = listed.to_vec();
    next.swap(at, to);
    Ok(next)
}

/// Stores `next` and saves it; a save that fails puts the previous list back,
/// so memory never claims what disk does not hold.
///
/// # Errors
/// The status line naming the save failure.
pub fn save(store: &mut Store, next: Vec<String>) -> Result<(), String> {
    let previous = store.runner_group_labels().to_vec();
    store.set_runner_group_labels(next);
    if let Err(error) = store.save() {
        store.set_runner_group_labels(previous);
        return Err(format!("Could not save the group labels: {error}"));
    }
    Ok(())
}

/// The Configure section: the stored list in priority order, each label with
/// how many runners carry it, and the custom labels a click could add.
///
/// `runner_labels` is every runner's label list as the GitHub state holds it
/// (registered and remembered alike, unknown ones left out), or `None` before
/// any organization's runners have been read — when "no runner carries this
/// label" would be a claim nothing has checked, and nothing has been seen.
pub fn section(listed: &[String], runner_labels: Option<&[Vec<String>]>) -> Value {
    let rows: Vec<Value> = listed
        .iter()
        .enumerate()
        .map(|(at, label)| {
            let carriers = match runner_labels {
                None => "runners not read yet".to_owned(),
                Some(runners) => match runners
                    .iter()
                    .filter(|carried| viewmodel::dashboard::carries_label(carried, label))
                    .count()
                {
                    0 => "no runner carries this label".to_owned(),
                    1 => "carried by 1 runner".to_owned(),
                    n => format!("carried by {n} runners"),
                },
            };
            json!({
                "label": label,
                "carriers": carriers,
                "first": at == 0,
                "last": at + 1 == listed.len(),
            })
        })
        .collect();
    json!({
        "heading": "Group by labels",
        "firstMatch": "First match wins: a runner joins the group of the first label below that it carries.",
        "shared": "Shared · saved immediately · applies everywhere runners appear: every Runners tile and the Full view.",
        "help": "Split runner groups by labels your runners carry — runner-large, say, to see large and small runners apart. An OS · architecture splits only once one of its runners carries a listed label; its runners carrying none of them are grouped as other. Labels match the way GitHub matches runs-on, ignoring case. With none listed, runners group by OS and architecture alone.",
        "empty": "No group labels — runners group by OS and architecture.",
        "inputLabel": "Runner label (e.g. runner-large)",
        "addLabel": "Add",
        "removeLabel": "Remove",
        "upLabel": "Move up",
        "downLabel": "Move down",
        "seenLabel": "Seen on your runners",
        "rows": rows,
        "seen": seen(listed, runner_labels.unwrap_or_default()),
    })
}

/// The custom labels some runner carries that are not listed under any
/// spelling, first spelling kept, sorted ignoring case.
fn seen(listed: &[String], runner_labels: &[Vec<String>]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for label in runner_labels.iter().flatten() {
        let known = |other: &String| other.eq_ignore_ascii_case(label);
        if viewmodel::dashboard::is_standard_label(label)
            || listed.iter().any(known)
            || seen.iter().any(known)
        {
            continue;
        }
        seen.push(label.clone());
    }
    seen.sort_by_key(|label| label.to_ascii_lowercase());
    seen
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    /// A group label is whatever the operator's fleet uses, so the gate refuses
    /// only what cannot be a label or would make two groups indistinguishable:
    /// GitHub matches labels case-insensitively, so `Runner-Large` beside
    /// `runner-large` would be one group listed twice.
    #[test]
    fn group_labels_are_trimmed_and_refused_only_when_unusable() {
        let listed = strings(&["runner-large"]);
        assert_eq!(
            validated_label("  gpu  ", &listed).expect("a new label"),
            "gpu"
        );
        for refused in ["", "   ", "Runner-Large", "runner\tlarge"] {
            assert!(
                validated_label(refused, &listed).is_err(),
                "{refused:?} must be refused"
            );
        }
        assert!(validated_label(&"a".repeat(GROUP_LABEL_MAX_BYTES), &[]).is_ok());
        assert!(validated_label(&"a".repeat(GROUP_LABEL_MAX_BYTES + 1), &[]).is_err());
        let full: Vec<String> = (0..GROUP_LABELS_MAX)
            .map(|i| format!("label-{i}"))
            .collect();
        assert!(
            validated_label("one-more", &full).is_err(),
            "the list has a ceiling"
        );
    }

    /// Order is the rule — the first listed label a runner carries wins — so
    /// reordering is an edit in its own right, refused where it would do nothing.
    #[test]
    fn moved_reorders_and_refuses_at_the_edges() {
        let abc = strings(&["a", "b", "c"]);
        assert_eq!(moved(&abc, "c", true), Ok(strings(&["a", "c", "b"])));
        assert_eq!(moved(&abc, "a", false), Ok(strings(&["b", "a", "c"])));
        let ab = strings(&["a", "b"]);
        assert_eq!(
            moved(&ab, "a", true),
            Err("Skipped — a is already first.".to_owned())
        );
        assert_eq!(
            moved(&ab, "b", false),
            Err("Skipped — b is already last.".to_owned())
        );
        assert_eq!(
            moved(&ab, "z", true),
            Err("Skipped — z is not listed.".to_owned())
        );
    }

    /// Removal matches the label exactly as it is stored: the editor sends back
    /// the spelling it was handed.
    #[test]
    fn removed_refuses_an_unlisted_label() {
        assert_eq!(removed(&strings(&["a", "b"]), "a"), Ok(strings(&["b"])));
        assert_eq!(
            removed(&strings(&["a"]), "A"),
            Err("Skipped — A is not listed.".to_owned())
        );
    }

    /// Each listed label says how many runners carry it, so a typo — which
    /// would quietly put every runner in `other` — is visible where it was
    /// typed. Before any runner has been read the count is unknown, not zero.
    #[test]
    fn each_group_label_says_how_many_runners_carry_it() {
        let listed = strings(&["runner-large", "gpu", "rare"]);
        let seen = vec![
            strings(&["self-hosted", "runner-large"]),
            strings(&["Runner-Large", "gpu"]),
            strings(&["self-hosted"]),
        ];
        let carriers = |section: &Value| {
            section["rows"]
                .as_array()
                .expect("rows")
                .iter()
                .map(|row| {
                    (
                        row["label"].as_str().unwrap().to_owned(),
                        row["carriers"].as_str().unwrap().to_owned(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            carriers(&section(&listed, Some(&seen))),
            vec![
                ("runner-large".to_owned(), "carried by 2 runners".to_owned()),
                ("gpu".to_owned(), "carried by 1 runner".to_owned()),
                ("rare".to_owned(), "no runner carries this label".to_owned()),
            ]
        );
        assert_eq!(
            carriers(&section(&listed[..1], None)),
            vec![("runner-large".to_owned(), "runners not read yet".to_owned())]
        );
    }

    #[test]
    fn the_section_marks_first_and_last() {
        let edges = |listed: &[String]| {
            section(listed, None)["rows"]
                .as_array()
                .expect("rows")
                .iter()
                .map(|row| (row["first"].clone(), row["last"].clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            edges(&strings(&["a", "b", "c"])),
            vec![
                (json!(true), json!(false)),
                (json!(false), json!(false)),
                (json!(false), json!(true)),
            ]
        );
        assert_eq!(edges(&strings(&["a"])), vec![(json!(true), json!(true))]);
    }

    /// The labels a click could add: custom ones some runner carries that are
    /// not listed under any spelling. GitHub's own labels are left out — every
    /// runner carries `self-hosted`, so grouping by it groups nothing.
    #[test]
    fn the_section_offers_seen_custom_labels_not_already_listed() {
        let runners = vec![
            strings(&["self-hosted", "Linux", "X64", "runner-large"]),
            strings(&["self-hosted", "GPU"]),
            strings(&["gpu", "docker"]),
        ];
        assert_eq!(
            section(&strings(&["runner-large"]), Some(&runners))["seen"],
            json!(["docker", "GPU"])
        );
        assert_eq!(
            section(&strings(&["DOCKER"]), Some(&runners))["seen"],
            json!(["GPU", "runner-large"])
        );
        assert_eq!(section(&[], None)["seen"], json!([]));
    }

    /// The section is in a tile's Configure but the list is not the tile's:
    /// it says so, and says the order is the rule.
    #[test]
    fn the_section_states_first_match_and_that_it_is_shared() {
        let section = section(&[], None);
        assert_eq!(section["heading"], "Group by labels");
        assert_eq!(
            section["firstMatch"],
            "First match wins: a runner joins the group of the first label below that it carries."
        );
        assert_eq!(
            section["shared"],
            "Shared · saved immediately · applies everywhere runners appear: every Runners tile and the Full view."
        );
    }

    /// Memory never claims what disk does not hold: a save that fails leaves
    /// the list as it was, and says it could not save.
    #[test]
    fn a_failed_save_leaves_the_list_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store::Store::open_in(dir.path(), false).unwrap();
        save(&mut store, strings(&["runner-large"])).expect("a writable store");
        let reopened = store::Store::open_in(dir.path(), false).unwrap();
        assert_eq!(reopened.runner_group_labels(), strings(&["runner-large"]));
        std::fs::remove_file(dir.path().join("store.json")).unwrap();
        std::fs::create_dir(dir.path().join("store.json")).unwrap();
        let refusal = save(&mut store, strings(&["gpu"])).expect_err("an unwritable store");
        assert!(
            refusal.starts_with("Could not save the group labels: "),
            "{refusal}"
        );
        assert_eq!(store.runner_group_labels(), strings(&["runner-large"]));
    }
}
