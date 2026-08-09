//! The cockpit grid layer, ported from `DevCanopy/Views/Cockpit/`:
//! `CockpitBreakpoints.swift` (reflow + host columns) and `CockpitPanel.swift`
//! (the panel table and the shipped arrangement). The palette those two share
//! lives in [`crate::color`]; [`theme`] hands it out as painted hex.
//!
//! Modelled on CSS `repeat(auto-fit, minmax(<min>, 1fr))` rather than on global
//! `sm`/`md`/`lg` tiers: **each panel declares its own [`PanelKind::min_width`]**
//! and a row breaks only when *its* panels stop fitting. Named tiers can't
//! express the case that actually matters here — the lean OpenClaw + Usage pair
//! still sits side-by-side at a width where the much hungrier host cards must
//! stack.
//!
//! A row is not an even split either. Every placement carries a [`PanelSpan`]
//! whose [`PanelSpan::weight`] is its share of the row, so Containers takes
//! half a row while OpenClaw and Usage take a quarter each. [`panel_widths`] is
//! the only place that arithmetic lives; the frontend paints the same weights
//! as CSS `fr` tracks.
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
    Services,
    SentryCrons,
}

impl PanelKind {
    /// Every kind, in the Swift `CaseIterable` declaration order — with the
    /// panels that have no Swift twin appended in the order they were added.
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
            PanelKind::Services => "services",
            PanelKind::SentryCrons => "sentryCrons",
        }
    }

    /// Parses an id a *stored layout* or an editor sent, strictly.
    ///
    /// Unknown means unknown: a panel this build has never heard of is a file
    /// written by a newer one, and inventing a substitute would silently move
    /// someone's cockpit around. The caller decides what to do about it (the
    /// app drops the slot and fills the gap from the default order).
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        PanelKind::ALL.into_iter().find(|kind| kind.id() == raw)
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
    /// breakpoint. [`reflow`] breaks a row apart only when *its* panels stop
    /// fitting, so a lean pair stays side-by-side at a width that forces a
    /// hungrier pair to stack.
    ///
    /// Each figure is the panel's widest fixed content plus its card padding —
    /// e.g. Repos sums seven fixed numeric columns (214pt), their gaps, the
    /// status dot and a 96pt name reservation. Widen a panel's content, widen
    /// this number.
    ///
    /// It is also the width **one content column** needs, which is what
    /// [`panel_columns`] reads it as. Repos was 560 while its columns carried
    /// half again the width their labels needed; sizing them to their text
    /// took it to 440, which is what lets it hold two columns on a display
    /// where it previously could not.
    ///
    /// OpenClaw's and Usage's figures were inherited guesses until spans made
    /// them load-bearing — a quarter-width panel is held to its minimum against
    /// a quarter of the row, so an overstated one splits a row that reads fine.
    /// Both are measured against their dumped fixtures, by shrinking the panel
    /// until something that must not truncate does:
    ///
    /// - **OpenClaw, 230pt.** Its agent rows put the name and the model ref on
    ///   separate lines, so the two no longer compete for one line's width and
    ///   the binding constraint is the cron summary. It was 340 while they
    ///   shared a line, which is what kept it out of a quarter beside a
    ///   three-quarter Containers on a ~1256pt cockpit.
    /// - **Usage, 300pt.** Below that its labels and project names clip. The
    ///   figure here is 340, deliberately above the measurement: every row is a
    ///   label paired with a right-aligned value, and the fixture's longest
    ///   label is not the longest a provider can produce. It is also the number
    ///   that decides whether Containers keeps a whole row's worth of width at
    ///   ~1256pt — drop it to 300 and the three-panel row re-forms there, with
    ///   Containers back to a single content column.
    pub fn min_width(self) -> f64 {
        match self {
            PanelKind::Hosts => HOST_CARD_MIN_WIDTH,
            PanelKind::GhWorkflows => 440.0,
            PanelKind::GhRunners => 400.0,
            PanelKind::Containers => 400.0,
            PanelKind::AzureCost => 400.0,
            PanelKind::OpenclawAgents => 240.0,
            PanelKind::ClaudeUsage => 340.0,
            // Five rows of `vendor · status word · dot`. The binding
            // constraint is "Services Degraded" beside the longest vendor
            // name. 240 rather than the ~220 that measures, for the same
            // reason OpenClaw is 240: it is also the width *one content
            // column* needs, and at 220 a quarter of an 1890pt cockpit would
            // split five one-line rows into two columns — a split that buys a
            // gutter and costs the scan.
            PanelKind::Services => 240.0,
            // Two-line rows: monitor slug plus its age, then `project/env` and
            // the state word. Same construction as OpenClaw's agent rows and the
            // same figure for the same reason — the binding constraint is a
            // 240pt content column, and at less than that a quarter of an 1890pt
            // cockpit would split a list of one-or-two failing monitors into two
            // columns. Splitting the name onto its own line is what keeps this at
            // 240 rather than the ~400 a single line of
            // `slug · platform/prd · error · 7d 22h` would need.
            PanelKind::SentryCrons => 240.0,
        }
    }
}

/// How wide a panel sits within its cockpit row, as a share of it: `Full` is the
/// whole row, `ThreeQuarters` three of its four quarters, `Half` two, `Quarter`
/// one. The frontend paints these as CSS `fr` tracks, so `Half + Quarter +
/// Quarter` is `2fr 1fr 1fr`.
///
/// Quarters, and only quarters: every rendered row adds up to exactly four of
/// them ([`fill_row`]), so a track is always one of these four widths. A row
/// that rendered at some other fraction — two thirds, say — would be a width no
/// editor can author and no user can name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelSpan {
    Full,
    ThreeQuarters,
    Half,
    Quarter,
}

