//! A dashboard tile is a view of a connection, not the connection itself.
//! Multiple tiles may name the same source; an empty or hidden tile list is
//! intentional and never changes polling or credentials.

use crate::settings::{
    HostOverflowMode, CORE_ROW_SPAN_RANGE, DEFAULT_CORE_ROW_SPAN, DEFAULT_ROW_GAP_PX,
    ROW_GAP_PX_RANGE,
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::ops::RangeInclusive;

/// A tile field whose value must lie in a range. [`TileRangeField::spec`] is
/// its one definition and [`RANGE_BOUNDED_TILE_FIELDS`] the one list of them.
/// The clamp on read and the refusal over IPC both read `spec`; only the
/// refusal walks the list (#669).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileRangeField {
    CoreRowSpan,
    RowGapPx,
}

/// What [`TileRangeField::spec`] says about a field.
#[derive(Debug, Clone)]
pub(crate) struct RangeBoundedField {
    /// The field's key in the tile's JSON.
    pub(crate) key: &'static str,
    pub(crate) range: RangeInclusive<u8>,
    /// What the field reads as when its key is absent (serde's `default`) or
    /// `null`.
    pub(crate) default: u8,
}

impl TileRangeField {
    /// Exhaustive, so a new variant does not compile until it is defined.
    pub(crate) fn spec(self) -> RangeBoundedField {
        match self {
            TileRangeField::CoreRowSpan => RangeBoundedField {
                key: "coreRowSpan",
                range: CORE_ROW_SPAN_RANGE,
                default: DEFAULT_CORE_ROW_SPAN,
            },
            TileRangeField::RowGapPx => RangeBoundedField {
                key: "rowGapPx",
                range: ROW_GAP_PX_RANGE,
                default: DEFAULT_ROW_GAP_PX,
            },
        }
    }
}

/// The range-bounded tile fields that [`DashboardLayout::from_ipc_value`]
/// refuses over IPC. It is a hand-maintained array: the clamp on read comes
/// from each field's deserialiser reading [`TileRangeField::spec`], not from
/// this list. Two tests keep them together: `every_numeric_tile_field_is_range_bounded`
/// fails when a tile field that serialises as a number or `null` is missing
/// here, and `every_listed_field_clamps_on_read_and_is_refused_over_ipc`
/// fails when a listed field does not also clamp on read.
pub(crate) const RANGE_BOUNDED_TILE_FIELDS: [TileRangeField; 2] =
    [TileRangeField::CoreRowSpan, TileRangeField::RowGapPx];

/// Why a layout arriving over IPC was not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileReadError {
    /// A range-bounded field held a value outside its range, or no whole
    /// number at all (a string, a float, a negative).
    OutOfRange(TileRangeField),
    /// The payload is not a layout.
    Unreadable,
}

impl DashboardLayout {
    /// Reads a layout arriving over IPC. `DashboardTile` clamps its
    /// range-bounded fields as it deserialises, which is right for a
    /// hand-edited `store.json` and wrong here: a request no picker can
    /// produce would be silently rewritten instead of refused. Each field of
    /// [`RANGE_BOUNDED_TILE_FIELDS`] is checked against its range first. Absent
    /// and `null` read as the default; a float such as `3.5` is refused.
    pub fn from_ipc_value(raw: Value) -> Result<Self, TileReadError> {
        for tile in raw["tiles"].as_array().into_iter().flatten() {
            for field in RANGE_BOUNDED_TILE_FIELDS {
                let f = field.spec();
                let outside = tile.get(f.key).is_some_and(|v| {
                    !v.is_null()
                        && v.as_u64()
                            .is_none_or(|n| !f.range.contains(&(n.min(255) as u8)))
                });
                if outside {
                    return Err(TileReadError::OutOfRange(field));
                }
            }
        }
        serde_json::from_value(raw).map_err(|_| TileReadError::Unreadable)
    }
}

