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

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::acceptance::{independent, AcceptanceEntry, Evidence};
use crate::ids::{canonical_json_hash, sha256_hex};
use crate::money::{CostCompleteness, MicroUsd};
use crate::policy::{
    named_command, CommandSpec, DependencyMode, Integrations, VerificationProfile,
};
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

/// Mandatory criteria whose declared evidence produced nothing to
/// check: a gap in this same report, not a second acceptance path — the
/// run is refused by the mechanism that already refuses gaps (SPEC §10).
///
/// Two kinds of evidence can come up empty. A named check may have
/// produced no outcome; a named check that ran and FAILED is already a
/// failing check and needs no separate gap, and preflight already
/// refuses a name no profile defines, so that branch here is defensive.
/// A human sign-off is unmet unless `signoffs` names this criterion's own
/// id — `relais decide --answer approve --criterion <id>` is the only
/// writer of that set (SPEC §10); nothing else can clear this gap, so a
/// mandatory criterion asking for one can never be silently waived.
///
/// An amont gate is unmet unless `gate_coverage` maps its name to
/// `Ok(true)` — the answer `amont attest covered` gave when asked
/// against the candidate's own tree (see [`amont_gate_coverage`]). A gate
/// this map does not mention at all is treated the same as `Ok(false)`:
/// conservative, and the branch is defensive since every gate a
/// declared criterion names is queried before this runs.
pub fn acceptance_gaps(
    entries: &[AcceptanceEntry],
    profile: &VerificationProfile,
    checks: &[CheckOutcome],
    signoffs: &HashSet<String>,
    gate_coverage: &BTreeMap<String, Result<bool, AttestError>>,
) -> Vec<AcceptanceGap> {
    let mut gaps = Vec::new();
    for entry in entries {
        if !entry.mandatory() {
            continue;
        }
        match entry.evidence() {
            Some(Evidence::Check { name }) => match named_command(profile, name) {
                Some(spec) => {
                    let label = check_label(spec);
                    if !checks.iter().any(|check| check.label == label) {
                        gaps.push(AcceptanceGap {
                            criterion_id: entry.id(),
                            missing: MissingEvidence::CheckProducedNothing { name: name.clone() },
                        });
                    }
                }
                None => gaps.push(AcceptanceGap {
                    criterion_id: entry.id(),
                    missing: MissingEvidence::CheckUndefined { name: name.clone() },
                }),
            },
            Some(Evidence::HumanSignOff) if !signoffs.contains(&entry.id()) => {
                gaps.push(AcceptanceGap {
                    criterion_id: entry.id(),
                    missing: MissingEvidence::SignOffUnrecorded,
                })
            }
            Some(Evidence::AmontGate { gate }) => match gate_coverage.get(gate) {
                Some(Ok(true)) => {}
                Some(Ok(false)) | None => gaps.push(AcceptanceGap {
                    criterion_id: entry.id(),
                    missing: MissingEvidence::GateNotCovered { gate: gate.clone() },
                }),
                Some(Err(cause)) => gaps.push(AcceptanceGap {
                    criterion_id: entry.id(),
                    missing: MissingEvidence::GateUnavailable {
                        gate: gate.clone(),
                        detail: cause.to_string(),
                    },
                }),
            },
            // A test runs inside the profile's own commands and an LLM
            // review is the reviewer's verdict: both are settled by the
            // report as a whole, so neither can come up empty on its own.
            // A bare string has declared no evidence to be missing. A
            // human sign-off already recorded for this id is met, not a
            // gap.
            Some(Evidence::Test { .. } | Evidence::LlmReview | Evidence::HumanSignOff) | None => {}
        }
    }
    gaps
}

/// One mandatory criterion whose declared evidence is missing, kept as
/// the criterion's id and what is missing rather than as the sentence a
/// person reads.
///
/// `VerificationReport.gaps` is — and stays — a list of sentences: it is
/// frozen into receipts and transition details, and a stored record's
/// wording is not something a later binary gets to change. But a caller
/// deciding what to DO about a gap must not read that prose. Two
/// spellings of one sentence in two modules is how a reworded message
/// silently stops clearing a gap, so the sentence is written in exactly
/// one place ([`AcceptanceGap::message`]) and recognized in exactly one
/// other ([`sign_off_gap_criterion`]), which is its inverse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptanceGap {
    pub criterion_id: String,
    pub missing: MissingEvidence,
}

/// What a mandatory criterion's declared evidence failed to produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissingEvidence {
    /// A named check the profile defines, which ran nothing.
    CheckProducedNothing { name: String },
    /// A named check no profile defines. Preflight already refuses this,
    /// so reaching it here is defensive.
    CheckUndefined { name: String },
    /// A human sign-off nobody has recorded. The one kind a person's own
    /// later answer can clear (SPEC §10).
    SignOffUnrecorded,
    /// `amont attest covered` was asked, on the candidate's own tree, and
    /// answered "no" — fail-open by design, so this cannot tell an
    /// absent attestation from one whose signature failed to verify
    /// (SPEC §10, §18).
    GateNotCovered { gate: String },
    /// `amont attest covered` could not be asked at all: amont is
    /// absent, too old to have the subcommand, or failed some other way.
    /// The cause relais actually observed, never guessed.
    GateUnavailable { gate: String, detail: String },
}

impl AcceptanceGap {
    /// The sentence that goes into `VerificationReport.gaps`. The only
    /// place any of these is written.
    pub fn message(&self) -> String {
        let id = &self.criterion_id;
        match &self.missing {
            MissingEvidence::CheckProducedNothing { name } => format!(
                "acceptance criterion `{id}` names check `{name}`, which produced no evidence"
            ),
            MissingEvidence::CheckUndefined { name } => format!(
                "acceptance criterion `{id}` names check `{name}`, which the verification profile \
                 does not define"
            ),
            MissingEvidence::SignOffUnrecorded => format!(
                "acceptance criterion `{id}` names a human sign-off, which nothing has recorded"
            ),
            MissingEvidence::GateNotCovered { gate } => format!(
                "acceptance criterion `{id}` names amont gate `{gate}`, which amont does not \
                 report as covered on this candidate — its own interface cannot tell an absent \
                 attestation from one whose signature failed to verify; run `amont attest \
                 covered {gate}` yourself to see the same answer"
            ),
            MissingEvidence::GateUnavailable { gate, detail } => format!(
                "acceptance criterion `{id}` names amont gate `{gate}`, which could not be \
                 asked about: {detail}"
            ),
        }
    }
}

/// The criterion id inside an ALREADY-STORED sign-off gap sentence, or
/// `None` when the sentence is some other gap.
///
/// This exists for one reader: `relais decide`, which must decide
/// whether a sign-off has since cleared a gap recorded in a transition
/// detail frozen months ago. That detail holds prose and nothing else,
/// so the prose is parsed — by the exact inverse of
/// [`AcceptanceGap::message`], with a round-trip test over every
/// [`MissingEvidence`] variant, rather than by a `contains` in the
/// caller.
pub fn sign_off_gap_criterion(gap: &str) -> Option<&str> {
    const PREFIX: &str = "acceptance criterion `";
    const SUFFIX: &str = "` names a human sign-off, which nothing has recorded";
    gap.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)
}

/// How independent the mandatory criteria's evidence was, taken
/// together (SPEC §10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndependenceSummary {
    AllIndependent,
    PartlyIndependent,
    NoneIndependent,
}

/// One acceptance criterion's settlement: whether it was met, and by
/// which evidence. `evidence` is absent for a bare string, which has
/// none declared.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CriterionOutcome {
    pub id: String,
    pub statement: String,
    pub mandatory: bool,
    pub met: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
}

