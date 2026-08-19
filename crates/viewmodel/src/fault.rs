//! The stock vocabulary for anticipated failures, re-exported.
//!
//! #352 introduced [`Fault`] here, in the panel layer, and stated the rule it
//! was protecting: **data access must never point at presentation**, so no
//! vendor crate may depend on `viewmodel`. That left the error → sentence
//! mapping with nowhere to live — the shell reads a failure through
//! `user_message()`, which is a method on the error, which lives in the vendor
//! crate — and Neon and Sentry ended up retyping two of these sentences.
//!
//! #354 moved the enum itself down to `crates/fault`, a leaf crate with no
//! dependencies that `viewmodel` and every vendor crate can point at without
//! either pointing at the other. Nothing about the rule changed; the words
//! simply stopped being reachable only through the layer that paints them.
//! `crates/fault/src/lib.rs` carries the full argument, and every consumer that
//! spelled this `viewmodel::fault::Fault` still resolves.

pub use fault::{http_status_message, Fault, MAX_MESSAGE_CHARS};
