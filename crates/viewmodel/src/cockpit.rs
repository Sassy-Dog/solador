//! The sizing a tile's Full view needs: which panels exist ([`PanelKind`] and
//! each one's [`PanelKind::min_width`]), how many host cards fit across a width
//! ([`host_columns`]), when they collapse into tabs ([`host_tabs`]) and how many
//! content columns a single panel affords ([`panel_columns`]). The palette lives
//! in [`crate::color`]; [`theme`] hands it out as painted hex.
//!
//! Modelled on CSS `repeat(auto-fit, minmax(<min>, 1fr))` rather than on global
//! `sm`/`md`/`lg` tiers: **each panel declares its own [`PanelKind::min_width`]**
//! and breaks only when *its* content stops fitting. A tile's Full view is one
//! panel across the window, so there is no arrangement of panels to decide here
//! (Solador's Full view, which had one, is gone, #647).
//!
//! Pure value math, like [`crate::layout`]: these functions decide column
//! counts, never how anything is drawn.

use serde_json::{json, Value};

/// Gap between cockpit cards, matching the grid spacing the shell applies.
pub const SPACING: f64 = 16.0;

/// Minimum comfortable width for one host card. Below this the per-core grid
/// loses its `Core N  xx%` labels and volume mounts truncate, so cards stack
/// instead of squeezing. Two cards therefore need >= 1816pt, three >= 2732pt.
pub const HOST_CARD_MIN_WIDTH: f64 = 900.0;

/// The distinct panels there are, one per source. A tile's Full view shows one
/// of them across the window; a panel never knows where it sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PanelKind {
    Hosts,
    GhRunners,
    Containers,
    GhWorkflows,
    ClaudeUsage,
    OpenclawAgents,
    AzureCost,
    Services,
    SentryCrons,
}

impl PanelKind {
    /// Every kind, in the original `CaseIterable` declaration order — with the
    /// panels that have no counterpart appended in the order they were added.
    pub const ALL: [PanelKind; 9] = [
        PanelKind::Hosts,
        PanelKind::GhRunners,
        PanelKind::Containers,
        PanelKind::GhWorkflows,
        PanelKind::ClaudeUsage,
        PanelKind::OpenclawAgents,
        PanelKind::AzureCost,
        PanelKind::Services,
        PanelKind::SentryCrons,
    ];

    /// Stable identifier — the original `rawValue`, so persisted layout state and
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
            PanelKind::Services => "services",
            PanelKind::SentryCrons => "sentryCrons",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            PanelKind::Hosts => "Hosts",
            PanelKind::GhRunners => "GitHub Runners",
            PanelKind::Containers => "Containers / VMs",
            PanelKind::GhWorkflows => "GitHub Repos",
            // "Usage", not "Claude Usage": the panel carries per-provider usage
            // beside the Claude token rollups. The id stays `claudeUsage`.
            PanelKind::ClaudeUsage => "Usage",
            PanelKind::OpenclawAgents => "OpenClaw",
            PanelKind::AzureCost => "Azure Cost",
            PanelKind::Services => "Services",
            // "Sentry Crons", not "Crons": the Services panel beside it answers
            // "is the vendor up?" and this one answers "is our own job green?",
            // and naming the source is what keeps the two apart at a glance.
            PanelKind::SentryCrons => "Sentry Crons",
        }
    }

    /// The narrowest width at which this panel still reads well — its personal
    /// breakpoint. [`panel_columns`] splits a panel into columns only when
    /// *its* content has room for them.
    ///
    /// Each figure is the panel's widest fixed content plus its card padding —
    /// e.g. Repos sums eight fixed numeric columns (232pt), their gaps, the
    /// status dot and a 96pt name reservation. Widen a panel's content, widen
    /// this number.
    ///
    /// It is also the width **one content column** needs, which is what
    /// [`panel_columns`] reads it as. Repos was 560 while its columns carried
    /// half again the width their labels needed; sizing them to their text
    /// took it to 440 — 458 since READY joined the row — which is what lets it
    /// hold two columns on a display where it previously could not.
    ///
    /// What the number decides today: [`panel_columns`] gives a panel a second
    /// content column once its width (a tile's Full view is the window) holds
    /// two of these plus a gutter, so an overstated figure keeps a panel in one
    /// column on a window that could carry two, and an understated one splits a
    /// list into columns too narrow to scan.
    ///
    /// OpenClaw's and Usage's figures were inherited guesses until they were
    /// measured against their dumped fixtures, by shrinking the panel until
    /// something that must not truncate does:
    ///
    /// - **OpenClaw, 240pt (230 measured).** Its agent rows put the name and the
    ///   model ref on separate lines, so the two no longer compete for one
    ///   line's width and the binding constraint is the cron summary. It was 340
    ///   while they shared a line. (History: the figure also decided how small a
    ///   card OpenClaw could share a row with Containers on the retired span
    ///   grid.)
    /// - **Usage, 340pt (300 measured).** Below 300 its labels and project names
    ///   clip. The figure is deliberately above the measurement: every row is a
    ///   label paired with a right-aligned value, and the fixture's longest
    ///   label is not the longest a provider can produce. It also sets where
    ///   Usage reaches a second content column (two of it plus a gutter, 696pt).
    pub fn min_width(self) -> f64 {
        match self {
            PanelKind::Hosts => HOST_CARD_MIN_WIDTH,
            PanelKind::GhWorkflows => 458.0,
            PanelKind::GhRunners => 400.0,
            PanelKind::Containers => 400.0,
            PanelKind::AzureCost => 400.0,
            PanelKind::OpenclawAgents => 240.0,
            PanelKind::ClaudeUsage => 340.0,
            // Five rows of `vendor · status word · dot`. The binding
            // constraint is "Services Degraded" beside the longest vendor
            // name. 240 rather than the ~220 that measures, for the same
            // reason OpenClaw is 240: it is also the width *one content
            // column* needs, and at 220 a window of 456pt would already split
            // five one-line rows into two columns — a split that buys a
            // gutter and costs the scan.
            PanelKind::Services => 240.0,
            // Two-line rows: monitor slug plus its age, then `project/env` and
            // the state word. Same construction as OpenClaw's agent rows and the
            // same figure for the same reason — the binding constraint is a
            // 240pt content column, and at less than that a narrow window
            // would split a list of one-or-two failing monitors into two
            // columns. Splitting the name onto its own line is what keeps this at
            // 240 rather than the ~400 a single line of
            // `slug · platform/prd · error · 7d 22h` would need.
            PanelKind::SentryCrons => 240.0,
        }
    }
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

