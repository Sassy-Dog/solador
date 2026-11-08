//! Every word, colour and figure of the Anthropic and OpenAI panels' **plan** rows: a Claude
//! or Codex subscription's own limits, as `crates/subscription` reads them.
//!
//! The types here mirror that crate's without depending on it. It carries an
//! HTTP stack and the view layer needs none, which is the arrangement
//! [`crate::agent_release`] has with `crates/agentrelease`.
//!
//! # One row per plan, one meter per limit
//!
//! A plan is one [`PlanRow`] (`Claude · Ultra`) carrying a [`Metric`] per
//! limit window (`5h`, `Week`, `Week · Fable`) and per spend cap (`Budget`,
//! `Extra usage`). A metric is the Machines tile's CPU/RAM meter: a label, a
//! percentage, and a fill coloured by [`color::usage_color`]'s 70/90 edges.
//!
//! The panel's token rows (`LAST 5H`, `LAST 7D`) are this machine's local
//! consumption, with no ceiling. A plan metric is the subscription's use of
//! its own allowance. Their labels never coincide.
//!
//! # Pace
//!
//! [`pace`] projects a window from the average pace since it opened: what
//! share is used against what share of the window has passed. One reading is
//! enough, so it survives a restart. It is computed **at the reading's time**,
//! never at render: the percentage is frozen when it is read, and dividing it
//! by a clock that keeps running would make a reader that has stopped look
//! like a burn that has slowed. It is not shown until [`PACE_MIN_ELAPSED`] of
//! the window has passed, nor once the reading is stale.
//!
//! A spend cap has no pace: the reply states no reset for it, and a guessed
//! billing period would be a fabricated one.
//!
//! # Unknown is not zero, and a past reset is not a present reading
//!
//! - An absent window has no metric; nothing here makes a 0%. A verified
//!   Ultra account has no 5-hour window, so it has no `5h` meter.
//! - A window with no reset instant has no countdown, never "resets now".
//! - A window whose reset has already passed is **muted, with no fill and no
//!   pace**. Its percentage was true of a window that has since rolled over,
//!   so an old 100% must not paint a red that is no longer the case.
//! - A window with no stated length is named by its slot (`Primary`,
//!   `Secondary`), never given a `5h` or `Week` it did not state, and has no
//!   pace.
//! - An amount with no currency is a bare amount, never dollars.

use crate::color;

/// The muted value of a provider that is on and still waiting on its first read.
pub const LOADING_VALUE: &str = "reading…";

/// The value of a provider with nothing to show: a failure before any reading,
/// or a reading with no limits. The header line carries the reason.
pub const NO_READING_VALUE: &str = "—";

/// The share of a window that must have passed before its pace is shown.
///
/// A burst of `b` points early in a window moves the projection by
/// `b ÷ elapsed share`. At a tenth, a 3-point burst moves it 30 points, which
/// is as jumpy as a projection should be allowed to get. That is 30 minutes of
/// the 5-hour session and about 17 hours of the week.
pub const PACE_MIN_ELAPSED: f64 = 0.10;

/// How long a window is. Mirrors `subscription::WindowSpan`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Span {
    /// A stated length, in whole minutes.
    Minutes(u32),
    /// A window the service sent without a length, in its first slot.
    Primary,
    /// The same, in its second slot.
    Secondary,
}

/// One window. Mirrors `subscription::PlanWindow`, with the reset as Unix
/// seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub span: Span,
    /// The model a window is scoped to (`"Fable"`), or `None` for the whole plan.
    pub scope: Option<String>,
    /// Utilisation, as the service reported it: finite and non-negative, and
    /// possibly above 100.
    pub used_percent: f64,
    pub resets_at: Option<i64>,
}

/// An amount in minor units. Mirrors `subscription::Money`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Money {
    pub minor: i64,
    pub exponent: u8,
    pub currency: Option<String>,
}

/// A spend against a cap. Mirrors `subscription::PlanSpend`.
#[derive(Debug, Clone, PartialEq)]
pub struct Spend {
    pub used: Option<Money>,
    pub limit: Option<Money>,
    pub stated_percent: Option<f64>,
    pub enabled: Option<bool>,
}

/// One reading of a plan. Mirrors `subscription::PlanUsage`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub name: Option<String>,
    pub windows: Vec<Window>,
    pub spend: Option<Spend>,
}

