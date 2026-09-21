//! Execution backends (SPEC §8, §15, §20).
//!
//! The mandatory Claude Code adapter launches `claude -p` child processes
//! with explicit model, effort, turn limits and budget controls, passing
//! prompts via stdin and arguments as an argv array. The installed version
//! is capability-checked against a compatibility matrix before any
//! launch; missing permissions produce a blocked result and no bypass
//! flags are introduced. The contract those backends implement — launch,
//! cancellation, effective profile, permission capability, sandbox
//! capability and usage completeness — is `crate::backend`, which a
//! second provider can implement without touching anything here. No
//! adapter may advertise guarantees its backend cannot enforce.

pub mod claude;
pub mod mock;

pub use mock::{MockBackend, MockOutcome};
