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
//! Nothing here launches anything; the launch path wires these together.

mod floor;
mod judge;
mod settings;

pub use floor::{credential_floor, Floor, FloorInputs};
pub use judge::{judge_managed, judge_user_config, managed_sources, Weakening};
pub use settings::{build_settings, SettingsInputs};
