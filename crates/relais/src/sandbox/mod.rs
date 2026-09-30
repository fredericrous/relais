//! What an OS-sandboxed worker may touch (SPEC §8, "Worker OS sandbox").
//!
//! Three questions, each answered by a function of explicit inputs — paths,
//! an environment lookup, parsed JSON — and never of the process
//! environment, so each is testable with fixtures:
//!
//! - [`credential_floor`]: the paths and environment names no sandboxed
//!   worker may read, whatever `[sandbox]` in machine.toml says;
//! - [`judge_managed`] and [`judge_user_config`]: whether the machine's
//!   Claude Code configuration weakens the sandbox we are about to rely on;
//! - [`build_settings`]: the `--settings` JSON that turns the sandbox on
//!   with the floor in it.
//!
//! Two more answer the launch path's own questions, again over explicit
//! inputs: [`worker_launch`] folds the three above into the launch a worker
//! gets, and [`preflight`] says whether the sandbox can be relied on at all
//! (platform, harness version, Linux helpers, weakening configuration).
//!
//! Nothing here launches anything; the runner wires these together.

mod floor;
mod judge;
mod launch;
mod preflight;
mod settings;

pub use floor::{credential_floor, Floor, FloorInputs};
pub use judge::{judge_managed, judge_user_config, managed_sources, Weakening};
pub use launch::{worker_launch, LaunchInputs, WorkerMode};
pub use preflight::{
    managed_root, preflight, PreflightInputs, RealSandboxHost, SandboxHost, SANDBOX_MIN_HARNESS,
};
pub use settings::{build_settings, SettingsInputs};