/// One meter.
#[derive(Debug, Clone, PartialEq)]
pub struct Metric {
    /// `5h`, `Week`, `Week · Fable`, `Budget`.
    pub label: String,
    /// `41%`, `$312 / $500 · 62%`.
    pub value: String,
    /// The fill, 0–1, or `None` for an empty track: a reset since the read, or
    /// a spend with no percentage.
    pub fraction: Option<f64>,
    pub color: u32,
    /// Where the pace tick sits, 0–1: the share of the window that had passed
    /// when it was read. A fill past the tick is a burn faster than even.
    /// `None` when no pace is shown.
    pub pace: Option<f64>,
    /// The reset and the pace in words, shown under the panel's meter and as
    /// its description: `resets in 3d 4h · out in ~1d 1h at this pace`.
    pub note: String,
    /// Spans the whole row rather than one column.
    pub full: bool,
}

/// One plan: its header line and its meters.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanRow {
    /// `Claude · Ultra`, or the provider alone when no name is known.
    pub label: String,
    /// The tightest verdict, short enough for the tile's value slot:
    /// `out in ~1d 1h`, `limit reached`, `resets in 2d 4h`.
    pub value: String,
    pub value_color: u32,
    /// The worst live meter's colour, for the row's dot.
    pub color: u32,
    /// The verdict as a sentence, for the tile's detailed view.
    pub detail: String,
    pub metrics: Vec<Metric>,
}

/// A window's pace at the moment it was read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pace {
    /// No stated length, no future reset, or a reset more than a window away.
    Unknown,
    /// Less than [`PACE_MIN_ELAPSED`] of the window had passed.
    TooEarly,
    /// On pace to finish the window at `projected` percent, at most 100.
    Under { projected: f64 },
    /// On pace to reach 100% at `runs_out_at` (Unix seconds), before the reset.
    Over { runs_out_at: i64 },
    /// Already at or past 100%.
    AtLimit,
}

/// The share of `window` that had passed at `read_at`, assuming it opened one
/// length before its reset. `None` without a stated length and a reset inside
/// one length of `read_at`.
#[must_use]
pub fn elapsed_share(window: &Window, read_at: i64) -> Option<f64> {
    let Span::Minutes(minutes) = window.span else {
        return None;
    };
    let len = i64::from(minutes) * 60;
    let remaining = window.resets_at? - read_at;
    if len <= 0 || remaining <= 0 || remaining >= len {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    Some((len - remaining) as f64 / len as f64)
}

/// The pace of `window` as read at `read_at`.
#[must_use]
pub fn pace(window: &Window, read_at: i64) -> Pace {
    if window.resets_at.is_some_and(|at| at <= read_at) {
        return Pace::Unknown;
    }
    let used = window.used_percent;
    if used >= 100.0 {
        return Pace::AtLimit;
    }
    let Some(share) = elapsed_share(window, read_at) else {
        return Pace::Unknown;
    };
    if share < PACE_MIN_ELAPSED {
        return Pace::TooEarly;
    }
    let projected = used / share;
    if projected <= 100.0 {
        return Pace::Under { projected };
    }
    // Here `used > 100 × share ≥ 10`, so the division is safe. At the same
    // average rate, the rest of the allowance lasts `elapsed × (100 − used) / used`.
    let Span::Minutes(minutes) = window.span else {
        return Pace::Unknown;
    };
    let elapsed = share * f64::from(minutes) * 60.0;
    #[allow(clippy::cast_possible_truncation)]
    let runs_out_at = read_at + (elapsed * (100.0 - used) / used).round() as i64;
    Pace::Over { runs_out_at }
}

/// Time until a reset, to two units: `45s`, `14m`, `2h 14m`, `3d 4h`.
#[must_use]
pub fn countdown(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3_600, secs % 3_600 / 60);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{m}m")
    } else if secs < 86_400 {
        format!("{h}h {m}m")
    } else {
        format!("{d}d {h}h")
    }
}

fn until(at: i64, now: i64) -> String {
    countdown(u64::try_from(at - now).unwrap_or(0))
}

/// `Claude · Ultra`. A name written all in lower case, as Codex's `plus` is,
/// gets a capital; one with any capital is kept as written.
#[must_use]
pub fn provider_label(provider: &str, plan: Option<&str>) -> String {
    let Some(name) = plan else {
        return provider.to_owned();
    };
    if name.chars().any(char::is_uppercase) {
        return format!("{provider} · {name}");
    }
    let mut chars = name.chars();
    let first = chars.next().map(|c| c.to_uppercase().to_string());
    format!(
        "{provider} · {}{}",
        first.unwrap_or_default(),
        chars.as_str()
    )
}

/// `5h`, `Week`, `Week · Fable`, else the largest whole unit, else the slot.
#[must_use]
pub fn metric_label(window: &Window) -> String {
    let span = match window.span {
        Span::Minutes(300) => "5h".to_owned(),
        Span::Minutes(10_080) => "Week".to_owned(),
        Span::Minutes(m) if m > 0 && m.is_multiple_of(1_440) => format!("{}d", m / 1_440),
        Span::Minutes(m) if m > 0 && m.is_multiple_of(60) => format!("{}h", m / 60),
        Span::Minutes(m) => format!("{m}m"),
        Span::Primary => "Primary".to_owned(),
        Span::Secondary => "Secondary".to_owned(),
    };
    match &window.scope {
        Some(scope) => format!("{span} · {scope}"),
        None => span,
    }
}

