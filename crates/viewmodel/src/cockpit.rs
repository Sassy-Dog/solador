//! The cockpit grid layer, ported from `DevCanopy/Views/Cockpit/`:
//! `CockpitBreakpoints.swift` (reflow + host columns) and `CockpitPanel.swift`
//! (the panel table and the shipped arrangement). The palette those two share
//! lives in [`crate::color`]; [`theme`] hands it out as painted hex.
//!
//! Modelled on CSS `repeat(auto-fit, minmax(<min>, 1fr))` rather than on global
//! `sm`/`md`/`lg` tiers: **each panel declares its own [`PanelKind::min_width`]**
//! and a row breaks only when *its* panels stop fitting. Named tiers can't
//! express the case that actually matters here — Usage + Azure Cost still sit
//! comfortably side-by-side at a width where the much hungrier host cards must
//! stack.
//!
//! Pure value math, like [`crate::layout`]: these functions decide rows and
//! column counts, never how anything is drawn.

use serde_json::{json, Value};

/// Gap between cockpit cards, matching the grid spacing the shell applies.
pub const SPACING: f64 = 16.0;

/// Minimum comfortable width for one host card. Below this the per-core grid
/// loses its `Core N  xx%` labels and volume mounts truncate, so cards stack
/// instead of squeezing. Two cards therefore need >= 1816pt, three >= 2732pt.
pub const HOST_CARD_MIN_WIDTH: f64 = 900.0;

/// The distinct panels the cockpit can show. The *arrangement* lives separately
/// in [`CockpitLayout`], so a panel never knows where it sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PanelKind {
    Hosts,
    GhRunners,
    Containers,
    GhWorkflows,
    ClaudeUsage,
    OpenclawAgents,
    AzureCost,
}

impl PanelKind {
    /// Every kind, in the Swift `CaseIterable` declaration order.
    pub const ALL: [PanelKind; 7] = [
        PanelKind::Hosts,
        PanelKind::GhRunners,
        PanelKind::Containers,
        PanelKind::GhWorkflows,
        PanelKind::ClaudeUsage,
        PanelKind::OpenclawAgents,
        PanelKind::AzureCost,
    ];

    /// Stable identifier — the Swift `rawValue`, so persisted layout state and
    /// the frontend agree on one spelling.
    pub fn id(self) -> &'static str {
        match self {
            PanelKind::Hosts => "hosts",
            PanelKind::GhRunners => "ghRunners",
            PanelKind::Containers => "containers",
            PanelKind::GhWorkflows => "ghWorkflows",
            PanelKind::ClaudeUsage => "claudeUsage",
            PanelKind::OpenclawAgents => "openclawAgents",
            PanelKind::AzureCost => "azureCost",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            PanelKind::Hosts => "Hosts",
            PanelKind::GhRunners => "GitHub Runners",
            PanelKind::Containers => "Containers / VMs",
            PanelKind::GhWorkflows => "Repos",
            // "Usage", not "Claude Usage": the panel carries per-provider usage
            // beside the Claude token rollups. The id stays `claudeUsage`.
            PanelKind::ClaudeUsage => "Usage",
            PanelKind::OpenclawAgents => "OpenClaw",
            PanelKind::AzureCost => "Azure Cost",
        }
    }

    /// The narrowest width at which this panel still reads well — its personal
    /// breakpoint. [`reflow`] breaks a row apart only when *its* panels stop
    /// fitting, so a lean pair stays side-by-side at a width that forces a
    /// hungrier pair to stack.
    ///
    /// Each figure is the panel's widest fixed content plus its 28pt of card
    /// padding — e.g. Repos sums seven fixed numeric columns (312pt), their
    /// gaps, the status dot and a legible repo name. Widen a panel's content,
    /// widen this number.
    pub fn min_width(self) -> f64 {
        match self {
            PanelKind::Hosts => HOST_CARD_MIN_WIDTH,
            PanelKind::GhWorkflows => 560.0,
            PanelKind::OpenclawAgents => 440.0,
            PanelKind::GhRunners => 400.0,
            PanelKind::Containers => 400.0,
            PanelKind::AzureCost => 400.0,
            PanelKind::ClaudeUsage => 360.0,
        }
    }
}

/// How wide a panel sits within its cockpit row. Only `Full`/`Half` appear in
/// shipped layouts; `Third` exists because the Swift enum has it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelSpan {
    Full,
    Half,
    Third,
}

impl PanelSpan {
    pub fn as_str(self) -> &'static str {
        match self {
            PanelSpan::Full => "full",
            PanelSpan::Half => "half",
            PanelSpan::Third => "third",
        }
    }
}

