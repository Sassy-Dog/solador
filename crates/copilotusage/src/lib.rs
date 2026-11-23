//! GitHub Copilot usage reader: month-to-date AI credits (and, where the plan
//! still bills them, premium requests) for an organization or for a person.
//!
//! One client ([`CopilotUsageClient`]) makes two reads per pass and folds each
//! with a pure function ([`summarize`]). The tile that shows them is
//! app/src-tauri's; this crate is the reader.
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
//! is a measured zero. The API reports no plan allowance, so there is no pool
//! and no fraction here.
//!
//! **Errors classify; `crates/fault` owns the words.** See [`error`].
//!
//! App-only: `agent/` must never resolve this crate.

pub mod client;
pub mod error;
pub mod models;
pub mod summary;

pub use client::{Billed, CopilotRead, CopilotUsageClient, API_VERSION, DEFAULT_BASE_URL};
pub use error::{CopilotUsageError, ErrorKind, Level};
pub use models::UsageItem;
pub use summary::{summarize, ModelUse, Usage, TOP_MODELS};

#[cfg(test)]
mod tests;
