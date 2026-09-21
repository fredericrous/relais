//! Verification and acceptance (SPEC §10, §18).
//!
//! Verification runs after all candidate-writing descendants have stopped
//! or relinquished their write leases, against an immutable copy of the
//! candidate — a verification worktree checked out at the candidate SHA,
//! so no concurrent worker can modify what is being verified. Record
//! identity, base SHA, contract hash, policy hash, commands, exit
//! statuses, timeouts and log hashes. Preflight captures baseline
//! failures; existing failures are never silently waived. A skipped,
//! inert, unavailable or untrusted required check is a gap, not a pass.
//! An accepted receipt is bound to one candidate and does not authorize
//! merge or survive edits.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::ids::{canonical_json_hash, sha256_hex};
use crate::money::{CostCompleteness, MicroUsd};
use crate::policy::{CommandSpec, DependencyMode, Integrations, VerificationProfile};
use crate::procs::Ended;
use crate::tooling::{ProgramVersion, VersionUnknown};
use crate::workspace::{Git, WorkspaceError};

/// Why verification could not be carried out as the policy declares it.
/// Distinct from a check that RAN and failed: this is the plan itself
/// being unusable, which blocks rather than fails (SPEC §10).
#[derive(Debug)]
pub enum VerifyError {
    /// A verification-input pattern is not a glob. Named, and blocking —
    /// the same treatment `check_scope` gives a malformed write scope,
    /// because a pattern nobody can compile protects nothing (audit V8).
    BadPattern {
        pattern: String,
        detail: String,
    },
    /// A command declares a timeout of zero. A check that is given no
    /// time is not a check; it was quietly rounded up to one second.
    ZeroTimeout {
        argv: Vec<String>,
    },
    Io(std::io::Error),
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadPattern { pattern, detail } => write!(
                f,
                "verification input pattern `{pattern}` is not a valid glob: {detail}"
            ),
            Self::ZeroTimeout { argv } => write!(
                f,
                "the verification command `{}` declares timeout_seconds = 0; give it a time or remove it",
                argv.join(" ")
            ),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for VerifyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::BadPattern { .. } | Self::ZeroTimeout { .. } => None,
        }
    }
}

impl From<std::io::Error> for VerifyError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckOutcome {
    pub label: String,
    pub argv: Vec<String>,
    /// How the check's process ended. A status of zero is the only pass;
    /// a timeout, a cancellation and a signal are each a distinct
    /// failure, which `exit: None, timed_out: false` could not say.
    pub ended: Ended,
    pub log_path: String,
    pub log_sha256: String,
}

