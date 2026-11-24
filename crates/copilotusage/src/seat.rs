//! The seat read: how much of their own Copilot limit the token's owner has
//! used this period, from `GET /copilot_internal/user`.
//!
//! **That endpoint is undocumented.** Copilot's own clients use it for their
//! quota display, and it is the only source that answers for a member of an
//! organization whose seat an employer assigns and pays for: the documented
//! billing endpoints are admin-only, and the user-level one leaves such a seat
//! out. Because it can change without notice, every rule here degrades to an
//! em dash with a reason rather than to a figure that might be wrong.
//!
//! The wire model keeps only the fields below. The body also carries the
//! user's organization list, enterprise ids and tracking ids, and none of it
//! is decoded, so none of it can be kept, logged or shown.
//!
//! # Credits are exact
//!
//! Under `token_based_billing` the figures are GitHub AI credits, and one
//! credit is $0.01. A credit count is kept as [`Credits`], whole thousandths of
//! a credit, so it is already the minor unit of a US dollar at exponent 5 and
//! no float reaches the meter. Equal figures stay equal, which is what makes
//! "used the whole limit" exact.
//!
//! # What is never a figure
//!
//! `token_based_billing` other than `true` (an annual premium-request plan:
//! requests, never dollars), a missing figure, a negative or non-finite one,
//! and a response whose `login` is not the account the operator named. Each is
//! a [`SeatError`], never a zero. `has_quota` is decoded and never branched
//! on: it turns `false` once the quota is used up, which is a figure to show,
//! not an absence of one.

use std::fmt;

use chrono::{DateTime, NaiveDate};
use fault::Fault;
use serde::Deserialize;

use crate::error::{ErrorKind, VENDOR};

/// Thousandths of a credit per credit. See the module note.
const SCALE: f64 = 1_000.0;
/// The decimal exponent of a US-dollar amount held in [`Credits`] (a credit is
/// $0.01, so a thousandth of a credit is $0.00001).
pub const USD_EXPONENT: u8 = 5;
/// Past this a figure is not a quota: it is refused rather than let to overflow
/// the scaled integer.
const MAX_CREDITS: f64 = 1.0e12;
/// The longest plan name shown. A longer string is not a plan name.
const MAX_PLAN_CHARS: usize = 40;

/// A credit amount in whole thousandths of a credit.
///
/// Also the amount in US dollars at [`USD_EXPONENT`], since 1 credit = $0.01.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Credits(i64);

impl Credits {
    /// `None` for a negative, non-finite or absurdly large figure.
    #[must_use]
    pub fn from_f64(credits: f64) -> Option<Credits> {
        if !credits.is_finite() || !(0.0..=MAX_CREDITS).contains(&credits) {
            return None;
        }
        #[allow(clippy::cast_possible_truncation)]
        Some(Credits((credits * SCALE).round() as i64))
    }

    /// The amount as US-dollar minor units at [`USD_EXPONENT`].
    #[must_use]
    pub fn usd_minor(self) -> i64 {
        self.0
    }
}

/// What the seat may use this period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Allowance {
    /// The limit GitHub enforces for this user this period.
    Capped(Credits),
    /// No limit. Used is a spend, not a share.
    Unlimited,
}

/// One reading of the token owner's own Copilot limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seat {
    /// `business`, `pro`, … as GitHub sent it. A display string only: nothing
    /// branches on it.
    pub plan: Option<String>,
    pub used: Credits,
    pub allowance: Allowance,
    /// When the period resets, Unix seconds.
    pub resets_at: Option<i64>,
}

// ---- wire ----

/// Only the fields this build keeps. Anything else in the body is skipped by
/// serde and never held.
#[derive(Debug, Deserialize)]
pub(crate) struct SeatBody {
    #[serde(default)]
    pub(crate) login: Option<String>,
    #[serde(default)]
    pub(crate) copilot_plan: Option<String>,
    #[serde(default)]
    pub(crate) quota_reset_date_utc: Option<String>,
    #[serde(default)]
    pub(crate) quota_reset_date: Option<String>,
    #[serde(default)]
    pub(crate) quota_snapshots: Option<Snapshots>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Snapshots {
    #[serde(default)]
    pub(crate) premium_interactions: Option<Quota>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Quota {
    #[serde(default)]
    pub(crate) credits_used: Option<f64>,
    #[serde(default)]
    pub(crate) entitlement: Option<f64>,
    #[serde(default)]
    pub(crate) unlimited: Option<bool>,
    #[serde(default)]
    pub(crate) token_based_billing: Option<bool>,
    /// Decoded to say it is known, never read: see the module note.
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) has_quota: Option<bool>,
}

// ---- errors ----

/// Why the seat read produced no figure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeatErrorKind {
    /// The request failed or GitHub answered with a failure status.
    Request(ErrorKind),
    /// `token_based_billing` was not `true`: an annual premium-request plan,
    /// whose figures are requests and never dollars.
    NotCreditBilled,
    /// A figure the row needs was absent.
    MissingFigure,
    /// A figure was negative, non-finite or out of range.
    InvalidFigure,
    /// The response named a different account than the configured username.
    WrongAccount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeatError {
    pub kind: SeatErrorKind,
}

impl SeatError {
    #[must_use]
    pub fn new(kind: SeatErrorKind) -> Self {
        SeatError { kind }
    }

