//! The exit/outcome model (SPEC §3, §17, §23): how a command ended and
//! the one table that turns that into a process exit code.
//!
//! # Exit codes
//!
//! One table, one function ([`exit_code`]), matched exhaustively over
//! [`CliOutcome`]. Codes that used to be shared are split wherever a
//! caller has to act differently — a script that retries on "the
//! contract was invalid" must not also retry on "a human has to decide",
//! and `1` means relais's own machinery failed, not that the task did.
//!
//! | code | outcome | meaning |
//! |---|---|---|
//! | 0 | `Accepted` | the command did what was asked |
//! | 1 | `OperationalFailure` | relais's own machinery failed: the ledger, the registry, a file it owns. Nothing is implied about the task |
//! | 2 | `InvalidInput` | the invocation or a file it names is wrong: a malformed contract, a missing policy, conflicting flags |
//! | 3 | `Blocked` | policy, trust, admission or the environment refuses; no model ran |
//! | 4 | `Failed` | the task executed and produced no acceptable candidate |
//! | 5 | `BudgetExhausted` | the spending ceiling was reached; the patch and evidence are preserved |
//! | 6 | `Interrupted` | the run stopped mid-flight; `relais resume` reconciles it |
//! | 7 | `Cancelled` | cancelled by request |
//! | 8 | `NeedsDecision` | a human must decide before this can continue |
//! | 9 | `NeedsReview` | a human must review the candidate |
//! | 10 | `UnknownRun` | the run or artifact named is not on record |
//! | 11 | `ResumeRefusedLive` | resume refused: a worker may still be running |
//! | 12 | `ResumeReconciled` | resume marked the run interrupted; nothing was replayed |
//! | 13 | `NotFullyApplied` | `--write` could not carry out part of its plan; the files it could not touch are named |
//! | 14 | `NoTrainingRecords` | nothing to train on yet |
//! | 15 | `NoTierCoverage` | not enough records for one tier |
//! | 16 | `SolverDiverged` | the learner did not converge |
//! | 17 | `PromotionRefused` | `recipe promote` refused: the comparison does not clear every gate; nothing was written |
//! | 18 | `StaleGrantKey` | `trust grant` refused: the key given is not this policy's; `relais trust show` prints the current one |
//! | 19 | `NothingDetected` | `init --detect` found no verification command and none was typed; nothing was written |
//! | 20 | `TrainingLeaseHeld` | `learn cycle`: another cycle holds the Training lease; the message names its pid and start time |
//!
//! `learn cycle` also ends 0 where it did nothing wrong and nothing was
//! asked of the person: `deferred` (agents of other sessions are active and
//! `training_when_idle` is set), `not due` (`--if-due`), and
//! `awaiting_promotion` (an artifact passed its gates and waits for `relais
//! promote`).
//!
//! The session-router commands (`native router-state`, `router-observe`,
//! `router-envelope`) use 0, 1 and 2 only: 2 is a bad payload or
//! invocation (nothing written), 1 a failure to retry.
//!
//! README.md carries the same table for people who do not read source.

use std::path::PathBuf;

use relais::paths;

/// How a command ended, independent of how it is reported. Every value
/// the process can exit with is a variant here; nothing else constructs
/// an exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CliOutcome {
    /// The command did what was asked.
    Accepted,
    /// Relais's own machinery failed — the ledger, the registry, a file
    /// it owns. Says nothing about the task.
    OperationalFailure,
    /// The invocation, or a file it named, is not something relais can
    /// act on.
    InvalidInput,
    /// Policy, trust, admission or the environment refuses. No model ran.
    Blocked,
    /// The task executed and produced no acceptable candidate.
    Failed,
    /// The spending ceiling was reached.
    BudgetExhausted,
    /// The run stopped mid-flight and can be reconciled.
    Interrupted,
    /// Cancelled by request.
    Cancelled,
    /// A human has to decide before this can continue.
    NeedsDecision,
    /// A human has to review the candidate.
    NeedsReview,
    /// The run or artifact named is not on record.
    UnknownRun,
    /// `resume` refused because a worker may still be running.
    ResumeRefusedLive,
    /// `resume` marked the run interrupted; nothing was replayed.
    ResumeReconciled,
    /// `--write` could not carry out part of its plan.
    NotFullyApplied,
    /// There is nothing to train on yet.
    NoTrainingRecords,
    /// One tier has too few records to train on.
    NoTierCoverage,
    /// The learner did not converge.
    SolverDiverged,
    /// `recipe promote`: the recomputed comparison failed a gate.
    PromotionRefused,
    /// `trust grant`: the key given is not the current policy's.
    StaleGrantKey,
    /// `init --detect` found no verification command and none was typed.
    NothingDetected,
    /// `learn cycle`: another cycle holds the Training lease.
    TrainingLeaseHeld,
    /// relais's own storage is full and the run could not be recorded;
    /// `relais resume <run>` reconciles from the last durable ledger state.
    StorageFull,
}

