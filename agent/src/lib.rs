//! The library half of `solador-agent`.
//!
//! The daemon — the axum server, the samplers, the container listing — lives
//! in `src/main.rs` and its modules, exactly as before. This library exists
//! for one module, [`update`]: the `solador-agent update` / `rollback`
//! transaction (#393) is the one piece of the agent whose *failure modes*
//! matter more than its happy path, and `tests/update_flow.rs` drives it end
//! to end against a loopback release server and a temporary install tree.
//! A binary crate's modules cannot be reached from `tests/`, so the module
//! lives here and the binary calls it.
//!
//! Nothing in here is a public API anyone else consumes; the crate is not
//! published. It is also, deliberately, free of `crates/updatefeed` — the
//! feed *producer* is release tooling, and `scripts/agent-deps-guard.sh`
//! fails CI on that edge.

pub mod update;
