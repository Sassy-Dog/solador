//! GitHub Copilot usage reader: month-to-date AI credits (and, where the plan
//! still bills them, premium requests) for an organization or for a person.
//! The `ai_credit` report is one shared pool that every GitHub product bills
//! into, Copilot among them, so [`summarize`] counts all of it and breaks it
//! down by the product names GitHub sends ([`ProductUse`]); it filters nothing
//! and matches no product, SKU or unit string.
//!
//! One client ([`CopilotUsageClient`]) makes two billing reads per pass and
//! folds each with a pure function ([`summarize`]). A third, separate read
//! ([`CopilotUsageClient::seat`], module [`seat`]) asks the undocumented
//! `GET /copilot_internal/user` for the token owner's own plan or employer-seat
//! limit, with its own typed model and its own error kinds. The tile that shows
//! them is app/src-tauri's; this crate is the reader.
//!
//! Rules that run through the crate:
//!
//! **The token is an argument.** An org read uses the Actions billing token
//! (org Administration: read); a personal read uses its own fine-grained PAT
//! (user permission Plan: read). Nothing here stores either, and neither
//! `Debug` nor any error carries it.
//!
//! **Nothing reads the wall clock.** `now` is always an argument.
//!
//! **Unknown is representable.** Every number on the wire is an `Option`, a
//! total is `Some` only when every item carried its figure, and an empty month
//! is a measured zero. The billing API reports no plan allowance, so there is
//! no pool and no fraction there; the seat read's limit is its own figure.
//!
//! **Errors classify; `crates/fault` owns the words.** See [`error`].
//!
//! App-only: `agent/` must never resolve this crate.

pub mod client;
pub mod error;
pub mod models;
pub mod seat;
pub mod summary;

pub use client::{Billed, CopilotRead, CopilotUsageClient, API_VERSION, DEFAULT_BASE_URL};
pub use error::{CopilotUsageError, ErrorKind, Level};
pub use models::UsageItem;
pub use seat::{Allowance, Credits, Seat, SeatError, SeatErrorKind, USD_EXPONENT};
pub use summary::{summarize, ModelUse, ProductUse, Usage, TOP_MODELS};

#[cfg(test)]
mod seat_tests;
#[cfg(test)]
mod tests;
