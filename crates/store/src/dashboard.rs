//! A dashboard tile is a view of a connection, not the connection itself.
//! Multiple tiles may name the same source; an empty or hidden tile list is
//! intentional and never changes polling, credentials, or the legacy layout.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardTile {
    pub id: String,
    pub source: String,
    pub title: String,
    pub scope: String,
    pub presentation: String,
    pub width: String,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default = "automatic_rows")]
    pub row_limit: String,
    /// Empty means all repositories within the scope, including future ones.
    #[serde(default)]
    pub selected_repos: Vec<String>,
    #[serde(default = "name_sort")]
    pub sort_by: String,
    #[serde(default)]
    pub sort_descending: bool,
    #[serde(default = "runner_list")]
    pub runner_view: String,
}

fn automatic_rows() -> String {
    "auto".into()
}
fn name_sort() -> String {
    "name".into()
}
fn runner_list() -> String {
    "list".into()
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardLayout {
    /// Optimistic concurrency: a delayed save cannot overwrite a newer layout.
    #[serde(default)]
    pub revision: u64,
    pub tiles: Vec<DashboardTile>,
}
