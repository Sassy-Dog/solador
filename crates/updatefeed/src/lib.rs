//! The update feed: the `latest.json` manifest `tauri-plugin-updater` reads,
//! and the signature check that has to pass before it may be published.
//!
//! # Why this is a crate and not a shell script
//!
//! Everything here is a decision that can be *wrong in a way nobody notices*
//! until an operator's app refuses to update, or — far worse — accepts
//! something it should not have. A manifest is a small JSON document, so the
//! temptation is `jq` in a workflow step; the reason it is Rust instead is
//! [`signature::verify`], which is the plugin's own verifier, at the plugin's
//! own dependency requirement, run over the artifact that is about to be
//! advertised. **A feed entry whose signature does not verify is never
//! written.** That is the negative case #308 exists to assert, and it is
//! asserted here rather than assumed of the toolchain that produced the file.
//!
//! # This crate is not linked into the app
//!
//! Nothing under `app/` depends on it. The running app verifies through
//! `tauri-plugin-updater`, which carries its own copy of the same verifier;
//! this crate is release tooling, used by
//! `.github/workflows/publish-feed.yml` through the `solador-update-feed`
//! binary. The two agreeing is not a coincidence — [`signature::verify`]
//! reproduces the plugin's `verify_signature` step for step, and the
//! `minisign-verify` requirement in `Cargo.toml` is the plugin's, so cargo
//! unifies them to one crate.

pub mod manifest;
pub mod signature;