impl CheckOutcome {
    pub fn failed(&self) -> bool {
        !self.ended.succeeded()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerificationReport {
    pub candidate_sha: String,
    pub base_sha: String,
    pub contract_hash: String,
    pub policy_hash: String,
    pub checks: Vec<CheckOutcome>,
    /// Required checks that were skipped, inert, unavailable or
    /// untrusted: gaps, never passes (SPEC §10).
    pub gaps: Vec<String>,
    /// Failures that also failed at the base: visible, not waived
    /// automatically (SPEC §10).
    pub baseline_failures: Vec<String>,
    pub amont_bypasses: Vec<String>,
    pub amont_downgrades: Vec<String>,
    /// Verification inputs the candidate changed (build manifests, tests,
    /// the commands themselves): reviewed explicitly, never silently
    /// accepted (SPEC §10).
    #[serde(default)]
    pub verification_inputs_changed: Vec<String>,
    /// Optional integrations that were not available: reported, never
    /// counted as passed (SPEC §5).
    #[serde(default)]
    pub integration_gaps: Vec<String>,
    /// Whether the baseline results came from the cache (SPEC §18) or a
    /// fresh run at the base.
    #[serde(default)]
    pub baseline_cached: bool,
    /// Why the baseline was not cacheable, when the profile asked for
    /// caching and the toolchain could not be identified (audit V2).
    #[serde(default)]
    pub baseline_cache_refused: Option<CacheRefused>,
}
impl VerificationReport {
    pub fn accepted(&self) -> bool {
        self.gaps.is_empty() && self.checks.iter().all(|check| !check.failed())
    }

    pub fn new_failures(&self) -> Vec<String> {
        self.checks
            .iter()
            .filter(|check| check.failed())
            .filter(|check| !self.baseline_failures.contains(&check.label))
            .map(|check| check.label.clone())
            .collect()
    }
}

/// Run one command against a directory, capturing the merged log to a file
/// under `logs_dir`, hashing it, and enforcing a wall timeout. The log is
/// evidence: it exists whether the check passed or failed. The LABEL is
/// stable across baseline and candidate runs (it identifies the command);
/// the LOG STEM differs so logs never collide.
pub fn run_command(
    dir: &Path,
    spec: &CommandSpec,
    logs_dir: &Path,
    label: &str,
    log_stem: &str,
) -> Result<CheckOutcome, VerifyError> {
    let timeout = command_timeout(spec)?;
    std::fs::create_dir_all(logs_dir)?;
    let log_path = logs_dir.join(format!("{log_stem}.log"));
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&log_path)?;
    let mut command = Command::new(&spec.argv[0]);
    command
        .args(&spec.argv[1..])
        .current_dir(dir)
        .stdout(log_file.try_clone()?)
        .stderr(log_file);
    crate::procs::own_process_group(&mut command);
    let mut child = command.spawn()?;
    // The whole process group goes when the check ends, however it ends:
    // a `cargo test` grandchild left running in a worktree that is about
    // to be removed corrupts the next thing that reads it (audit V1).
    let supervised = crate::procs::wait_for_exit(&mut child, timeout, None)?;
    let log_bytes = std::fs::read(&log_path)?;
    Ok(CheckOutcome {
        label: label.to_string(),
        argv: spec.argv.clone(),
        ended: supervised.ended,
        log_path: log_path.to_string_lossy().into_owned(),
        log_sha256: sha256_hex(&log_bytes),
    })
}

/// A command's wall clock. Zero is refused rather than rounded up to a
/// second: a policy that gives a check no time is a policy mistake, and
/// silently running it for one second made the mistake invisible
/// (style, audit §2).
pub fn command_timeout(spec: &CommandSpec) -> Result<Duration, VerifyError> {
    if spec.timeout_seconds == 0 {
        return Err(VerifyError::ZeroTimeout {
            argv: spec.argv.clone(),
        });
    }
    Ok(Duration::from_secs(spec.timeout_seconds))
}

/// A check's label: the command's own identity (`make check@1a2b3c4d`),
/// so a baseline failure matches the same command at the candidate even
/// after the profile is reordered or a command is inserted — a position
/// (`cmd0`) would silently re-pair them.
pub fn check_label(spec: &CommandSpec) -> String {
    let identity = sha256_hex(
        serde_json::to_string(&spec.argv)
            .expect("argv serializes")
            .as_bytes(),
    );
    format!(
        "{}@{}",
        spec.argv.first().map(String::as_str).unwrap_or("?"),
        &identity[..8]
    )
}

/// Run a full profile against a directory. The SAME labels identify the
/// same commands at the base and at every candidate, which is what makes
/// baseline-vs-candidate comparison meaningful (SPEC §10).
pub fn run_profile(
    dir: &Path,
    profile: &VerificationProfile,
    logs_dir: &Path,
    prefix: &str,
) -> Result<Vec<CheckOutcome>, VerifyError> {
    let mut outcomes = Vec::new();
    for (index, command) in profile.commands.iter().enumerate() {
        let label = check_label(command);
        let log_stem = format!("{prefix}-cmd{index}");
        outcomes.push(run_command(dir, command, logs_dir, &label, &log_stem)?);
    }
    Ok(outcomes)
}

/// What verification IS, as opposed to what it tests: build manifests,
/// lockfiles, toolchain pins, the check runner's own configuration, and
/// any repository-relative program the profile runs. A candidate that
/// edits one of these does not pass the checks — it changes them, and
/// the tree the checks ran on is no longer the tree the policy
/// described. SPEC §9 puts that in the user's hands ("changes protected
/// verification → needs_decision"): no model review can settle whether
/// relaxing a lockfile or a build flag is what the task wanted.
/// Matched by BASENAME at any depth: a monorepo keeps its manifests in
/// `apps/web/package.json` and `services/api/go.mod`, and a root-anchored
/// list left a candidate free to edit those and be accepted without the
/// user ever deciding on it (audit V9). The bare spellings are kept
/// beside the `**/` ones so a root-level file matches whatever a glob
/// engine does with a leading `**/`.
pub const POLICY_VERIFICATION_INPUTS: &[&str] = &[
    "Makefile",
    "makefile",
    "GNUmakefile",
    "justfile",
    "**/Makefile",
    "**/makefile",
    "**/GNUmakefile",
    "**/justfile",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "rust-toolchain",
    "**/Cargo.toml",
    "**/Cargo.lock",
    "**/rust-toolchain.toml",
    "**/rust-toolchain",
    "package.json",
    "package-lock.json",
    "pnpm-lock.yaml",
    "pnpm-workspace.yaml",
    "yarn.lock",
    "**/package.json",
    "**/package-lock.json",
    "**/pnpm-lock.yaml",
    "**/pnpm-workspace.yaml",
    "**/yarn.lock",
    "pyproject.toml",
    "requirements*.txt",
    "uv.lock",
    "poetry.lock",
    "**/pyproject.toml",
    "**/requirements*.txt",
    "**/uv.lock",
    "**/poetry.lock",
    "go.mod",
    "go.sum",
    "**/go.mod",
    "**/go.sum",
    "Gemfile",
    "Gemfile.lock",
    "**/Gemfile",
    "**/Gemfile.lock",
    "**/pytest.ini",
    "**/tox.ini",
];

/// The test tree: the cases and fixtures the checks execute. SPEC §10
/// explicitly invites candidates to add regression tests ("requested
/// behavior may need new regression tests"), so an edit here is normal
/// work — it just cannot go in unlooked-at, because a deleted or
/// loosened test weakens the contract as surely as a changed command.
/// That is the "explicit review" of §10, not a `needs_decision`.
pub const TEST_TREE_INPUTS: &[&str] = &[
    "**/tests/**",
    "**/test/**",
    "**/__tests__/**",
    "**/*_test.*",
    "**/*.test.*",
    "**/*.spec.*",
    "**/test_*.*",
    "**/conftest.py",
    "**/fixtures/**",
];

/// The policy-class patterns for a profile: the built-in list, the
/// profile's own `inputs` declarations (the repository naming what its
/// verdict depends on), and each command's program when it lives in the
/// repository (`./scripts/check.sh`) — the commands' own inputs.
pub fn verification_inputs(profile: &VerificationProfile) -> Vec<String> {
    let mut patterns: Vec<String> = POLICY_VERIFICATION_INPUTS
        .iter()
        .map(|p| p.to_string())
        .collect();
    patterns.extend(profile.inputs.iter().cloned());
    for command in &profile.commands {
        if let Some(program) = command.argv.first() {
            let relative = program.trim_start_matches("./");
            if program.starts_with("./") || program.contains('/') && !program.starts_with('/') {
                patterns.push(relative.to_string());
            }
        }
    }
    patterns.sort();
    patterns.dedup();
    patterns
}

/// Verification inputs a candidate touched, split by what the two
/// classes mean for the run. Only path classes are detectable: a
/// `#[cfg(test)]` module inside a source file is a test too, and nothing
/// here can see it — which is why `tests` is a review, not a proof.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TouchedInputs {
    /// Changes the user decides on (`needs_decision`).
    pub policy: Vec<String>,
    /// Changes a reviewer judges (review required).
    pub tests: Vec<String>,
}

impl TouchedInputs {
    pub fn is_empty(&self) -> bool {
        self.policy.is_empty() && self.tests.is_empty()
    }

    /// Every touched input, policy class first — what the receipt
    /// records and what a reviewer is shown.
    pub fn all(&self) -> Vec<String> {
        let mut all = self.policy.clone();
        all.extend(self.tests.iter().cloned());
        all
    }
}

/// Compile a pattern list. A pattern that will not compile BLOCKS, with
/// its own text in the message: dropping it silently left the input it
/// was written to protect unprotected, while `check_scope` refuses the
/// very same mistake in a write scope (audit V8).
pub fn matcher(patterns: &[String]) -> Result<globset::GlobSet, VerifyError> {
    let mut builder = globset::GlobSetBuilder::new();
    for pattern in patterns {
        let glob = globset::GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .map_err(|e| VerifyError::BadPattern {
                pattern: pattern.clone(),
                detail: e.to_string(),
            })?;
        builder.add(glob);
    }
    builder.build().map_err(|e| VerifyError::BadPattern {
        pattern: patterns.join(", "),
        detail: e.to_string(),
    })
}

