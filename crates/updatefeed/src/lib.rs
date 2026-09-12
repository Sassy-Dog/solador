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
//! # The agent feed lives here too, and shares only the CalVer rule
//!
//! [`agent`] builds `agent-latest.json` (#391): the same verify-before-write
//! discipline, under a **different key** and a **different signature
//! convention** — plain minisign, no base64 wrapper — because there is no
//! Tauri in the agent. The one thing the two feeds share in code is
//! `manifest::is_calver`, because both carry the release's CalVer under the
//! same rule. Its consumer is `solador-agent update` (`agent/src/update.rs`,
//! #393), which does not depend on this crate: `agent/` compiles the public
//! keys in and verifies with `minisign-verify` on its own, and the wire
//! contract in [`agent`]'s module docs is what the two agree on
//! (`scripts/agent-deps-guard.sh` asserts the absence of the edge).
//!
//! # This crate is not linked into the app, or into the agent
//!
//! Nothing under `app/` or `agent/` depends on it (`scripts/agent-deps-guard.sh`
//! asserts the agent half with `cargo tree`, in CI and `./dev lint`). The running app verifies through
//! `tauri-plugin-updater`, which carries its own copy of the same verifier;
//! this crate is release tooling, used by
//! `.github/workflows/publish-feed.yml` through the `solador-update-feed` and
//! `solador-agent-feed` binaries. The two agreeing is not a coincidence — [`signature::verify`]
//! reproduces the plugin's `verify_signature` step for step, and the
//! `minisign-verify` requirement in `Cargo.toml` is the plugin's, so cargo
//! unifies them to one crate.

pub mod agent;
pub mod manifest;
pub mod signature;