/// The one exit-code table. Pure, total, and exhaustive over
/// [`CliOutcome`]: adding an outcome without giving it a code does not
/// compile. The module header documents what each code means, and
/// README.md repeats it.
pub(crate) const fn exit_code(outcome: &CliOutcome) -> i32 {
    match outcome {
        CliOutcome::Accepted => 0,
        CliOutcome::OperationalFailure => 1,
        CliOutcome::InvalidInput => 2,
        CliOutcome::Blocked => 3,
        CliOutcome::Failed => 4,
        CliOutcome::BudgetExhausted => 5,
        CliOutcome::Interrupted => 6,
        CliOutcome::Cancelled => 7,
        CliOutcome::NeedsDecision => 8,
        CliOutcome::NeedsReview => 9,
        CliOutcome::UnknownRun => 10,
        CliOutcome::ResumeRefusedLive => 11,
        CliOutcome::ResumeReconciled => 12,
        CliOutcome::NotFullyApplied => 13,
        CliOutcome::NoTrainingRecords => 14,
        CliOutcome::NoTierCoverage => 15,
        CliOutcome::SolverDiverged => 16,
        CliOutcome::PromotionRefused => 17,
        CliOutcome::StaleGrantKey => 18,
        CliOutcome::NothingDetected => 19,
        CliOutcome::TrainingLeaseHeld => 20,
        CliOutcome::StorageFull => 21,
    }
}

/// Why a handler could not get as far as an outcome. Typed, with the
/// operation and the entity in every variant, so `main` can both print
/// one line and pick one exit code — the helpers used to return
/// `Result<_, i32>` and their codes were then thrown away for a
/// hardcoded `2` at the call site (C10).
#[derive(Debug)]
pub(crate) enum CliError {
    /// Neither `HOME` nor `USERPROFILE`, so no settings and no state
    /// directory.
    Home(paths::HomeUnset),
    /// The working directory could not be read — it was deleted under
    /// the process, which is what a worktree teardown does (C14).
    Cwd(std::io::Error),
    /// The cwd is not somewhere a repository-level command can act.
    Locate(relais::repo::LocateError),
    /// A file the invocation named could not be read.
    Read {
        what: &'static str,
        path: PathBuf,
        cause: std::io::Error,
    },
    /// A file was read and is not valid.
    Invalid {
        what: &'static str,
        path: PathBuf,
        cause: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Machine-local state relais owns failed: the ledger, the registry,
    /// an artifact directory.
    Operational {
        operation: &'static str,
        cause: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The invocation itself does not name a thing to do.
    Usage { detail: String },
    /// No dataset has been built yet.
    NoDataset { dir: PathBuf },
    /// A recipe amendment could not be built.
    Amend(relais::learn::promote::AmendError),
    /// A built amendment could not be appended to the repository policy:
    /// opening relais.toml for append, or writing to it, failed.
    AppendPolicy {
        operation: relais::learn::promote::Operation,
        path: PathBuf,
        cause: std::io::Error,
    },
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::Home(e) => write!(f, "{e}"),
            CliError::Cwd(cause) => write!(
                f,
                "the working directory cannot be read ({cause}); it was removed under this \
                 process — run the command from a directory that exists"
            ),
            CliError::Locate(e) => write!(f, "{e}"),
            CliError::Read { what, path, cause } => {
                write!(f, "cannot read {what} {}: {cause}", path.display())
            }
            CliError::Invalid { what, path, cause } => {
                write!(f, "{what} {} is invalid: {cause}", path.display())
            }
            CliError::Operational { operation, cause } => write!(f, "{operation}: {cause}"),
            CliError::Usage { detail } => f.write_str(detail),
            CliError::Amend(e) => write!(f, "{e}"),
            CliError::AppendPolicy {
                operation,
                path,
                cause,
            } => write!(
                f,
                "{operation}: cannot append to {}: {cause}",
                path.display()
            ),
            CliError::NoDataset { dir } => write!(
                f,
                "no dataset under {} — run `relais dataset build` first",
                dir.display()
            ),
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CliError::Home(e) => Some(e),
            CliError::Cwd(e) => Some(e),
            CliError::Locate(e) => Some(e),
            CliError::Read { cause, .. } | CliError::AppendPolicy { cause, .. } => Some(cause),
            CliError::Invalid { cause, .. } | CliError::Operational { cause, .. } => {
                Some(cause.as_ref())
            }
            CliError::Amend(e) => Some(e),
            CliError::Usage { .. } | CliError::NoDataset { .. } => None,
        }
    }
}

