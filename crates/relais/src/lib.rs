//! relais — a coding-agent execution companion. See `docs/SPEC.md`.
//!
//! The core unit is a task with an immutable contract and a sequence of
//! recorded attempts. The runner, not a model, owns transitions and
//! acceptance (SPEC §1). Nothing in this crate equates a check pass with
//! semantic correctness, or a worker's completion message with acceptance.

pub mod adapter;
pub mod admission;
pub mod backend;
pub mod context;
pub mod contract;
pub mod coordinator;
pub mod doctor;
pub mod ids;
pub mod install;
pub mod ipc;
pub mod learn;
pub mod ledger;
pub mod lifecycle;
pub mod money;
pub mod paths;
pub mod policy;
pub mod procs;
pub mod repo;
pub mod report;
pub mod resume;
pub mod rng;
pub mod route;
pub mod runner;
pub mod tooling;
pub mod verify;
pub mod workspace;

/// Crate version as reported by `relais --version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
