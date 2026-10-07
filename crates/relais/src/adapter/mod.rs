//! Execution backends (SPEC §8, §15, §20).
//!
//! relais never launches Claude Code itself: every dispatch is a native
//! agent the relais plugin spawns ([`native`]), and the Claude Code
//! adapter ([`claude`]) only probes the installed harness with `--version`
//! and `--help`. The contract those backends implement — launch,
//! cancellation, effective profile, permission capability and usage
//! completeness — is `crate::backend`, which a second provider can
//! implement without touching anything here. No adapter may advertise
//! guarantees its backend cannot enforce.

pub mod claude;
pub mod mock;
pub mod native;

pub use mock::{MockBackend, MockOutcome};