impl PanelSpan {
    pub fn as_str(self) -> &'static str {
        match self {
            PanelSpan::Full => "full",
            PanelSpan::ThreeQuarters => "threeQuarters",
            PanelSpan::Half => "half",
            PanelSpan::Quarter => "quarter",
        }
    }

    /// Every span, widest first — the order an editor's picker offers them in.
    pub const ALL: [PanelSpan; 4] = [
        PanelSpan::Full,
        PanelSpan::ThreeQuarters,
        PanelSpan::Half,
        PanelSpan::Quarter,
    ];

    /// Parses a span a *stored layout* or an editor sent, strictly. Same
    /// argument as [`PanelKind::parse`]: silently substituting a width nobody
    /// picked is worse than declining the slot.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        PanelSpan::ALL.into_iter().find(|span| span.as_str() == raw)
    }

    /// The label an editor shows for this width.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            PanelSpan::Full => "Full width",
            PanelSpan::ThreeQuarters => "Three-quarters",
            PanelSpan::Half => "Half",
            PanelSpan::Quarter => "Quarter",
        }
    }

    /// The span's share of a row, in quarters.
    ///
    /// A rendered row always holds exactly four of them — [`fill_row`] widens
    /// the spans of a row reflow left short until it does — so the weight is
    /// literally "how many quarters of this row", and [`panel_widths`] can
    /// divide by the row's own total without ever producing a fraction that
    /// is not a quarter.
    #[must_use]
    pub fn weight(self) -> u32 {
        match self {
            PanelSpan::Full => 4,
            PanelSpan::ThreeQuarters => 3,
            PanelSpan::Half => 2,
            PanelSpan::Quarter => 1,
        }
    }

    /// The span carrying `weight` quarters, if one does.
    #[must_use]
    pub fn from_weight(weight: u32) -> Option<Self> {
        PanelSpan::ALL
            .into_iter()
            .find(|span| span.weight() == weight)
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

    /// The shipped arrangement, in one ordered list — the seam the Settings
    /// editor and the stored layout both speak. Rows are *derived* from it by
    /// [`CockpitLayout::from_order`], never authored twice.
    ///
    /// The lean panels are quarters beside a bigger neighbour rather than halves
    /// of their own rows: OpenClaw is four lines, Usage is a label/value stack and
    /// Services and Sentry Crons are short lists, so a half-row each left the row
    /// ragged and the width unspent. Both of the last two rows are therefore the
    /// same shape — a Half and two Quarters — which is also what puts their
    /// gridlines on top of each other.
    ///
    /// Azure Cost keeps enough width to put its top-resource breakdowns beside the
    /// costs rather than under them (`--panel-cols`, from [`panel_columns`]), but
    /// only just; see the note on its slot below.
    pub const DEFAULT_ORDER: [(PanelKind, PanelSpan); 9] = [
        (PanelKind::Hosts, PanelSpan::Full),
        (PanelKind::GhWorkflows, PanelSpan::Half),
        (PanelKind::GhRunners, PanelSpan::Half),
        (PanelKind::Containers, PanelSpan::Half),
        (PanelKind::OpenclawAgents, PanelSpan::Quarter),
        (PanelKind::ClaudeUsage, PanelSpan::Quarter),
        // Azure Cost gives up its quarters to the two lean panels rather than
        // either of them taking a row of its own: a handful of one-line rows
        // would leave most of a full-width band empty. It was Three-quarters
        // beside Services alone and is a Half now that Sentry Crons shares the
        // row — a deliberate trade, measured rather than estimated. Azure Cost's
        // body splits at twice its own minimum plus the gap (2 * 400 + 16 =
        // 816pt), so at Three-quarters it held two content columns down to a
        // 1094pt cockpit and at Half it holds them only down to 1648pt. Below
        // that it drops to one column earlier than it used to —
        // `azure_cost_holds_two_columns_down_to_1648pt_at_half_width` pins both
        // boundaries so a future span change has to look at them.
        (PanelKind::AzureCost, PanelSpan::Half),
        (PanelKind::Services, PanelSpan::Quarter),
        (PanelKind::SentryCrons, PanelSpan::Quarter),
    ];

    /// Layout B — "hosts-forward": big host cards on top, work surfaces below.
    /// The shipped default, and what a store with no layout of its own renders.
    pub fn hosts_forward() -> Self {
        Self::from_order("Hosts-forward", &Self::DEFAULT_ORDER)
    }

    /// Packs an ordered list of panels into rows: each row takes placements
    /// until the next one would push it past a full row's four quarters.
    ///
    /// This is why the persisted layout is a *list* and not rows. A user
    /// reordering panels or widening one should not also have to say where the
    /// rows break — that answer is arithmetic, and it is the same arithmetic
    /// CSS does when a flex line wraps. `reflow` still runs afterwards, so a
    /// row packed here can still split on a narrow window.
    ///
    /// A placement wider than a whole row cannot exist ([`PanelSpan::Full`] is
    /// the maximum), so no slot can be dropped for not fitting anywhere.
    pub fn from_order(name: &'static str, order: &[(PanelKind, PanelSpan)]) -> Self {
        let mut rows: Vec<Vec<Placement>> = Vec::new();
        let mut current: Vec<Placement> = Vec::new();
        let mut weight = 0;
        for (kind, span) in order {
            if !current.is_empty() && weight + span.weight() > PanelSpan::Full.weight() {
                rows.push(std::mem::take(&mut current));
                weight = 0;
            }
            current.push(Placement::new(*kind, *span));
            weight += span.weight();
        }
        if !current.is_empty() {
            rows.push(current);
        }
        Self { name, rows }
    }

    /// The layout's ordered list — the inverse of [`CockpitLayout::from_order`],
    /// and what the Settings editor and the store persist.
    pub fn order(&self) -> Vec<(PanelKind, PanelSpan)> {
        self.rows
            .iter()
            .flatten()
            .map(|placement| (placement.kind, placement.span))
            .collect()
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

/// One authored row -> one or more rendered rows, each one **widened to fill**.
fn pack(row: &[Placement], available: f64, spacing: f64) -> Vec<Vec<Placement>> {
    let mut packed: Vec<Vec<Placement>> = Vec::new();
    let mut current: Vec<Placement> = Vec::new();

    // Closing a row is where the authored spans become the rendered ones: a row
    // reflow cut short is short of a full four quarters, and `fill_row` is what
    // decides who grows into the gap.
    let close = |row: Vec<Placement>| filled(&row, available, spacing);

    for placement in row {
        // A lone panel always stays on its row even if it's wider than the
        // window — better an overflowing card than an empty layout.
        if current.is_empty() {
            current.push(*placement);
            continue;
        }
        current.push(*placement);
        if fill_row(&current, available, spacing).is_none() {
            current.pop();
            packed.push(close(std::mem::take(&mut current)));
            current.push(*placement);
        }
    }
    if !current.is_empty() {
        packed.push(close(current));
    }
    packed
}

/// [`fill_row`]'s answer, or the row widened regardless when nothing fits.
///
/// A row reaches this having already been accepted, with one exception: the
/// lone panel `pack` never splits off. That one renders full width even in a
/// window narrower than its own minimum, which is the same "an overflowing card
/// beats an empty layout" rule `pack` follows.
fn filled(row: &[Placement], available: f64, spacing: f64) -> Vec<Placement> {
    fill_row(row, available, spacing).unwrap_or_else(|| {
        let mut widened = row.to_vec();
        if let [only] = widened.as_mut_slice() {
            only.span = PanelSpan::Full;
        }
        widened
    })
}

/// Widens a candidate row's spans until they add up to a whole row, or reports
/// that no way of doing so leaves every panel legible.
///
/// **Why widen at all.** A row's tracks are its spans as CSS `fr`, so a row that
/// adds up to three quarters stretches them: Containers (Half) beside OpenClaw
/// (Quarter), left as a pair when reflow evicted Usage, rendered two thirds and
/// one third. Nobody authored a third, no picker offers one, and a user looking
/// at it can only read it as a bug. Filling the row to four quarters keeps every
/// rendered track a width the vocabulary actually has.
///
/// **Who grows.** The distribution closest to the authored proportions, by least
/// squared error — so Half + Quarter becomes Three-quarters + Quarter (the big
/// panel was already twice its neighbour and stays roughly twice it), while
/// Quarter + Quarter becomes Half + Half rather than lopsiding one of them. Ties
/// go to the earlier panel.
///
/// **Nobody grows below a minimum.** Filling *shrinks* the panels that do not
/// grow — a quarter of four is less than a third of three — so every candidate
/// is checked against [`PanelKind::min_width`] at its final width, and a row
/// with no legible filling is refused. That refusal is what [`pack`] reads as
/// "this panel does not fit here", so the fit test and the widths it tests are
/// the same arithmetic rather than two that can disagree.
fn fill_row(row: &[Placement], available: f64, spacing: f64) -> Option<Vec<Placement>> {
    let full = PanelSpan::Full.weight();
    let weights: Vec<u32> = row.iter().map(|p| p.span.weight()).collect();
    let total: u32 = weights.iter().sum();
    if row.is_empty() || total > full {
        return None;
    }

    let mut best: Option<(f64, Vec<u32>)> = None;
    for candidate in distributions(&weights, full - total) {
        let widened: Vec<Placement> = row
            .iter()
            .zip(&candidate)
            .map(|(placement, weight)| Placement {
                span: PanelSpan::from_weight(*weight).expect("a whole number of quarters"),
                ..*placement
            })
            .collect();
        if !row_fits(&widened, available, spacing) {
            continue;
        }
        // Distance from the authored proportions, so the fill that disturbs the
        // row least wins. `<` and not `<=`: an equal-scoring later candidate
        // gives its extra quarter to a further-right panel, and the earlier
        // panel is the one the reader's eye starts on.
        let error: f64 = candidate
            .iter()
            .zip(&weights)
            .map(|(after, before)| {
                let delta =
                    f64::from(*after) / f64::from(full) - f64::from(*before) / f64::from(total);
                delta * delta
            })
            .sum();
        if best.as_ref().is_none_or(|(lowest, _)| error < *lowest) {
            best = Some((error, candidate));
        }
    }

    let (_, weights) = best?;
    Some(
        row.iter()
            .zip(weights)
            .map(|(placement, weight)| Placement {
                span: PanelSpan::from_weight(weight).expect("a whole number of quarters"),
                ..*placement
            })
            .collect(),
    )
}

/// Every way of handing out `extra` quarters across `weights`, one panel at a
/// time. At most three quarters across at most four panels, so this is a
/// handful of candidates and enumerating them beats reasoning about a greedy
/// rule that has to be right at every width.
fn distributions(weights: &[u32], extra: u32) -> Vec<Vec<u32>> {
    if weights.is_empty() {
        return Vec::new();
    }
    if weights.len() == 1 {
        return vec![vec![weights[0] + extra]];
    }
    (0..=extra)
        .flat_map(|take| {
            distributions(&weights[1..], extra - take)
                .into_iter()
                .map(move |rest| std::iter::once(weights[0] + take).chain(rest).collect())
        })
        .collect()
}

/// Whether every panel in a candidate row clears its own
/// [`PanelKind::min_width`] at the width its span actually gives it.
///
/// Per panel, and against the *rendered* width — not against the row's sum of
/// minimums, which is what this used to compare. The sum let a hungry panel
/// borrow width from a lean neighbour that it then never received, because the
/// track it gets is its span's share and not its minimum: Repos + Runners
/// "fitted" at 856pt and each rendered at 420, 20pt under what Repos declares
/// it needs. Spans made that gap impossible to keep ignoring — a Quarter gets a
/// quarter however hungry its neighbours are — so the check moved to the number
/// the panel is actually painted at.
fn row_fits(row: &[Placement], available: f64, spacing: f64) -> bool {
    panel_widths(row, available, spacing)
        .iter()
        .zip(row)
        .all(|(width, placement)| *width >= placement.kind.min_width())
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

/// The width each panel in a rendered row gets: its span's share of **one
/// four-quarter grid**, the same grid on every row of the cockpit.
///
/// `CockpitView.panelWidth(inRowOf:of:)` generalised to spans, and the reason it
/// exists is the rule in CLAUDE.md: *panels never measure themselves*. One
/// measurement at the cockpit root becomes every panel's width by arithmetic, so
/// a panel can decide its own content layout without a second, disagreeing,
/// measurement of its own.
///
/// One quarter track is `(available - 3 * spacing) / 4`, and a span of `k`
/// quarters gets `k` tracks plus the `k - 1` gutters it swallows. That is
/// exactly what CSS `repeat(4, minmax(0, 1fr))` plus `grid-column: span k`
/// paints, which is the point: the frontend and this function are one
/// construction rather than two that can drift. It telescopes to the whole row —
/// `sum(k*q + (k-1)*g) + (n-1)*g == 4q + 3g == available` — so no row ever
/// leaves a sliver unpainted.
///
/// **The denominator is a fixed four, not the row's own weight.** It used to be
/// the row's, on the reasoning that [`fill_row`] widens every rendered row to
/// four quarters anyway. The weights did add up; the *gutters* did not. Dividing
/// `available - (n-1)*spacing` by the row's weight makes the subtracted gutter
/// total depend on how many panels happen to share the row, so a Half came out
/// `spacing/2` narrower beside two Quarters than beside one Half — and the
/// gridline under Repos|Runners missed the one under Containers|OpenClaw by 8pt
/// on the shipped cockpit. A quarter is now a quarter wherever it lands.
///
/// An unknown or nonsensical `available` yields `0.0` widths rather than negative
/// ones — the same "degrade to the layout that cannot be unreadable" rule
/// [`host_columns`] follows, and [`panel_columns`] reads `0.0` as one column.
#[must_use]
pub fn panel_widths(row: &[Placement], available: f64, spacing: f64) -> Vec<f64> {
    if row.is_empty() {
        return Vec::new();
    }
    let quarters = f64::from(PanelSpan::Full.weight());
    let quarter = if available.is_nan() {
        0.0
    } else {
        ((available - spacing * (quarters - 1.0)) / quarters).max(0.0)
    };
    row.iter()
        .map(|p| {
            // Guarded rather than clamped per panel: at a dead width a Half
            // would otherwise still claim the gutter it spans, reporting
            // `spacing` points of width for a row that has none.
            if quarter <= 0.0 {
                return 0.0;
            }
            let k = f64::from(p.span.weight());
            k * quarter + (k - 1.0) * spacing
        })
        .collect()
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

    /// One rendered row's spans — what a reader would call each track's width.
    fn spans_of(row: &[Placement]) -> Vec<PanelSpan> {
        row.iter().map(|p| p.span).collect()
    }

    // MARK: reflow

    /// The reported case: a ~1568pt portrait window (1528pt of content). Every
    /// authored row still fits — the halves get 756pt each and the quarter row's
    /// tracks are 748/374/374 — so the cockpit rows must not move; only the host
    /// cards inside the Hosts panel stack. See `host_columns` below.
    #[test]
    fn portrait_width_leaves_every_panel_row_paired() {
        assert_eq!(reflowed(1528.0), kinds(&layout().rows));
    }

    /// At the 880pt window floor (840pt of content) no authored row survives
    /// whole: the halves would render 412pt each (Repos needs 440), and the
    /// quarter row cannot seat three panels at 202pt a quarter.
    ///
    /// What the per-panel model buys is the pair that *does* survive, and
    /// filling is what makes it survivable: Containers keeps OpenClaw because
    /// the two of them, widened from Half + Quarter to Half + Half, are 412pt
    /// each — both above their minimums. Before rows were filled this pair was
    /// scored at two thirds and one third (549 and 274), OpenClaw fell 66pt
    /// under its floor, and all three panels stacked. A global tier would have
    /// stacked them too.
    #[test]
    fn window_floor_breaks_only_the_rows_that_dont_fit() {
        assert_eq!(
            reflowed(840.0),
            vec![
                vec![PanelKind::Hosts],
                vec![PanelKind::GhWorkflows], // (840 - 16) / 2 = 412 < 440
                vec![PanelKind::GhRunners],
                vec![PanelKind::Containers, PanelKind::OpenclawAgents], // 412 >= 400, 340
                vec![PanelKind::ClaudeUsage],
                // A quarter of 840 is 198, under both lean panels' 240, so the
                // last row cannot seat three. Azure Cost keeps the first of them
                // by widening Half + Quarter to two halves (412 each, both above
                // their minimums) and Sentry Crons takes a row of its own.
                vec![PanelKind::AzureCost, PanelKind::Services],
                vec![PanelKind::SentryCrons],
            ]
        );
        // …and the pair really is rendered as two halves, not as a two-thirds
        // and a third.
        assert_eq!(
            spans_of(&reflow(&layout().rows, 840.0, SPACING)[3]),
            vec![PanelSpan::Half, PanelSpan::Half]
        );
    }

    /// Boundary: a half-width panel is held to its minimum against *half* the
    /// row, so Repos + Runners need 2 * 440 + 16 = 896pt — not the 856pt the
    /// old sum-of-minimums check let them share, where Repos rendered at 420.
    #[test]
    fn row_splits_once_below_its_exact_requirement() {
        assert_eq!(
            reflowed(896.0)[1],
            vec![PanelKind::GhWorkflows, PanelKind::GhRunners],
            "896pt is enough"
        );
        assert_eq!(
            reflowed(895.0)[1],
            vec![PanelKind::GhWorkflows],
            "895pt is not"
        );
    }

    /// Boundary: the quarter row holds while its narrowest track clears 340pt —
    /// four quarter tracks and the three gutters between them, 4 * 340 + 3 * 16
    /// = 1408. Below that Containers keeps the roomier neighbour it can still
    /// afford and Usage takes its own row rather than a track it does not fit.
    ///
    /// This is the arithmetic the comment always described; until the row-local
    /// denominator went, the code was subtracting the two gutters *this row*
    /// happens to have rather than the grid's three, and the boundary sat 16pt
    /// low at 1392.
    #[test]
    fn the_quarter_row_splits_at_its_narrowest_track() {
        assert_eq!(
            reflowed(1408.0)[2],
            vec![
                PanelKind::Containers,
                PanelKind::OpenclawAgents,
                PanelKind::ClaudeUsage
            ],
            "1408pt gives every track its minimum"
        );
        assert_eq!(
            &reflowed(1407.0)[2..],
            &[
                vec![PanelKind::Containers, PanelKind::OpenclawAgents],
                vec![PanelKind::ClaudeUsage],
                vec![
                    PanelKind::AzureCost,
                    PanelKind::Services,
                    PanelKind::SentryCrons
                ],
            ]
        );
    }

    /// The reported case, pinned end to end: a ~1256pt window (1216pt of
    /// content), where the quarter row loses Usage and the two panels left
    /// behind used to render at two thirds and one third — widths nobody
    /// authored and no picker offers.
    ///
    /// They now render Three-quarters + Quarter, the fill closest to the
    /// proportions they were authored at, and the point of the whole exercise
    /// is the second assertion: 908pt is two content columns of Containers,
    /// where 816 of the old two-thirds was one. That only became reachable when
    /// OpenClaw's floor dropped to 240 — with the name and the model ref on one
    /// line it needed 340, and a quarter of this cockpit is 292.
    #[test]
    fn a_row_reflow_cut_short_still_renders_whole_quarters() {
        let rows = reflow(&layout().rows, 1216.0, SPACING);
        assert_eq!(
            kinds(&rows)[2],
            vec![PanelKind::Containers, PanelKind::OpenclawAgents]
        );
        assert_eq!(
            spans_of(&rows[2]),
            vec![PanelSpan::ThreeQuarters, PanelSpan::Quarter]
        );
        let widths = panel_widths(&rows[2], 1216.0, SPACING);
        assert_eq!(widths, vec![908.0, 292.0]);
        assert_eq!(
            panel_columns(PanelKind::Containers, widths[0], SPACING),
            2,
            "three-quarters is what buys Containers its second column here"
        );
        // Below 1008pt a quarter no longer clears OpenClaw's 240 floor, and the
        // fill falls back to the even split that does — down to 816, where
        // Containers' own 400 stops fitting in half and the pair splits.
        let rows = reflow(&layout().rows, 900.0, SPACING);
        assert_eq!(spans_of(&rows[2]), vec![PanelSpan::Half, PanelSpan::Half]);
        assert_eq!(panel_widths(&rows[2], 900.0, SPACING), vec![442.0, 442.0]);
    }

    /// Filling never lopsides a row that was already even: two quarters left
    /// alone become two halves, not a three-quarter and a quarter. Least
    /// distortion, not "widen the widest".
    #[test]
    fn filling_keeps_the_authored_proportions() {
        let evens = vec![
            Placement::new(PanelKind::OpenclawAgents, PanelSpan::Quarter),
            Placement::new(PanelKind::ClaudeUsage, PanelSpan::Quarter),
        ];
        assert_eq!(
            spans_of(&reflow(&[evens], 2000.0, SPACING)[0]),
            vec![PanelSpan::Half, PanelSpan::Half]
        );

        // A lone panel of any width fills the row it was left on.
        for span in PanelSpan::ALL {
            let lone = vec![Placement::new(PanelKind::ClaudeUsage, span)];
            assert_eq!(
                spans_of(&reflow(&[lone], 2000.0, SPACING)[0]),
                vec![PanelSpan::Full],
                "{span:?} alone"
            );
        }
    }

    /// Whatever the width, a rendered row is exactly four quarters — the
    /// property that makes "full / three-quarters / half / quarter" the whole
    /// vocabulary a reader ever sees.
    #[test]
    fn every_rendered_row_adds_up_to_a_whole_row() {
        let mut width = 200.0f64;
        while width <= 3000.0 {
            for row in reflow(&layout().rows, width, SPACING) {
                let total: u32 = row.iter().map(|p| p.span.weight()).sum();
                assert_eq!(
                    total,
                    PanelSpan::Full.weight(),
                    "{:?} at {width}pt",
                    row.iter().map(|p| p.kind.id()).collect::<Vec<_>>()
                );
            }
            width += 37.0;
        }
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

    // MARK: panel_widths

    fn row(spans: &[(PanelKind, PanelSpan)]) -> Vec<Placement> {
        spans
            .iter()
            .map(|(kind, span)| Placement::new(*kind, *span))
            .collect()
    }

    /// A `Full` alone takes the whole width; a `Quarter` alone takes a quarter
    /// and leaves three tracks empty, which is what CSS `grid-column: span 1`
    /// does with the same row. The layout never renders that second case —
    /// `filled` widens a lone panel to `Full` — but the arithmetic has to be the
    /// grid's rather than "whatever is left over", or a span stops meaning one
    /// thing.
    #[test]
    fn a_lone_full_row_takes_the_whole_width() {
        let full = row(&[(PanelKind::AzureCost, PanelSpan::Full)]);
        assert_eq!(panel_widths(&full, 1890.0, SPACING), vec![1890.0]);
        let quarter = row(&[(PanelKind::ClaudeUsage, PanelSpan::Quarter)]);
        assert_eq!(panel_widths(&quarter, 1890.0, SPACING), vec![460.5]);
        assert!(panel_widths(&[], 1890.0, SPACING).is_empty());
    }

    /// Each panel takes its span's share of the same four-quarter grid: `k`
    /// quarter tracks plus the `k - 1` gutters that span swallows.
    #[test]
    fn a_shared_row_splits_the_width_by_span() {
        // A quarter of an 1890pt cockpit is (1890 - 3 * 16) / 4 = 460.5, so a
        // half is 2 * 460.5 + 16 = 937 -- the width the shipped cockpit gives a
        // two-panel row.
        let halves = row(&[
            (PanelKind::GhWorkflows, PanelSpan::Half),
            (PanelKind::GhRunners, PanelSpan::Half),
        ]);
        assert_eq!(panel_widths(&halves, 1890.0, SPACING), vec![937.0, 937.0]);

        // The shipped quarter row, off the *same* grid: the half is the 937 it
        // is one row up, not the 929 a row-local denominator used to give it.
        let mixed = row(&[
            (PanelKind::Containers, PanelSpan::Half),
            (PanelKind::OpenclawAgents, PanelSpan::Quarter),
            (PanelKind::ClaudeUsage, PanelSpan::Quarter),
        ]);
        assert_eq!(
            panel_widths(&mixed, 1890.0, SPACING),
            vec![937.0, 460.5, 460.5]
        );
    }

    /// The regression this grid exists for. The old arithmetic divided
    /// `available - (n-1) * spacing` by the row's own weight, so the subtracted
    /// gutter total moved with the panel count: a Half came out `spacing/2`
    /// narrower beside two Quarters than beside one Half, and the gridline under
    /// Repos|Runners missed the one under Containers|OpenClaw by 8pt on the
    /// shipped cockpit. Same span, same width, whatever else shares the row.
    #[test]
    fn a_span_is_the_same_width_in_every_row() {
        let halves = row(&[
            (PanelKind::GhWorkflows, PanelSpan::Half),
            (PanelKind::GhRunners, PanelSpan::Half),
        ]);
        let mixed = row(&[
            (PanelKind::Containers, PanelSpan::Half),
            (PanelKind::OpenclawAgents, PanelSpan::Quarter),
            (PanelKind::ClaudeUsage, PanelSpan::Quarter),
        ]);
        assert_eq!(
            panel_widths(&halves, 1890.0, SPACING)[0],
            panel_widths(&mixed, 1890.0, SPACING)[0],
            "a Half is a Half in a two-panel row and a three-panel one"
        );

        // …and every rendered row still fills its width exactly, gutters
        // included — the grid must not leave a sliver unpainted at any width.
        for available in [816.0, 1216.0, 1890.0, 2732.0] {
            for row in reflow(&layout().rows, available, SPACING) {
                let widths = panel_widths(&row, available, SPACING);
                let total: f64 = widths.iter().sum::<f64>() + SPACING * (row.len() - 1) as f64;
                assert_eq!(total, available, "row {row:?} at {available}pt");
            }
        }
    }

    /// A width that cannot be trusted degrades to zero rather than to a
    /// negative — `panel_columns` reads zero as "one column", so a dead
    /// measurement produces the layout that is never unreadable.
    #[test]
    fn an_unusable_width_never_goes_negative() {
        let pair = row(&[
            (PanelKind::GhWorkflows, PanelSpan::Half),
            (PanelKind::GhRunners, PanelSpan::Half),
        ]);
        assert_eq!(panel_widths(&pair, 0.0, SPACING), vec![0.0, 0.0]);
        assert_eq!(panel_widths(&pair, 10.0, SPACING), vec![0.0, 0.0]);
        assert_eq!(panel_widths(&pair, f64::NAN, SPACING), vec![0.0, 0.0]);
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
        // Repos carries 214pt of fixed numeric columns plus a 96pt name, so
        // its minimum is 440 and it pairs at 2 * 440 + 16 = 896.
        assert_eq!(panel_columns(PanelKind::GhWorkflows, 895.0, SPACING), 1);
        assert_eq!(panel_columns(PanelKind::GhWorkflows, 896.0, SPACING), 2);
    }

    /// The shipped case, pinned at the width the cockpit is tuned for: the
    /// reflowed layout itself, its tracks, and the content columns each track
    /// affords. Derived from `hosts_forward` rather than restated, so a
    /// re-authored row is visible here as a failure and not as silent drift.
    ///
    /// Repos only just clears its split — 896 of its 937 — and it did not at all
    /// until its numeric columns were sized to their labels rather than half
    /// again that. If a column widens and this drops back to 1, the panel has
    /// outgrown the display it was tuned for.
    ///
    /// Azure Cost is the same 937 as every other Half since Sentry Crons joined
    /// its row, and still clears its own 816pt split. It has 121pt of headroom
    /// here where it used to have 597 — the measured cost of that trade, which is
    /// paid on a *narrower* display than this one (see `DEFAULT_ORDER`).
    #[test]
    fn the_shipped_layout_splits_every_list_panel_at_1890pt() {
        let mut seen = std::collections::BTreeMap::new();
        for row in reflow(&layout().rows, 1890.0, SPACING) {
            for (placement, width) in row.iter().zip(panel_widths(&row, 1890.0, SPACING)) {
                seen.insert(
                    placement.kind,
                    (width, panel_columns(placement.kind, width, SPACING)),
                );
            }
        }
        assert_eq!(seen[&PanelKind::GhWorkflows], (937.0, 2));
        assert_eq!(seen[&PanelKind::GhRunners], (937.0, 2));
        // The same 937 as the halves above it, off the same grid — Containers
        // sits under Repos and the edge between the cards is one straight line.
        assert_eq!(seen[&PanelKind::Containers], (937.0, 2));
        // Half of 1890 still clears two content columns, which is what Azure
        // Cost's breakdowns-beside-costs layout needs — by 121pt rather than the
        // 597 a Three-quarters gave it.
        assert_eq!(seen[&PanelKind::AzureCost], (937.0, 2));
        assert_eq!(seen[&PanelKind::Services], (460.5, 1));
        assert_eq!(seen[&PanelKind::SentryCrons], (460.5, 1));
        // The quarter tracks stay one column, which is the point of a quarter.
        assert_eq!(seen[&PanelKind::OpenclawAgents], (460.5, 1));
        assert_eq!(seen[&PanelKind::ClaudeUsage], (460.5, 1));
    }

    /// The cost of Azure Cost giving up a quarter, pinned as a boundary rather
    /// than described. Its body splits at `2 * 400 + 16 = 816pt`, so a Half needs
    /// a cockpit wide enough to *give* it 816: `2q + 16 >= 816` with
    /// `q = (w - 48) / 4` lands at 1648pt. At Three-quarters it cleared the same
    /// split from 1094pt.
    ///
    /// This is a deliberate trade, not a regression — but it is the kind of trade
    /// that gets forgotten, so the two numbers live here where a future span
    /// change has to look at them.
    #[test]
    fn azure_cost_holds_two_columns_down_to_1648pt_at_half_width() {
        let half_width = |available: f64| {
            let row = row(&[
                (PanelKind::AzureCost, PanelSpan::Half),
                (PanelKind::Services, PanelSpan::Quarter),
                (PanelKind::SentryCrons, PanelSpan::Quarter),
            ]);
            panel_widths(&row, available, SPACING)[0]
        };
        assert_eq!(
            panel_columns(PanelKind::AzureCost, half_width(1648.0), SPACING),
            2
        );
        assert_eq!(
            panel_columns(PanelKind::AzureCost, half_width(1647.0), SPACING),
            1
        );
        // …and the width it used to have at three quarters cleared the same split
        // from a cockpit 554pt narrower: `3q + 32 >= 816` lands at 1094pt.
        let three_quarter_width = |available: f64| {
            let row = row(&[
                (PanelKind::AzureCost, PanelSpan::ThreeQuarters),
                (PanelKind::Services, PanelSpan::Quarter),
            ]);
            panel_widths(&row, available, SPACING)[0]
        };
        assert_eq!(
            panel_columns(PanelKind::AzureCost, three_quarter_width(1094.0), SPACING),
            2
        );
        assert_eq!(
            panel_columns(PanelKind::AzureCost, three_quarter_width(1093.0), SPACING),
            1
        );
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
        assert_eq!(entries.len(), PanelKind::ALL.len());
        assert_eq!(entries.len(), 9);
        assert_eq!(entries[0]["id"], "hosts");
        assert_eq!(entries[0]["title"], "Hosts");
        assert_eq!(entries[0]["minWidth"], 900.0);
        // `ghWorkflows` renders as "GitHub Repos" — the id and the title diverge
        // on purpose, so a table built from ids alone would be wrong.
        assert_eq!(entries[3]["id"], "ghWorkflows");
        assert_eq!(entries[3]["title"], "GitHub Repos");
        assert_eq!(entries[3]["minWidth"], 440.0);
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
    fn spans_carry_a_wire_name_and_a_weight() {
        assert_eq!(PanelSpan::Full.as_str(), "full");
        assert_eq!(PanelSpan::Half.as_str(), "half");
        assert_eq!(PanelSpan::Quarter.as_str(), "quarter");
        // Quarters, so a row of Half + Quarter + Quarter is a whole row.
        assert_eq!(PanelSpan::Full.weight(), 4);
        assert_eq!(PanelSpan::Half.weight(), 2);
        assert_eq!(PanelSpan::Quarter.weight(), 1);
    }

    /// No authored row may promise more than a whole row: a row of weights
    /// summing past four would render every track under its share, silently, at
    /// every width — which no reflow test would catch because reflow only ever
    /// splits rows it cannot fit.
    #[test]
    fn no_authored_row_spans_more_than_a_full_row() {
        for row in &layout().rows {
            let weight: u32 = row.iter().map(|p| p.span.weight()).sum();
            assert!(
                weight <= PanelSpan::Full.weight(),
                "{:?} spans {weight} quarters",
                row.iter().map(|p| p.kind.id()).collect::<Vec<_>>()
            );
        }
    }

    /// The shipped arrangement, spans included: a full-width host row, one pair
    /// of halves, and two identical half + quarter + quarter rows.
    #[test]
    fn the_shipped_layout_is_authored_in_spans() {
        let spans: Vec<Vec<PanelSpan>> = layout()
            .rows
            .iter()
            .map(|r| r.iter().map(|p| p.span).collect())
            .collect();
        assert_eq!(
            spans,
            vec![
                vec![PanelSpan::Full],
                vec![PanelSpan::Half, PanelSpan::Half],
                vec![PanelSpan::Half, PanelSpan::Quarter, PanelSpan::Quarter],
                vec![PanelSpan::Half, PanelSpan::Quarter, PanelSpan::Quarter],
            ]
        );
    }

    // MARK: the ordered layout (the persisted / editable form)

    /// The rows are *derived* from `DEFAULT_ORDER` now rather than authored, so
    /// this pins the packer against the arrangement that used to be written out
    /// by hand. A packer bug would otherwise silently re-shape the cockpit.
    #[test]
    fn the_default_order_packs_into_the_shipped_rows() {
        assert_eq!(
            kinds(&layout().rows),
            vec![
                vec![PanelKind::Hosts],
                vec![PanelKind::GhWorkflows, PanelKind::GhRunners],
                vec![
                    PanelKind::Containers,
                    PanelKind::OpenclawAgents,
                    PanelKind::ClaudeUsage
                ],
                vec![
                    PanelKind::AzureCost,
                    PanelKind::Services,
                    PanelKind::SentryCrons
                ],
            ]
        );
    }

    /// A row takes placements until the next one would push it past four
    /// quarters — the same wrap CSS does, which is why the stored layout is a
    /// list and not rows.
    #[test]
    fn from_order_wraps_when_the_next_span_would_overflow_the_row() {
        let three_halves = CockpitLayout::from_order(
            "test",
            &[
                (PanelKind::Hosts, PanelSpan::Half),
                (PanelKind::GhRunners, PanelSpan::Half),
                (PanelKind::Containers, PanelSpan::Half),
            ],
        );
        assert_eq!(
            kinds(&three_halves.rows),
            vec![
                vec![PanelKind::Hosts, PanelKind::GhRunners],
                vec![PanelKind::Containers],
            ]
        );

        // A half after two quarters does not fit in the same row (1+1+2 = 4 is
        // fine; 2+1+2 is not), and the leftover starts the next one.
        let mixed = CockpitLayout::from_order(
            "test",
            &[
                (PanelKind::Hosts, PanelSpan::Half),
                (PanelKind::GhRunners, PanelSpan::Quarter),
                (PanelKind::Containers, PanelSpan::Half),
            ],
        );
        assert_eq!(
            kinds(&mixed.rows),
            vec![
                vec![PanelKind::Hosts, PanelKind::GhRunners],
                vec![PanelKind::Containers],
            ]
        );
    }

    /// No slot is ever dropped for not fitting: `Full` is the widest span there
    /// is, so every placement fits in some row.
    #[test]
    fn from_order_keeps_every_slot_in_order() {
        let order: Vec<(PanelKind, PanelSpan)> = PanelKind::ALL
            .into_iter()
            .map(|kind| (kind, PanelSpan::Full))
            .collect();
        let packed = CockpitLayout::from_order("test", &order);
        assert_eq!(packed.rows.len(), PanelKind::ALL.len(), "one full row each");
        assert_eq!(packed.order(), order);
    }

    /// `order()` is `from_order`'s inverse, which is what lets the editor read
    /// the layout back out of the thing the cockpit renders.
    #[test]
    fn order_round_trips_through_the_packer() {
        assert_eq!(layout().order(), CockpitLayout::DEFAULT_ORDER.to_vec());
    }

    /// Strict on the way in: a panel id or span name this build does not know
    /// is declined rather than substituted, because a substitution silently
    /// rearranges someone's cockpit.
    #[test]
    fn panel_ids_and_span_names_parse_strictly() {
        for kind in PanelKind::ALL {
            assert_eq!(PanelKind::parse(kind.id()), Some(kind));
        }
        for span in PanelSpan::ALL {
            assert_eq!(PanelSpan::parse(span.as_str()), Some(span));
        }
        assert_eq!(PanelKind::parse("ghworkflows"), None, "case matters");
        assert_eq!(PanelKind::parse(""), None);
        assert_eq!(PanelSpan::parse("third"), None, "a span we removed");
        assert_eq!(PanelSpan::parse("FULL"), None);
    }
}