/// Settle every acceptance criterion against a finished verification
/// report. A named check is met exactly when its own outcome succeeded.
/// A test, a review, or a bare string with no evidence declared is met
/// exactly when the report as a whole is accepted, because nothing here
/// can isolate one test's result inside a command that runs many. A
/// criterion settled only by a model-added test or an LLM review is
/// still accepted when the checks pass: relais reports what it knows
/// (the evidence was not independent) rather than inventing a stricter
/// rule than the contract asked for (SPEC §10).
///
/// A human sign-off is the one kind that passing checks cannot settle,
/// because it is not a claim about the code: it is met exactly when
/// `signoffs` names this criterion's own id, recorded by nothing but
/// `relais decide --answer approve --criterion <id>` (SPEC §10). Reading
/// it off `accepted` would report a sign-off nobody gave.
///
/// An amont gate is settled the same way: met exactly when
/// `gate_coverage` maps its name to `Ok(true)`, amont's own answer to
/// `amont attest covered` on the candidate's own tree — never by reading
/// `accepted`, which says nothing about a gate amont owns.
pub fn settle_acceptance(
    entries: &[AcceptanceEntry],
    profile: &VerificationProfile,
    report: &VerificationReport,
    signoffs: &HashSet<String>,
    gate_coverage: &BTreeMap<String, Result<bool, AttestError>>,
) -> (Vec<CriterionOutcome>, Option<IndependenceSummary>) {
    let accepted = report.accepted();
    let criteria: Vec<CriterionOutcome> = entries
        .iter()
        .map(|entry| {
            let evidence = entry.evidence().cloned();
            let met = match &evidence {
                Some(Evidence::Check { name }) => {
                    named_command(profile, name).is_some_and(|spec| {
                        let label = check_label(spec);
                        report
                            .checks
                            .iter()
                            .any(|check| check.label == label && !check.failed())
                    })
                }
                Some(Evidence::Test { .. } | Evidence::LlmReview) | None => accepted,
                Some(Evidence::HumanSignOff) => signoffs.contains(&entry.id()),
                Some(Evidence::AmontGate { gate }) => {
                    matches!(gate_coverage.get(gate), Some(Ok(true)))
                }
            };
            CriterionOutcome {
                id: entry.id(),
                statement: entry.statement().to_string(),
                mandatory: entry.mandatory(),
                met,
                evidence,
            }
        })
        .collect();

    let summary = independence_summary(&criteria);
    (criteria, summary)
}

/// The `mandatory_evidence_independence` a set of already-settled
/// criteria imply, taken together (SPEC §10): every mandatory
/// criterion's evidence independent, none of it, or a mix. Shared by
/// [`settle_acceptance`] and `relais decide`'s receipt re-seal, so
/// recomputing this after a sign-off is recorded is the same
/// computation the receipt was built with the first time, not a second
/// one that could drift from it.
pub fn independence_summary(criteria: &[CriterionOutcome]) -> Option<IndependenceSummary> {
    let mandatory_independence: Vec<bool> = criteria
        .iter()
        .filter(|criterion| criterion.mandatory)
        .map(|criterion| match &criterion.evidence {
            Some(evidence) => independent(evidence),
            // Settled by the verification profile as a whole: a check,
            // in spirit, and so independent.
            None => true,
        })
        .collect();
    if mandatory_independence.is_empty() {
        None
    } else if mandatory_independence
        .iter()
        .all(|&is_independent| is_independent)
    {
        Some(IndependenceSummary::AllIndependent)
    } else if mandatory_independence
        .iter()
        .all(|&is_independent| !is_independent)
    {
        Some(IndependenceSummary::NoneIndependent)
    } else {
        Some(IndependenceSummary::PartlyIndependent)
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
    let ended = match command.spawn() {
        Ok(mut child) => {
            // The whole process group goes when the check ends, however
            // it ends: a `cargo test` grandchild left running in a
            // worktree that is about to be removed corrupts the next
            // thing that reads it (audit V1).
            crate::procs::wait_for_exit(&mut child, timeout, None)?.ended
        }
        // A program that is not there is the same fact whether relais
        // spawned it directly or a shell looked for it: the shell says
        // "command not found" and exits 127, and so does this. Written
        // into the log so the evidence reads the same either way, and
        // recorded as an outcome — not a runner failure — so the
        // baseline can say "unrunnable" instead of "interrupted" (see
        // `unrunnable`). Only NotFound: a program that exists and cannot
        // be executed is a different problem, not an install away.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            use std::io::Write as _;
            let mut log = std::fs::OpenOptions::new().append(true).open(&log_path)?;
            writeln!(log, "relais: {}: command not found", spec.argv[0])?;
            Ended::Exited(COMMAND_NOT_FOUND)
        }
        Err(e) => return Err(VerifyError::Io(e)),
    };
    let log_bytes = std::fs::read(&log_path)?;
    Ok(CheckOutcome {
        label: label.to_string(),
        argv: spec.argv.clone(),
        ended,
        log_path: log_path.to_string_lossy().into_owned(),
        log_sha256: sha256_hex(&log_bytes),
    })
}

/// The shell's status for "command not found". A check that ends this
/// way did not run: nothing it was meant to test was tested.
pub const COMMAND_NOT_FOUND: i32 = 127;

/// Did this check end without running at all — the program the argv
/// names, or one a script inside it needed, was not found? 127 is what
/// `sh` reports for that, and what `run_command` records when the spawn
/// itself finds nothing; a bare `npm` and one behind `sh -c` say the
/// same thing. At the base this is not a failure to compare candidates
/// against, it is the absence of a verdict (SPEC §10).
pub fn unrunnable(outcome: &CheckOutcome) -> bool {
    outcome.ended == Ended::Exited(COMMAND_NOT_FOUND)
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
            .expect("an argv is a vector of owned strings")
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

/// A setup outcome's label: the command's identity behind a `setup:`
/// prefix, so a setup can never be paired with a check of the same argv
/// in `baseline_failures` — `npm ci` at the base and `npm ci` at the
/// candidate are the same step, but neither is a check.
pub fn setup_label(spec: &CommandSpec) -> String {
    format!("setup:{}", check_label(spec))
}

/// Run a profile's setup in a directory, before its commands: the
/// install step that puts the tree's own dependencies in place. It stops
/// at the first failure — a `pnpm install` that did not finish makes the
/// `playwright install` after it meaningless, and the commands after
/// both would only report that nothing is installed. The outcomes are
/// evidence (their logs are hashed and recorded like a check's), never
/// checks: a setup that succeeded passed nothing (SPEC §10).
pub fn run_setup(
    dir: &Path,
    profile: &VerificationProfile,
    logs_dir: &Path,
    prefix: &str,
) -> Result<Vec<CheckOutcome>, VerifyError> {
    let mut outcomes = Vec::new();
    for (index, command) in profile.setup.iter().enumerate() {
        let label = setup_label(command);
        let log_stem = format!("{prefix}-setup{index}");
        let outcome = run_command(dir, command, logs_dir, &label, &log_stem)?;
        let failed = outcome.failed();
        outcomes.push(outcome);
        if failed {
            break;
        }
    }
    Ok(outcomes)
}

/// The setup command that did not succeed, when one did not. `run_setup`
/// stops there, so it is the last outcome or none.
pub fn setup_failure(outcomes: &[CheckOutcome]) -> Option<&CheckOutcome> {
    outcomes.last().filter(|outcome| outcome.failed())
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
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "pnpm-workspace.yaml",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
    "**/package.json",
    "**/package-lock.json",
    "**/npm-shrinkwrap.json",
    "**/pnpm-lock.yaml",
    "**/pnpm-workspace.yaml",
    "**/yarn.lock",
    "**/bun.lock",
    "**/bun.lockb",
    "pyproject.toml",
    "requirements*.txt",
    "uv.lock",
    "poetry.lock",
    "Pipfile",
    "Pipfile.lock",
    "**/pyproject.toml",
    "**/requirements*.txt",
    "**/uv.lock",
    "**/poetry.lock",
    "**/Pipfile",
    "**/Pipfile.lock",
    "go.mod",
    "go.sum",
    "**/go.mod",
    "**/go.sum",
    "Gemfile",
    "Gemfile.lock",
    "**/Gemfile",
    "**/Gemfile.lock",
    "composer.json",
    "composer.lock",
    "**/composer.json",
    "**/composer.lock",
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
    // A repository-relative setup program (`./scripts/bootstrap.sh`) is
    // verification too: it decides what the commands run against.
    for command in profile.setup.iter().chain(&profile.commands) {
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

/// Why a run has no check inventory.
///
/// Every variant is fail-closed at the gate — [`amont_gaps`] turns any of
/// them into a gap for every required check, never into a pass — but
/// *which* it was is the operator's only clue, so it is carried rather
/// than dropped (`errors.never-swallowed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InventoryError {
    /// Nothing was asked: the amont integration is off for this run, the
    /// verification worktree was not created, or a fixture names none.
    NotAsked,
    /// `amont list` could not be started, timed out, or was cancelled.
    NotRun { detail: String },
    /// It ran and did not exit 0.
    Refused { ended: String },
    /// It exited 0 and what it printed is not an `amont-list-v1`
    /// envelope this relais can read.
    Unreadable { detail: String },
}

impl std::fmt::Display for InventoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAsked => write!(f, "no check inventory was asked for"),
            Self::NotRun { detail } => write!(f, "`amont list --json` did not run: {detail}"),
            Self::Refused { ended } => write!(f, "`amont list --json` {ended}"),
            Self::Unreadable { detail } => {
                write!(
                    f,
                    "`amont list --json` printed no readable inventory: {detail}"
                )
            }
        }
    }
}