/// Which of `changed` are verification inputs, and of which class. A
/// path in both classes (a `tests/Cargo.toml`) is policy: the stricter
/// outcome wins.
pub fn classify_verification_inputs(
    profile: &VerificationProfile,
    changed: &[String],
) -> Result<TouchedInputs, VerifyError> {
    let policy_set = matcher(&verification_inputs(profile))?;
    let test_set = matcher(
        &TEST_TREE_INPUTS
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>(),
    )?;
    let mut touched = TouchedInputs::default();
    for path in changed {
        if policy_set.is_match(path.as_str()) {
            touched.policy.push(path.clone());
        } else if test_set.is_match(path.as_str()) {
            touched.tests.push(path.clone());
        }
    }
    Ok(touched)
}

/// The effective check inventory from `amont list --json`
/// (envelope `amont-list-v1`). Parsed defensively: the envelope's `format`
/// field is asserted before reading, and unknown check statuses are
/// treated as gaps rather than passes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AmontInventory {
    pub checks: Vec<AmontCheck>,
    pub bypasses: Vec<String>,
    pub downgrades: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AmontCheck {
    pub id: String,
    pub stage: Option<String>,
    pub source: Option<String>,
    pub effective_severity: Option<String>,
    pub status: Option<AmontStatus>,
    pub reason: Option<String>,
}

/// What amont's inventory says about a check. Docs say
/// `ready|inert|skipped|unavailable`; code has emitted `runs` — only the
/// in-force spellings count as such, and a spelling this relais does not
/// know is kept verbatim and treated as a gap, the conservative direction
/// (SPEC §10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AmontStatus {
    Ready,
    Runs,
    Active,
    Inert,
    Skipped,
    Unavailable,
    #[serde(untagged)]
    Unknown(String),
}

impl AmontStatus {
    pub fn parse(text: &str) -> Self {
        serde_json::from_value(serde_json::Value::String(text.to_string()))
            .unwrap_or_else(|_| Self::Unknown(text.to_string()))
    }

    /// Genuinely in force, as opposed to inert, skipped, unavailable or
    /// something this relais cannot vouch for.
    pub fn in_force(&self) -> bool {
        matches!(self, Self::Ready | Self::Runs | Self::Active)
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Ready => "ready",
            Self::Runs => "runs",
            Self::Active => "active",
            Self::Inert => "inert",
            Self::Skipped => "skipped",
            Self::Unavailable => "unavailable",
            Self::Unknown(text) => text,
        }
    }
}

pub fn parse_amont_list(stdout: &str) -> Option<AmontInventory> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    match value.get("format").and_then(|f| f.as_str()) {
        Some("amont-list-v1") => {}
        other => {
            let _ = other;
            return None;
        }
    }
    let mut checks = Vec::new();
    for check in value.get("checks")?.as_array()? {
        let id = check.get("id").and_then(|id| id.as_str())?.to_string();
        checks.push(AmontCheck {
            id,
            stage: check
                .get("stage")
                .and_then(|s| s.as_str())
                .map(String::from),
            source: check
                .get("source")
                .and_then(|s| s.as_str())
                .map(String::from),
            effective_severity: check
                .get("effective_severity")
                .and_then(|s| s.as_str())
                .map(String::from),
            status: check
                .get("status")
                .and_then(|s| s.as_str())
                .map(AmontStatus::parse),
            reason: check
                .get("reason")
                .and_then(|s| s.as_str())
                .map(String::from),
        });
    }
    let strings = |key: &str| -> Vec<String> {
        value
            .get(key)
            .and_then(|v| v.as_array())
            .map(|entries| entries.iter().map(|entry| entry.to_string()).collect())
            .unwrap_or_default()
    };
    Some(AmontInventory {
        checks,
        bypasses: strings("bypasses"),
        downgrades: strings("downgrades"),
    })
}

/// Which side of a push the inventory is asked about. An enum, not a
/// `pushed: bool` at a call site where `false` says nothing about what
/// was meant (audit V14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// The checks as they stand for this working tree: what a commit
    /// here would run.
    Local,
    /// The checks a push would run.
    Pushed,
}

/// How long `amont list` may take. An inventory is a question about a
/// repository, so it is bounded and cancellable like every other probe
/// (audit V6) — generously, because it walks the repository's own
/// declarations.
pub const INVENTORY_TIMEOUT: Duration = Duration::from_secs(30);

/// The effective check inventory, as a port relais owns. The runner asks
/// a `HookInventory`; only `AmontCli` knows the binary's name.
pub trait HookInventory {
    /// The inventory for `dir`. `None` is no inventory — which
    /// `amont_gaps` turns into a gap for every required check, never
    /// into a pass.
    fn list(&self, dir: &Path, stage: Stage) -> Option<AmontInventory>;
}

/// The `amont` binary on this machine — the one place it is spawned.
#[derive(Debug, Default)]
pub struct AmontCli {
    cancel: Option<Arc<AtomicBool>>,
}

impl AmontCli {
    pub fn new() -> Self {
        Self { cancel: None }
    }

    /// The same, stopping with the run.
    pub fn watching(cancel: Arc<AtomicBool>) -> Self {
        Self {
            cancel: Some(cancel),
        }
    }
}

impl HookInventory for AmontCli {
    fn list(&self, dir: &Path, stage: Stage) -> Option<AmontInventory> {
        amont_list(dir, stage, self.cancel.as_deref())
    }
}

/// A fixed answer, for tests and for a run whose policy has the
/// integration off.
#[derive(Debug, Default)]
pub struct FixedInventory(pub Option<AmontInventory>);

impl HookInventory for FixedInventory {
    fn list(&self, _dir: &Path, _stage: Stage) -> Option<AmontInventory> {
        self.0.clone()
    }
}

/// Run `amont list --json` in a directory, bounded and cancellable.
pub fn amont_list(
    repo_dir: &Path,
    stage: Stage,
    cancel: Option<&AtomicBool>,
) -> Option<AmontInventory> {
    let mut command = Command::new("amont");
    command.arg("list").arg("--json").current_dir(repo_dir);
    match stage {
        Stage::Pushed => {
            command.arg("--pushed");
        }
        Stage::Local => {}
    }
    let end =
        crate::procs::run_with_timeout(command, INVENTORY_TIMEOUT, None, cancel, None).ok()?;
    if end.ended != Ended::Exited(0) {
        return None;
    }
    parse_amont_list(&end.stdout)
}

/// Gaps among the checks the run depends on: required amont checks that
/// are inert, skipped, unavailable or of unknown status are gaps, and a
/// check the inventory does not list at all is a gap too (SPEC §10).
pub fn amont_gaps(inventory: Option<&AmontInventory>, required_ids: &[String]) -> Vec<String> {
    let Some(inventory) = inventory else {
        return required_ids
            .iter()
            .map(|id| format!("{id}: inventory unavailable"))
            .collect();
    };
    let mut gaps = Vec::new();
    for required in required_ids {
        match inventory.checks.iter().find(|check| &check.id == required) {
            None => gaps.push(format!("{required}: not in effective inventory")),
            Some(check) => match &check.status {
                Some(status) if status.in_force() => {}
                Some(status) => gaps.push(format!(
                    "{required}: status `{}` — a skipped, inert, unavailable or untrusted required check is a gap, not a pass",
                    status.as_str()
                )),
                None => gaps.push(format!("{required}: no status reported")),
            },
        }
    }
    gaps
}

