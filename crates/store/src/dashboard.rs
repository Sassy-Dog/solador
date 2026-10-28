//! A dashboard tile is a view of a connection, not the connection itself.
//! Multiple tiles may name the same source; an empty or hidden tile list is
//! intentional and never changes polling or credentials.

use crate::settings::{
    clamp_core_row_span, clamp_row_gap_px, HostOverflowMode, DEFAULT_CORE_ROW_SPAN,
    DEFAULT_ROW_GAP_PX,
};
use serde::{Deserialize, Deserializer, Serialize};

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
    /// Machines tiles only: false drops the Disk strip from this tile's rows.
    /// The volumes still colour the status line and raise attention.
    #[serde(default = "disk_shown")]
    pub show_disk: bool,
    /// Machines tiles only: how many grid rows the per-host CPU core chart
    /// spans in this tile's Full view. Within `CORE_ROW_SPAN_RANGE`; an
    /// out-of-range value read from disk is clamped, since the intent survives.
    #[serde(
        default = "default_core_row_span",
        deserialize_with = "de_core_row_span"
    )]
    pub core_row_span: u8,
    /// Machines tiles only: what this tile's Full view does when host cards no
    /// longer fit side by side.
    #[serde(default)]
    pub host_overflow: HostOverflowMode,
    /// Points between two rows of the same list in this tile's views. Within
    /// `ROW_GAP_PX_RANGE`; `0` is a chosen value (flush), not an absence.
    #[serde(default = "default_row_gap_px", deserialize_with = "de_row_gap_px")]
    pub row_gap_px: u8,
    /// Table or List in this tile's Detail.
    #[serde(default = "table_view")]
    pub detail_view: String,
    /// Runners tiles only: Table or List in this tile's Full view, separate from
    /// Detail's.
    #[serde(default = "table_view")]
    pub runner_full_view: String,
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
fn disk_shown() -> bool {
    true
}
fn default_core_row_span() -> u8 {
    DEFAULT_CORE_ROW_SPAN
}
fn default_row_gap_px() -> u8 {
    DEFAULT_ROW_GAP_PX
}
fn table_view() -> String {
    "table".into()
}
fn de_core_row_span<'de, D: Deserializer<'de>>(d: D) -> Result<u8, D::Error> {
    Ok(Option::<u8>::deserialize(d)?.map_or(DEFAULT_CORE_ROW_SPAN, clamp_core_row_span))
}
fn de_row_gap_px<'de, D: Deserializer<'de>>(d: D) -> Result<u8, D::Error> {
    Ok(Option::<u8>::deserialize(d)?.map_or(DEFAULT_ROW_GAP_PX, clamp_row_gap_px))
}

impl DashboardTile {
    /// A tile of `source` with every option at its default.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        source: impl Into<String>,
        title: impl Into<String>,
        scope: impl Into<String>,
    ) -> Self {
        let source = source.into();
        DashboardTile {
            id: id.into(),
            title: title.into(),
            scope: scope.into(),
            presentation: "summary".into(),
            width: "small".into(),
            hidden: false,
            row_limit: automatic_rows(),
            selected_repos: Vec::new(),
            sort_by: name_sort(),
            sort_descending: false,
            // Grouped answers "is this pool healthy?" per OS and architecture;
            // it is only valid on Runners.
            runner_view: if source == "ghRunners" {
                "grouped".into()
            } else {
                runner_list()
            },
            show_disk: true,
            core_row_span: DEFAULT_CORE_ROW_SPAN,
            host_overflow: HostOverflowMode::default(),
            row_gap_px: DEFAULT_ROW_GAP_PX,
            detail_view: table_view(),
            runner_full_view: table_view(),
            source,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardLayout {
    /// Optimistic concurrency: a delayed save cannot overwrite a newer layout.
    #[serde(default)]
    pub revision: u64,
    pub tiles: Vec<DashboardTile>,
    /// **Retired (#647): the per-source Detail Table/List choice, read once and
    /// never written.** [`crate::Store::open_in`] copies each entry onto every
    /// tile of that source (for Runners, into both the Detail and the Full view
    /// field) and takes it.
    #[serde(default, rename = "detailViews", skip_serializing)]
    pub legacy_detail_views: std::collections::BTreeMap<String, String>,
}

/// The tiles a store that never configured an overview shows: source id and
/// tile title. The app's `SOURCES` table carries the same titles and a test
/// there keeps the two equal; they live here so the upgrade copy can
/// materialise this overview when a value it must carry has no tile to land on.
pub const SHIPPED_TILES: [(&str, &str); 5] = [
    ("hosts", "Machines"),
    ("ghWorkflows", "GitHub Repos"),
    ("ghRunners", "Runners"),
    ("services", "Service Health"),
    ("sentryCrons", "Scheduled Jobs"),
];

impl DashboardLayout {
    /// The overview a store that never configured one shows.
    #[must_use]
    pub fn shipped() -> Self {
        DashboardLayout {
            revision: 0,
            legacy_detail_views: Default::default(),
            tiles: SHIPPED_TILES
                .iter()
                .enumerate()
                .map(|(i, (source, title))| {
                    let scope = if *source == "sentryCrons" {
                        "active"
                    } else {
                        "all"
                    };
                    let mut tile =
                        DashboardTile::new(format!("overview-{source}"), *source, *title, scope);
                    tile.width = if i < 2 { "medium" } else { "small" }.into();
                    tile
                })
                .collect(),
        }
    }
}
