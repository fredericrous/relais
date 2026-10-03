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

use super::{
    explain_miss, managed_bytes, worker_launch, LaunchInputs, VerificationKey, VerificationStore,
};
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
    match dispatch_lookup(inputs) {
        Ok(Lookup::Verified(key)) => Ok(key),
        Ok(Lookup::Unverified(reason)) => Err(unverified(format!(
            "the sandbox is unverified for Claude Code {} on {}: {reason}; run `relais doctor \
             --verify-sandbox`",
            inputs.harness_version, inputs.platform
        ))),
        Err(why) => Err(unverified(why)),
    }
}

/// What the store says about this configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Verified(VerificationKey),
    /// No record holds; the reason, from [`explain_miss`].
    Unverified(String),
}

/// The key when a record holds for this configuration, why none does
/// otherwise, and the reason when the store or the managed files cannot be read
/// at all: the two answers `relais doctor` words differently.
pub fn dispatch_lookup(inputs: &GateInputs) -> Result<Lookup, String> {
    let managed = managed_bytes(inputs.managed_root, inputs.extra_managed_root).map_err(|why| {
        format!(
            "the managed configuration the verification is keyed by cannot be read ({why}); \
             relais fails closed"
        )
    })?;
    let key = dispatch_key(&DispatchKeyInputs {
        harness_version: inputs.harness_version,
        platform: inputs.platform,
        launch: inputs.launch,
        managed: &managed,
    });
    let store = VerificationStore::load(inputs.store).map_err(|why| {
        format!(
            "{why} ({}); delete it and run `relais doctor --verify-sandbox`",
            inputs.store.display()
        )
    })?;
    Ok(match store.find(&key) {
        Some(_) => Lookup::Verified(key),
        None => Lookup::Unverified(explain_miss(
            store.records(),
            inputs.harness_version,
            inputs.platform,
        )),
    })
}