/// The check id a `bypasses`/`downgrades` entry names. The inventory
/// keeps entries verbatim as JSON, because amont spells a bypass as a
/// bare string and a downgrade as an object (`{"id":…,"severity":…}`),
/// and neither shape is worth guessing away at parse time. A waiver is
/// written by the user as a plain check id, so this is where the two
/// meet.
pub fn waived_id(entry: &str) -> String {
    let trimmed = entry.trim();
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(serde_json::Value::String(id)) => id,
        Ok(serde_json::Value::Object(fields)) => fields
            .get("id")
            .and_then(|id| id.as_str())
            .unwrap_or(trimmed)
            .to_string(),
        _ => trimmed.to_string(),
    }
}

/// Bypasses and downgrades the profile has not waived, as gaps. amont's
/// inventory declares them about ITSELF: a bypassed check did not run
/// and a downgraded one cannot fail the gate, so a candidate verified
/// under either was verified by something weaker than the policy
/// describes. SPEC §10 — "a skipped, inert, unavailable or untrusted
/// required check is a gap, not a pass" — makes that a gap unless the
/// profile names the check in `amont_waivers`, which is the "waiver
/// already in policy" the same section allows.
pub fn amont_waiver_gaps(inventory: Option<&AmontInventory>, waivers: &[String]) -> Vec<String> {
    let Some(inventory) = inventory else {
        return Vec::new();
    };
    let mut gaps = Vec::new();
    for (kind, entries) in [
        ("bypassed", &inventory.bypasses),
        ("downgraded", &inventory.downgrades),
    ] {
        for entry in entries {
            let id = waived_id(entry);
            if waivers.iter().any(|waiver| waiver == &id) {
                continue;
            }
            gaps.push(format!(
                "{id}: {kind} in amont's effective inventory ({entry}) and not waived by the \
                 verification profile — a check that cannot fail is not a pass"
            ));
        }
    }
    gaps
}

/// The checks a profile that names none should require: everything the
/// inventory reports as in force at blocking severity. A profile with an
/// empty `amont_checks` otherwise depends on nothing, so a bypassed or
/// inert gate would be invisible to the run that relies on it (SPEC §10:
/// "use amont's effective inventory to identify checks and gaps").
pub fn default_required_checks(inventory: &AmontInventory) -> Vec<String> {
    inventory
        .checks
        .iter()
        .filter(|check| {
            check.status.as_ref().is_some_and(AmontStatus::in_force)
                && check.effective_severity.as_deref() == Some("block")
        })
        .map(|check| check.id.clone())
        .collect()
}

/// What one candidate's verification established: the checks that ran,
/// the required checks that are gaps, and what amont's inventory declares
/// about itself.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Verified {
    pub checks: Vec<CheckOutcome>,
    pub gaps: Vec<String>,
    pub amont_bypasses: Vec<String>,
    pub amont_downgrades: Vec<String>,
}

/// The toolchain a profile's commands actually run on: for every program
/// the argv names, where it resolved and what version it reports, plus
/// the machine's declared identity.
///
/// `ToolVersions` names relais, aval, amont and Claude Code — none of
/// which decides whether `cargo test` passes. After a `rustup update` or
/// a new `make`, the old key still matched and a real regression came
/// back as "failed at the base too" (audit V2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Toolchain {
    pub programs: Vec<ProgramVersion>,
    pub os: String,
    pub arch: String,
}

/// Why a baseline verdict may not be cached for this profile. Recorded
/// in the receipt: a run that could not cache says why, rather than
/// quietly re-running (or quietly reusing) the baseline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheRefused {
    pub reason: String,
}

impl std::fmt::Display for CacheRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

/// Resolve every program the profile's commands run. The probe is a
/// parameter, so this stays a pure function over what the machine
/// answered. A program that cannot be versioned refuses the cache for
/// the whole profile: a key that cannot name the toolchain cannot say
/// two runs shared one.
pub fn resolve_toolchain(
    profile: &VerificationProfile,
    probe: &dyn Fn(&str) -> Result<ProgramVersion, VersionUnknown>,
) -> Result<Toolchain, CacheRefused> {
    let mut programs: Vec<ProgramVersion> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for command in &profile.commands {
        let Some(program) = command.argv.first() else {
            continue;
        };
        if seen.contains(program) {
            continue;
        }
        seen.push(program.clone());
        match probe(program) {
            Ok(resolved) => programs.push(resolved),
            Err(unknown) => {
                return Err(CacheRefused {
                    reason: format!(
                        "the baseline cannot be cached: {unknown}, so the key cannot say which \
                         toolchain produced the verdict"
                    ),
                })
            }
        }
    }
    programs.sort_by(|a, b| a.program.cmp(&b.program));
    Ok(Toolchain {
        programs,
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
    })
}

/// The baseline cache key: everything the base verdict is a function of
/// that relais can observe (SPEC §18: "candidate content, dependency
/// lockfiles, toolchain, command, configuration"). Lockfiles are part of
/// the base tree; the toolchain is the resolved programs the commands
/// run, on the machine identity they run on.
pub fn baseline_key(
    base_sha: &str,
    profile: &VerificationProfile,
    tools: &crate::context::ToolVersions,
    toolchain: &Toolchain,
) -> String {
    sha256_hex(
        serde_json::json!({
            "base": base_sha,
            "profile": profile,
            "tools": tools,
            "toolchain": toolchain,
        })
        .to_string()
        .as_bytes(),
    )
}

/// Cached baseline failures, one JSON file per key, opt-in per profile
/// (SPEC §18: not cacheable by default). A miss reruns the checks; a
/// corrupt entry is a miss.
pub struct BaselineCache {
    dir: PathBuf,
}

impl BaselineCache {
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
        }
    }

    pub fn get(&self, key: &str) -> Option<Vec<String>> {
        let text = std::fs::read_to_string(self.dir.join(format!("{key}.json"))).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn put(&self, key: &str, failures: &[String]) {
        if std::fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let _ = std::fs::write(
            self.dir.join(format!("{key}.json")),
            serde_json::to_string(failures).expect("serializes"),
        );
    }
}