    /// The vocabulary state this failure is.
    #[must_use]
    pub fn fault(&self) -> Fault {
        match self.kind {
            SeatErrorKind::Request(kind) => match kind {
                ErrorKind::Unauthorized | ErrorKind::Forbidden => Fault::CredentialRejected,
                ErrorKind::NotFound => Fault::NotFound,
                ErrorKind::RateLimited => Fault::RateLimited,
                ErrorKind::VendorFailure { .. } => Fault::VendorFailure,
                ErrorKind::UnexpectedStatus { .. } => Fault::Unexpected,
                ErrorKind::Undecodable => Fault::Undecodable,
                ErrorKind::Unreachable => Fault::Unreachable,
            },
            SeatErrorKind::WrongAccount => Fault::CredentialRejected,
            SeatErrorKind::NotCreditBilled
            | SeatErrorKind::MissingFigure
            | SeatErrorKind::InvalidFigure => Fault::Undecodable,
        }
    }

    /// Whether a figure kept from an earlier good read may stay beside this
    /// failure. A failure to reach GitHub or to be let in says nothing about
    /// the figure; one that says the response no longer means what it did, or
    /// belongs to someone else, does, and the figure goes.
    #[must_use]
    pub fn keeps_last_figure(&self) -> bool {
        match self.kind {
            SeatErrorKind::Request(kind) => kind != ErrorKind::Undecodable,
            _ => false,
        }
    }

    /// Operator-facing sentence. Never built from `to_string()`.
    #[must_use]
    pub fn user_message(&self) -> String {
        let stock = self.fault().message(VENDOR);
        match self.kind {
            SeatErrorKind::Request(ErrorKind::Unauthorized | ErrorKind::Forbidden) => {
                format!("{stock} \u{2014} it needs User permissions \u{2192} Plan: Read")
            }
            SeatErrorKind::Request(ErrorKind::NotFound) => format!(
                "{stock} \u{2014} check the username, whose account needs a Copilot plan or seat"
            ),
            SeatErrorKind::Request(ErrorKind::UnexpectedStatus { status }) => {
                fault::http_status_message(status, VENDOR)
            }
            SeatErrorKind::Request(_) => stock,
            SeatErrorKind::NotCreditBilled => {
                format!("{stock} \u{2014} this plan isn't billed in AI credits")
            }
            SeatErrorKind::MissingFigure => {
                format!("{stock} \u{2014} it carried no credit figure")
            }
            SeatErrorKind::InvalidFigure => {
                format!("{stock} \u{2014} a credit figure was out of range")
            }
            SeatErrorKind::WrongAccount => format!(
                "{stock} \u{2014} the token belongs to a different account than the username"
            ),
        }
    }
}

impl fmt::Display for SeatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            SeatErrorKind::Request(
                ErrorKind::VendorFailure { status } | ErrorKind::UnexpectedStatus { status },
            ) => write!(f, "GitHub returned HTTP {status} for the Copilot seat read"),
            _ => write!(f, "the Copilot seat read failed"),
        }
    }
}

impl std::error::Error for SeatError {}

// ---- the fold ----

/// A body, checked against the account it must belong to and folded into a
/// [`Seat`].
///
/// # Errors
/// [`SeatError`], never a zero.
pub(crate) fn interpret(body: &SeatBody, expected_login: &str) -> Result<Seat, SeatError> {
    // First: another account's figures are never read further, let alone shown
    // under this username.
    let same = body
        .login
        .as_deref()
        .is_some_and(|login| login.trim().eq_ignore_ascii_case(expected_login.trim()));
    if !same {
        return Err(SeatError::new(SeatErrorKind::WrongAccount));
    }
    let quota = body
        .quota_snapshots
        .as_ref()
        .and_then(|s| s.premium_interactions.as_ref())
        .ok_or(SeatError::new(SeatErrorKind::MissingFigure))?;
    if quota.token_based_billing != Some(true) {
        return Err(SeatError::new(SeatErrorKind::NotCreditBilled));
    }
    let used = credits(quota.credits_used)?;
    let allowance = if quota.unlimited == Some(true) {
        Allowance::Unlimited
    } else {
        Allowance::Capped(credits(quota.entitlement)?)
    };
    Ok(Seat {
        plan: body
            .copilot_plan
            .as_deref()
            .map(str::trim)
            .filter(|p| {
                !p.is_empty()
                    && p.chars().count() <= MAX_PLAN_CHARS
                    && !p.chars().any(char::is_control)
            })
            .map(str::to_owned),
        used,
        allowance,
        resets_at: reset(body),
    })
}

fn credits(figure: Option<f64>) -> Result<Credits, SeatError> {
    let value = figure.ok_or(SeatError::new(SeatErrorKind::MissingFigure))?;
    Credits::from_f64(value).ok_or(SeatError::new(SeatErrorKind::InvalidFigure))
}

/// The reset instant: the timestamp when it parses, else the date's midnight
/// UTC. A reset that parses as neither is no reset, not a failed read.
fn reset(body: &SeatBody) -> Option<i64> {
    let stamp = body
        .quota_reset_date_utc
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s.trim()).ok())
        .map(|t| t.timestamp());
    stamp.or_else(|| {
        body.quota_reset_date
            .as_deref()
            .and_then(|s| NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok())
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|t| t.and_utc().timestamp())
    })
}