impl std::error::Error for InventoryError {}

/// Read an `amont-list-v1` envelope. A shape this relais cannot read is
/// [`InventoryError::Unreadable`] naming what was wrong with it — never
/// an empty inventory, which would read as "nothing is enforced".
pub fn parse_amont_list(stdout: &str) -> Result<AmontInventory, InventoryError> {
    let unreadable = |detail: String| InventoryError::Unreadable { detail };
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).map_err(|e| unreadable(format!("not JSON: {e}")))?;
    match value.get("format").and_then(|f| f.as_str()) {
        Some("amont-list-v1") => {}
        other => {
            return Err(unreadable(format!(
                "envelope format is {}, not `amont-list-v1`",
                other.unwrap_or("absent")
            )))
        }
    }
    let mut checks = Vec::new();
    let listed = value
        .get("checks")
        .and_then(|checks| checks.as_array())
        .ok_or_else(|| unreadable("no `checks` array".to_string()))?;
    for check in listed {
        let id = check
            .get("id")
            .and_then(|id| id.as_str())
            .ok_or_else(|| unreadable(format!("a check has no `id`: {check}")))?
            .to_string();
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
    Ok(AmontInventory {
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
    /// The inventory for `dir`, or why there is none — which
    /// `amont_gaps` turns into a gap for every required check, never
    /// into a pass.
    fn list(&self, dir: &Path, stage: Stage) -> Result<AmontInventory, InventoryError>;
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
    fn list(&self, dir: &Path, stage: Stage) -> Result<AmontInventory, InventoryError> {
        amont_list(dir, stage, self.cancel.as_deref())
    }
}

/// A fixed answer, for tests and for a run whose policy has the
/// integration off. `None` is [`InventoryError::NotAsked`]: nothing was
/// asked, as opposed to something asked that would not answer.
#[derive(Debug, Default)]
pub struct FixedInventory(pub Option<AmontInventory>);

impl HookInventory for FixedInventory {
    fn list(&self, _dir: &Path, _stage: Stage) -> Result<AmontInventory, InventoryError> {
        self.0.clone().ok_or(InventoryError::NotAsked)
    }
}

/// Run `amont list --json` in a directory, bounded and cancellable. A
/// probe that could not run, would not exit 0, or answered something
/// unreadable says which it was: all three fail the gate, and an
/// operator reading the gap list needs to know what to fix.
pub fn amont_list(
    repo_dir: &Path,
    stage: Stage,
    cancel: Option<&AtomicBool>,
) -> Result<AmontInventory, InventoryError> {
    let mut command = Command::new("amont");
    command.arg("list").arg("--json").current_dir(repo_dir);
    match stage {
        Stage::Pushed => {
            command.arg("--pushed");
        }
        Stage::Local => {}
    }
    let end = crate::procs::run_with_timeout(command, INVENTORY_TIMEOUT, None, cancel, None)
        .map_err(|e| InventoryError::NotRun {
            detail: e.to_string(),
        })?;
    if end.ended != Ended::Exited(0) {
        return Err(InventoryError::Refused {
            ended: end.ended.describe(),
        });
    }
    parse_amont_list(&end.stdout)
}

/// Gaps among the checks the run depends on: required amont checks that
/// are inert, skipped, unavailable or of unknown status are gaps, and a
/// check the inventory does not list at all is a gap too (SPEC §10).
/// No inventory at all is a gap for every required check, naming why
/// there is none.
pub fn amont_gaps(
    inventory: Result<&AmontInventory, &InventoryError>,
    required_ids: &[String],
) -> Vec<String> {
    let inventory = match inventory {
        Ok(inventory) => inventory,
        Err(e) => {
            return required_ids
                .iter()
                .map(|id| format!("{id}: inventory unavailable ({e})"))
                .collect()
        }
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
pub fn amont_waiver_gaps(
    inventory: Result<&AmontInventory, &InventoryError>,
    waivers: &[String],
) -> Vec<String> {
    // No inventory declares no bypass and no downgrade. The missing
    // inventory is not lost here: `amont_gaps` has already turned it
    // into a gap for every required check, with the reason.
    let Ok(inventory) = inventory else {
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

/// Why amont could not say whether a gate covers this candidate's tree.
/// Distinct from amont answering "no": every variant here means relais
/// could not reach a documented answer at all, and turns into a gap
/// naming the cause it actually observed, never a silent pass and never
/// a crash (SPEC §10, §18).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttestError {
    /// `amont attest covered` could not be started, timed out, or was
    /// cancelled — including amont being absent from this machine.
    NotRun { detail: String },
    /// It ran and did not exit 0: a usage error, such as an amont too
    /// old to have this subcommand. The documented interface's own
    /// "not covered" answer is `Ok(false)`, not this — this is relais
    /// failing to ask, not amont answering.
    Refused { ended: String },
}

impl std::fmt::Display for AttestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRun { detail } => write!(f, "`amont attest covered` did not run: {detail}"),
            Self::Refused { ended } => write!(f, "`amont attest covered` {ended}"),
        }
    }
}

impl std::error::Error for AttestError {}

/// Whether a valid signed attestation covers one gate on one tree, as a
/// port relais owns. The runner asks a `HookAttest`; only `AmontCli`
/// knows the binary's name. The gate and its attestation stay amont's to
/// own (SPEC §10, §18): this asks amont's own documented interface and
/// nothing here reads `refs/notes/amont-attest`, parses an attestation
/// format, or verifies a signature.
pub trait HookAttest {
    /// Whether `gate` is covered by a valid signed attestation on the
    /// tree at `dir`, or why relais could not ask. amont's own interface
    /// is fail-open — it prints nothing and exits 0 on any failure, so
    /// `Ok(false)` cannot tell an absent attestation from one whose
    /// signature failed to verify; only `Err` means relais itself could
    /// not reach an answer.
    fn covered(&self, dir: &Path, gate: &str) -> Result<bool, AttestError>;
}

impl HookAttest for AmontCli {
    fn covered(&self, dir: &Path, gate: &str) -> Result<bool, AttestError> {
        amont_attest_covered(dir, gate, self.cancel.as_deref())
    }
}

/// A fixed set of answers, for tests. A gate this map does not mention
/// answers [`AttestError::NotRun`] — a fixture that means to grant
/// coverage says so by name, rather than a missing entry reading as
/// silently granted.
#[derive(Debug, Default)]
pub struct FixedAttest(pub BTreeMap<String, Result<bool, AttestError>>);

impl HookAttest for FixedAttest {
    fn covered(&self, _dir: &Path, gate: &str) -> Result<bool, AttestError> {
        self.0
            .get(gate)
            .cloned()
            .unwrap_or(Err(AttestError::NotRun {
                detail: format!("no fixed answer for gate `{gate}`"),
            }))
    }
}

/// How long `amont attest covered` may take. Bounded and cancellable
/// like every other probe (audit V6).
pub const ATTEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Run `amont attest covered <gate>` against the tree at `dir` — the
/// candidate's own tree, never the user's checkout, so a signed
/// attestation is asked about the exact commit being verified. This is
/// the only place relais spawns `amont attest`: the gate and its
/// attestation are amont's to own, so relais asks through this one
/// documented interface and reads nothing else — no git notes, no
/// signature verifier (SPEC §10, §18).
pub fn amont_attest_covered(
    dir: &Path,
    gate: &str,
    cancel: Option<&AtomicBool>,
) -> Result<bool, AttestError> {
    // No gate argument: `amont attest covered` PRINTS the gate names a
    // valid attestation covers and ignores anything else on its command
    // line — passing the gate and reading "did it print something" says
    // only that SOME gate is covered, so a criterion naming an uncovered
    // gate would read as covered whenever any other one was. Measured:
    // `amont attest covered zzz-not-a-gate` prints the full list and
    // exits 0. The name is matched against the list below instead.
    let mut command = Command::new("amont");
    command.arg("attest").arg("covered").current_dir(dir);
    let end = crate::procs::run_with_timeout(command, ATTEST_TIMEOUT, None, cancel, None).map_err(
        |e| AttestError::NotRun {
            detail: e.to_string(),
        },
    )?;
    if end.ended != Ended::Exited(0) {
        return Err(AttestError::Refused {
            ended: end.ended.describe(),
        });
    }
    Ok(covers_gate(&end.stdout, gate))
}

/// Whether `amont attest covered`'s output names this gate.
///
/// Pure, and tested, because it is the whole of what relais concludes
/// from amont: the command prints the covered gate names separated by
/// whitespace, and everything else about the attestation — that it
/// exists, that it verifies, that it is for this tree — amont already
/// decided before printing.
///
/// Matching the WHOLE name matters. `amont attest covered <gate>`
/// ignores its extra argument and prints the full list either way
/// (measured: `amont attest covered zzz-not-a-gate` prints all five
/// gates and exits 0), so asking "did it print anything" would report a
/// criterion's gate as covered whenever some OTHER gate was. A prefix
/// match would do the same for `pre-push-audit` against
/// `pre-push-audit-rust`.
///
/// Empty output means covered by nothing: amont prints nothing and exits
/// 0 on any failure, so an absent name is uncovered whatever the reason,
/// which is the safe direction for a gap.
fn covers_gate(stdout: &str, gate: &str) -> bool {
    stdout.split_whitespace().any(|covered| covered == gate)
}

/// The distinct amont gates any declared acceptance criterion names as
/// its evidence — mandatory or not, so every declared criterion can be
/// settled, not only the ones that can block acceptance.
pub fn amont_gate_names(entries: &[AcceptanceEntry]) -> Vec<String> {
    let mut gates: Vec<String> = entries
        .iter()
        .filter_map(|entry| {
            entry.evidence().and_then(|evidence| match evidence {
                Evidence::AmontGate { gate } => Some(gate.clone()),
                Evidence::Check { .. }
                | Evidence::Test { .. }
                | Evidence::LlmReview
                | Evidence::HumanSignOff => None,
            })
        })
        .collect();
    gates.sort();
    gates.dedup();
    gates
}

/// Ask amont, once per distinct gate, whether a valid signed attestation
/// covers it on the tree at `dir`.
pub fn amont_gate_coverage(
    gates: &[String],
    hooks: &dyn HookAttest,
    dir: &Path,
) -> BTreeMap<String, Result<bool, AttestError>> {
    gates
        .iter()
        .map(|gate| (gate.clone(), hooks.covered(dir, gate)))
        .collect()
}

/// What one candidate's verification established: the checks that ran,
/// the required checks that are gaps, and what amont's inventory declares
/// about itself.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Verified {
    pub checks: Vec<CheckOutcome>,
    pub gaps: Vec<String>,
    /// The acceptance gaps among `gaps`, kept typed. `gaps` holds every
    /// gap as the sentence that goes into the report; a caller deciding
    /// what to DO about one reads this instead of the prose.
    pub acceptance_gaps: Vec<AcceptanceGap>,
    pub amont_bypasses: Vec<String>,
    pub amont_downgrades: Vec<String>,
    /// What amont answered about every gate a declared criterion named,
    /// keyed by gate name — settlement reads this, never `accepted`
    /// (SPEC §10, §18).
    pub gate_coverage: BTreeMap<String, Result<bool, AttestError>>,
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

/// Resolve every program the profile's setup and commands run. The probe
/// is a parameter, so this stays a pure function over what the machine
/// answered. A program that cannot be versioned refuses the cache for
/// the whole profile: a key that cannot name the toolchain cannot say
/// two runs shared one. The setup's programs count as much as the
/// commands': hashing the profile captures `["uv", "sync"]`, not which
/// uv installed the tree the checks then ran against.
pub fn resolve_toolchain(
    profile: &VerificationProfile,
    probe: &dyn Fn(&str) -> Result<ProgramVersion, VersionUnknown>,
) -> Result<Toolchain, CacheRefused> {
    let mut programs: Vec<ProgramVersion> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for command in profile.setup.iter().chain(&profile.commands) {
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
            // What a cached entry MEANS, not just what it was computed
            // from. Before this version a base check killed by the wall
            // clock was cached as an ordinary failure label, so an
            // opted-in repo would keep reading that entry and keep
            // ending `baseline_failure_not_waived` however many times
            // the base was fixed — the stored answer outliving the
            // judgement that produced it. Bumping this retires every
            // entry written under the old meaning, at the cost of one
            // baseline run each.
            "meaning": BASELINE_MEANING_VERSION,
            "base": base_sha,
            "profile": profile,
            "tools": tools,
            "toolchain": toolchain,
        })
        .to_string()
        .as_bytes(),
    )
}