/// `1234567` as `1,234,567`. Public for the other counts the app says out
/// loud (Settings' "This token can read 1,031 repos").
pub fn grouped(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        out.insert(0, '-');
    }
    out
}

/// An amount for a meter: whole units from 100 up, else at most two decimals,
/// rounded half up in integer arithmetic. `$` for US dollars only; another
/// currency's code follows the number, and no currency is a bare number.
#[must_use]
pub fn money(m: &Money) -> String {
    let scale = 10_i64.pow(u32::from(m.exponent.min(6)));
    let decimals = if m.minor / scale >= 100 {
        0
    } else {
        u32::from(m.exponent.min(2))
    };
    let unit = 10_i64.pow(decimals);
    // The amount in units of 10^-decimals, rounded half up.
    let scaled = (m.minor.saturating_mul(unit) + scale / 2) / scale;
    let number = if decimals == 0 {
        grouped(scaled)
    } else {
        format!(
            "{}.{:0width$}",
            grouped(scaled / unit),
            scaled % unit,
            width = decimals as usize
        )
    };
    match m.currency.as_deref() {
        Some("USD") => format!("${number}"),
        Some(code) => format!("{number} {code}"),
        None => number,
    }
}

/// The share of a cap that is spent, in percent: from the amounts when both
/// are in one currency and the cap is above zero, else the service's own.
fn spend_percent(spend: &Spend) -> Option<f64> {
    let from_amounts = match (&spend.used, &spend.limit) {
        (Some(used), Some(limit)) if used.currency == limit.currency && limit.minor > 0 => {
            #[allow(clippy::cast_precision_loss)]
            let major = |m: &Money| m.minor as f64 / 10_f64.powi(i32::from(m.exponent));
            Some(major(used) / major(limit) * 100.0)
        }
        _ => None,
    };
    from_amounts.or(spend.stated_percent)
}

/// A spend's meter: `Budget` when it is the plan's only limit (an Enterprise
/// seat), `Extra usage` beside windows. Extra usage that is switched off has
/// no meter.
#[must_use]
pub fn spend_metric(spend: &Spend, beside_windows: bool) -> Option<Metric> {
    if beside_windows && spend.enabled == Some(false) {
        return None;
    }
    let percent = spend_percent(spend);
    let amounts = match (&spend.used, &spend.limit) {
        (Some(used), Some(limit)) => format!("{} / {}", money(used), money(limit)),
        (Some(used), None) => format!("{} spent", money(used)),
        (None, Some(limit)) => format!("{NO_READING_VALUE} / {}", money(limit)),
        (None, None) => NO_READING_VALUE.to_owned(),
    };
    Some(Metric {
        label: if beside_windows {
            "Extra usage"
        } else {
            "Budget"
        }
        .to_owned(),
        value: match percent {
            Some(p) => format!("{amounts} · {p:.0}%"),
            None => amounts,
        },
        fraction: percent.map(|p| (p / 100.0).clamp(0.0, 1.0)),
        color: percent.map_or(color::MUTED, color::usage_color),
        pace: None,
        note: if beside_windows {
            "spend past the plan's limits"
        } else {
            "no reset date reported"
        }
        .to_owned(),
        full: true,
    })
}

/// A window's meter at render time `now`, and its pace. `read_at` is when it
/// was read; `fresh` is false once that reading is stale, which hides the pace.
#[must_use]
pub fn window_metric(window: &Window, now: i64, read_at: i64, fresh: bool) -> (Metric, Pace) {
    let label = metric_label(window);
    let used = window.used_percent;
    let value = format!("{used:.0}%");
    if window.resets_at.is_some_and(|at| at <= now) {
        let metric = Metric {
            label,
            value,
            fraction: None,
            color: color::MUTED,
            pace: None,
            note: "reset since read".to_owned(),
            full: false,
        };
        return (metric, Pace::Unknown);
    }
    let pace = if fresh {
        pace(window, read_at)
    } else {
        Pace::Unknown
    };
    let tick = match pace {
        Pace::Under { .. } | Pace::Over { .. } | Pace::AtLimit => elapsed_share(window, read_at),
        Pace::Unknown | Pace::TooEarly => None,
    };
    let reset = window
        .resets_at
        .map(|at| format!("resets in {}", until(at, now)));
    let words = match pace {
        Pace::Under { projected } => Some(format!("on pace for {projected:.0}%")),
        Pace::Over { runs_out_at } if runs_out_at > now => {
            Some(format!("out in ~{} at this pace", until(runs_out_at, now)))
        }
        Pace::Over { .. } => Some("out by now at this pace".to_owned()),
        Pace::TooEarly => Some("too early to project".to_owned()),
        Pace::AtLimit | Pace::Unknown => None,
    };
    let note = match pace {
        Pace::AtLimit => ["limit reached".to_owned()]
            .into_iter()
            .chain(reset)
            .collect::<Vec<_>>(),
        _ => reset.into_iter().chain(words).collect(),
    }
    .join(" · ");
    let metric = Metric {
        label,
        value,
        fraction: Some((used / 100.0).clamp(0.0, 1.0)),
        color: color::usage_color(used),
        pace: tick,
        note,
        full: false,
    };
    (metric, pace)
}