/// A runner receipt (SPEC §12). Written by the runner into the ledger and
/// the run's artifact directory; a worker JSON document cannot impersonate
/// one because only `store_receipt` writes this shape and the ledger never
/// reads it back from worker output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    pub run_id: String,
    pub candidate_sha: String,
    pub base_sha: String,
    pub contract_hash: String,
    pub policy_hash: String,
    pub outcome: String,
    pub verification: VerificationReport,
    pub models_used: Vec<String>,
    pub attempts: u32,
    pub cost_completeness: CostCompleteness,
    pub cost: MicroUsd,
}

impl Receipt {
    pub fn hash(&self) -> String {
        canonical_json_hash(&serde_json::to_value(self).expect("receipt serializes"))
    }
}

/// Verification needs a working tree at an exact SHA that no worker can
/// touch: a throwaway worktree checked out at the candidate commit, then
/// removed. If creation fails the outcome is blocked, not a pass.
pub fn verification_worktree<'a>(
    git: &'a dyn Git,
    repo_dir: &Path,
    candidate_sha: &str,
    path: &Path,
) -> Result<VerificationWorktree<'a>, WorkspaceError> {
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
    git.run(
        repo_dir,
        &[
            "worktree",
            "add",
            "--detach",
            &path.to_string_lossy(),
            candidate_sha,
        ],
    )?;
    Ok(VerificationWorktree {
        git,
        repo_dir: repo_dir.to_path_buf(),
        path: path.to_path_buf(),
        released: false,
    })
}

/// A throwaway worktree at one candidate SHA. The lifetime is the
/// borrowed git port's, not a phantom: this value really does hold the
/// thing it needs to release itself.
pub struct VerificationWorktree<'a> {
    git: &'a dyn Git,
    repo_dir: PathBuf,
    path: PathBuf,
    released: bool,
}

impl VerificationWorktree<'_> {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Remove the worktree and prune its registration, reporting what
    /// git said. A verification worktree is throwaway by design — it
    /// exists for the length of one profile run and nothing is ever
    /// integrated from it — so releasing it is what keeps `git worktree
    /// list` from growing an entry per attempt; but a release that FAILS
    /// leaves a directory and a registration behind, and the caller is
    /// the only one who can put that in the run's record.
    pub fn release(&mut self) -> Result<(), WorkspaceError> {
        if self.released {
            return Ok(());
        }
        self.released = true;
        let removed = self.git.run(
            &self.repo_dir,
            &[
                "worktree",
                "remove",
                "--force",
                &self.path.to_string_lossy(),
            ],
        );
        // Prune whatever the removal left, including the administrative
        // entry for a directory that vanished from underneath us, and
        // report the first failure rather than the last.
        let pruned = self.git.run(&self.repo_dir, &["worktree", "prune"]);
        removed.and(pruned).map(|_| ())
    }
}

impl Drop for VerificationWorktree<'_> {
    fn drop(&mut self) {
        // Last resort: `release` is the way this is meant to end, and
        // the runner calls it. Reaching here means an early return or a
        // panic got there first, so the failure has nobody left to
        // return to — it is reported on stderr rather than discarded.
        if let Err(e) = self.release() {
            eprintln!(
                "relais: the verification worktree {} could not be released: {e}",
                self.path.display()
            );
        }
    }
}

