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
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardLayout {
    /// Optimistic concurrency: a delayed save cannot overwrite a newer layout.
    #[serde(default)]
    pub revision: u64,
    pub tiles: Vec<DashboardTile>,
}
