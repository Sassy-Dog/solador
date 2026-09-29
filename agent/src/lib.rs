//! The library half of `solador-agent`.
//!
//! The daemon — the axum server, the samplers, the container listing — lives
//! in `src/main.rs` and its modules, exactly as before. This library exists
//! so `tests/update_flow.rs` can reach [`update`] (the `solador-agent
//! update` / `rollback` transaction, #393 — the one piece of the agent whose
//! *failure modes* matter more than its happy path) end to end against a
//! loopback release server and a temporary install tree. A binary crate's
//! modules cannot be reached from `tests/`, so it lives here and the binary
//! calls it. [`tls`] (#447) lives here for the same reason: its own
//! certificate-pinning test drives a real TLS handshake, and `update`'s
//! health probe reads the certificate it writes.
//!
//! Nothing in here is a public API anyone else consumes; the crate is not
//! published. It is also, deliberately, free of `crates/updatefeed` — the
//! feed *producer* is release tooling, and `scripts/agent-deps-guard.sh`
//! fails CI on that edge.

pub mod tls;
pub mod update;