/// One panel placed in the layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub kind: PanelKind,
    pub span: PanelSpan,
}

impl Placement {
    pub const fn new(kind: PanelKind, span: PanelSpan) -> Self {
        Self { kind, span }
    }
}

/// A data-driven cockpit arrangement: ordered rows of placements. This is the
/// seam that makes layouts swappable without touching any panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CockpitLayout {
    pub name: &'static str,
    pub rows: Vec<Vec<Placement>>,
}

impl CockpitLayout {
    /// Every panel kind present in this layout, flattened in render order.
    pub fn panel_kinds(&self) -> Vec<PanelKind> {
        self.rows.iter().flatten().map(|p| p.kind).collect()
    }

    /// Layout B — "hosts-forward": big host cards on top, work surfaces below.
    pub fn hosts_forward() -> Self {
        Self {
            name: "Hosts-forward",
            rows: vec![
                vec![Placement::new(PanelKind::Hosts, PanelSpan::Full)],
                vec![
                    Placement::new(PanelKind::GhWorkflows, PanelSpan::Half),
                    Placement::new(PanelKind::GhRunners, PanelSpan::Half),
                ],
                vec![
                    Placement::new(PanelKind::Containers, PanelSpan::Half),
                    Placement::new(PanelKind::OpenclawAgents, PanelSpan::Half),
                ],
                vec![
                    Placement::new(PanelKind::ClaudeUsage, PanelSpan::Half),
                    Placement::new(PanelKind::AzureCost, PanelSpan::Half),
                ],
            ],
        }
    }
}

/// Repacks layout rows so no row asks for more width than it has.
///
/// Greedy and order-preserving: panels stay in their authored sequence and are
/// never merged across authored rows — a row only ever splits into more rows.
/// Every placement survives exactly once.
///
/// A non-positive `available` means the width is unknown (a panel rendered
/// outside the cockpit); the authored rows pass through untouched. Unlike
/// [`host_columns`], "as authored" is the safe answer here — panel minimums are
/// small, so an authored pair is never the unreadable option. `NaN` takes the
/// same branch, exactly as Swift's `guard available > 0` does.
pub fn reflow(rows: &[Vec<Placement>], available: f64, spacing: f64) -> Vec<Vec<Placement>> {
    if available.is_nan() || available <= 0.0 {
        return rows.to_vec();
    }
    rows.iter()
        .flat_map(|row| pack(row, available, spacing))
        .collect()
}

/// One authored row -> one or more rendered rows.
fn pack(row: &[Placement], available: f64, spacing: f64) -> Vec<Vec<Placement>> {
    let mut packed: Vec<Vec<Placement>> = Vec::new();
    let mut current: Vec<Placement> = Vec::new();
    let mut current_width = 0.0f64;

    for placement in row {
        let needed = placement.kind.min_width();
        // A lone panel always stays on its row even if it's wider than the
        // window — better an overflowing card than an empty layout.
        if current.is_empty() {
            current.push(*placement);
            current_width = needed;
        } else if current_width + spacing + needed <= available {
            current.push(*placement);
            current_width += spacing + needed;
        } else {
            packed.push(std::mem::take(&mut current));
            current.push(*placement);
            current_width = needed;
        }
    }
    if !current.is_empty() {
        packed.push(current);
    }
    packed
}

/// How many host cards fit across `available` points, capped at `host_count`.
///
/// The `auto-fit` formula: each card occupies a slot of `min_card_width +
/// spacing`, and the trailing card needs no gap after it — hence the `+
/// spacing` on the numerator. Returns 1 when nothing fits side-by-side, which
/// is the caller's cue to stack (or tab).
///
/// **Unknown width (`available <= 0`) stacks.** An earlier version assumed wide
/// here, which quietly turned a dead width reading into a cockpit of crushed
/// cards that looked deliberate. Stacking can never be unreadable, and it's
/// visibly wrong on a big display, so a broken measurement gets reported
/// instead of tolerated.
pub fn host_columns(available: f64, host_count: usize, min_card_width: f64, spacing: f64) -> usize {
    if host_count == 0 || available.is_nan() || available <= 0.0 {
        return 1;
    }
    let slot = min_card_width + spacing;
    if slot <= 0.0 {
        return 1;
    }
    let fits = ((available + spacing) / slot).floor();
    // A saturating cast: an infinite `available` lands on `usize::MAX` and is
    // then clamped by `host_count`, never wrapping to zero.
    let fits = if fits >= 1.0 { fits as usize } else { 1 };
    fits.min(host_count)
}