/// The worse of two meter colours: red over amber over green.
fn worse(a: u32, b: u32) -> u32 {
    let rank = |c: u32| match c {
        c if c == color::RED => 3,
        c if c == color::AMBER => 2,
        c if c == color::GREEN => 1,
        _ => 0,
    };
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
}

/// A plan's row at render time `now`. `read_at` is when the reading was taken
/// and `fresh` is whether it is still inside its stale edge.
///
/// The verdict is the tightest one: a window at its limit, else the earliest
/// run-out among live windows, else the busiest window's reset (or its share,
/// when it states no reset). A plan whose only limit is a spend cap reads its
/// share of the budget. It is never blank: a plan whose every window has reset
/// since its read says so.
#[must_use]
pub fn plan_row(provider: &str, plan: &Plan, now: i64, read_at: i64, fresh: bool) -> PlanRow {
    let label = provider_label(provider, plan.name.as_deref());
    let mut metrics = Vec::new();
    let mut live: Vec<(&Window, Pace)> = Vec::new();
    let mut dot = color::MUTED;
    for window in &plan.windows {
        let (metric, pace) = window_metric(window, now, read_at, fresh);
        if metric.fraction.is_some() {
            live.push((window, pace));
            dot = worse(dot, metric.color);
        }
        metrics.push(metric);
    }
    let spend = plan
        .spend
        .as_ref()
        .and_then(|s| spend_metric(s, !plan.windows.is_empty()));
    if let Some(metric) = &spend {
        if metric.fraction.is_some() {
            dot = worse(dot, metric.color);
        }
    }

    let reset_words = |w: &Window| {
        w.resets_at
            .map(|at| format!("its reset in {}", until(at, now)))
    };
    let used = |w: &Window| format!("{} is at {:.0}%", metric_label(w), w.used_percent);

    let earliest_out = live
        .iter()
        .filter_map(|(w, p)| match p {
            Pace::Over { runs_out_at } => Some((*w, *runs_out_at)),
            _ => None,
        })
        .min_by_key(|(_, at)| *at);
    let at_limit = live
        .iter()
        .find(|(_, p)| matches!(p, Pace::AtLimit))
        .map(|(w, _)| *w);
    let busiest = live
        .iter()
        .max_by(|a, b| a.0.used_percent.total_cmp(&b.0.used_percent));

    // A limit already reached outranks a forecast: a plan locked out of its
    // session now is not "out in ~1d" because its week is over pace too.
    let (value, value_color, detail) = if let Some(w) = at_limit {
        let until_reset = reset_words(w).map_or(String::new(), |r| format!(" until {r}"));
        (
            "limit reached".to_owned(),
            color::RED,
            format!("{}{until_reset}.", used(w)),
        )
    } else if let Some((w, at)) = earliest_out {
        let when = if at > now {
            format!("out in ~{}", until(at, now))
        } else {
            "out by now".to_owned()
        };
        let before = reset_words(w).map_or(String::new(), |r| format!(", before {r}"));
        (
            when.clone(),
            color::AMBER,
            format!("{} and, at this pace, will run {when}{before}.", used(w)),
        )
    } else if let Some((w, p)) = busiest {
        match w.resets_at {
            Some(at) => {
                let projected = match p {
                    Pace::Under { projected } => format!(", on pace for {projected:.0}% by"),
                    _ => ";".to_owned(),
                };
                (
                    format!("resets in {}", until(at, now)),
                    color::MUTED,
                    format!(
                        "{}{projected} {}.",
                        used(w),
                        reset_words(w).unwrap_or_default()
                    ),
                )
            }
            // No reset was stated, so there is no clock to read against.
            None => (
                format!("{:.0}% used", w.used_percent),
                color::MUTED,
                format!("{}; no reset time was reported.", used(w)),
            ),
        }
    } else if let Some(metric) = spend.as_ref().filter(|_| plan.windows.is_empty()) {
        // With no share to state, the amounts are the verdict: `$312 spent`.
        let value = match metric.fraction {
            Some(_) => format!(
                "{} of budget",
                metric.value.rsplit(" · ").next().unwrap_or_default()
            ),
            None => metric.value.clone(),
        };
        (
            value,
            metric.color,
            format!("{}: {}.", metric.label, metric.value),
        )
    } else if !plan.windows.is_empty() {
        // Every window rolled over after it was read; the next read has the
        // new one. Until then there is no share to state.
        (
            "reset since read".to_owned(),
            color::MUTED,
            "Every limit has reset since it was read.".to_owned(),
        )
    } else {
        (
            NO_READING_VALUE.to_owned(),
            color::MUTED,
            "No limit was reported.".to_owned(),
        )
    };
    metrics.extend(spend);
    PlanRow {
        label,
        value,
        value_color,
        color: dot,
        detail,
        metrics,
    }
}