impl CliError {
    /// The exit code this failure ends the process with. Exhaustive: a
    /// new variant has to say which half of the table it belongs to.
    pub(crate) fn outcome(&self) -> CliOutcome {
        match self {
            // Nowhere to keep settings or state is an environment
            // problem, not a malformed invocation.
            CliError::Home(_) | CliError::Cwd(_) => CliOutcome::Blocked,
            CliError::Locate(_)
            | CliError::Read { .. }
            | CliError::Invalid { .. }
            | CliError::Usage { .. }
            | CliError::Amend(_) => CliOutcome::InvalidInput,
            // "No dataset has been built yet" IS "nothing to train on
            // yet", which the table already has a code for; exit 2 told
            // a caller its invocation was wrong when it was not.
            CliError::NoDataset { .. } => CliOutcome::NoTrainingRecords,
            CliError::Operational { .. } | CliError::AppendPolicy { .. } => {
                CliOutcome::OperationalFailure
            }
        }
    }
}

/// A ledger, registry or filesystem operation the CLI cannot continue
/// without. The ledger is external state — locked by another relais,
/// truncated by a crash, or written by a newer version — so a failed read
/// is one `relais: …` line and a non-zero exit, never a panic.
pub(crate) fn operational<T, E>(
    result: Result<T, E>,
    operation: &'static str,
) -> Result<T, CliError>
where
    E: std::error::Error + Send + Sync + 'static,
{
    result.map_err(|cause| CliError::Operational {
        operation,
        cause: Box::new(cause),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every outcome the table has. Listed here rather than derived,
    /// because the point of the test below is that two of them never
    /// share a code; `exit_code` itself is exhaustive, so a new variant
    /// stops the build until it is given one.
    const ALL: &[CliOutcome] = &[
        CliOutcome::Accepted,
        CliOutcome::OperationalFailure,
        CliOutcome::InvalidInput,
        CliOutcome::Blocked,
        CliOutcome::Failed,
        CliOutcome::BudgetExhausted,
        CliOutcome::Interrupted,
        CliOutcome::Cancelled,
        CliOutcome::NeedsDecision,
        CliOutcome::NeedsReview,
        CliOutcome::UnknownRun,
        CliOutcome::ResumeRefusedLive,
        CliOutcome::ResumeReconciled,
        CliOutcome::NotFullyApplied,
        CliOutcome::NoTrainingRecords,
        CliOutcome::NoTierCoverage,
        CliOutcome::SolverDiverged,
        CliOutcome::PromotionRefused,
        CliOutcome::StaleGrantKey,
        CliOutcome::NothingDetected,
        CliOutcome::TrainingLeaseHeld,
        CliOutcome::StorageFull,
    ];

    #[test]
    fn the_exit_code_table_is_total_and_injective() {
        let mut seen: Vec<i32> = ALL.iter().map(exit_code).collect();
        assert_eq!(seen.len(), 22, "every documented outcome is listed");
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            seen.len(),
            ALL.len(),
            "two outcomes share a code, so a caller cannot tell them apart"
        );
        // A shell can only see 0..=255, so the table has to live there.
        assert!(seen.iter().all(|code| (0..=255).contains(code)), "{seen:?}");
    }

    /// The distinctions the review asked for, spelled out as assertions
    /// so collapsing any of them again fails a test (C10).
    #[test]
    fn the_codes_callers_must_tell_apart_are_different() {
        let distinct = |a: CliOutcome, b: CliOutcome| {
            assert_ne!(exit_code(&a), exit_code(&b), "{a:?} vs {b:?}");
        };
        // Needs a human, versus the contract was wrong, versus no such run.
        distinct(CliOutcome::NeedsDecision, CliOutcome::InvalidInput);
        distinct(CliOutcome::NeedsReview, CliOutcome::InvalidInput);
        distinct(CliOutcome::NeedsDecision, CliOutcome::NeedsReview);
        distinct(CliOutcome::UnknownRun, CliOutcome::InvalidInput);
        // Resume refused because something may still be running, versus
        // resume having reconciled the run.
        distinct(CliOutcome::ResumeRefusedLive, CliOutcome::ResumeReconciled);
        // `1` is relais's own machinery, not the task.
        assert_eq!(exit_code(&CliOutcome::OperationalFailure), 1);
        assert_eq!(exit_code(&CliOutcome::Accepted), 0);
    }

    #[test]
    fn a_cli_error_carries_its_cause_and_picks_one_code() {
        let unreadable = CliError::Read {
            what: "the task contract",
            path: PathBuf::from("/tmp/task.json"),
            cause: std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
        };
        let rendered = unreadable.to_string();
        assert!(rendered.contains("/tmp/task.json"), "{rendered}");
        assert!(rendered.contains("no such file"), "{rendered}");
        assert_eq!(unreadable.outcome(), CliOutcome::InvalidInput);

        // A deleted working directory is an environment problem, not a
        // malformed invocation, and never a panic (C14).
        let gone = CliError::Cwd(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "No such file or directory",
        ));
        assert_eq!(gone.outcome(), CliOutcome::Blocked);
        assert!(gone.to_string().contains("removed under this process"));

        // The ledger failing is code 1 and says nothing about the task.
        let ledger = CliError::Operational {
            operation: "status",
            cause: Box::new(std::io::Error::other("database is locked")),
        };
        assert_eq!(exit_code(&ledger.outcome()), 1);
        assert!(ledger.to_string().contains("database is locked"));
    }
}
