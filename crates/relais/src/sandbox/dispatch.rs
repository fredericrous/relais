//! The key a sandboxed dispatch runs under, and the gate that requires a
//! verification for it.
//!
//! One function, [`dispatch_key`], names the configuration: the probe
//! records its pass under it and the dispatch gate looks it up by it, so
//! what was probed and what is launched cannot drift apart. The key is
//! computed from the DISPATCH form of the settings — what `worker_launch`
//! builds for a worker, without the probe's own fixture — with the scratch
//! path normalised by [`VerificationKey::compute`].

use std::path::{Path, PathBuf};

use super::{managed_bytes, worker_launch, LaunchInputs, VerificationKey, VerificationStore};
use crate::policy::{BlockCode, Blocker};

/// What the key is computed from. `launch` is a worker's launch inputs (no
/// probe fixture in its `[sandbox]` settings); `managed` the bytes of the
/// managed files the preflight judged.
pub struct DispatchKeyInputs<'a> {
    pub harness_version: &'a str,
    pub platform: &'a str,
    pub launch: &'a LaunchInputs<'a>,
    pub managed: &'a [(PathBuf, Vec<u8>)],
}

pub fn dispatch_key(inputs: &DispatchKeyInputs) -> VerificationKey {
    let launch = worker_launch(inputs.launch);
    VerificationKey::compute(
        inputs.harness_version,
        inputs.platform,
        &launch.settings,
        inputs.launch.scratch,
        inputs.managed,
    )
}

/// What the dispatch gate reads: the store, and the facts the key is
/// computed from. The managed files are read here, from the same roots the
/// preflight judged.
pub struct GateInputs<'a> {
    pub store: &'a Path,
    pub harness_version: &'a str,
    pub platform: &'a str,
    pub launch: &'a LaunchInputs<'a>,
    pub managed_root: &'a Path,
    pub extra_managed_root: Option<&'a Path>,
}

fn unverified(detail: String) -> Blocker {
    Blocker {
        code: BlockCode::SandboxUnverified,
        detail,
    }
}

/// The key this dispatch runs under, when a probe verified it; otherwise
/// the reason it may not run. Fails closed: a store that cannot be read, or
/// managed files that cannot be read, verify nothing.
pub fn dispatch_gate(inputs: &GateInputs) -> Result<VerificationKey, Blocker> {
    let managed = managed_bytes(inputs.managed_root, inputs.extra_managed_root).map_err(|why| {
        unverified(format!(
            "the managed configuration the verification is keyed by cannot be read ({why}); \
                 relais fails closed"
        ))
    })?;
    let key = dispatch_key(&DispatchKeyInputs {
        harness_version: inputs.harness_version,
        platform: inputs.platform,
        launch: inputs.launch,
        managed: &managed,
    });
    let store = VerificationStore::load(inputs.store).map_err(|why| {
        unverified(format!(
            "{why} ({}); delete it and run `relais doctor --verify-sandbox`",
            inputs.store.display()
        ))
    })?;
    match store.find(&key) {
        Some(_) => Ok(key),
        None => Err(unverified(format!(
            "no probe session has verified the sandbox for Claude Code {} on {} with this \
             configuration; run `relais doctor --verify-sandbox` (a change to the harness, the \
             platform, `[sandbox]` or the managed settings needs it again)",
            inputs.harness_version, inputs.platform
        ))),
    }
}