/// A plan's single row while it has nothing to show.
#[must_use]
pub fn placeholder_row(label: &str, loading: bool) -> PlanRow {
    PlanRow {
        label: label.to_owned(),
        value: if loading {
            LOADING_VALUE
        } else {
            NO_READING_VALUE
        }
        .to_owned(),
        value_color: color::MUTED,
        color: color::MUTED,
        detail: String::new(),
        metrics: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_792_843_200; // 2026-10-24T12:00:00Z
    const HOUR: i64 = 3_600;
    const DAY: i64 = 86_400;
    const WEEK: u32 = 10_080;

    fn window(span: Span, used: f64, resets_at: Option<i64>) -> Window {
        Window {
            span,
            scope: None,
            used_percent: used,
            resets_at,
        }
    }

    /// A week read at `NOW` with `days_left` to its reset.
    fn week(used: f64, days_left: f64) -> Window {
        #[allow(clippy::cast_possible_truncation)]
        let left = (days_left * DAY as f64) as i64;
        window(Span::Minutes(WEEK), used, Some(NOW + left))
    }

    fn usd(minor: i64) -> Money {
        Money {
            minor,
            exponent: 2,
            currency: Some("USD".into()),
        }
    }

    // ---- labels and money ------------------------------------------------

    #[test]
    fn metrics_read_as_the_plan_names_its_windows_and_unstated_ones_keep_their_slot() {
        let label = |span| metric_label(&window(span, 1.0, None));
        assert_eq!(label(Span::Minutes(300)), "5h");
        assert_eq!(label(Span::Minutes(WEEK)), "Week");
        assert_eq!(label(Span::Minutes(1_440)), "1d");
        assert_eq!(label(Span::Minutes(120)), "2h");
        assert_eq!(label(Span::Minutes(2)), "2m");
        assert_eq!(label(Span::Primary), "Primary");
        assert_eq!(label(Span::Secondary), "Secondary");
        let scoped = Window {
            scope: Some("Fable".into()),
            ..week(1.0, 1.0)
        };
        assert_eq!(metric_label(&scoped), "Week · Fable");
    }

    /// The display name as written; a lower-case one (Codex's) gets a capital.
    #[test]
    fn the_provider_label_carries_the_plan_name_when_there_is_one() {
        assert_eq!(provider_label("Claude", None), "Claude");
        assert_eq!(provider_label("Claude", Some("Ultra")), "Claude · Ultra");
        assert_eq!(provider_label("Codex", Some("plus")), "Codex · Plus");
        assert_eq!(provider_label("Claude", Some("Max 5x")), "Claude · Max 5x");
    }

    #[test]
    fn money_reads_whole_units_from_100_and_names_only_dollars_with_a_sign() {
        assert_eq!(money(&usd(31_245)), "$312");
        assert_eq!(money(&usd(31_250)), "$313", "half up");
        assert_eq!(money(&usd(123_456_789)), "$1,234,568");
        assert_eq!(money(&usd(1_250)), "$12.50");
        assert_eq!(money(&usd(5)), "$0.05");
        let eur = Money {
            currency: Some("EUR".into()),
            ..usd(200_000)
        };
        assert_eq!(money(&eur), "2,000 EUR");
        let yen = Money {
            minor: 1_500,
            exponent: 0,
            currency: Some("JPY".into()),
        };
        assert_eq!(money(&yen), "1,500 JPY");
        let fine = Money {
            minor: 1_234_567,
            exponent: 6,
            currency: None,
        };
        assert_eq!(
            money(&fine),
            "1.23",
            "at most two decimals, and no currency"
        );
    }

    #[test]
    fn a_countdown_has_two_units_at_most() {
        assert_eq!(countdown(0), "0s");
        assert_eq!(countdown(45), "45s");
        assert_eq!(countdown(14 * 60 + 59), "14m");
        assert_eq!(countdown(2 * 3600 + 14 * 60), "2h 14m");
        assert_eq!(countdown(3 * 3600), "3h 0m");
        assert_eq!(countdown(3 * 86_400 + 4 * 3600 + 59), "3d 4h");
    }

    // ---- pace ------------------------------------------------------------

    #[test]
    fn pace_waits_for_a_tenth_of_the_window() {
        // 5h session: 30 minutes is the floor.
        let session = |elapsed_min: i64| {
            window(
                Span::Minutes(300),
                5.0,
                Some(NOW + (300 - elapsed_min) * 60),
            )
        };
        assert_eq!(pace(&session(29), NOW), Pace::TooEarly);
        assert!(matches!(pace(&session(30), NOW), Pace::Under { .. }));
    }

    /// 41% with 2 of 7 days gone is on pace for ~144%, so it runs out first.
    #[test]
    fn a_burn_faster_than_even_runs_out_before_the_reset() {
        let w = week(41.0, 5.0);
        let Pace::Over { runs_out_at } = pace(&w, NOW) else {
            panic!("over pace");
        };
        // 2 days for 41 points; 59 points more at that rate is ~2.88 days.
        let expected = NOW + (2.0 * DAY as f64 * 59.0 / 41.0).round() as i64;
        assert_eq!(runs_out_at, expected);
        assert!(runs_out_at < w.resets_at.unwrap());
    }

    #[test]
    fn a_burn_slower_than_even_projects_its_percentage_at_the_reset() {
        assert_eq!(pace(&week(20.0, 3.5), NOW), Pace::Under { projected: 40.0 });
        assert_eq!(pace(&week(0.0, 3.5), NOW), Pace::Under { projected: 0.0 });
    }

    #[test]
    fn a_full_window_is_at_its_limit_and_unknowable_ones_are_unknown() {
        assert_eq!(pace(&week(100.0, 3.0), NOW), Pace::AtLimit);
        assert_eq!(pace(&week(104.0, 3.0), NOW), Pace::AtLimit);
        for w in [
            window(Span::Primary, 50.0, Some(NOW + HOUR)),
            window(Span::Minutes(WEEK), 50.0, None),
            // A reset at or before the read is a window that had rolled over.
            week(50.0, 0.0),
            week(50.0, -1.0),
            // A reset more than a window away is not a window that opened one
            // length before it.
            week(50.0, 8.0),
        ] {
            assert_eq!(pace(&w, NOW), Pace::Unknown, "{w:?}");
        }
    }

    // ---- meters ----------------------------------------------------------

    /// The CPU/RAM thresholds: green under 70, amber under 90, red from 90.
    #[test]
    fn a_live_window_is_a_meter_coloured_like_cpu_and_ram() {
        let (m, _) = window_metric(&week(42.0, 6.0), NOW, NOW, true);
        assert_eq!(m.label, "Week");
        assert_eq!(m.value, "42%");
        assert_eq!(m.fraction, Some(0.42));
        assert_eq!(m.color, color::GREEN);
        for (used, expected) in [
            (69.4, color::GREEN),
            (70.0, color::AMBER),
            (90.0, color::RED),
        ] {
            let (m, _) = window_metric(&week(used, 6.0), NOW, NOW, true);
            assert_eq!(m.color, expected, "{used}");
        }
        let (m, _) = window_metric(&week(104.0, 6.0), NOW, NOW, true);
        assert_eq!((m.value.as_str(), m.fraction), ("104%", Some(1.0)));
    }

    #[test]
    fn the_note_says_the_reset_and_the_pace_and_the_tick_marks_even_pace() {
        let (m, _) = window_metric(&week(20.0, 3.5), NOW, NOW, true);
        assert_eq!(m.note, "resets in 3d 12h · on pace for 40%");
        assert_eq!(m.pace, Some(0.5));

        let (m, _) = window_metric(&week(41.0, 5.0), NOW, NOW, true);
        assert_eq!(m.note, "resets in 5d 0h · out in ~2d 21h at this pace");

        let (m, _) = window_metric(&week(100.0, 3.0), NOW, NOW, true);
        assert_eq!(m.note, "limit reached · resets in 3d 0h");

        let (m, _) = window_metric(&week(1.0, 6.95), NOW, NOW, true);
        assert_eq!(m.note, "resets in 6d 22h · too early to project");
        assert_eq!(m.pace, None);

        let (m, _) = window_metric(&window(Span::Minutes(WEEK), 9.0, None), NOW, NOW, true);
        assert_eq!(m.note, "", "no reset is no countdown");
    }

    /// The pace belongs to the reading: as the clock moves on, the projection
    /// and the tick stay where they were read, and only the countdowns move.
    #[test]
    fn pace_is_frozen_at_the_reading_while_the_countdowns_move() {
        let w = week(20.0, 3.5);
        let (then, _) = window_metric(&w, NOW, NOW, true);
        let (later, pace) = window_metric(&w, NOW + 6 * HOUR, NOW, true);
        assert_eq!(pace, Pace::Under { projected: 40.0 });
        assert_eq!(later.pace, then.pace);
        assert_eq!(later.note, "resets in 3d 6h · on pace for 40%");
    }

    #[test]
    fn a_stale_reading_shows_no_pace() {
        let (m, pace) = window_metric(&week(41.0, 5.0), NOW, NOW, false);
        assert_eq!(pace, Pace::Unknown);
        assert_eq!(m.pace, None);
        assert_eq!(m.note, "resets in 5d 0h");
        assert_eq!(m.fraction, Some(0.41), "the reading itself still shows");
    }

    /// The reading is of a window that has rolled over since.
    #[test]
    fn a_window_whose_reset_has_passed_is_muted_with_no_fill_and_no_pace() {
        for resets_at in [NOW, NOW - 600] {
            let w = window(Span::Minutes(300), 100.0, Some(resets_at));
            let (m, pace) = window_metric(&w, NOW, NOW - HOUR, true);
            assert_eq!(m.value, "100%");
            assert_eq!(m.color, color::MUTED);
            assert_eq!((m.fraction, m.pace), (None, None));
            assert_eq!(m.note, "reset since read");
            assert_eq!(pace, Pace::Unknown);
        }
    }

    #[test]
    fn a_spend_cap_alone_is_the_budget_and_beside_windows_it_is_extra_usage() {
        let spend = Spend {
            used: Some(usd(31_245)),
            limit: Some(usd(50_000)),
            stated_percent: Some(99.0),
            enabled: Some(true),
        };
        let budget = spend_metric(&spend, false).expect("budget");
        assert_eq!(budget.label, "Budget");
        assert_eq!(budget.value, "$312 / $500 · 62%", "amounts win");
        assert_eq!(budget.color, color::GREEN);
        assert_eq!((budget.pace, budget.full), (None, true));
        assert_eq!(budget.note, "no reset date reported");

        let extra = spend_metric(&spend, true).expect("extra");
        assert_eq!(extra.label, "Extra usage");
        let off = Spend {
            enabled: Some(false),
            ..spend.clone()
        };
        assert_eq!(spend_metric(&off, true), None);
        assert!(
            spend_metric(&off, false).is_some(),
            "a budget is shown off or on"
        );
    }

    #[test]
    fn a_spend_without_comparable_amounts_takes_the_stated_share_or_none() {
        let eur = Money {
            currency: Some("EUR".into()),
            ..usd(100)
        };
        let mixed = Spend {
            used: Some(eur),
            limit: Some(usd(50_000)),
            stated_percent: None,
            enabled: None,
        };
        let m = spend_metric(&mixed, false).expect("metric");
        assert_eq!(m.value, "1.00 EUR / $500");
        assert_eq!((m.fraction, m.color), (None, color::MUTED));

        let zero_cap = Spend {
            used: Some(usd(100)),
            limit: Some(usd(0)),
            stated_percent: Some(12.0),
            enabled: None,
        };
        assert_eq!(
            spend_metric(&zero_cap, false).expect("metric").value,
            "$1.00 / $0.00 · 12%"
        );
        let only_used = Spend {
            used: Some(usd(100)),
            limit: None,
            stated_percent: None,
            enabled: None,
        };
        assert_eq!(
            spend_metric(&only_used, false).expect("metric").value,
            "$1.00 spent"
        );
    }

    // ---- rows ------------------------------------------------------------

    fn ultra(windows: Vec<Window>) -> Plan {
        Plan {
            name: Some("Ultra".into()),
            windows,
            spend: None,
        }
    }

    #[test]
    fn the_row_verdict_is_the_earliest_run_out() {
        let fable = Window {
            scope: Some("Fable".into()),
            ..week(78.0, 4.0)
        };
        let row = plan_row(
            "Claude",
            &ultra(vec![week(41.0, 5.0), fable]),
            NOW,
            NOW,
            true,
        );
        assert_eq!(row.label, "Claude · Ultra");
        // Fable: 78 points in 3 days, 22 more at that rate is ~20h 18m.
        assert_eq!(row.value, "out in ~20h 18m");
        assert_eq!(row.value_color, color::AMBER);
        assert_eq!(row.color, color::AMBER, "the worst meter: Fable at 78%");
        assert_eq!(
            row.detail,
            "Week · Fable is at 78% and, at this pace, will run out in ~20h 18m, before its reset in 4d 0h."
        );
        assert_eq!(row.metrics.len(), 2);
        assert!(
            row.value.chars().count() <= 18,
            "fits the tile's value slot"
        );
    }

    #[test]
    fn with_no_run_out_the_row_reads_the_limit_or_the_busiest_reset() {
        let row = plan_row("Claude", &ultra(vec![week(100.0, 2.0)]), NOW, NOW, true);
        assert_eq!(
            (row.value.as_str(), row.value_color),
            ("limit reached", color::RED)
        );
        assert_eq!(row.detail, "Week is at 100% until its reset in 2d 0h.");

        let session = window(Span::Minutes(300), 30.0, Some(NOW + 2 * HOUR));
        let row = plan_row(
            "Claude",
            &ultra(vec![session, week(20.0, 3.5)]),
            NOW,
            NOW,
            true,
        );
        assert_eq!(
            (row.value.as_str(), row.value_color),
            ("resets in 2h 0m", color::MUTED)
        );
        assert_eq!(
            row.detail,
            "5h is at 30%, on pace for 50% by its reset in 2h 0m."
        );
        assert_eq!(row.color, color::GREEN);
    }

    /// A window that has rolled over since its read is out of the verdict.
    #[test]
    fn a_muted_window_never_decides_the_verdict_or_the_colour() {
        let rolled = window(Span::Minutes(300), 100.0, Some(NOW - 60));
        let row = plan_row(
            "Claude",
            &ultra(vec![rolled, week(20.0, 3.5)]),
            NOW,
            NOW - HOUR,
            true,
        );
        assert_ne!(row.value, "limit reached");
        assert_eq!(row.color, color::GREEN);
    }

    /// Locked out of the session now is tighter than a week forecast to run
    /// out tomorrow, and the words agree with the red dot.
    #[test]
    fn a_reached_limit_outranks_a_projected_run_out() {
        let session = window(Span::Minutes(300), 100.0, Some(NOW + 2 * HOUR));
        let fable = Window {
            scope: Some("Fable".into()),
            ..week(78.0, 4.0)
        };
        let row = plan_row("Claude", &ultra(vec![session, fable]), NOW, NOW, true);
        assert_eq!(
            (row.value.as_str(), row.value_color, row.color),
            ("limit reached", color::RED, color::RED)
        );
        assert_eq!(row.detail, "5h is at 100% until its reset in 2h 0m.");
    }

    #[test]
    fn a_row_is_never_blank() {
        // A live window that states no reset: its share is the verdict.
        let unstated = window(Span::Minutes(WEEK), 41.0, None);
        let row = plan_row("Claude", &ultra(vec![unstated]), NOW, NOW, true);
        assert_eq!(
            (row.value.as_str(), row.value_color),
            ("41% used", color::MUTED)
        );
        assert_eq!(row.detail, "Week is at 41%; no reset time was reported.");

        // Every window rolled over after the read: nothing live to state.
        let rolled = |used| window(Span::Minutes(WEEK), used, Some(NOW - 60));
        let row = plan_row(
            "Claude",
            &ultra(vec![rolled(41.0), rolled(78.0)]),
            NOW,
            NOW - HOUR,
            true,
        );
        assert_eq!(
            (row.value.as_str(), row.value_color, row.color),
            ("reset since read", color::MUTED, color::MUTED)
        );
        assert_eq!(row.detail, "Every limit has reset since it was read.");

        let nothing = plan_row("Claude", &ultra(Vec::new()), NOW, NOW, true);
        assert_eq!(
            (nothing.value.as_str(), nothing.detail.as_str()),
            (NO_READING_VALUE, "No limit was reported.")
        );
    }

    #[test]
    fn an_enterprise_seat_reads_its_budget() {
        let plan = Plan {
            name: Some("Enterprise".into()),
            windows: Vec::new(),
            spend: Some(Spend {
                used: Some(usd(31_245)),
                limit: Some(usd(50_000)),
                stated_percent: None,
                enabled: Some(true),
            }),
        };
        let row = plan_row("Claude", &plan, NOW, NOW, true);
        assert_eq!(row.label, "Claude · Enterprise");
        assert_eq!(row.value, "62% of budget");
        assert_eq!(row.value_color, color::GREEN);
        assert_eq!(row.detail, "Budget: $312 / $500 · 62%.");
        assert_eq!(row.metrics.len(), 1);
        assert_eq!(row.metrics[0].label, "Budget");
    }

    #[test]
    fn a_provider_without_limits_says_reading_or_shows_the_dash() {
        let loading = placeholder_row("Claude", true);
        assert_eq!(loading.label, "Claude");
        assert_eq!(loading.value, LOADING_VALUE);
        assert_eq!(loading.color, color::MUTED);
        assert!(loading.metrics.is_empty());
        let none = placeholder_row("Codex · Plus", false);
        assert_eq!(none.value, NO_READING_VALUE);
        assert_eq!(none.value_color, color::MUTED);
    }
}