/// Bumped when what a cached baseline entry means changes, rather than
/// what it was computed from. v2: a check that did not run to a verdict
/// is `BaselineUnrunnable` and is never cached as a failure.
const BASELINE_MEANING_VERSION: u32 = 2;

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

    /// A cached baseline, or `None`. This is a cache: a miss and an
    /// unreadable entry have the same consequence — the baseline is
    /// re-run — so neither is an error to report.
    pub fn get(&self, key: &str) -> Option<Vec<String>> {
        let text = std::fs::read_to_string(self.dir.join(format!("{key}.json"))).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn put(&self, key: &str, failures: &[String]) {
        if std::fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        // Best effort: an entry that cannot be written is a cache miss
        // next time, which costs one baseline run and nothing else.
        let _ = std::fs::write(
            self.dir.join(format!("{key}.json")),
            serde_json::to_string(failures).expect("a list of owned strings serializes"),
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
    /// Per-criterion settlement: whether each acceptance criterion was
    /// met, and by which evidence. Defaults to empty so a receipt
    /// written before this existed still parses (SPEC §12).
    #[serde(default)]
    pub criteria: Vec<CriterionOutcome>,
    /// How independent the mandatory criteria's evidence was, taken
    /// together; `None` when there was nothing mandatory to summarize.
    #[serde(default)]
    pub mandatory_evidence_independence: Option<IndependenceSummary>,
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
            name: None,
            argv: argv.iter().map(|a| a.to_string()).collect(),
            timeout_seconds,
        }
    }

    #[test]
    fn passing_and_failing_commands_are_recorded_with_log_hashes() {
        let dir = temp_dir("cmd");
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
        let dir = temp_dir("timeout");
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
        let dir = temp_dir("labels");
        let logs = dir.join("logs");
        let profile = VerificationProfile {
            setup: Vec::new(),
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
            setup: vec![command(&["./scripts/bootstrap.sh"], 10)],
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
                "scripts/bootstrap.sh".into(),
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
                "scripts/bootstrap.sh",
                "ci/lint.yml",
            ],
            "build manifests, the profile's declared inputs and the commands' own programs"
        );
        assert_eq!(
            touched.tests,
            vec!["tests/smoke.rs", "src/foo_test.go", "tests/test_parser.py"],
            "the test tree is work SPEC §10 invites, not a policy change"
        );
        assert_eq!(touched.all().len(), 8);
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
            setup: Vec::new(),
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
        let dir = temp_dir("zero");
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
        let dir = temp_dir("bcache");
        let cache = BaselineCache::new(&dir);
        let profile = VerificationProfile {
            setup: Vec::new(),
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
            setup: Vec::new(),
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

    /// A setup step runs first, under its own log stem, with a label no
    /// check can share: `setup:` in front of the command identity.
    #[test]
    fn a_setup_step_runs_before_the_commands_and_is_labelled_apart() {
        let dir = temp_dir("setup");
        let logs = dir.join("logs");
        let profile = VerificationProfile {
            setup: vec![command(&["sh", "-c", "echo installed > marker"], 10)],
            commands: vec![command(&["sh", "-c", "test -f marker"], 10)],
            amont_checks: Vec::new(),
            amont_waivers: Vec::new(),
            inputs: Vec::new(),
            cache_baseline: false,
        };
        let setup = run_setup(&dir, &profile, &logs, "base").expect("setup");
        assert_eq!(setup.len(), 1);
        assert!(!setup[0].failed());
        assert!(
            setup[0].label.starts_with("setup:sh@"),
            "{}",
            setup[0].label
        );
        assert!(
            setup[0].log_path.ends_with("base-setup0.log"),
            "{}",
            setup[0].log_path
        );
        assert_ne!(
            setup[0].label,
            check_label(&profile.setup[0]),
            "a setup label never equals the check label of the same argv"
        );
        assert!(setup_failure(&setup).is_none());
        let checks = run_profile(&dir, &profile, &logs, "base").expect("checks");
        assert!(
            !checks[0].failed(),
            "the command sees what the setup installed"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The first failing setup command ends the setup: the next command
    /// is not attempted, and the failure is the last outcome.
    #[test]
    fn setup_stops_at_its_first_failure() {
        let dir = temp_dir("setup-stop");
        let logs = dir.join("logs");
        let profile = VerificationProfile {
            setup: vec![
                command(&["sh", "-c", "echo boom >&2; exit 1"], 10),
                command(&["sh", "-c", "echo never > second"], 10),
            ],
            commands: Vec::new(),
            amont_checks: Vec::new(),
            amont_waivers: Vec::new(),
            inputs: Vec::new(),
            cache_baseline: false,
        };
        let setup = run_setup(&dir, &profile, &logs, "task").expect("setup ran");
        assert_eq!(setup.len(), 1, "the second command was not attempted");
        let failed = setup_failure(&setup).expect("the failure is reported");
        assert_eq!(failed.ended, Ended::Exited(1));
        assert!(
            std::fs::read_to_string(&failed.log_path)
                .expect("log")
                .contains("boom"),
            "the failing setup's log is evidence"
        );
        assert!(!logs.join("task-setup1.log").exists());
        assert!(!dir.join("second").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A program that is not there is an outcome (exit 127, "command not
    /// found" in the log), not an io error that interrupts the run — and
    /// the same outcome whether relais spawned it or a shell looked for
    /// it, which is what the application-landscape baseline actually
    /// did (`npm run typecheck` → `sh: react-router: command not found`).
    #[test]
    fn a_program_that_is_not_there_is_an_unrunnable_check_not_a_runner_failure() {
        let dir = temp_dir("notfound");
        let logs = dir.join("logs");
        let direct = run_command(
            &dir,
            &command(&["relais-no-such-binary-4f3a"], 10),
            &logs,
            "relais-no-such-binary-4f3a@0",
            "base-cmd0",
        )
        .expect("not a runner failure");
        assert_eq!(direct.ended, Ended::Exited(COMMAND_NOT_FOUND));
        assert!(direct.failed());
        assert!(unrunnable(&direct));
        let log = std::fs::read_to_string(&direct.log_path).expect("log exists");
        assert!(log.contains("command not found"), "{log}");
        assert_eq!(direct.log_sha256, sha256_hex(log.as_bytes()));

        let via_shell = run_command(
            &dir,
            &command(&["sh", "-c", "relais-no-such-binary-4f3a"], 10),
            &logs,
            "sh@0",
            "base-cmd1",
        )
        .expect("the shell ran");
        assert!(unrunnable(&via_shell), "{:?}", via_shell.ended);

        let ordinary = run_command(
            &dir,
            &command(&["sh", "-c", "exit 1"], 10),
            &logs,
            "sh@1",
            "base-cmd2",
        )
        .expect("ran");
        assert!(ordinary.failed());
        assert!(
            !unrunnable(&ordinary),
            "exit 1 is a check that ran and said no"
        );
        // Per check, not through an aggregator: the runner asks
        // `unrunnable` about one outcome at a time, and a helper that
        // only its own test calls is a leftover of the shape before it.
        assert!(unrunnable(&direct), "{}", direct.label);
        assert!(unrunnable(&via_shell), "{}", via_shell.label);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A candidate check the wall clock kills is a failure, full stop —
    /// `unrunnable` keeps meaning "command not found" and says nothing
    /// about it. Reclassifying a timeout as unrunnable at a candidate
    /// would let a hanging test through instead of failing it; that
    /// judgement is made only at the base, in the runner (SPEC §10).
    #[test]
    fn a_candidate_check_the_wall_clock_kills_is_a_failure_not_unrunnable() {
        let dir = temp_dir("candidate-timeout");
        let logs = dir.join("logs");
        let hung = run_command(
            &dir,
            &command(&["sh", "-c", "sleep 5"], 1),
            &logs,
            "sh@0",
            "cand0",
        )
        .expect("run");
        assert_eq!(hung.ended, Ended::TimedOut);
        assert!(hung.failed(), "a hung candidate check is a failure");
        assert!(
            !unrunnable(&hung),
            "a timeout is not the same fact as a missing program"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The setup's programs are part of the toolchain: a new `uv` may
    /// install a different tree for the same lockfile, and the baseline
    /// verdict that ran against the old one must not be reused. Hashing
    /// the profile sees `["uv", "sync"]`, not which uv.
    #[test]
    fn a_setup_program_version_change_rekeys_the_baseline() {
        let profile = VerificationProfile {
            setup: vec![command(&["uv", "sync", "--frozen"], 10)],
            commands: vec![command(&["uv", "run", "pytest"], 10)],
            amont_checks: Vec::new(),
            amont_waivers: Vec::new(),
            inputs: Vec::new(),
            cache_baseline: true,
        };
        let tools = crate::context::ToolVersions {
            relais: "0.2.0".into(),
            aval: None,
            amont: None,
            claude_code: None,
        };
        let probe_with = |version: &'static str| {
            move |program: &str| {
                Ok(ProgramVersion {
                    program: program.to_string(),
                    path: format!("/usr/bin/{program}"),
                    version: format!("{program} {version}"),
                })
            }
        };
        let old = resolve_toolchain(&profile, &probe_with("0.4.0")).expect("resolved");
        let new = resolve_toolchain(&profile, &probe_with("0.5.0")).expect("resolved");
        assert_eq!(
            old.programs.len(),
            1,
            "uv is one program, named by setup and command"
        );
        assert_ne!(
            baseline_key("base", &profile, &tools, &old),
            baseline_key("base", &profile, &tools, &new),
            "the same profile on a new setup program is a different key"
        );

        // A profile whose ONLY use of the program is in the setup is
        // fingerprinted by it just the same.
        let setup_only = VerificationProfile {
            setup: vec![command(&["npm", "ci"], 10)],
            commands: vec![command(&["make", "check"], 10)],
            ..profile.clone()
        };
        let resolved = resolve_toolchain(&setup_only, &probe_with("10.0.0")).expect("resolved");
        let names: Vec<&str> = resolved
            .programs
            .iter()
            .map(|p| p.program.as_str())
            .collect();
        assert_eq!(names, vec!["make", "npm"]);
    }

    /// A setup program that cannot be versioned refuses the cache for
    /// the profile, exactly as a check program does: the key could not
    /// say which installer produced the tree the verdict is about.
    #[test]
    fn a_setup_program_that_cannot_be_versioned_refuses_the_cache() {
        let profile = VerificationProfile {
            setup: vec![command(&["./scripts/bootstrap.sh"], 10)],
            commands: vec![command(&["make", "check"], 10)],
            amont_checks: Vec::new(),
            amont_waivers: Vec::new(),
            inputs: Vec::new(),
            cache_baseline: true,
        };
        let refused = resolve_toolchain(&profile, &|program| {
            if program == "make" {
                Ok(ProgramVersion {
                    program: program.to_string(),
                    path: "/usr/bin/make".into(),
                    version: "make 4.4".into(),
                })
            } else {
                Err(VersionUnknown::NoAnswer {
                    program: program.to_string(),
                    detail: "it printed nothing".into(),
                })
            }
        })
        .expect_err("a setup program with no version refuses the cache");
        assert!(refused.reason.contains("bootstrap.sh"), "{refused}");
    }

    /// Unique fixture directories under parallel test threads.
    fn temp_dir(tag: &str) -> crate::test_support::TempDir {
        crate::test_support::temp_dir(&format!("verify-{tag}"))
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
            Ok(&inventory),
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
        let gaps = amont_waiver_gaps(Ok(&inventory), &[]);
        assert_eq!(gaps.len(), 1, "one downgrade, no bypasses: {gaps:?}");
        assert!(
            gaps[0].starts_with("pre-push-cargo-test: downgraded"),
            "{gaps:?}"
        );
        assert!(
            amont_waiver_gaps(Ok(&inventory), &["pre-push-cargo-test".to_string()]).is_empty(),
            "an explicit policy waiver is the one thing that clears it"
        );
        // A bypass is a bare string in the envelope; its id is the
        // string, not the quoted JSON.
        let bypassed = parse_amont_list(
            r#"{"format":"amont-list-v1","checks":[],"bypasses":["pre-commit-fmt"],"downgrades":[]}"#,
        )
        .expect("parses");
        let gaps = amont_waiver_gaps(Ok(&bypassed), &[]);
        assert!(gaps[0].starts_with("pre-commit-fmt: bypassed"), "{gaps:?}");
        assert!(amont_waiver_gaps(Ok(&bypassed), &["pre-commit-fmt".to_string()]).is_empty());
        assert!(
            amont_waiver_gaps(Err(&InventoryError::NotAsked), &[]).is_empty(),
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
            !amont_gaps(Ok(&inventory), &default_required_checks(&inventory))
                .iter()
                .any(|gap| gap.contains("pre-commit-lint-shell"))
        );
    }

    #[test]
    fn a_verification_worktree_releases_itself_when_its_checks_are_done() {
        let dir = temp_dir("worktree");
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
        assert!(parse_amont_list(r#"{"format": "something-else-v9"}"#).is_err());
        assert!(parse_amont_list("not json").is_err());
    }

    #[test]
    fn missing_inventory_means_every_required_check_is_a_gap() {
        let gaps = amont_gaps(
            Err(&InventoryError::NotRun {
                detail: "no such file or directory".to_string(),
            }),
            &["pre-push-cargo-test".to_string()],
        );
        assert_eq!(
            gaps,
            vec![
                "pre-push-cargo-test: inventory unavailable (`amont list --json` did not run: no \
                 such file or directory)"
            ],
            "and it names WHY there is no inventory, not just that there is none"
        );
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

    fn named(name: &str, argv: &[&str], timeout_seconds: u64) -> CommandSpec {
        CommandSpec {
            name: Some(name.to_string()),
            argv: argv.iter().map(|a| a.to_string()).collect(),
            timeout_seconds,
        }
    }

    /// A mandatory declared criterion — the default a bare string has
    /// always had.
    fn declared(statement: &str, evidence: crate::acceptance::Evidence) -> AcceptanceEntry {
        AcceptanceEntry::Declared(crate::acceptance::DeclaredCriterion {
            statement: statement.to_string(),
            id: None,
            mandatory: true,
            evidence,
        })
    }

    /// A criterion the contract declared optional.
    fn declared_optional(
        statement: &str,
        evidence: crate::acceptance::Evidence,
    ) -> AcceptanceEntry {
        AcceptanceEntry::Declared(crate::acceptance::DeclaredCriterion {
            statement: statement.to_string(),
            id: None,
            mandatory: false,
            evidence,
        })
    }

    /// SPEC §10: a mandatory criterion whose declared evidence never
    /// produced anything to check is a gap in the SAME report the
    /// required-check gaps live in, not a second acceptance path — so a
    /// candidate whose only mandatory criterion is unmet is refused by
    /// the mechanism that already refuses gaps.
    #[test]
    fn a_candidate_whose_only_mandatory_criterion_is_unmet_is_not_accepted() {
        let profile = VerificationProfile {
            setup: Vec::new(),
            commands: vec![named("lint", &["sh", "-c", "true"], 10)],
            amont_checks: Vec::new(),
            amont_waivers: Vec::new(),
            inputs: Vec::new(),
            cache_baseline: false,
        };
        let entries = vec![declared(
            "the api rejects malformed input",
            crate::acceptance::Evidence::Check {
                name: "security-scan".into(),
            },
        )];
        // The profile's own commands all ran and passed; `security-scan`
        // is simply not among them.
        let checks = vec![CheckOutcome {
            label: check_label(&profile.commands[0]),
            argv: profile.commands[0].argv.clone(),
            ended: Ended::Exited(0),
            log_path: "x.log".into(),
            log_sha256: "h".into(),
        }];
        let gaps = acceptance_gaps(
            &entries,
            &profile,
            &checks,
            &HashSet::new(),
            &BTreeMap::new(),
        );
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        assert_eq!(
            gaps[0].missing,
            MissingEvidence::CheckUndefined {
                name: "security-scan".into()
            },
            "{gaps:?}"
        );
        let gaps: Vec<String> = gaps.iter().map(AcceptanceGap::message).collect();

        let report = VerificationReport {
            candidate_sha: "abc".into(),
            base_sha: "def".into(),
            contract_hash: "ch".into(),
            policy_hash: "ph".into(),
            checks,
            gaps,
            baseline_failures: Vec::new(),
            amont_bypasses: Vec::new(),
            amont_downgrades: Vec::new(),
            verification_inputs_changed: Vec::new(),
            integration_gaps: Vec::new(),
            baseline_cached: false,
            baseline_cache_refused: None,
        };
        assert!(
            !report.accepted(),
            "an unmet mandatory criterion must not be accepted"
        );
    }

    /// A criterion settled only by a model-added test or an LLM review
    /// is still accepted once the checks pass; the receipt says the
    /// evidence was not independent rather than inventing a stricter
    /// acceptance rule than the contract asked for (SPEC §10).
    #[test]
    fn settling_reports_met_when_checks_pass_and_independence_honestly() {
        let profile = VerificationProfile {
            setup: Vec::new(),
            commands: vec![named("check", &["sh", "-c", "true"], 10)],
            amont_checks: Vec::new(),
            amont_waivers: Vec::new(),
            inputs: Vec::new(),
            cache_baseline: false,
        };
        let checks = vec![CheckOutcome {
            label: check_label(&profile.commands[0]),
            argv: profile.commands[0].argv.clone(),
            ended: Ended::Exited(0),
            log_path: "x.log".into(),
            log_sha256: "h".into(),
        }];
        let report = VerificationReport {
            candidate_sha: "abc".into(),
            base_sha: "def".into(),
            contract_hash: "ch".into(),
            policy_hash: "ph".into(),
            checks,
            gaps: Vec::new(),
            baseline_failures: Vec::new(),
            amont_bypasses: Vec::new(),
            amont_downgrades: Vec::new(),
            verification_inputs_changed: Vec::new(),
            integration_gaps: Vec::new(),
            baseline_cached: false,
            baseline_cache_refused: None,
        };
        assert!(report.accepted());

        let entries = vec![
            declared(
                "the new endpoint is exercised",
                crate::acceptance::Evidence::Test {
                    authorship: crate::acceptance::TestAuthorship::ModelAdded,
                },
            ),
            declared(
                "the change is coherent",
                crate::acceptance::Evidence::LlmReview,
            ),
        ];
        let (criteria, summary) = settle_acceptance(
            &entries,
            &profile,
            &report,
            &HashSet::new(),
            &BTreeMap::new(),
        );
        assert!(
            criteria.iter().all(|c| c.met),
            "checks passed, so every criterion is met: {criteria:?}"
        );
        assert_eq!(
            summary,
            Some(IndependenceSummary::NoneIndependent),
            "neither a model-added test nor an LLM review is independent"
        );
    }

    /// `AcceptanceGap::message` is the only writer of a gap sentence and
    /// `sign_off_gap_criterion` the only reader of one, and `relais
    /// decide` clears a gap recorded months ago by pairing them. A
    /// reworded message that silently stopped parsing would leave an
    /// answered criterion blocking its run forever, so the pair is
    /// round-tripped over every variant — including the two that must
    /// NOT parse as sign-offs.
    #[test]
    fn a_sign_off_gap_sentence_round_trips_and_the_others_do_not() {
        let signed = AcceptanceGap {
            criterion_id: "c-0123456789ab".into(),
            missing: MissingEvidence::SignOffUnrecorded,
        };
        assert_eq!(
            sign_off_gap_criterion(&signed.message()),
            Some("c-0123456789ab")
        );

        for other in [
            MissingEvidence::CheckProducedNothing {
                name: "security-scan".into(),
            },
            MissingEvidence::CheckUndefined {
                name: "security-scan".into(),
            },
        ] {
            let gap = AcceptanceGap {
                criterion_id: "c-0123456789ab".into(),
                missing: other,
            };
            assert_eq!(
                sign_off_gap_criterion(&gap.message()),
                None,
                "a check gap is not a sign-off gap: {}",
                gap.message()
            );
        }

        // A gap from somewhere else entirely — amont's inventory — is
        // not a criterion at all.
        assert_eq!(sign_off_gap_criterion("amont inventory: unavailable"), None);
    }

    /// The failure this test exists for: a mandatory criterion asking
    /// for a human sign-off used to read its answer off `accepted`, so
    /// passing checks reported a sign-off nobody gave — and
    /// `independent()` then counted that invention toward
    /// `AllIndependent`. With no sign-off recorded for this id, the
    /// criterion is unmet and the report carries a gap.
    #[test]
    fn a_human_sign_off_nobody_gave_is_a_gap_not_a_pass() {
        let profile = VerificationProfile {
            setup: Vec::new(),
            commands: vec![named("check", &["sh", "-c", "true"], 10)],
            amont_checks: Vec::new(),
            amont_waivers: Vec::new(),
            inputs: Vec::new(),
            cache_baseline: false,
        };
        let checks = vec![CheckOutcome {
            label: check_label(&profile.commands[0]),
            argv: profile.commands[0].argv.clone(),
            ended: Ended::Exited(0),
            log_path: "x.log".into(),
            log_sha256: "h".into(),
        }];
        let entries = vec![declared(
            "a person signed off on the migration",
            crate::acceptance::Evidence::HumanSignOff,
        )];

        let gaps = acceptance_gaps(
            &entries,
            &profile,
            &checks,
            &HashSet::new(),
            &BTreeMap::new(),
        );
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        assert_eq!(
            gaps[0].missing,
            MissingEvidence::SignOffUnrecorded,
            "{gaps:?}"
        );
        assert_eq!(gaps[0].criterion_id, entries[0].id());
        let gaps: Vec<String> = gaps.iter().map(AcceptanceGap::message).collect();

        let report = VerificationReport {
            candidate_sha: "abc".into(),
            base_sha: "def".into(),
            contract_hash: "ch".into(),
            policy_hash: "ph".into(),
            checks,
            gaps,
            baseline_failures: Vec::new(),
            amont_bypasses: Vec::new(),
            amont_downgrades: Vec::new(),
            verification_inputs_changed: Vec::new(),
            integration_gaps: Vec::new(),
            baseline_cached: false,
            baseline_cache_refused: None,
        };
        assert!(
            !report.accepted(),
            "every check passed, but the sign-off is still missing"
        );

        let (criteria, summary) = settle_acceptance(
            &entries,
            &profile,
            &report,
            &HashSet::new(),
            &BTreeMap::new(),
        );
        assert!(
            !criteria[0].met,
            "an unrecorded sign-off is not met: {criteria:?}"
        );
        assert_eq!(summary, Some(IndependenceSummary::AllIndependent));
    }

    /// The other half of the test above: once `relais decide --answer
    /// approve --criterion <id>` has recorded a sign-off for this exact
    /// criterion id, it is no longer a gap and settles as met — the
    /// person's answer is the ONLY thing that clears it (SPEC §10).
    #[test]
    fn a_recorded_sign_off_clears_the_gap_and_settles_met() {
        let profile = VerificationProfile {
            setup: Vec::new(),
            commands: vec![named("check", &["sh", "-c", "true"], 10)],
            amont_checks: Vec::new(),
            amont_waivers: Vec::new(),
            inputs: Vec::new(),
            cache_baseline: false,
        };
        let checks = vec![CheckOutcome {
            label: check_label(&profile.commands[0]),
            argv: profile.commands[0].argv.clone(),
            ended: Ended::Exited(0),
            log_path: "x.log".into(),
            log_sha256: "h".into(),
        }];
        let entries = vec![declared(
            "a person signed off on the migration",
            crate::acceptance::Evidence::HumanSignOff,
        )];
        let signed_off: HashSet<String> = [entries[0].id()].into_iter().collect();

        let gaps = acceptance_gaps(&entries, &profile, &checks, &signed_off, &BTreeMap::new());
        assert!(gaps.is_empty(), "{gaps:?}");
        let gaps: Vec<String> = gaps.iter().map(AcceptanceGap::message).collect();

        let report = VerificationReport {
            candidate_sha: "abc".into(),
            base_sha: "def".into(),
            contract_hash: "ch".into(),
            policy_hash: "ph".into(),
            checks,
            gaps,
            baseline_failures: Vec::new(),
            amont_bypasses: Vec::new(),
            amont_downgrades: Vec::new(),
            verification_inputs_changed: Vec::new(),
            integration_gaps: Vec::new(),
            baseline_cached: false,
            baseline_cache_refused: None,
        };
        assert!(report.accepted());

        let (criteria, summary) =
            settle_acceptance(&entries, &profile, &report, &signed_off, &BTreeMap::new());
        assert!(criteria[0].met, "a recorded sign-off is met: {criteria:?}");
        assert_eq!(
            summary,
            Some(IndependenceSummary::AllIndependent),
            "a human sign-off is independent by definition"
        );
    }

    /// A mandatory criterion can name an amont gate as its evidence, and
    /// The failure this test exists for: the first candidate passed the
    /// gate to `amont attest covered` and read "did it print anything"
    /// as the answer. That command ignores the argument and prints every
    /// covered gate, so a criterion naming an UNCOVERED gate read as
    /// covered whenever any other gate was — a silent pass, through the
    /// one evidence kind relais cannot observe for itself. Every test
    /// passed anyway, because they all stand in for amont at the
    /// `HookAttest` port and never reach the parsing.
    #[test]
    fn a_gate_is_covered_only_when_the_list_names_that_gate() {
        let printed = "pre-push-branch-protect pre-push-secrets pre-push-audit-rust\n";
        assert!(covers_gate(printed, "pre-push-secrets"));
        assert!(covers_gate(printed, "pre-push-audit-rust"));
        assert!(
            !covers_gate(printed, "pre-push-cargo-test"),
            "a gate the list does not name is not covered by the ones it does"
        );
        assert!(
            !covers_gate(printed, "pre-push-audit"),
            "a prefix of a covered gate is not that gate"
        );
        assert!(
            !covers_gate(printed, "audit-rust"),
            "a suffix of a covered gate is not that gate either"
        );
        // Nothing printed is amont's fail-open answer: covered by nothing.
        assert!(!covers_gate("", "pre-push-secrets"));
        assert!(!covers_gate("   \n  ", "pre-push-secrets"));
    }

    /// `amont_gate_coverage` asks the `HookAttest` port once per distinct
    /// gate, on the candidate's own tree. Covered settles the criterion
    /// met and independent — the gate ran outside this run and its
    /// attestation is signed (SPEC §10, §18).
    #[test]
    fn a_covered_amont_gate_settles_met_and_independent() {
        let profile = VerificationProfile {
            setup: Vec::new(),
            commands: vec![named("check", &["sh", "-c", "true"], 10)],
            amont_checks: Vec::new(),
            amont_waivers: Vec::new(),
            inputs: Vec::new(),
            cache_baseline: false,
        };
        let checks = vec![CheckOutcome {
            label: check_label(&profile.commands[0]),
            argv: profile.commands[0].argv.clone(),
            ended: Ended::Exited(0),
            log_path: "x.log".into(),
            log_sha256: "h".into(),
        }];
        let entries = vec![declared(
            "the pre-push cargo test gate covers this candidate",
            crate::acceptance::Evidence::AmontGate {
                gate: "pre-push-cargo-test".into(),
            },
        )];
        assert_eq!(
            amont_gate_names(&entries),
            vec!["pre-push-cargo-test".to_string()]
        );
        let hooks = FixedAttest(
            [("pre-push-cargo-test".to_string(), Ok(true))]
                .into_iter()
                .collect(),
        );
        let coverage = amont_gate_coverage(
            &amont_gate_names(&entries),
            &hooks,
            Path::new("/does/not/matter"),
        );

        let gaps = acceptance_gaps(&entries, &profile, &checks, &HashSet::new(), &coverage);
        assert!(gaps.is_empty(), "{gaps:?}");

        let report = VerificationReport {
            candidate_sha: "abc".into(),
            base_sha: "def".into(),
            contract_hash: "ch".into(),
            policy_hash: "ph".into(),
            checks,
            gaps: Vec::new(),
            baseline_failures: Vec::new(),
            amont_bypasses: Vec::new(),
            amont_downgrades: Vec::new(),
            verification_inputs_changed: Vec::new(),
            integration_gaps: Vec::new(),
            baseline_cached: false,
            baseline_cache_refused: None,
        };
        let (criteria, summary) =
            settle_acceptance(&entries, &profile, &report, &HashSet::new(), &coverage);
        assert!(criteria[0].met, "a covered gate is met: {criteria:?}");
        assert_eq!(
            summary,
            Some(IndependenceSummary::AllIndependent),
            "an amont gate is independent — it ran outside this run"
        );
    }

    /// amont's own interface is fail-open by design: it answers "not
    /// covered" for an absent attestation and for one whose signature
    /// failed to verify alike. relais must not turn that into a pass —
    /// it is a gap, through the same mechanism every other gap goes
    /// through — and the gap's message says it cannot tell which of the
    /// two amont meant, naming `amont attest covered` as the command a
    /// person can run to see the same answer (SPEC §10, §18).
    #[test]
    fn an_uncovered_amont_gate_is_a_gap_that_admits_the_ambiguity() {
        let profile = VerificationProfile {
            setup: Vec::new(),
            commands: vec![named("check", &["sh", "-c", "true"], 10)],
            amont_checks: Vec::new(),
            amont_waivers: Vec::new(),
            inputs: Vec::new(),
            cache_baseline: false,
        };
        let checks = vec![CheckOutcome {
            label: check_label(&profile.commands[0]),
            argv: profile.commands[0].argv.clone(),
            ended: Ended::Exited(0),
            log_path: "x.log".into(),
            log_sha256: "h".into(),
        }];
        let entries = vec![declared(
            "the pre-push cargo test gate covers this candidate",
            crate::acceptance::Evidence::AmontGate {
                gate: "pre-push-cargo-test".into(),
            },
        )];
        let coverage: BTreeMap<String, Result<bool, AttestError>> =
            [("pre-push-cargo-test".to_string(), Ok(false))]
                .into_iter()
                .collect();

        let gaps = acceptance_gaps(&entries, &profile, &checks, &HashSet::new(), &coverage);
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        assert_eq!(
            gaps[0].missing,
            MissingEvidence::GateNotCovered {
                gate: "pre-push-cargo-test".into()
            }
        );
        let message = gaps[0].message();
        assert!(message.contains("amont attest covered"), "{message}");
        assert!(message.contains("cannot tell"), "{message}");

        let report = VerificationReport {
            candidate_sha: "abc".into(),
            base_sha: "def".into(),
            contract_hash: "ch".into(),
            policy_hash: "ph".into(),
            checks,
            gaps: vec![message],
            baseline_failures: Vec::new(),
            amont_bypasses: Vec::new(),
            amont_downgrades: Vec::new(),
            verification_inputs_changed: Vec::new(),
            integration_gaps: Vec::new(),
            baseline_cached: false,
            baseline_cache_refused: None,
        };
        assert!(!report.accepted());
        let (criteria, _) =
            settle_acceptance(&entries, &profile, &report, &HashSet::new(), &coverage);
        assert!(
            !criteria[0].met,
            "an uncovered gate is not met: {criteria:?}"
        );
    }

    /// amont being absent, too old to have the subcommand, or refusing
    /// for any other reason is a gap naming the cause relais actually
    /// observed — never a silent pass and never confused with the
    /// documented "not covered" answer (SPEC §10, §18).
    #[test]
    fn an_amont_that_cannot_be_asked_is_a_gap_naming_the_cause() {
        let profile = VerificationProfile::default();
        let entries = vec![declared(
            "the pre-push cargo test gate covers this candidate",
            crate::acceptance::Evidence::AmontGate {
                gate: "pre-push-cargo-test".into(),
            },
        )];
        let coverage: BTreeMap<String, Result<bool, AttestError>> = [(
            "pre-push-cargo-test".to_string(),
            Err(AttestError::NotRun {
                detail: "amont: command not found".into(),
            }),
        )]
        .into_iter()
        .collect();

        let gaps = acceptance_gaps(&entries, &profile, &[], &HashSet::new(), &coverage);
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        let message = gaps[0].message();
        assert!(
            message.contains("amont: command not found"),
            "the cause relais actually observed is named, not guessed: {message}"
        );
    }

    #[test]
    fn an_all_independent_and_a_mixed_mandatory_set_summarize_honestly() {
        let profile = VerificationProfile::default();
        let accepted = VerificationReport {
            candidate_sha: "abc".into(),
            base_sha: "def".into(),
            contract_hash: "ch".into(),
            policy_hash: "ph".into(),
            checks: Vec::new(),
            gaps: Vec::new(),
            baseline_failures: Vec::new(),
            amont_bypasses: Vec::new(),
            amont_downgrades: Vec::new(),
            verification_inputs_changed: Vec::new(),
            integration_gaps: Vec::new(),
            baseline_cached: false,
            baseline_cache_refused: None,
        };
        let (_, all_independent) = settle_acceptance(
            &[AcceptanceEntry::Bare("it builds".into())],
            &profile,
            &accepted,
            &HashSet::new(),
            &BTreeMap::new(),
        );
        assert_eq!(all_independent, Some(IndependenceSummary::AllIndependent));

        let mixed = vec![
            AcceptanceEntry::Bare("it builds".into()),
            declared(
                "reviewed for coherence",
                crate::acceptance::Evidence::LlmReview,
            ),
        ];
        let (_, partly) = settle_acceptance(
            &mixed,
            &profile,
            &accepted,
            &HashSet::new(),
            &BTreeMap::new(),
        );
        assert_eq!(partly, Some(IndependenceSummary::PartlyIndependent));

        // A non-mandatory criterion does not enter the summary at all.
        let only_optional = vec![declared_optional(
            "nice to have",
            crate::acceptance::Evidence::LlmReview,
        )];
        let (_, none_mandatory) = settle_acceptance(
            &only_optional,
            &profile,
            &accepted,
            &HashSet::new(),
            &BTreeMap::new(),
        );
        assert_eq!(none_mandatory, None);
    }

    /// A receipt written before per-criterion settlement existed has
    /// neither field; both default, so it still parses (SPEC §12).
    #[test]
    fn a_pre_existing_receipt_without_criteria_still_parses() {
        let json = r#"{
            "run_id": "run-1",
            "candidate_sha": "abc",
            "base_sha": "def",
            "contract_hash": "ch",
            "policy_hash": "ph",
            "outcome": "accepted",
            "verification": {
                "candidate_sha": "abc",
                "base_sha": "def",
                "contract_hash": "ch",
                "policy_hash": "ph",
                "checks": [],
                "gaps": [],
                "baseline_failures": [],
                "amont_bypasses": [],
                "amont_downgrades": []
            },
            "models_used": ["sonnet"],
            "attempts": 1,
            "cost_completeness": "actual",
            "cost": 12345
        }"#;
        let receipt: Receipt = serde_json::from_str(json).expect("a pre-existing receipt parses");
        assert!(receipt.criteria.is_empty());
        assert_eq!(receipt.mandatory_evidence_independence, None);
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
            criteria: Vec::new(),
            mandatory_evidence_independence: None,
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
