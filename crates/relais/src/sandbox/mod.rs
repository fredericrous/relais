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
//! Whether the sandbox holds is judged from what a real probe session did,
//! never assumed: [`probe_plan`] and [`evaluate`] say what the session is
//! asked to do and whether its transcript shows it, and
//! [`VerificationStore`] keeps the passes by [`VerificationKey`].
//!
//! A sandboxed worker runs only on a configuration a probe has verified.
//! [`dispatch_key`] names that configuration once, for the probe that
//! records it ([`verify_sandbox`], behind `relais doctor --verify-sandbox`) and
//! for the dispatch gate ([`dispatch_gate`]) that requires the record.
//!
//! What a sandboxed attempt was denied, and how complete that account is,
//! is [`scan`]'s answer over its transcript and scratch files.
//!
//! Nothing here launches a session itself; the runner and the doctor wire
//! the real launcher in.

mod denials;
mod dispatch;
mod floor;
mod judge;
mod launch;
mod preflight;
mod probe;
mod settings;
mod verification;
mod verify;

pub use denials::{scan, transcript_path, Coverage, Denial, DenialReport};
pub use dispatch::{dispatch_gate, dispatch_key, dispatch_lookup, DispatchKeyInputs, GateInputs};
pub use floor::{credential_floor, Floor, FloorInputs};
pub use judge::{judge_managed, judge_user_config, managed_sources, Weakening};
pub use launch::{worker_launch, LaunchInputs, TmpLink, WorkerMode};
pub use preflight::{
    managed_bytes, managed_root, preflight, weakenings, PreflightInputs, RealSandboxHost,
    SandboxHost, SANDBOX_MIN_HARNESS,
};
pub use probe::{
    evaluate, probe_plan, probe_plan_allowlist, probe_prompt, Expect, InitExpect, ProbePlanInputs,
    ProbeReport, ProbeStep, ProbeTool, StepResult, Verdict,
};
pub use settings::{build_settings, SettingsInputs};
pub use verification::{
    store_path, StoreError, VerificationKey, VerificationRecord, VerificationStore,
};
pub use verify::{verify_sandbox, VerifyError, VerifyInputs, VerifyOutcome};