/// Minimum height of the tabbed host container, in points.
///
/// `HostsPanel.tabbedHosts`'s `.frame(minHeight: 780)` in Swift, and it carries
/// the same intent: only one card is on screen at a time, so the container has
/// nothing to size itself against and would otherwise collapse to the height of
/// its tab bar.
pub const HOST_TABS_MIN_HEIGHT: f64 = 780.0;

/// Whether the host cards collapse into a tab bar rather than stacking.
///
/// The three conditions are `HostsPanel.content`'s, in the same order and for
/// the same reasons: **below the side-by-side breakpoint** (`columns <= 1`,
/// which is [`host_columns`]'s answer and never re-derived here), **more than
/// one host** (a tab bar over a single card is chrome around nothing), and the
/// user having asked for it. Stacking stays the default, so an unset — or
/// unreadable — preference keeps every host visible, which is the point of an
/// always-on cockpit.
#[must_use]
pub fn host_tabs(columns: usize, host_count: usize, prefers_tabs: bool) -> bool {
    prefers_tabs && columns <= 1 && host_count > 1
}

/// Splits items into rows of at most `columns`. `columns <= 1` gives one item
/// per row — the stacked case.
pub fn rows<T: Clone>(items: &[T], columns: usize) -> Vec<Vec<T>> {
    if columns <= 1 {
        return items.iter().map(|i| vec![i.clone()]).collect();
    }
    items.chunks(columns).map(<[T]>::to_vec).collect()
}

/// The panel table as data, in [`PanelKind::ALL`] order — the frontend reads
/// its titles and breakpoints from here rather than restating them.
pub fn panel_table() -> Value {
    Value::Array(
        PanelKind::ALL
            .iter()
            .map(|k| {
                json!({
                    "id": k.id(),
                    "title": k.title(),
                    "minWidth": k.min_width(),
                })
            })
            .collect(),
    )
}