/// Integration availability recorded into the report: required gaps block,
/// optional gaps are visible and never passes (SPEC §5). `available` is
/// asked about the BINARY the declaration names (`bin = …` overrides the
/// default), not the integration's config name.
pub fn integration_gaps(
    integrations: &Integrations,
    available: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut gaps = Vec::new();
    for (name, default_bin, dependency) in [
        ("aval", "aval", integrations.aval.as_ref()),
        ("amont", "amont", integrations.amont.as_ref()),
        (
            "amont-agent",
            "amont-agent",
            integrations.amont_agent.as_ref(),
        ),
    ] {
        let Some(dependency) = dependency else {
            continue;
        };
        let bin = dependency.bin().unwrap_or(default_bin);
        let installed = available(bin);
        match dependency.mode() {
            DependencyMode::Required if !installed => {
                gaps.push(format!("{name}: required integration unavailable"))
            }
            DependencyMode::Optional if !installed => gaps.push(format!(
                "{name}: optional integration not installed (reported, not passed)"
            )),
            // Present, or switched off in policy: an integration the run
            // does not consult is not a gap in the run.
            DependencyMode::Required | DependencyMode::Optional | DependencyMode::Off => {}
        }
    }
    gaps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(argv: &[&str], timeout_seconds: u64) -> CommandSpec {
        CommandSpec {
            argv: argv.iter().map(|a| a.to_string()).collect(),
            timeout_seconds,
        }
    }

    #[test]
    fn passing_and_failing_commands_are_recorded_with_log_hashes() {
        let dir = std::env::temp_dir().join(format!("relais-verify-{}", std::process::id()));
        let logs = dir.join("logs");
        let pass = run_command(
            &dir,
            &command(&["sh", "-c", "echo ok"], 10),
            &logs,
            "cmd0",
            "x-cmd0",
        )
        .expect("run");
        assert!(!pass.failed());
        assert_eq!(pass.ended, Ended::Exited(0));
        assert_eq!(pass.log_sha256.len(), 64);
        let fail = run_command(
            &dir,
            &command(&["sh", "-c", "echo bad >&2; exit 1"], 10),
            &logs,
            "cmd1",
            "x-cmd1",
        )
        .expect("run");
        assert!(fail.failed());
        assert_eq!(fail.ended, Ended::Exited(1));
        let log = std::fs::read_to_string(&fail.log_path).expect("log exists");
        assert!(log.contains("bad"), "stderr is captured into the log");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn timeouts_kill_the_check_and_count_as_failures() {
        let dir = std::env::temp_dir().join(format!("relais-verify-t-{}", std::process::id()));
        let logs = dir.join("logs");
        let hung = run_command(
            &dir,
            &command(&["sh", "-c", "sleep 5"], 1),
            &logs,
            "cmd0",
            "x-cmd0",
        )
        .expect("run");
        assert_eq!(hung.ended, Ended::TimedOut);
        assert!(hung.failed());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn labels_are_stable_across_baseline_and_candidate_runs() {
        let dir = std::env::temp_dir().join(format!("relais-verify-l-{}", std::process::id()));
        let logs = dir.join("logs");
        let profile = VerificationProfile {
            commands: vec![command(&["sh", "-c", "true"], 10)],
            amont_checks: Vec::new(),
            amont_waivers: Vec::new(),
            inputs: Vec::new(),
            cache_baseline: false,
        };
        let base = run_profile(&dir, &profile, &logs, "base").expect("base");
        let candidate = run_profile(&dir, &profile, &logs, "attempt1").expect("candidate");
        assert_eq!(base[0].label, check_label(&profile.commands[0]));
        assert!(base[0].label.starts_with("sh@"), "{}", base[0].label);
        assert_eq!(
            candidate[0].label, base[0].label,
            "the same command keeps its label"
        );
        assert_ne!(
            base[0].log_path, candidate[0].log_path,
            "logs never collide"
        );
        // A reordered profile does not re-pair a baseline failure with a
        // different command: the label is the command's, not its slot's.
        let reordered = VerificationProfile {
            commands: vec![
                command(&["sh", "-c", "false"], 10),
                profile.commands[0].clone(),
            ],
            ..profile.clone()
        };
        let again = run_profile(&dir, &reordered, &logs, "attempt2").expect("reordered");
        assert_eq!(again[1].label, base[0].label);
        assert_ne!(again[0].label, base[0].label);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn verification_inputs_split_policy_from_the_test_tree() {
        let profile = VerificationProfile {
            commands: vec![
                command(&["./scripts/check.sh"], 10),
                command(&["make", "check"], 10),
            ],
            amont_checks: Vec::new(),
            amont_waivers: Vec::new(),
            inputs: vec!["ci/**".into()],
            cache_baseline: false,
        };
        let touched = classify_verification_inputs(
            &profile,
            &[
                "src/lib.rs".into(),
                "Makefile".into(),
                "crates/x/Cargo.toml".into(),
                "tests/smoke.rs".into(),
                "src/foo_test.go".into(),
                "tests/test_parser.py".into(),
                "scripts/check.sh".into(),
                "ci/lint.yml".into(),
                "docs/README.md".into(),
            ],
        )
        .expect("the built-in patterns compile");
        assert_eq!(
            touched.policy,
            vec![
                "Makefile",
                "crates/x/Cargo.toml",
                "scripts/check.sh",
                "ci/lint.yml",
            ],
            "build manifests, the profile's declared inputs and the commands' own programs"
        );
        assert_eq!(
            touched.tests,
            vec!["tests/smoke.rs", "src/foo_test.go", "tests/test_parser.py"],
            "the test tree is work SPEC §10 invites, not a policy change"
        );
        assert_eq!(touched.all().len(), 7);
        assert!(
            classify_verification_inputs(&profile, &["src/lib.rs".into()])
                .expect("compiles")
                .is_empty()
        );
    }

    /// V9: a monorepo keeps its manifests below the root. Editing
    /// `apps/web/package.json` changes what the checks build as surely
    /// as editing the root one, and the root-anchored list accepted it
    /// without the user deciding anything.
    #[test]
    fn a_nested_manifest_is_a_policy_change_at_any_depth() {
        let profile = VerificationProfile::default();
        let touched = classify_verification_inputs(
            &profile,
            &[
                "apps/web/package.json".into(),
                "services/api/go.mod".into(),
                "libs/py/pyproject.toml".into(),
                "libs/py/requirements-dev.txt".into(),
                "tools/Makefile".into(),
                "crates/x/rust-toolchain.toml".into(),
                "apps/web/pnpm-lock.yaml".into(),
                "package.json".into(),
                "Makefile".into(),
                "apps/web/src/index.ts".into(),
            ],
        )
        .expect("the built-in patterns compile");
        assert_eq!(
            touched.policy,
            vec![
                "apps/web/package.json",
                "services/api/go.mod",
                "libs/py/pyproject.toml",
                "libs/py/requirements-dev.txt",
                "tools/Makefile",
                "crates/x/rust-toolchain.toml",
                "apps/web/pnpm-lock.yaml",
                "package.json",
                "Makefile",
            ],
            "manifests and lockfiles are policy wherever they live"
        );
        assert!(touched.tests.is_empty());
    }

    /// V8: a pattern nobody can compile protects nothing. Dropping it
    /// silently left the input it was written for unguarded, while
    /// `check_scope` refuses the same mistake in a write scope.
    #[test]
    fn a_malformed_input_pattern_blocks_and_names_itself() {
        let profile = VerificationProfile {
            commands: Vec::new(),
            amont_checks: Vec::new(),
            amont_waivers: Vec::new(),
            inputs: vec!["ci/[unclosed".into()],
            cache_baseline: false,
        };
        let error = classify_verification_inputs(&profile, &["ci/lint.yml".into()])
            .expect_err("a pattern that will not compile blocks the run");
        assert!(
            matches!(&error, VerifyError::BadPattern { pattern, .. } if pattern == "ci/[unclosed"),
            "{error:?}"
        );
        assert!(error.to_string().contains("ci/[unclosed"), "{error}");
    }

    /// A check with no time to run in is a policy mistake, not a
    /// one-second check.
    #[test]
    fn a_command_with_a_zero_timeout_is_refused() {
        let dir = std::env::temp_dir().join(format!(
            "relais-zero-{}-{}",
            std::process::id(),
            next_fixture()
        ));
        let error = run_command(
            &dir,
            &command(&["sh", "-c", "true"], 0),
            &dir.join("logs"),
            "cmd0",
            "x-cmd0",
        )
        .expect_err("zero seconds is refused");
        assert!(matches!(error, VerifyError::ZeroTimeout { .. }), "{error}");
        assert!(error.to_string().contains("timeout_seconds = 0"), "{error}");
        assert_eq!(
            command_timeout(&command(&["sh"], 12)).expect("a real timeout"),
            Duration::from_secs(12)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A path both classes could claim is judged by the stricter one:
    /// a manifest inside the test tree still decides what gets built.
    #[test]
    fn a_manifest_in_the_test_tree_is_a_policy_change() {
        let profile = VerificationProfile::default();
        let touched = classify_verification_inputs(&profile, &["tests/fixtures/Cargo.toml".into()])
            .expect("compiles");
        assert_eq!(touched.policy, vec!["tests/fixtures/Cargo.toml"]);
        assert!(touched.tests.is_empty());
    }

    /// V2: the key must move when the TOOLCHAIN moves. `ToolVersions`
    /// names relais, aval, amont and Claude Code, none of which decides
    /// whether `cargo test` passes: after a `rustup update` the old key
    /// still matched and a genuine regression came back as
    /// "failed at the base too".
    #[test]
    fn the_baseline_key_moves_with_the_compiler_that_runs_the_checks() {
        let dir = std::env::temp_dir().join(format!(
            "relais-bcache-{}-{}",
            std::process::id(),
            next_fixture()
        ));
        std::fs::remove_dir_all(&dir).ok();
        let cache = BaselineCache::new(&dir);
        let profile = VerificationProfile {
            commands: vec![command(&["cargo", "test"], 10)],
            amont_checks: Vec::new(),
            amont_waivers: Vec::new(),
            inputs: Vec::new(),
            cache_baseline: true,
        };
        let tools = crate::context::ToolVersions {
            relais: "0.1.0".into(),
            aval: None,
            amont: Some("amont 1.36.1".into()),
            claude_code: None,
        };
        let toolchain = Toolchain {
            programs: vec![ProgramVersion {
                program: "cargo".into(),
                path: "/usr/local/bin/cargo".into(),
                version: "cargo 1.88.0".into(),
            }],
            os: "linux".into(),
            arch: "x86_64".into(),
        };
        let key = baseline_key("abc", &profile, &tools, &toolchain);
        assert_eq!(cache.get(&key), None, "a miss reruns the checks");
        cache.put(&key, &["cargo@deadbeef".to_string()]);
        assert_eq!(cache.get(&key), Some(vec!["cargo@deadbeef".to_string()]));

        let updated = Toolchain {
            programs: vec![ProgramVersion {
                version: "cargo 1.90.0".into(),
                ..toolchain.programs[0].clone()
            }],
            ..toolchain.clone()
        };
        assert_ne!(
            key,
            baseline_key("abc", &profile, &tools, &updated),
            "a compiler update is a different baseline"
        );
        let elsewhere = Toolchain {
            programs: vec![ProgramVersion {
                path: "/home/dev/.cargo/bin/cargo".into(),
                ..toolchain.programs[0].clone()
            }],
            ..toolchain.clone()
        };
        assert_ne!(
            key,
            baseline_key("abc", &profile, &tools, &elsewhere),
            "the same version from another path is another toolchain"
        );
        let other_machine = Toolchain {
            arch: "aarch64".into(),
            ..toolchain.clone()
        };
        assert_ne!(
            key,
            baseline_key("abc", &profile, &tools, &other_machine),
            "os and arch are part of the environment identity"
        );
        assert_ne!(
            key,
            baseline_key("abd", &profile, &tools, &toolchain),
            "base is in the key"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A program the machine cannot version refuses the cache, with the
    /// reason the receipt records — rather than a key that quietly says
    /// nothing about what ran.
    #[test]
    fn a_profile_whose_programs_cannot_be_versioned_refuses_the_cache() {
        let profile = VerificationProfile {
            commands: vec![
                command(&["make", "check"], 10),
                command(&["make", "lint"], 10),
            ],
            amont_checks: Vec::new(),
            amont_waivers: Vec::new(),
            inputs: Vec::new(),
            cache_baseline: true,
        };
        let resolved = resolve_toolchain(&profile, &|program| {
            Ok(ProgramVersion {
                program: program.to_string(),
                path: format!("/usr/bin/{program}"),
                version: format!("{program} 4.4"),
            })
        })
        .expect("every program answered");
        assert_eq!(
            resolved.programs.len(),
            1,
            "one entry per distinct program, not per command"
        );
        assert_eq!(resolved.os, std::env::consts::OS);

        let refused = resolve_toolchain(&profile, &|program| {
            Err(VersionUnknown::NoAnswer {
                program: program.to_string(),
                detail: "it printed nothing".into(),
            })
        })
        .expect_err("a program that will not say its version refuses the cache");
        assert!(refused.reason.contains("make"), "{refused}");
        assert!(refused.reason.contains("cannot be cached"), "{refused}");
    }

    /// Unique fixture directories under parallel test threads.
    fn next_fixture() -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    const AMONT_SAMPLE: &str = r#"{
        "format": "amont-list-v1",
        "stage_filter": null,
        "pushed": false,
        "checks": [
            {"id": "pre-commit-lint-shell", "stage": "pre-commit", "source": "builtin", "effective_severity": "block", "status": "runs", "reason": null, "scope_files": [], "command": "sh"},
            {"id": "declared-rubocop", "stage": "pre-commit", "source": "declared", "effective_severity": "block", "status": "inert", "reason": "untrusted declaration", "command": "rubocop"}
        ],
        "bypasses": [],
        "downgrades": [{"id": "pre-push-cargo-test", "severity": "warn"}]
    }"#;

    #[test]
    fn amont_inventory_parses_and_flags_inert_checks_as_gaps() {
        let inventory = parse_amont_list(AMONT_SAMPLE).expect("envelope parses");
        assert_eq!(inventory.checks.len(), 2);
        assert_eq!(inventory.downgrades.len(), 1);
        let gaps = amont_gaps(
            Some(&inventory),
            &[
                "pre-commit-lint-shell".to_string(),
                "declared-rubocop".to_string(),
                "missing-check".to_string(),
            ],
        );
        assert_eq!(
            gaps.len(),
            2,
            "active check passes; inert and missing are gaps"
        );
        assert!(gaps[0].contains("inert"), "{gaps:?}");
        assert!(gaps[1].contains("not in effective inventory"), "{gaps:?}");
    }

    #[test]
    fn bypasses_and_downgrades_are_gaps_unless_the_profile_waives_them() {
        let inventory = parse_amont_list(AMONT_SAMPLE).expect("envelope parses");
        let gaps = amont_waiver_gaps(Some(&inventory), &[]);
        assert_eq!(gaps.len(), 1, "one downgrade, no bypasses: {gaps:?}");
        assert!(
            gaps[0].starts_with("pre-push-cargo-test: downgraded"),
            "{gaps:?}"
        );
        assert!(
            amont_waiver_gaps(Some(&inventory), &["pre-push-cargo-test".to_string()]).is_empty(),
            "an explicit policy waiver is the one thing that clears it"
        );
        // A bypass is a bare string in the envelope; its id is the
        // string, not the quoted JSON.
        let bypassed = parse_amont_list(
            r#"{"format":"amont-list-v1","checks":[],"bypasses":["pre-commit-fmt"],"downgrades":[]}"#,
        )
        .expect("parses");
        let gaps = amont_waiver_gaps(Some(&bypassed), &[]);
        assert!(gaps[0].starts_with("pre-commit-fmt: bypassed"), "{gaps:?}");
        assert!(amont_waiver_gaps(Some(&bypassed), &["pre-commit-fmt".to_string()]).is_empty());
        assert!(
            amont_waiver_gaps(None, &[]).is_empty(),
            "no inventory is the missing-inventory gap's business, not this one"
        );
    }

    #[test]
    fn a_profile_naming_no_checks_requires_the_in_force_blocking_ones() {
        let inventory = parse_amont_list(AMONT_SAMPLE).expect("envelope parses");
        assert_eq!(
            default_required_checks(&inventory),
            vec!["pre-commit-lint-shell".to_string()],
            "the inert `block` check is not required into existence; it is simply not in force"
        );
        // …and requiring it is what turns an inert check into a gap.
        assert!(
            !amont_gaps(Some(&inventory), &default_required_checks(&inventory))
                .iter()
                .any(|gap| gap.contains("pre-commit-lint-shell"))
        );
    }

    #[test]
    fn a_verification_worktree_releases_itself_when_its_checks_are_done() {
        let dir = std::env::temp_dir().join(format!(
            "relais-vwt-{}-{}",
            std::process::id(),
            next_fixture()
        ));
        std::fs::remove_dir_all(&dir).ok();
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        let no_hooks = dir.join("no-hooks");
        std::fs::create_dir_all(&no_hooks).expect("mkdir");
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .expect("git");
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["config", "core.hooksPath", &no_hooks.to_string_lossy()]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(repo.join("f.txt"), "base\n").expect("write");
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "base"]);
        let head = git(&["rev-parse", "HEAD"]);
        let system = crate::workspace::SystemGit;
        let path = dir.join("verify/verify-1");
        {
            // The caller releases it and sees what git said; releasing
            // twice is not an error, and `Drop` has nothing left to do.
            let mut holder = verification_worktree(&system, &repo, &head, &path).expect("worktree");
            assert!(holder.path().join("f.txt").is_file());
            assert!(git(&["worktree", "list"]).contains("verify-1"));
            holder.release().expect("released, and git said so");
            assert!(!path.exists(), "release is what removes it");
            holder.release().expect("releasing twice is not an error");
        }
        assert!(
            !git(&["worktree", "list"]).contains("verify-1"),
            "the throwaway worktree is gone, administrative entry included"
        );

        // And a holder nobody releases is still cleaned up by `Drop`:
        // the last resort, not the way it is meant to end.
        let dropped = dir.join("verify/verify-2");
        {
            let _holder = verification_worktree(&system, &repo, &head, &dropped).expect("worktree");
            assert!(git(&["worktree", "list"]).contains("verify-2"));
        }
        assert!(!dropped.exists());
        assert!(!git(&["worktree", "list"]).contains("verify-2"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn wrong_amont_envelope_is_rejected_not_guessed() {
        assert!(parse_amont_list(r#"{"format": "something-else-v9"}"#).is_none());
        assert!(parse_amont_list("not json").is_none());
    }

    #[test]
    fn missing_inventory_means_every_required_check_is_a_gap() {
        let gaps = amont_gaps(None, &["pre-push-cargo-test".to_string()]);
        assert_eq!(gaps, vec!["pre-push-cargo-test: inventory unavailable"]);
    }

    #[test]
    fn accepted_requires_checks_pass_and_no_gaps() {
        let report = VerificationReport {
            candidate_sha: "abc".into(),
            base_sha: "def".into(),
            contract_hash: "ch".into(),
            policy_hash: "ph".into(),
            checks: vec![CheckOutcome {
                label: "cmd0".into(),
                argv: vec!["make".into(), "check".into()],
                ended: Ended::Exited(0),
                log_path: "x.log".into(),
                log_sha256: "h".into(),
            }],
            gaps: vec![],
            baseline_failures: vec![],
            amont_bypasses: vec![],
            amont_downgrades: vec![],
            verification_inputs_changed: Vec::new(),
            integration_gaps: Vec::new(),
            baseline_cached: false,
            baseline_cache_refused: None,
        };
        assert!(report.accepted());
        assert!(report.new_failures().is_empty());

        let blocked = VerificationReport {
            gaps: vec!["declared-rubocop: status `inert`".into()],
            ..report
        };
        assert!(!blocked.accepted(), "a gap is never a pass");
    }

    #[test]
    fn baseline_failures_are_visible_but_not_waived() {
        let failing_check = CheckOutcome {
            label: "flaky".into(),
            argv: vec![],
            ended: Ended::Exited(1),
            log_path: "f.log".into(),
            log_sha256: "h".into(),
        };
        let report = VerificationReport {
            candidate_sha: "abc".into(),
            base_sha: "def".into(),
            contract_hash: "ch".into(),
            policy_hash: "ph".into(),
            checks: vec![failing_check],
            gaps: vec![],
            baseline_failures: vec!["flaky".into()],
            amont_bypasses: vec![],
            amont_downgrades: vec![],
            verification_inputs_changed: Vec::new(),
            integration_gaps: Vec::new(),
            baseline_cached: false,
            baseline_cache_refused: None,
        };
        assert!(!report.accepted(), "baseline failures are not auto-waived");
        assert!(
            report.new_failures().is_empty(),
            "the pre-existing failure is visible, not new"
        );
    }

    #[test]
    fn receipt_hash_binds_the_candidate_and_policy() {
        let receipt = Receipt {
            run_id: "run-1".into(),
            candidate_sha: "abc".into(),
            base_sha: "def".into(),
            contract_hash: "ch".into(),
            policy_hash: "ph".into(),
            outcome: "accepted".into(),
            verification: VerificationReport {
                candidate_sha: "abc".into(),
                base_sha: "def".into(),
                contract_hash: "ch".into(),
                policy_hash: "ph".into(),
                checks: vec![],
                gaps: vec![],
                baseline_failures: vec![],
                amont_bypasses: vec![],
                amont_downgrades: vec![],
                verification_inputs_changed: Vec::new(),
                integration_gaps: Vec::new(),
                baseline_cached: false,
                baseline_cache_refused: None,
            },
            models_used: vec!["sonnet".into()],
            attempts: 1,
            cost_completeness: CostCompleteness::Actual,
            cost: MicroUsd::from_micros(12_345),
        };
        let hash = receipt.hash();
        let mut other = receipt.clone();
        other.candidate_sha = "different".into();
        assert_ne!(
            hash,
            other.hash(),
            "a changed candidate has a different identity"
        );
    }

    #[test]
    fn optional_integration_gaps_are_reported_not_passed() {
        let integrations = crate::policy::Integrations {
            aval: Some(crate::policy::Dependency::Mode(DependencyMode::Required)),
            amont: Some(crate::policy::Dependency::Mode(DependencyMode::Optional)),
            amont_agent: None,
        };
        let gaps = integration_gaps(&integrations, &|name| name == "aval");
        assert_eq!(
            gaps,
            vec!["amont: optional integration not installed (reported, not passed)"],
            "required-but-missing is NOT a gap here because aval is available"
        );
        let none = integration_gaps(&integrations, &|_| false);
        assert_eq!(none.len(), 2, "required missing + optional missing");
        assert!(none[0].starts_with("aval: required"));
    }
}