/// The shared read of a range-bounded field: `null` is the default, a `u8` is
/// clamped into the range, and anything else (300, -1, 3.5, "2") fails the
/// read.
fn read_ranged<'de, D: Deserializer<'de>>(d: D, field: TileRangeField) -> Result<u8, D::Error> {
    let f = field.spec();
    Ok(Option::<u8>::deserialize(d)?
        .map_or(f.default, |v| v.clamp(*f.range.start(), *f.range.end())))
}

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
    /// GitHub Runners tiles only: Table or List in this tile's Full view, separate from
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
    TileRangeField::CoreRowSpan.spec().default
}
fn default_row_gap_px() -> u8 {
    TileRangeField::RowGapPx.spec().default
}
fn table_view() -> String {
    "table".into()
}
fn de_core_row_span<'de, D: Deserializer<'de>>(d: D) -> Result<u8, D::Error> {
    read_ranged(d, TileRangeField::CoreRowSpan)
}
fn de_row_gap_px<'de, D: Deserializer<'de>>(d: D) -> Result<u8, D::Error> {
    read_ranged(d, TileRangeField::RowGapPx)
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
    ("ghRunners", "GitHub Runners"),
    ("services", "Service Health"),
    ("sentryCrons", "Sentry Crons"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tile_json() -> Value {
        serde_json::to_value(DashboardTile::new("t", "hosts", "T", "all")).unwrap()
    }

    fn layout_with(key: &str, value: Value) -> Value {
        let mut tile = tile_json();
        tile[key] = value;
        json!({"revision": 0, "tiles": [tile]})
    }

    /// Walks the list, which drives only the IPC refusal, and checks that each
    /// listed field also clamps on read (its deserialiser reads `spec()`): a
    /// value past the top clamps on read and is refused over IPC, and null
    /// reads as the default on both.
    #[test]
    fn every_listed_field_clamps_on_read_and_is_refused_over_ipc() {
        for field in RANGE_BOUNDED_TILE_FIELDS {
            let f = field.spec();
            let over = u64::from(*f.range.end()) + 1;
            let raw = layout_with(f.key, json!(over));
            let read: DashboardLayout = serde_json::from_value(raw.clone()).unwrap();
            let read_back = serde_json::to_value(&read.tiles[0]).unwrap();
            assert_eq!(read_back[f.key], json!(*f.range.end()), "{} clamps", f.key);
            assert_eq!(
                DashboardLayout::from_ipc_value(raw),
                Err(TileReadError::OutOfRange(field)),
                "{} refused",
                f.key
            );
            for bad in [json!(3.5), json!("2"), json!(-1), json!(100_000)] {
                assert_eq!(
                    DashboardLayout::from_ipc_value(layout_with(f.key, bad.clone())),
                    Err(TileReadError::OutOfRange(field)),
                    "{} = {bad}",
                    f.key
                );
            }
            let null = DashboardLayout::from_ipc_value(layout_with(f.key, Value::Null)).unwrap();
            assert_eq!(
                serde_json::to_value(&null.tiles[0]).unwrap()[f.key],
                json!(f.default)
            );
            for ok in [*f.range.start(), *f.range.end()] {
                assert!(
                    DashboardLayout::from_ipc_value(layout_with(f.key, json!(ok))).is_ok(),
                    "{} = {ok}",
                    f.key
                );
            }
            // An absent key takes serde's `default`, which is the same entry.
            let mut tile = tile_json();
            tile.as_object_mut().unwrap().remove(f.key);
            let absent = DashboardLayout::from_ipc_value(json!({"tiles": [tile]})).unwrap();
            assert_eq!(
                serde_json::to_value(&absent.tiles[0]).unwrap()[f.key],
                json!(f.default),
                "{} absent",
                f.key
            );
        }
    }

    /// A numeric tile field missing from the list would clamp on read and
    /// slip through IPC unrefused. Every tile field that serialises as a
    /// number, or as `null` (an unset `Option<u8>`), must be in
    /// [`RANGE_BOUNDED_TILE_FIELDS`]; a deliberately unbounded one would have
    /// to be exempted here, by name. A field that serialises as absent
    /// (`skip_serializing_if`) is not seen by this probe.
    #[test]
    fn every_numeric_tile_field_is_range_bounded() {
        let listed: Vec<&str> = RANGE_BOUNDED_TILE_FIELDS
            .iter()
            .map(|f| f.spec().key)
            .collect();
        let tile = tile_json();
        for (key, value) in tile.as_object().unwrap() {
            if value.is_number() || value.is_null() {
                assert!(
                    listed.contains(&key.as_str()),
                    "{key} is a number but not in RANGE_BOUNDED_TILE_FIELDS"
                );
            }
        }
        for key in listed {
            assert!(
                tile[key].is_number(),
                "{key} is listed but not a tile number"
            );
        }
    }

    #[test]
    fn an_unreadable_payload_is_unreadable_not_out_of_range() {
        assert_eq!(
            DashboardLayout::from_ipc_value(json!({"tiles": [{"id": 1}]})),
            Err(TileReadError::Unreadable)
        );
    }
}