/// `CockpitTheme` as painted hex, keyed by the Swift member names.
pub fn theme() -> Value {
    use crate::color;
    json!({
        "background": color::hex(color::BACKGROUND),
        "panel": color::hex(color::PANEL),
        "panelAlt": color::hex(color::PANEL_ALT),
        "line": color::hex(color::LINE),
        "green": color::hex(color::GREEN),
        "greenDim": color::hex(color::GREEN_DIM),
        "amber": color::hex(color::AMBER),
        "red": color::hex(color::RED),
        "muted": color::hex(color::MUTED),
        "ink": color::hex(color::INK),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rows as panel kinds — the kinds are what the assertions are about.
    fn kinds(rows: &[Vec<Placement>]) -> Vec<Vec<PanelKind>> {
        rows.iter()
            .map(|r| r.iter().map(|p| p.kind).collect())
            .collect()
    }

    fn layout() -> CockpitLayout {
        CockpitLayout::hosts_forward()
    }

    fn reflowed(available: f64) -> Vec<Vec<PanelKind>> {
        kinds(&reflow(&layout().rows, available, SPACING))
    }

    // MARK: reflow

    /// The reported case: a ~1568pt portrait window (1528pt of content). Every
    /// authored pair still fits, so the cockpit rows must not move — only the
    /// host cards inside the Hosts panel stack. See `host_columns` below.
    #[test]
    fn portrait_width_leaves_every_panel_row_paired() {
        assert_eq!(reflowed(1528.0), kinds(&layout().rows));
    }

    /// At the 880pt window floor (840pt of content) the two hungrier pairs
    /// break apart, while Usage + Azure Cost (360 + 16 + 400 = 776) stay paired.
    #[test]
    fn window_floor_breaks_only_the_rows_that_dont_fit() {
        assert_eq!(
            reflowed(840.0),
            vec![
                vec![PanelKind::Hosts],
                vec![PanelKind::GhWorkflows], // 560 + 16 + 400 = 976 > 840
                vec![PanelKind::GhRunners],
                vec![PanelKind::Containers], // 400 + 16 + 440 = 856 > 840
                vec![PanelKind::OpenclawAgents],
                vec![PanelKind::ClaudeUsage, PanelKind::AzureCost], // 776 <= 840
            ]
        );
    }

    /// Boundary: Repos + Runners need exactly 976pt.
    #[test]
    fn row_splits_once_below_its_exact_requirement() {
        assert_eq!(
            reflowed(976.0)[1],
            vec![PanelKind::GhWorkflows, PanelKind::GhRunners],
            "976pt is enough"
        );
        assert_eq!(
            reflowed(975.0)[1],
            vec![PanelKind::GhWorkflows],
            "975pt is not"
        );
    }

    /// Width unknown on first render — pass the authored rows through untouched
    /// rather than flashing a stacked layout.
    #[test]
    fn unmeasured_width_passes_rows_through() {
        assert_eq!(reflowed(0.0), kinds(&layout().rows));
        assert_eq!(reflowed(f64::NAN), kinds(&layout().rows));
    }

    /// A panel wider than the window still gets its own row rather than
    /// vanishing.
    #[test]
    fn panel_wider_than_available_still_renders() {
        let rows = reflowed(100.0);
        assert_eq!(rows.len(), PanelKind::ALL.len(), "one panel per row");
        assert!(rows.iter().all(|r| r.len() == 1));
    }

    /// Reflow only ever splits rows: no panel is dropped, duplicated, or
    /// reordered, and no row comes back empty — at any width.
    #[test]
    fn no_panel_lost_duplicated_or_reordered() {
        let authored = layout().panel_kinds();

        let mut width = 100.0f64;
        while width <= 3000.0 {
            let rows = reflow(&layout().rows, width, SPACING);
            let flat: Vec<PanelKind> = rows.iter().flatten().map(|p| p.kind).collect();

            assert_eq!(
                flat, authored,
                "order and membership must survive at {width}pt"
            );
            assert!(!rows.iter().any(|r| r.is_empty()), "empty row at {width}pt");
            width += 50.0;
        }
    }

    /// Reflow never merges across authored rows, even when two would fit
    /// together — the layout's pairings are intent, not a packing suggestion.
    #[test]
    fn wide_widths_never_merge_authored_rows() {
        assert_eq!(reflowed(5000.0), kinds(&layout().rows));
    }

    // MARK: host_columns

    fn cols(available: f64, host_count: usize) -> usize {
        host_columns(available, host_count, HOST_CARD_MIN_WIDTH, SPACING)
    }

    /// The bug this math fixes: two cards at 1528pt would be ~752pt each.
    #[test]
    fn two_hosts_stack_on_a_portrait_window() {
        assert_eq!(cols(1528.0, 2), 1);
    }

    #[test]
    fn two_hosts_pair_up_once_both_cards_clear_the_minimum() {
        // 2 * 900 + 16 = 1816
        assert_eq!(cols(1815.0, 2), 1);
        assert_eq!(cols(1816.0, 2), 2);
        assert_eq!(cols(1880.0, 2), 2);
    }

    #[test]
    fn three_hosts_need_an_ultrawide() {
        // 3 * 900 + 2 * 16 = 2732
        assert_eq!(cols(2731.0, 3), 2);
        assert_eq!(cols(2732.0, 3), 3);
    }

    #[test]
    fn columns_never_exceed_host_count_or_drop_below_one() {
        assert_eq!(cols(5000.0, 1), 1);
        assert_eq!(cols(5000.0, 2), 2);
        assert_eq!(cols(100.0, 4), 1);
        assert_eq!(cols(1000.0, 0), 1);
    }

    /// Regression guard: an unknown width must NOT be treated as "wide enough".
    /// Assuming wide is what let a dead width reading masquerade as a
    /// deliberate (unreadable) side-by-side layout. Stacking always reads.
    #[test]
    fn unknown_width_stacks_rather_than_assuming_wide() {
        assert_eq!(cols(0.0, 3), 1);
        assert_eq!(cols(f64::NAN, 3), 1);
    }

    // MARK: host_tabs

    /// The whole point of the mode: below the breakpoint, tabs replace the
    /// stack — and only there.
    #[test]
    fn tabs_replace_the_stack_only_below_the_breakpoint() {
        assert!(host_tabs(1, 3, true), "one column, three hosts");
        assert!(
            !host_tabs(2, 3, true),
            "two columns still fit side by side, so there is nothing to collapse"
        );
        assert!(!host_tabs(3, 3, true));
    }

    /// A tab bar over one card is chrome around nothing, and the local card
    /// alone is the fresh-install cockpit.
    #[test]
    fn one_host_never_gets_a_tab_bar() {
        assert!(!host_tabs(1, 1, true));
        assert!(!host_tabs(1, 0, true));
    }

    /// Stacking is the default, and the fallback: an unset or unreadable
    /// preference reads as `Stack` one layer down, and this must honour it.
    #[test]
    fn stacking_stays_the_default() {
        for columns in [0, 1, 2, 3] {
            for hosts in [0, 1, 2, 5] {
                assert!(
                    !host_tabs(columns, hosts, false),
                    "{columns} columns, {hosts} hosts"
                );
            }
        }
    }

    /// The tabbed container needs a floor, or it collapses to its tab bar:
    /// one card is on screen at a time, so there is nothing else sizing it.
    #[test]
    fn the_tabbed_container_declares_a_positive_minimum_height() {
        assert_eq!(HOST_TABS_MIN_HEIGHT, 780.0);
    }

    // MARK: rows

    #[test]
    fn rows_chunk_by_column_count() {
        assert_eq!(rows(&[1, 2, 3, 4], 2), vec![vec![1, 2], vec![3, 4]]);
        assert_eq!(
            rows(&[1, 2, 3], 2),
            vec![vec![1, 2], vec![3]],
            "short last row"
        );
        assert_eq!(rows(&[1, 2, 3], 3), vec![vec![1, 2, 3]]);
    }

    #[test]
    fn single_column_gives_one_item_per_row() {
        assert_eq!(rows(&[1, 2, 3], 1), vec![vec![1], vec![2], vec![3]]);
        assert_eq!(
            rows(&[1, 2], 0),
            vec![vec![1], vec![2]],
            "0 columns degrades to stacked"
        );
    }

    #[test]
    fn rows_of_nothing_is_no_rows() {
        assert!(rows::<i32>(&[], 2).is_empty());
    }

    // MARK: the panel table (CockpitLayoutTests)

    /// The shipped layout must render every panel exactly once — guards against
    /// dropping or duplicating a panel when arranging rows.
    #[test]
    fn hosts_forward_layout_contains_every_panel_exactly_once() {
        let mut placed = layout().panel_kinds();
        placed.sort();
        let mut expected = PanelKind::ALL.to_vec();
        expected.sort();
        assert_eq!(placed, expected);
    }

    #[test]
    fn layout_rows_are_non_empty() {
        assert!(!layout().rows.is_empty());
        assert!(layout().rows.iter().all(|r| !r.is_empty()));
    }

    /// Every panel declares its own breakpoint. A new kind that forgets one
    /// would reflow against a zero minimum and never break out of a cramped row.
    #[test]
    fn every_panel_kind_declares_a_positive_min_width() {
        for kind in PanelKind::ALL {
            assert!(kind.min_width() > 0.0, "{} needs a minWidth", kind.id());
        }
    }

    /// Ids and titles are the frontend's vocabulary: a duplicate in either
    /// would silently collapse two panels into one.
    #[test]
    fn panel_ids_and_titles_are_distinct() {
        for (i, a) in PanelKind::ALL.iter().enumerate() {
            for b in PanelKind::ALL.iter().skip(i + 1) {
                assert_ne!(a.id(), b.id());
                assert_ne!(a.title(), b.title());
            }
        }
    }

    #[test]
    fn the_panel_table_travels_as_data() {
        let table = panel_table();
        let entries = table.as_array().unwrap();
        assert_eq!(entries.len(), 7);
        assert_eq!(entries[0]["id"], "hosts");
        assert_eq!(entries[0]["title"], "Hosts");
        assert_eq!(entries[0]["minWidth"], 900.0);
        // `ghWorkflows` renders as "Repos" — the id and the title diverge on
        // purpose, so a table built from ids alone would be wrong.
        assert_eq!(entries[3]["id"], "ghWorkflows");
        assert_eq!(entries[3]["title"], "Repos");
        assert_eq!(entries[3]["minWidth"], 560.0);
    }

    #[test]
    fn the_theme_travels_as_painted_hex() {
        let t = theme();
        assert_eq!(t["background"], "#000000");
        assert_eq!(t["panel"], "#050805");
        assert_eq!(t["panelAlt"], "#0a0f0c");
        assert_eq!(t["line"], "#13301f");
        assert_eq!(t["green"], "#33d17a");
        assert_eq!(t["greenDim"], "#1c6b41");
        assert_eq!(t["amber"], "#e09a26");
        assert_eq!(t["red"], "#e05a4f");
        assert_eq!(t["muted"], "#5a6b60");
        assert_eq!(t["ink"], "#cfe9d8");
    }

    #[test]
    fn spans_carry_a_wire_name() {
        assert_eq!(PanelSpan::Full.as_str(), "full");
        assert_eq!(PanelSpan::Half.as_str(), "half");
        assert_eq!(PanelSpan::Third.as_str(), "third");
        // The shipped layout is one full-width row of hosts over three pairs.
        let l = layout();
        assert_eq!(l.rows[0][0].span, PanelSpan::Full);
        assert!(l.rows[1..]
            .iter()
            .all(|r| r.iter().all(|p| p.span == PanelSpan::Half)));
    }
}