/// The most content columns a panel of `width` can hold, capped at
/// [`PANEL_MAX_COLUMNS`].
///
/// Same `auto-fit` arithmetic as [`host_columns`], over the panel's own
/// [`PanelKind::min_width`]: the width at which this panel's content still
/// reads well is exactly the width one *column* of it needs. That keeps the
/// breakpoint in one place — widen a panel's columns and `min_width` moves,
/// and this follows automatically.
///
/// Slightly conservative on purpose: `min_width` includes the panel's own
/// chrome, which a second column does not repeat. Over-reserving errs toward
/// one legible column instead of two cramped ones, and it avoids restating
/// `.panel`'s padding here where it could drift from the CSS.
#[must_use]
pub fn panel_columns(kind: PanelKind, width: f64, spacing: f64) -> usize {
    if width.is_nan() || width <= 0.0 {
        return 1;
    }
    let slot = kind.min_width() + spacing;
    if slot <= 0.0 {
        return 1;
    }
    let fits = ((width + spacing) / slot).floor();
    let fits = if fits >= 1.0 { fits as usize } else { 1 };
    fits.min(PANEL_MAX_COLUMNS)
}

/// Two. A third column of a short list costs more in scanning than the gutter
/// it reclaims, and every panel this applies to is a list.
pub const PANEL_MAX_COLUMNS: usize = 2;

/// Minimum height of the tabbed host container, in points.
///
/// `HostsPanel.tabbedHosts`'s `.frame(minHeight: 780)` in the original, and it carries
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

/// `CockpitTheme` as painted hex, keyed by the original member names.
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

    // MARK: panel_columns

    /// Each panel splits at twice its own `min_width` plus the gap — the
    /// breakpoint lives in exactly one place, so widening a panel's content
    /// moves both numbers together.
    #[test]
    fn a_panel_splits_at_twice_its_own_minimum() {
        // Runners and Containers: 2 * 400 + 16 = 816
        assert_eq!(panel_columns(PanelKind::GhRunners, 815.0, SPACING), 1);
        assert_eq!(panel_columns(PanelKind::GhRunners, 816.0, SPACING), 2);
        assert_eq!(panel_columns(PanelKind::Containers, 815.0, SPACING), 1);
        assert_eq!(panel_columns(PanelKind::Containers, 816.0, SPACING), 2);
        // Repos carries 232pt of fixed numeric columns plus a 96pt name, so
        // its minimum is 458 and it pairs at 2 * 458 + 16 = 932.
        assert_eq!(panel_columns(PanelKind::GhWorkflows, 931.0, SPACING), 1);
        assert_eq!(panel_columns(PanelKind::GhWorkflows, 932.0, SPACING), 2);
    }

    /// Capped at two: a third column of a short list costs more scanning than
    /// the gutter it wins back.
    #[test]
    fn columns_are_capped_at_two_however_wide_the_panel() {
        assert_eq!(panel_columns(PanelKind::GhRunners, 5000.0, SPACING), 2);
        assert_eq!(
            panel_columns(PanelKind::ClaudeUsage, f64::INFINITY, SPACING),
            2
        );
    }

    /// Same rule as `host_columns`: an unmeasured panel gets one column rather
    /// than an assumed-wide two.
    #[test]
    fn an_unmeasured_panel_gets_one_column() {
        for kind in PanelKind::ALL {
            assert_eq!(panel_columns(kind, 0.0, SPACING), 1, "{kind:?} at 0");
            assert_eq!(panel_columns(kind, f64::NAN, SPACING), 1, "{kind:?} at NaN");
            assert!(
                panel_columns(kind, 1200.0, SPACING) >= 1,
                "{kind:?} must always have at least one column"
            );
        }
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

    // MARK: the panel table

    /// Every panel declares its own breakpoint. A new kind that forgets one
    /// would split against a zero minimum and never leave a cramped column.
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
    fn the_theme_travels_as_painted_hex() {
        let t = theme();
        assert_eq!(t["background"], "#000000");
        assert_eq!(t["panel"], crate::color::hex(crate::color::PANEL));
        assert_eq!(t["panelAlt"], crate::color::hex(crate::color::PANEL_ALT));
        assert_eq!(t["line"], crate::color::hex(crate::color::LINE));
        assert_eq!(t["green"], crate::color::hex(crate::color::GREEN));
        assert_eq!(t["greenDim"], crate::color::hex(crate::color::GREEN_DIM));
        assert_eq!(t["amber"], crate::color::hex(crate::color::AMBER));
        assert_eq!(t["red"], crate::color::hex(crate::color::RED));
        assert_eq!(t["muted"], crate::color::hex(crate::color::MUTED));
        assert_eq!(t["ink"], crate::color::hex(crate::color::INK));
    }
}
