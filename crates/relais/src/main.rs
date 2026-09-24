//! CLI entry point (SPEC §3, §17, §23). Handlers live in the library
//! modules; this file is argument parsing, wiring and exit codes.
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
//!
//! README.md carries the same table for people who do not read source.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};

use relais::acceptance::Evidence;
use relais::backend::Backend;
use relais::contract::TaskContract;
use relais::ids::{IdSource, RunId, TaskId};
use relais::install::{InstallReport, InstallRequest, Mode, Scope};
use relais::learn::predict::RegistryPredictor;
use relais::policy::{effective_authority, MachineSettings, RepoPolicy};
use relais::runner::{execute, Reason, RunConfig, State, Terminal};
use relais::verify::{independence_summary, Receipt};
use relais::{
    doctor,
    ledger::{EvidenceKind, EvidenceOrigin, Ledger},
    paths, report, resume, route, workspace,
};

#[derive(Parser)]
#[command(
    name = "relais",
    version,
    about = "Coding-agent execution companion: routing, context, verification, accounting"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Diagnose environment, integrations, coordinator, ledger and registry health
    Doctor {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
        /// Probe what Claude Code's hooks actually send: wire the hook
        /// targets relais will use into a throwaway settings file, run
        /// one real `claude -p` session through it, and write a
        /// compatibility record of what fired. Costs money and touches
        /// the network; never part of `make check`.
        #[arg(long = "probe-hooks")]
        probe_hooks: bool,
    },
    /// Create a relais.toml policy for this repository
    Init,
    /// Preflight a task contract: validate, resolve base, explain the
    /// route without launching a model (SPEC §3)
    Plan {
        /// Path to the task contract
        #[arg(long = "task")]
        task: PathBuf,
        /// Revise an existing task: this run's cost joins its total
        /// rather than starting a new one. Refused if the contract
        /// declares a different task, or if this id names no task on
        /// record.
        #[arg(long = "revise")]
        revise: Option<String>,
    },
    /// Validate the contract and start an execution (SPEC §3)
    Run {
        /// Path to the task contract
        #[arg(long = "task")]
        task: PathBuf,
        /// Revise an existing task: this run's cost joins its total
        /// rather than starting a new one. Refused if the contract
        /// declares a different task, or if this id names no task on
        /// record.
        #[arg(long = "revise")]
        revise: Option<String>,
    },
    /// Show recent runs, or one run's current state
    Status { run_id: Option<String> },
    /// Explain one run: route reasons, transitions, costs, decisions
    Explain { run_id: String },
    /// Reconcile interrupted state; never blindly repeats the last
    /// command (SPEC §3). With --retire, also retire the worktree of a
    /// terminal run (SPEC §8): everything tracked that no candidate holds
    /// is named and exported, then the directory goes
    Resume {
        /// The run to reconcile; omitted with --retire, every terminal
        /// run that still holds a worktree is retired
        #[arg(required_unless_present = "retire")]
        run_id: Option<String>,
        /// Retire the worktree(s) of terminal run(s) whose dispatches
        /// are provably dead
        #[arg(long)]
        retire: bool,
        /// Every run rather than one: the default when no run is named
        #[arg(long, requires = "retire", conflicts_with = "run_id")]
        all: bool,
    },
    /// Cost and outcome reporting since a date (SPEC §11)
    Report {
        /// Inclusive lower bound, e.g. 2026-09-01
        #[arg(long)]
        since: Option<String>,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
        /// Group the window's tasks into cohorts over one dimension, so
        /// cost, acceptance, escalation and correction figures compare
        /// like task classes rather than blending every task together
        #[arg(long, value_enum)]
        by: Option<report::Dimension>,
    },
    /// Owned-dataset and learned-routing operations (SPEC §17)
    Dataset {
        #[command(subcommand)]
        cmd: DatasetCommand,
    },
    /// Fit feature transforms and predictors on an owned dataset
    Train,
    /// Compare a candidate artifact with deterministic baselines on
    /// held-out data
    Evaluate {
        /// Artifact ID to evaluate
        #[arg(long = "artifact")]
        artifact: String,
    },
    /// Atomically activate an evaluated artifact; the previous artifact
    /// stays available for rollback (SPEC §17)
    Promote { artifact_id: String },
    /// Record a final outcome for an accepted candidate (SPEC §20)
    Feedback {
        /// The run whose accepted candidate this is about; omit with
        /// `--task` to name the task's accepted run instead
        #[arg(required_unless_present = "task", conflicts_with = "task")]
        run_id: Option<String>,
        /// The task whose accepted run this is about, instead of naming
        /// the run directly
        #[arg(long = "task")]
        task: Option<String>,
        /// What actually happened to the accepted change
        #[arg(long = "outcome")]
        outcome: FeedbackOutcome,
        /// The candidate this feedback is about; verified against the
        /// run's receipt, and refused on a mismatch
        #[arg(long = "candidate")]
        candidate: Option<String>,
        /// How large the correction was; required for `corrected`,
        /// refused for every other outcome
        #[arg(long = "magnitude")]
        magnitude: Option<f64>,
        /// Evidence references (paths, URLs, run ids) supporting this
        /// feedback
        #[arg(long = "evidence")]
        evidence: Vec<String>,
        /// Who is recording this feedback
        #[arg(long = "actor")]
        actor: String,
    },
    /// Install the Claude Code integration; preview-first, --write applies
    /// (SPEC §3). User-level installation is explicit, not the default.
    Install {
        /// Install the Claude Code skill and agent definitions
        #[arg(long = "claude")]
        claude: bool,
        /// Apply the reviewed changes instead of previewing
        #[arg(long)]
        write: bool,
        /// Install at the user level (~/.claude) instead of this project
        #[arg(long)]
        user: bool,
    },
    /// Remove only owned, unchanged artifacts (SPEC §3).
    Uninstall {
        /// Remove the Claude Code skill and agent definitions
        #[arg(long = "claude")]
        claude: bool,
        /// Apply the removal instead of previewing
        #[arg(long)]
        write: bool,
        /// Remove from the user level (~/.claude) instead of this project
        #[arg(long)]
        user: bool,
    },
    /// Coordinator operations (SPEC §23): status, cancellation, stop.
    /// The daemon starts lazily on the first managed dispatch.
    Coordinator {
        #[command(subcommand)]
        cmd: CoordinatorCommand,
    },
    /// Answer a run waiting on a person: who decided what and when, so
    /// `needs_review`/`needs_decision`/`interrupted` stop being a dead end
    Decide {
        run_id: String,
        /// What the person decided
        #[arg(long = "answer")]
        answer: DecideAnswer,
        /// Who is recording this decision
        #[arg(long = "actor")]
        actor: String,
        /// Free-form context for the decision
        #[arg(long = "note")]
        note: Option<String>,
        /// The run that carries a revision this decision led to, if any
        #[arg(long = "successor")]
        successor: Option<String>,
        /// The acceptance criterion this answer signs off, when the
        /// contract names one whose evidence is a human sign-off
        #[arg(long = "criterion")]
        criterion: Option<String>,
    },
    /// Evidence operations (SPEC §12, §18)
    Evidence {
        #[command(subcommand)]
        cmd: EvidenceCommand,
    },
    /// Record one Claude Code hook payload verbatim. Internal: this is
    /// the command `relais doctor --probe-hooks` wires into its
    /// throwaway settings file, not something a person runs directly.
    Hook {
        /// The only mode this command has: record, decide nothing.
        #[arg(long)]
        probe: bool,
        /// Where to write the payload
        #[arg(long = "record")]
        record: PathBuf,
    },
}

#[derive(Subcommand)]
enum EvidenceCommand {
    /// Record one piece of evidence produced by another tool against a
    /// run relais already reasons over. Recording only: this never marks
    /// a criterion met, changes a run's state, or clears a gap.
    Attach {
        run_id: String,
        /// Path to the evidence file; its content is hashed and the
        /// hash, not the bytes, is what the ledger stores
        #[arg(long = "path")]
        path: PathBuf,
        /// The tool that produced this evidence
        #[arg(long = "tool")]
        tool: Option<String>,
        /// That tool's own identifier for this evidence
        #[arg(long = "external-id")]
        external_id: Option<String>,
        /// The subject this evidence attests to
        #[arg(long = "subject")]
        subject: Option<String>,
        /// The acceptance criterion this evidence answers, when it
        /// answers one; refused if the run's contract does not declare
        /// it
        #[arg(long = "criterion")]
        criterion: Option<String>,
    },
}

#[derive(Subcommand)]
enum CoordinatorCommand {
    /// Run the shared per-user coordinator in the foreground (internal;
    /// the CLI spawns this detached)
    Daemon,
    /// Sessions, runs, agent trees, queued work and the limits in force
    Status {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Cancel one run, one agent subtree, or one session — never
    /// unrelated tabs
    Cancel {
        #[arg(long)]
        run: Option<String>,
        #[arg(long)]
        dispatch: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// Ask the running coordinator to exit; runs in flight keep their
    /// ledger state and `relais resume` reconciles them
    Stop,
}

#[derive(Subcommand)]
enum DatasetCommand {
    /// Snapshot a versioned dataset from the ledger (SPEC §17)
    Build,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum FeedbackOutcome {
    /// Merged and unchanged
    Accepted,
    /// Needed corrections; magnitude and evidence recorded where available
    Corrected,
    /// Backed out
    Reverted,
    /// A later user-reported regression was confirmed
    Regression,
}

impl From<FeedbackOutcome> for relais::outcome::OutcomeKind {
    fn from(outcome: FeedbackOutcome) -> Self {
        match outcome {
            FeedbackOutcome::Accepted => Self::AcceptedUnchanged,
            FeedbackOutcome::Corrected => Self::Corrected,
            FeedbackOutcome::Reverted => Self::Reverted,
            FeedbackOutcome::Regression => Self::ConfirmedRegression,
        }
    }
}

/// What a person answered `relais decide` with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum DecideAnswer {
    /// The candidate is approved. Refused while the run's verification
    /// still carries gaps — a waiver is a policy or contract change, not
    /// a CLI flag.
    Approve,
    /// The candidate is rejected.
    Reject,
    /// The task is being revised; `--successor` names the run that
    /// carries the revision, if one exists yet.
    Revise,
    /// A decision was recorded that is neither an approval, a rejection
    /// nor a revision.
    Decided,
    /// The run is abandoned rather than answered further.
    Abandon,
}

impl DecideAnswer {
    fn resolution(self) -> Reason {
        match self {
            Self::Approve => Reason::DecisionApproved,
            Self::Reject => Reason::DecisionRejected,
            Self::Revise => Reason::DecisionRevised,
            Self::Decided => Reason::DecisionRecorded,
            Self::Abandon => Reason::DecisionAbandoned,
        }
    }

    /// The terminal state a person's answer assigns (SPEC §9): approval
    /// accepts, every other answer ends the run without one. Matched
    /// directly over `DecideAnswer`, not the much larger `Reason`, so a
    /// sixth answer added to this enum without a case here does not
    /// compile — the property `decide_answers_every_variant_assigns_a_projected_status`
    /// exercises.
    fn terminal_state(self) -> State {
        match self {
            Self::Approve => State::Accepted,
            Self::Reject | Self::Revise | Self::Decided | Self::Abandon => State::Cancelled,
        }
    }
}

/// How a command ended, independent of how it is reported. Every value
/// the process can exit with is a variant here; nothing else constructs
/// an exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliOutcome {
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
}

/// The one exit-code table. Pure, total, and exhaustive over
/// [`CliOutcome`]: adding an outcome without giving it a code does not
/// compile. The module header documents what each code means, and
/// README.md repeats it.
const fn exit_code(outcome: &CliOutcome) -> i32 {
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
    }
}

/// Why a handler could not get as far as an outcome. Typed, with the
/// operation and the entity in every variant, so `main` can both print
/// one line and pick one exit code — the helpers used to return
/// `Result<_, i32>` and their codes were then thrown away for a
/// hardcoded `2` at the call site (C10).
#[derive(Debug)]
enum CliError {
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
            CliError::Read { cause, .. } => Some(cause),
            CliError::Invalid { cause, .. } | CliError::Operational { cause, .. } => {
                Some(cause.as_ref())
            }
            CliError::Usage { .. } | CliError::NoDataset { .. } => None,
        }
    }
}

impl CliError {
    /// The exit code this failure ends the process with. Exhaustive: a
    /// new variant has to say which half of the table it belongs to.
    fn outcome(&self) -> CliOutcome {
        match self {
            // Nowhere to keep settings or state is an environment
            // problem, not a malformed invocation.
            CliError::Home(_) | CliError::Cwd(_) => CliOutcome::Blocked,
            CliError::Locate(_)
            | CliError::Read { .. }
            | CliError::Invalid { .. }
            | CliError::Usage { .. } => CliOutcome::InvalidInput,
            // "No dataset has been built yet" IS "nothing to train on
            // yet", which the table already has a code for; exit 2 told
            // a caller its invocation was wrong when it was not.
            CliError::NoDataset { .. } => CliOutcome::NoTrainingRecords,
            CliError::Operational { .. } => CliOutcome::OperationalFailure,
        }
    }
}

/// A ledger, registry or filesystem operation the CLI cannot continue
/// without. The ledger is external state — locked by another relais,
/// truncated by a crash, or written by a newer version — so a failed read
/// is one `relais: …` line and a non-zero exit, never a panic.
fn operational<T, E>(result: Result<T, E>, operation: &'static str) -> Result<T, CliError>
where
    E: std::error::Error + Send + Sync + 'static,
{
    result.map_err(|cause| CliError::Operational {
        operation,
        cause: Box::new(cause),
    })
}

fn main() {
    let cli = Cli::parse();
    let outcome = match dispatch(cli.command) {
        Ok(outcome) => outcome,
        Err(e) => {
            eprintln!("relais: {e}");
            e.outcome()
        }
    };
    std::process::exit(exit_code(&outcome));
}

/// Route one parsed invocation to its handler. Every handler returns the
/// same pair, so the exit code is decided in exactly one place.
fn dispatch(command: Command) -> Result<CliOutcome, CliError> {
    match command {
        Command::Doctor { json, probe_hooks } => match probe_hooks {
            true => doctor_probe_hooks_command(),
            false => doctor_command(json),
        },
        Command::Hook { probe, record } => match probe {
            true => hook_command(&record),
            false => Err(CliError::Usage {
                detail: "hook: only `--probe --record <dir>` is implemented".into(),
            }),
        },
        Command::Init => init_command(),
        Command::Plan { task, revise } => plan_command(&task, revise.as_deref()),
        Command::Run { task, revise } => run_command(&task, revise.as_deref()),
        Command::Status { run_id } => status_command(run_id.as_deref()),
        Command::Explain { run_id } => explain_command(&run_id),
        Command::Resume {
            run_id,
            retire,
            all: _,
        } => match run_id {
            Some(run_id) => resume_command(&run_id, Retire::from_flag(retire)),
            None => retire_all_command(),
        },
        Command::Report { since, json, by } => report_command(since.as_deref(), json, by),
        Command::Dataset { cmd } => match cmd {
            DatasetCommand::Build => dataset_build_command(),
        },
        Command::Train => train_command(),
        Command::Evaluate { artifact } => evaluate_command(&artifact),
        Command::Promote { artifact_id } => promote_command(&artifact_id),
        Command::Feedback {
            run_id,
            task,
            outcome,
            candidate,
            magnitude,
            evidence,
            actor,
        } => feedback_command(FeedbackRequest {
            run_id,
            task,
            outcome,
            candidate,
            magnitude,
            evidence,
            actor,
        }),
        Command::Install {
            claude,
            write,
            user,
        } => match claude {
            true => install_command(write, user),
            false => Err(CliError::Usage {
                detail: "install: name what to install (--claude)".into(),
            }),
        },
        Command::Uninstall {
            claude,
            write,
            user,
        } => match claude {
            true => uninstall_command(write, user),
            false => Err(CliError::Usage {
                detail: "uninstall: name what to remove (--claude)".into(),
            }),
        },
        Command::Coordinator { cmd } => coordinator_command(cmd),
        Command::Decide {
            run_id,
            answer,
            actor,
            note,
            successor,
            criterion,
        } => decide_command(
            &run_id,
            answer,
            &actor,
            note.as_deref(),
            successor.as_deref(),
            criterion.as_deref(),
        ),
        Command::Evidence { cmd } => match cmd {
            EvidenceCommand::Attach {
                run_id,
                path,
                tool,
                external_id,
                subject,
                criterion,
            } => evidence_attach_command(
                &run_id,
                &path,
                tool.as_deref(),
                external_id.as_deref(),
                subject.as_deref(),
                criterion.as_deref(),
            ),
        },
    }
}

/// The request the two `--claude` commands share. `--write` is the only
/// thing that applies anything; `--user` is the only thing that leaves
/// the project.
fn install_request(write: bool, user: bool) -> Result<InstallRequest, CliError> {
    Ok(InstallRequest {
        scope: match user {
            true => Scope::User,
            false => Scope::Project(project_dir()?),
        },
        mode: match write {
            true => Mode::Apply,
            false => Mode::Preview,
        },
    })
}

/// The home directory an install request needs. Only the user scope
/// reads one; a project install must not be refused on a machine without
/// a home directory, so the project scope passes its own root (which
/// `InstallRequest::root` ignores) rather than resolving `HOME`.
fn install_home(scope: &Scope) -> Result<PathBuf, CliError> {
    match scope {
        Scope::User => paths::home_dir().map_err(CliError::Home),
        Scope::Project(dir) => Ok(dir.clone()),
    }
}

/// Print a plan and what applying it did. Returns the outcome: an apply
/// that could not carry out part of its plan exits non-zero and names the
/// files, rather than reporting "applied 0 change(s)" and 0 (C9).
fn render_install(verb: &str, request: &InstallRequest, report: &InstallReport) -> CliOutcome {
    println!(
        "relais {verb} --claude ({})\n{}",
        request.scope_label(),
        report.plan.render()
    );
    match report.mode {
        Mode::Preview => {
            if report.plan.applicable_count() == 0 {
                println!("nothing to do");
            } else {
                println!("preview only: re-run with --write to apply");
            }
            CliOutcome::Accepted
        }
        Mode::Apply => {
            println!("applied {} change(s)", report.applied.len());
            if report.not_applied.is_empty() {
                return CliOutcome::Accepted;
            }
            for action in &report.not_applied {
                eprintln!(
                    "relais {verb}: could not apply {} — it changed between the preview and \
                     --write; re-run to see what it is now",
                    action.relative().display()
                );
            }
            CliOutcome::NotFullyApplied
        }
    }
}

fn install_command(write: bool, user: bool) -> Result<CliOutcome, CliError> {
    let request = install_request(write, user)?;
    let home = install_home(&request.scope)?;
    let report = operational(relais::install::install(&request, &home), "install")?;
    Ok(render_install("install", &request, &report))
}

fn uninstall_command(write: bool, user: bool) -> Result<CliOutcome, CliError> {
    let request = install_request(write, user)?;
    let home = install_home(&request.scope)?;
    let report = operational(relais::install::uninstall(&request, &home), "uninstall")?;
    Ok(render_install("uninstall", &request, &report))
}

/// The registry, when learned routing is on and the registry opens; a
/// missing or unreadable registry is the conservative baseline, not an
/// error (SPEC §21: missing trained evidence produces conservative
/// execution without disabling the rest of the product).
///
/// "Unreadable" is not "absent", though, and the difference is the
/// operator's to act on: a registry that will not open is said so on
/// stderr before routing falls back.
fn learned_registry(machine: &MachineSettings) -> Option<relais::learn::registry::Registry> {
    if !machine.routing.learned_enabled {
        return None;
    }
    let dir = match paths::registry_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!(
                "relais: learned routing is on and the registry directory is unavailable ({e}); \
                 routing falls back to the policy rules"
            );
            return None;
        }
    };
    match relais::learn::registry::Registry::open(&dir) {
        Ok(registry) => Some(registry),
        Err(e) => {
            eprintln!(
                "relais: learned routing is on and the registry at {} is unreadable ({e}); \
                 routing falls back to the policy rules",
                dir.display()
            );
            None
        }
    }
}

/// `<backend> <version>` for `plan`, which must not need the harness to
/// answer: unknown when it is not installed. Which of "not installed"
/// and "installed and would not answer" it was goes to stderr — the
/// plan is still printed either way.
fn harness_identity() -> Option<String> {
    let backend = match relais::adapter::claude::ClaudeBackend::discover() {
        Ok(backend) => backend,
        Err(e) => {
            eprintln!("relais plan: the harness is unknown ({e})");
            return None;
        }
    };
    let capabilities = match backend.probe_report() {
        Ok(capabilities) => capabilities,
        Err(failure) => {
            eprintln!("relais plan: the harness is unknown ({failure})");
            return None;
        }
    };
    Some(format!(
        "{} {}",
        backend.name(),
        capabilities.version.as_deref().unwrap_or("?")
    ))
}

/// The identifier source this process mints with: the wall clock and
/// this process's id, read once, here, at the boundary. `ids` itself
/// takes both as parameters (`effects.no-ambient-access`), which is
/// what lets a test assert on a minted identifier.
fn id_source() -> IdSource {
    IdSource::new(std::time::SystemTime::now, std::process::id())
}

fn registry() -> Result<relais::learn::registry::Registry, CliError> {
    let dir = paths::registry_dir().map_err(CliError::Home)?;
    operational(
        relais::learn::registry::Registry::open(&dir),
        "the artifact registry is unavailable",
    )
}

fn datasets_dir() -> Result<PathBuf, CliError> {
    Ok(paths::state_dir().map_err(CliError::Home)?.join("datasets"))
}

fn latest_dataset() -> Result<relais::learn::dataset::Dataset, CliError> {
    let dir = datasets_dir()?;
    let newest = match std::fs::read_dir(&dir) {
        // Nothing built yet is the ordinary case, and it has its own
        // message; anything else about the directory is a real failure.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(CliError::Read {
                what: "the dataset directory",
                path: dir,
                cause: e,
            })
        }
        Ok(entries) => entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .max(),
    };
    let Some(path) = newest else {
        return Err(CliError::NoDataset { dir });
    };
    let text = std::fs::read_to_string(&path).map_err(|cause| CliError::Read {
        what: "the dataset",
        path: path.clone(),
        cause,
    })?;
    serde_json::from_str(&text).map_err(|cause| CliError::Invalid {
        what: "the dataset",
        path,
        cause: Box::new(cause),
    })
}

fn dataset_build_command() -> Result<CliOutcome, CliError> {
    let (_, repo) = load_repo_policy()?;
    let ledger = open_ledger()?;
    // A stored contract revision that does not parse is a corrupt ledger
    // row, not a run without a contract: dataset construction must see
    // the difference, or a version skew silently empties the dataset.
    // The ledger parses both the contract and the tier now, so what
    // arrives here is already a value or already an error.
    let contract_of = |run_id: &str| -> Result<
        Option<(TaskContract, String, String)>,
        relais::ledger::LedgerError,
    > {
        let run = RunId::from_stored(run_id);
        let Some((contract, tier)) = ledger.run_contract_and_tier(&run)? else {
            return Ok(None);
        };
        let objective = contract.objective.clone();
        Ok(Some((contract, objective, tier.as_str().to_string())))
    };
    let dataset = operational(
        relais::learn::dataset::build(&ledger, &contract_of, &repo),
        "dataset build",
    )?;
    let distribution = dataset.acceptance_labels();
    let dir = datasets_dir()?;
    operational(std::fs::create_dir_all(&dir), "dataset build")?;
    let path = dir.join(format!(
        "{}.json",
        chrono::Utc::now().format("%Y%m%dT%H%M%S")
    ));
    let serialized = operational(serde_json::to_string_pretty(&dataset), "dataset build")?;
    operational(std::fs::write(&path, serialized), "dataset build")?;
    println!(
        "dataset: {} (fingerprint {})",
        path.display(),
        dataset.fingerprint
    );
    println!(
        "records: {} ({} accepted-without-escalation, {} not)",
        distribution.records, distribution.accepted_without_escalation, distribution.other
    );
    println!("coverage: {}", {
        let mut coverage: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for record in &dataset.records {
            *coverage
                .entry(record.tier.as_str().to_string())
                .or_default() += 1;
        }
        coverage
            .into_iter()
            .map(|(tier, count)| format!("{tier}:{count}"))
            .collect::<Vec<_>>()
            .join(", ")
    });
    if dataset.exclusions.is_empty() {
        println!("exclusions: none");
    } else {
        println!("exclusions ({}):", dataset.exclusions.len());
        for exclusion in dataset.exclusions.iter().take(20) {
            println!("  {exclusion}");
        }
    }
    Ok(CliOutcome::Accepted)
}

fn train_command() -> Result<CliOutcome, CliError> {
    let dataset = latest_dataset()?;
    // Gate thresholds are machine settings; without a machine.toml the
    // conservative defaults apply, as they do for routing.
    let routing = load_machine()
        .map(|machine| machine.routing)
        .unwrap_or_default();
    let artifact_id = format!("artifact-{}", chrono::Utc::now().format("%Y%m%dT%H%M%S"));
    let settings = relais::learn::evaluate::EvalSettings {
        artifact_id: artifact_id.clone(),
        solver: relais::learn::learner::SolverSettings::default(),
        quality_floor: routing
            .quality_floor
            .unwrap_or(relais::learn::evaluate::DEFAULT_QUALITY_FLOOR),
        min_records_per_tier: relais::learn::evaluate::DEFAULT_MIN_RECORDS_PER_TIER,
        min_supported_test_records: routing.min_supported_test_records,
        max_abstention_rate: routing.max_abstention_rate,
    };
    println!(
        "training on {} record(s) (seed {})",
        dataset.records.len(),
        settings.solver.seed
    );
    let outcome = match relais::learn::evaluate::train_and_evaluate(&dataset, &settings) {
        Ok(outcome) => outcome,
        Err(e) => {
            eprintln!("relais train: {e}");
            // Distinct causes, distinct codes: a scheduler retrying a
            // build cares whether there is nothing yet, not enough of one
            // tier, or a solver that ran away.
            return Ok(match e {
                relais::learn::evaluate::TrainError::NoTrainingRecords { .. } => {
                    CliOutcome::NoTrainingRecords
                }
                relais::learn::evaluate::TrainError::NoTierCoverage { .. } => {
                    CliOutcome::NoTierCoverage
                }
                relais::learn::evaluate::TrainError::SolverDiverged { .. } => {
                    CliOutcome::SolverDiverged
                }
            });
        }
    };
    let artifact = relais::learn::registry::Artifact {
        schema_version: relais::learn::registry::ARTIFACT_SCHEMA_VERSION,
        artifact_id: artifact_id.clone(),
        feature_schema: relais::learn::features::FeatureSchema::standard(),
        standardization: outcome.standardization,
        acceptance: outcome.acceptance,
        cost: outcome.cost,
        tiers_supported: outcome.tiers_supported,
        observed_identities: outcome.observed_identities,
        cohorts: vec!["change".into(), "inspect".into()],
        label_policy_version: dataset.label_policy_version,
        dataset_fingerprint: dataset.fingerprint,
        solver: settings.solver,
        trained_at: relais::ledger::now_rfc3339(),
        relais_version: relais::version().into(),
        evaluation: Some(
            serde_json::to_value(&outcome.report)
                .expect("an EvalReport serializes: owned strings and finite f64"),
        ),
    };
    let reg = registry()?;
    operational(reg.store(&artifact), "train")?;
    print!("{}", outcome.report.render());
    println!("candidate artifact: {artifact_id}");
    if outcome.report.gates.gates_passed {
        println!("`relais promote {artifact_id}` activates it (rollback is immediate)");
    } else {
        println!("gates failed; promote will refuse this artifact");
    }
    Ok(CliOutcome::Accepted)
}

fn evaluate_command(artifact_id: &str) -> Result<CliOutcome, CliError> {
    let reg = registry()?;
    let artifact = match reg.load(artifact_id) {
        Ok(artifact) => artifact,
        Err(e) => {
            eprintln!("relais evaluate: {e}");
            return Ok(CliOutcome::UnknownRun);
        }
    };
    let Some(evaluation) = &artifact.evaluation else {
        eprintln!("relais evaluate: artifact {artifact_id} carries no evaluation report");
        return Ok(CliOutcome::InvalidInput);
    };
    // A stored evaluation is registry data on disk, possibly written by
    // another version: unreadable is an error to print, not an abort.
    let report: relais::learn::evaluate::EvalReport =
        match serde_json::from_value(evaluation.clone()) {
            Ok(report) => report,
            Err(e) => {
                eprintln!(
                    "relais evaluate: artifact {artifact_id} carries an evaluation report this \
                     relais cannot read: {e}"
                );
                return Ok(CliOutcome::InvalidInput);
            }
        };
    print!("{}", report.render());
    println!("dataset fingerprint: {}", artifact.dataset_fingerprint);
    println!(
        "trained at: {} with relais {}",
        artifact.trained_at, artifact.relais_version
    );
    let failures = report.gates.failures();
    if failures.is_empty() {
        return Ok(CliOutcome::Accepted);
    }
    // The verdict is recomputed here too: a stored `gates_passed` is a
    // claim, and `promote` will check it the same way.
    for failure in failures {
        eprintln!("relais evaluate: gate: {failure}");
    }
    Ok(CliOutcome::Blocked)
}

fn promote_command(artifact_id: &str) -> Result<CliOutcome, CliError> {
    let reg = registry()?;
    // The registry checks the evidence and hands back a type that says so;
    // `promote` cannot be called with anything else.
    let evaluated = match reg.evaluated(artifact_id) {
        Ok(evaluated) => evaluated,
        Err(e) => {
            eprintln!("relais promote: {e}");
            return Ok(CliOutcome::Blocked);
        }
    };
    match reg.promote(&evaluated) {
        Ok(()) => {
            println!(
                "promoted {artifact_id}; the previous artifact stays for `relais promote` rollback"
            );
            Ok(CliOutcome::Accepted)
        }
        Err(e) => {
            eprintln!("relais promote: {e}");
            Ok(CliOutcome::OperationalFailure)
        }
    }
}

/// `coordinator status --json`, in every state. A typed document, never a
/// human sentence on stdout: a caller parsing this had to tell "no daemon"
/// from a parse failure, and the client's error was thrown away
/// altogether (C3).
#[derive(Debug, serde::Serialize)]
#[serde(tag = "coordinator", rename_all = "snake_case")]
enum CoordinatorStatusDocument {
    /// A daemon answered.
    Serving {
        socket: String,
        snapshot: Box<relais::admission::StatusSnapshot>,
    },
    /// Nothing answered on the socket. `cause` is the client's own
    /// error, which distinguishes "no socket" from "connection refused".
    Absent { socket: String, cause: String },
    /// A daemon IS serving and speaks another wire protocol.
    VersionSkew {
        socket: String,
        cause: String,
        ours: u32,
        theirs: u32,
        daemon_version: String,
    },
}

fn coordinator_status_command(socket: &Path, json: bool) -> Result<CliOutcome, CliError> {
    use relais::coordinator::{self, CoordinatorError};
    let client = coordinator::Client::new(socket.to_path_buf());
    let socket_text = socket.display().to_string();
    let snapshot = match client.status() {
        Ok(snapshot) => snapshot,
        // A daemon IS serving and speaks another wire protocol:
        // reporting that as "no coordinator" would send the operator
        // looking for one that is right there.
        Err(CoordinatorError::VersionSkew {
            socket: reported,
            ours,
            theirs,
            daemon_version,
        }) => {
            // Rebuilt so the human line and the JSON `cause` are the same
            // sentence, written once in `CoordinatorError`'s `Display`.
            let skew = CoordinatorError::VersionSkew {
                socket: reported,
                ours,
                theirs,
                daemon_version: daemon_version.clone(),
            };
            if json {
                print_document(&CoordinatorStatusDocument::VersionSkew {
                    socket: socket_text,
                    cause: skew.to_string(),
                    ours,
                    theirs,
                    daemon_version,
                })?;
            } else {
                eprintln!("relais coordinator status: {skew}");
            }
            return Ok(CliOutcome::Blocked);
        }
        Err(e) => {
            if json {
                print_document(&CoordinatorStatusDocument::Absent {
                    socket: socket_text,
                    cause: e.to_string(),
                })?;
            } else {
                println!(
                    "no coordinator is serving this user ({socket_text}); one starts on the \
                     first managed dispatch ({e})"
                );
            }
            return Ok(CliOutcome::Accepted);
        }
    };
    if json {
        print_document(&CoordinatorStatusDocument::Serving {
            socket: socket_text,
            snapshot: Box::new(snapshot),
        })?;
        return Ok(CliOutcome::Accepted);
    }
    println!("coordinator: {socket_text}");
    println!(
        "limits: agents {} (per session {}), heavy {}, training {}, depth {}, per run {}",
        opt(snapshot.limits.max_active_agents),
        opt(snapshot.limits.max_active_agents_per_session),
        opt(snapshot.limits.max_heavy_commands),
        opt(snapshot.limits.max_training_jobs),
        opt(snapshot.limits.max_agent_depth),
        opt(snapshot.limits.max_agents_per_run),
    );
    println!(
        "active: {} | waiting parents: {} | queued: {} | stale leases: {} | over cap: {}",
        snapshot
            .active_by_class
            .iter()
            .map(|(class, count)| format!("{class}={count}"))
            .collect::<Vec<_>>()
            .join(" "),
        snapshot.waiting,
        snapshot.queued,
        snapshot.stale_leases,
        snapshot.over_admitted
    );
    println!("sessions: {}", snapshot.sessions.join(", "));
    if !snapshot.bound_processes.is_empty() {
        // How old the liveness check behind each binding is: the window
        // in which the OS could have recycled the number cannot be
        // closed, so it is shown (SPEC §23, C6).
        println!("bound processes:");
        for (dispatch, bound) in &snapshot.bound_processes {
            println!(
                "  {dispatch} -> pid {} (checked alive {}s ago)",
                bound.pid, bound.bound_for_secs
            );
        }
    }
    if !snapshot.write_leases.is_empty() {
        println!("write leases:");
        for (worktree, holder) in &snapshot.write_leases {
            println!("  {worktree} <- {holder}");
        }
    }
    for (run_id, run) in &snapshot.runs {
        println!(
            "  {run_id} [{}] active {} waiting {} queued {} admitted {} | budget {} reserved {} settled {}{}{}",
            run.session_id,
            run.active,
            run.waiting,
            run.queued,
            run.admitted_total,
            run.budget_micros
                .map(|micros| relais::money::MicroUsd::from_micros(micros).to_string())
                .unwrap_or_else(|| "none".into()),
            relais::money::MicroUsd::from_micros(run.reserved_micros),
            relais::money::MicroUsd::from_micros(run.settled_micros),
            if run.uncertain_settlements > 0 {
                format!(" ({} uncertain: lower bound)", run.uncertain_settlements)
            } else {
                String::new()
            },
            if run.cancelled { " CANCELLED" } else { "" }
        );
    }
    println!("enforcement: managed dispatch only; native subagents are observed, not capped");
    Ok(CliOutcome::Accepted)
}

/// Print one JSON document on stdout. Serialization of a document this
/// file built is an invariant, not a runtime condition, but it is still
/// reported rather than unwrapped.
fn print_document<T: serde::Serialize>(document: &T) -> Result<(), CliError> {
    let text = operational(serde_json::to_string_pretty(document), "rendering JSON")?;
    println!("{text}");
    Ok(())
}

fn coordinator_command(cmd: CoordinatorCommand) -> Result<CliOutcome, CliError> {
    use relais::coordinator::{self, Request, Response};
    let socket = coordinator::socket_path().map_err(CliError::Home)?;
    match cmd {
        CoordinatorCommand::Daemon => {
            // ABSENT machine settings are not an error for the daemon:
            // the defaults apply and the grant check stays with `run`.
            // A machine.toml that exists and does not parse is a
            // different thing entirely — the concurrency limits it
            // states would silently become the defaults, which are
            // wider (X5) — so it stops the daemon instead.
            let path = paths::machine_settings_path().map_err(CliError::Home)?;
            let limits = match std::fs::read_to_string(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    coordinator::effective_limits(&Default::default())
                }
                Err(cause) => {
                    return Err(CliError::Read {
                        what: "the machine settings",
                        path,
                        cause,
                    })
                }
                Ok(text) => match MachineSettings::from_toml_str(&text) {
                    Ok(machine) => coordinator::effective_limits(&machine.concurrency),
                    Err(e) => {
                        eprintln!(
                            "relais coordinator: {} is invalid: {e}; refusing to serve with \
                             default limits in place of the ones it states",
                            path.display()
                        );
                        return Ok(CliOutcome::InvalidInput);
                    }
                },
            };
            // The daemon serves without a ledger, but it then adopts no
            // live dispatch at election: an unreadable ledger is said
            // so, rather than looking like a machine with no runs.
            let ledger = match paths::ledger_path().map(|path| (Ledger::open(&path), path)) {
                Ok((Ok(ledger), _)) => Some(ledger),
                Ok((Err(e), path)) => {
                    eprintln!(
                        "relais coordinator: the ledger at {} is unreadable ({e}); serving \
                         without it means no dispatch is adopted at election",
                        path.display()
                    );
                    None
                }
                Err(e) => {
                    eprintln!(
                        "relais coordinator: the ledger path is unavailable ({e}); serving \
                         without it means no dispatch is adopted at election"
                    );
                    None
                }
            };
            match coordinator::run_daemon(&socket, limits, ledger.as_ref()) {
                Ok(()) => Ok(CliOutcome::Accepted),
                Err(e) => {
                    eprintln!("relais coordinator: {e}");
                    Ok(CliOutcome::OperationalFailure)
                }
            }
        }
        CoordinatorCommand::Status { json } => coordinator_status_command(&socket, json),
        CoordinatorCommand::Cancel {
            run,
            dispatch,
            session,
        } => {
            let request = match (run, dispatch, session) {
                (Some(run_id), None, None) => Request::CancelRun { run_id },
                (None, Some(dispatch_id), None) => Request::CancelDispatch { dispatch_id },
                (None, None, Some(session_id)) => Request::CancelSession { session_id },
                _ => {
                    return Err(CliError::Usage {
                        detail: "coordinator cancel: name exactly one of --run, --dispatch, \
                                 --session"
                            .into(),
                    })
                }
            };
            let client = coordinator::Client::new(socket);
            match client.request(&request) {
                Ok(Response::Cancelled { dispatches }) => {
                    println!(
                        "cancelled; {} bound worker process(es) signalled: {}",
                        dispatches.len(),
                        dispatches.join(", ")
                    );
                    Ok(CliOutcome::Accepted)
                }
                Ok(other) => {
                    eprintln!("relais coordinator cancel: unexpected reply {other:?}");
                    Ok(CliOutcome::OperationalFailure)
                }
                Err(e) => {
                    eprintln!("relais coordinator cancel: {e}");
                    Ok(CliOutcome::Blocked)
                }
            }
        }
        CoordinatorCommand::Stop => {
            let client = coordinator::Client::new(socket);
            match client.request(&Request::Shutdown) {
                Ok(_) => {
                    // The accept loop observes the flag on its next
                    // connection; a refused ping IS the daemon going
                    // away, which is what was asked for.
                    let _ = client.ping();
                    println!("coordinator asked to stop");
                    Ok(CliOutcome::Accepted)
                }
                Err(e) => {
                    eprintln!("relais coordinator stop: {e}");
                    Ok(CliOutcome::Blocked)
                }
            }
        }
    }
}

fn opt(value: Option<u32>) -> String {
    value.map_or_else(|| "unlimited".into(), |value| value.to_string())
}

/// The process's working directory. An error, never a panic: tearing down
/// a task worktree deletes the directory a long-running relais is sitting
/// in, and a backtrace is the wrong way to say so (C14).
fn cwd() -> Result<PathBuf, CliError> {
    std::env::current_dir().map_err(CliError::Cwd)
}

/// The directory a repository-level command acts on: the root whose
/// `relais.toml` governs the cwd; failing that, the repository root
/// (where `init` belongs); failing that, the cwd itself. Never a
/// locate error: `doctor`, `init` and `install` report what they find
/// there.
fn project_dir() -> Result<PathBuf, CliError> {
    let cwd = cwd()?;
    Ok(match relais::repo::locate_repo_root(&cwd) {
        Ok(root) | Err(relais::repo::LocateError::RepoWithoutPolicy(root)) => root,
        Err(relais::repo::LocateError::NotInRepository(start)) => start,
    })
}

/// The repository root and its policy. `relais.toml` is looked up from
/// the cwd upward to the nearest `.git`, so a Claude Code session whose
/// cwd is a subdirectory, or the shell's `cd repo && relais …`, both land
/// on the repository's policy; a directory outside any repository is
/// refused by name rather than guessed at.
fn load_repo_policy() -> Result<(PathBuf, RepoPolicy), CliError> {
    let root = relais::repo::locate_repo_root(&cwd()?).map_err(CliError::Locate)?;
    let path = root.join("relais.toml");
    let text = std::fs::read_to_string(&path).map_err(|cause| CliError::Read {
        what: "the repository policy",
        path: path.clone(),
        cause,
    })?;
    let policy = RepoPolicy::from_toml_str(&text).map_err(|cause| CliError::Invalid {
        what: "the repository policy",
        path,
        cause: Box::new(cause),
    })?;
    Ok((root, policy))
}

fn load_machine() -> Result<MachineSettings, CliError> {
    let path = paths::machine_settings_path().map_err(CliError::Home)?;
    let text = std::fs::read_to_string(&path).map_err(|cause| {
        if cause.kind() == std::io::ErrorKind::NotFound {
            eprintln!(
                "relais: machine settings {} do not exist; runs stay blocked on the trust grant",
                path.display()
            );
        }
        // Unreadable is not absent: ceilings and grants the file states
        // would silently not apply.
        CliError::Read {
            what: "the machine settings",
            path: path.clone(),
            cause,
        }
    })?;
    MachineSettings::from_toml_str(&text).map_err(|cause| CliError::Invalid {
        what: "the machine settings",
        path,
        cause: Box::new(cause),
    })
}

fn load_contract(task: &Path) -> Result<TaskContract, CliError> {
    let text = std::fs::read_to_string(task).map_err(|cause| CliError::Read {
        what: "the task contract",
        path: task.to_path_buf(),
        cause,
    })?;
    TaskContract::from_json_str(&text).map_err(|cause| CliError::Invalid {
        what: "the task contract",
        path: task.to_path_buf(),
        cause: Box::new(cause),
    })
}

fn open_ledger() -> Result<Ledger, CliError> {
    let path = paths::ledger_path().map_err(CliError::Home)?;
    operational(Ledger::open(&path), "the ledger is unavailable")
}

/// What this run's task identity will be, decided before any dispatch
/// (SPEC's task spine): `None` for a fresh task, derived once preflight
/// runs; `Some(id)` for a task the contract declares or `--revise`
/// names, already confirmed on record. A disagreement between the two,
/// or an id naming no task at all, is refused here by name rather than
/// silently picked or created.
fn resolve_task_override(
    ledger: &Ledger,
    contract: &TaskContract,
    revise: Option<&str>,
) -> Result<Option<TaskId>, String> {
    let revise = revise.map(TaskId::from_stored);
    let link = relais::contract::resolve_task_link(contract.task_id.as_ref(), revise.as_ref())
        .map_err(|e| e.to_string())?;
    match link {
        relais::contract::TaskLink::Fresh => Ok(None),
        relais::contract::TaskLink::Declared(id) | relais::contract::TaskLink::Revises(id) => {
            let known = ledger
                .runs_of_task(&id)
                .map_err(|e| format!("ledger: {e}"))?;
            if known.is_empty() {
                Err(format!("unknown task {id}: no run is on record for it"))
            } else {
                Ok(Some(id))
            }
        }
    }
}

fn doctor_command(json: bool) -> Result<CliOutcome, CliError> {
    let report = doctor::doctor(&project_dir()?);
    if json {
        print_document(&report)?;
    } else {
        print!("{}", report.render());
    }
    match report.failed() {
        true => Ok(CliOutcome::Blocked),
        false => Ok(CliOutcome::Accepted),
    }
}

/// `relais hook --probe --record <dir>`: the record-only handler
/// `relais doctor --probe-hooks` wires into its throwaway settings file.
/// Reads one payload from stdin, writes it verbatim, decides nothing,
/// and always accepts — the handler cannot fail the session it is
/// watching.
fn hook_command(dir: &Path) -> Result<CliOutcome, CliError> {
    let order = relais::hook::next_order(dir);
    relais::hook::record(dir, order, std::io::stdin().lock());
    Ok(CliOutcome::Accepted)
}

/// `relais doctor --probe-hooks`: wire the seven hook targets into a
/// throwaway settings file, run one real session through it, and print
/// what was observed. Never part of `relais doctor`'s normal report —
/// this needs a real Claude Code, costs money and touches the network.
fn doctor_probe_hooks_command() -> Result<CliOutcome, CliError> {
    let claude_binary = relais::tooling::which("claude").ok_or_else(|| CliError::Operational {
        operation: "doctor --probe-hooks",
        cause: Box::new(relais::hook::ProbeHooksError::NoClaudeBinary),
    })?;
    let relais_binary = operational(std::env::current_exe(), "doctor --probe-hooks")?;
    let report = operational(
        relais::hook::probe(&claude_binary, &relais_binary),
        "doctor --probe-hooks",
    )?;
    println!(
        "hook probe: Claude Code {}",
        report.record.claude_code_version
    );
    println!("  settings:   {}", report.settings_path.display());
    println!("  recordings: {}", report.recording_dir.display());
    for target in &report.record.targets {
        let fields = if target.fields.is_empty() {
            "(no fields)".to_string()
        } else {
            target.fields.join(", ")
        };
        println!(
            "  {:<20} {:<12} {}",
            target.target,
            if target.fired {
                "fired"
            } else {
                "did not fire"
            },
            fields
        );
    }
    Ok(CliOutcome::Accepted)
}

fn init_command() -> Result<CliOutcome, CliError> {
    // At the repository root, not the shell's cwd: a policy written in a
    // subdirectory would govern nothing.
    let path = project_dir()?.join("relais.toml");
    match relais::repo::write_init_template(&path) {
        Ok(true) => {
            println!(
                "wrote {} (edit the model IDs and verification profile, then add a trust grant in machine.toml)",
                path.display()
            );
            Ok(CliOutcome::Accepted)
        }
        Ok(false) => {
            eprintln!(
                "relais init: {} already exists; init never overwrites",
                path.display()
            );
            Ok(CliOutcome::InvalidInput)
        }
        Err(e) => {
            eprintln!("relais init: {e}");
            Ok(CliOutcome::OperationalFailure)
        }
    }
}

fn plan_command(task: &Path, revise: Option<&str>) -> Result<CliOutcome, CliError> {
    let (root, repo) = load_repo_policy()?;
    let machine = load_machine()?;
    let contract = load_contract(task)?;
    // Only a contract that NAMES a task needs the ledger, and `plan`
    // writes nothing: opening it unconditionally would create the state
    // directory, the database and its migrations as a side effect of a
    // preflight whose whole promise is that nothing happens yet.
    if contract.task_id.is_some() || revise.is_some() {
        let ledger = open_ledger()?;
        if let Err(detail) = resolve_task_override(&ledger, &contract, revise) {
            eprintln!("relais plan: {detail}");
            return Ok(CliOutcome::Blocked);
        }
    }

    // Matched, not `if let Ok`: a `git status` that FAILS is not a clean
    // tree, and `run` has blocked on exactly this since v0.1.3 (C6).
    match workspace::dirty_paths(&root) {
        Ok(dirty) if dirty.is_empty() => {}
        Ok(dirty) => {
            eprintln!(
                "relais plan: working tree is dirty ({}); commit or stash first",
                dirty.join(", ")
            );
            return Ok(CliOutcome::Blocked);
        }
        Err(e) => {
            eprintln!(
                "relais plan: blocked (dirty_base): the working tree of {} could not be read: {e}",
                root.display()
            );
            return Ok(CliOutcome::Blocked);
        }
    }
    let base_sha = match workspace::resolve_base(&root, &contract.base_ref) {
        Ok(sha) => sha,
        Err(e) => {
            eprintln!(
                "relais plan: cannot resolve base_ref {}: {e}",
                contract.base_ref
            );
            return Ok(CliOutcome::Blocked);
        }
    };
    let repo_identity = relais::repo::identity(&root);
    let authority = effective_authority(&repo, &machine, &contract, &repo_identity);
    let harness = harness_identity();
    let registry = learned_registry(&machine);
    let predictor = registry
        .as_ref()
        .map(|registry| RegistryPredictor::new(registry, &repo, harness.as_deref()));
    let decision = route::route(route::RouteInputs {
        contract: &contract,
        repo: &repo,
        machine: &machine,
        authority: &authority,
        predictor: predictor
            .as_ref()
            .map(|predictor| predictor as &dyn route::RoutePredictor),
    });
    println!("contract hash: {}", contract.hash());
    println!("policy hash: {}", authority.authority_hash);
    println!("repository: {repo_identity}");
    println!("base: {} ({})", base_sha, contract.base_ref);
    // The same line `relais doctor` prints, on stderr so the plan's
    // stdout stays what it was: a lockfile with no setup declared is
    // worth knowing before a run spends a baseline on exit 127.
    if let Some(finding) = relais::doctor::setup_finding(&repo, &relais::repo::lockfiles(&root)) {
        if finding.level == relais::doctor::Level::Warn {
            eprintln!("relais plan: warning: {}", finding.detail);
        }
    }
    if authority.trust_granted {
        println!("trust grant: {} (in machine.toml)", authority.grant_key);
    } else {
        println!(
            "trust grant: MISSING for {}. Review the execution declaration, then paste this \n\
             into {}:\n\n\
             [trust.\"{}\"]\n\
             granted_at = \"{}\"\n\
             reviewed_by = \"<your name>\"\n\
             repo = \"{}\"\n\
             note = \"<what you reviewed>\"\n",
            authority.grant_key,
            paths::machine_settings_path()
                .map_err(CliError::Home)?
                .display(),
            authority.grant_key,
            chrono::Utc::now().format("%Y-%m-%d"),
            repo_identity.label(),
        );
    }
    match &decision {
        route::Routed::Route(routed) => {
            let model = authority
                .models
                .get(&routed.tier)
                .map(|profile| profile.id.as_str());
            print!("{}", routed.explain(model));
            Ok(CliOutcome::Accepted)
        }
        route::Routed::Blocked(blocked) => {
            // `explain` already lists every blocker with its code; this
            // used to print them a second time on stderr, so a plan
            // blocked by one thing reported it twice.
            print!("{}", blocked.explain());
            Ok(CliOutcome::Blocked)
        }
    }
}

fn run_command(task: &Path, revise: Option<&str>) -> Result<CliOutcome, CliError> {
    let (root, repo) = load_repo_policy()?;
    let machine = load_machine()?;
    let contract = load_contract(task)?;
    let ledger = open_ledger()?;
    let task_override = match resolve_task_override(&ledger, &contract, revise) {
        Ok(task_override) => task_override,
        Err(detail) => {
            eprintln!("relais run: {detail}");
            return Ok(CliOutcome::Blocked);
        }
    };
    let artifacts_dir = paths::runs_dir().map_err(CliError::Home)?;
    let backend = match relais::adapter::claude::ClaudeBackend::discover() {
        Ok(backend) => std::sync::Arc::from(backend),
        Err(e) => {
            eprintln!("relais run: {e}");
            return Ok(CliOutcome::Blocked);
        }
    };
    // The three tools a run talks to, each behind the port this crate
    // owns: git, the decision corpus and the check inventory.
    let git = relais::workspace::SystemGit;
    let aval_resolver = relais::context::AvalCli::new(root.clone());
    let hooks = relais::verify::AmontCli::new();
    let attest = relais::verify::AmontCli::new();
    // What every worker this run dispatches will have in its
    // environment: an allowlist of this process's own, never the whole
    // of it (SPEC §8).
    let worker_env = relais::backend::LaunchEnv::from_process_env();
    // Managed dispatch is the only path `relais run` takes: a missing
    // coordinator blocks the run rather than launching unmanaged
    // (SPEC §23).
    let socket = relais::coordinator::socket_path().map_err(CliError::Home)?;
    if let Err(e) = relais::coordinator::ensure_running(&socket) {
        eprintln!("relais run: blocked (admission_unavailable): {e}");
        return Ok(CliOutcome::Blocked);
    }
    let gate = relais::coordinator::RemoteGate::new(socket);
    // Learned routing reads the registry's active artifact, pinned for
    // this run (SPEC §17); disabled routing leaves everything else intact.
    let harness = backend.probe().map(|capabilities| {
        format!(
            "{} {}",
            backend.name(),
            capabilities.version.as_deref().unwrap_or("?")
        )
    });
    let registry = learned_registry(&machine);
    let predictor = registry
        .as_ref()
        .map(|registry| RegistryPredictor::new(registry, &repo, harness.as_deref()));
    let ids = id_source();
    let outcome = match execute(&RunConfig {
        repo_dir: &root,
        contract: &contract,
        repo_policy: &repo,
        machine: &machine,
        ledger: &ledger,
        ids: &ids,
        backend: backend.as_ref(),
        git: &git,
        hooks: &hooks,
        attest: &attest,
        worker_env,
        artifacts_dir: artifacts_dir.clone(),
        aval_resolver: &aval_resolver,
        predictor: predictor
            .as_ref()
            .map(|predictor| predictor as &dyn route::RoutePredictor),
        gate: Some(&gate),
        session_id: relais::coordinator::session_id(),
        heartbeat_every: std::time::Duration::from_secs(30),
        task_override: task_override.as_ref(),
    }) {
        Ok(outcome) => outcome,
        Err(e) => {
            eprintln!("relais run: {e}");
            return Ok(CliOutcome::OperationalFailure);
        }
    };
    let run_dir = artifacts_dir.join(outcome.run_id());
    match &outcome.terminal {
        Terminal::Accepted(receipt) => {
            println!("accepted: {}", receipt.candidate_sha);
            println!("receipt: {}/receipt.json", run_dir.display());
            println!(
                "patch:   {}/candidate-{}.patch",
                run_dir.display(),
                receipt.attempts
            );
            println!(
                "cost:    {}",
                report::cost_line(receipt.cost, receipt.cost_completeness)
            );
            println!("run:     {}", outcome.run_id());
            Ok(CliOutcome::Accepted)
        }
        Terminal::NeedsDecision { reason, detail } => {
            eprintln!("needs_decision ({reason}): {detail}");
            eprintln!(
                "evidence and the preserved candidate are under {}",
                run_dir.display()
            );
            Ok(CliOutcome::NeedsDecision)
        }
        Terminal::NeedsReview { detail } => {
            eprintln!("needs_review: {detail}");
            eprintln!(
                "evidence and the preserved candidate are under {}",
                run_dir.display()
            );
            Ok(CliOutcome::NeedsReview)
        }
        Terminal::Blocked { code, detail } => {
            eprintln!("blocked ({code}): {detail}");
            Ok(CliOutcome::Blocked)
        }
        Terminal::Failed { detail } => {
            eprintln!("failed: {detail}");
            eprintln!("the preserved candidate is under {}", run_dir.display());
            Ok(CliOutcome::Failed)
        }
        Terminal::BudgetExhausted { detail } => {
            eprintln!("budget_exhausted: {detail}");
            eprintln!(
                "the patch and evidence are preserved under {}",
                run_dir.display()
            );
            Ok(CliOutcome::BudgetExhausted)
        }
        Terminal::Interrupted { detail } => {
            eprintln!("interrupted: {detail}");
            eprintln!("run `relais resume {}` to reconcile", outcome.run_id());
            Ok(CliOutcome::Interrupted)
        }
        Terminal::Cancelled { detail } => {
            eprintln!("cancelled: {detail}");
            eprintln!(
                "the preserved candidate and evidence are under {}",
                run_dir.display()
            );
            Ok(CliOutcome::Cancelled)
        }
    }
}

fn status_command(run_id: Option<&str>) -> Result<CliOutcome, CliError> {
    let ledger = open_ledger()?;
    let run_id = run_id.map(RunId::from_stored);
    match run_id {
        None => {
            let report = operational(
                report::runs_report(&ledger, "2000-01-01T00:00:00+00:00", None),
                "status",
            )?;
            print!("{}", report.render());
            Ok(CliOutcome::Accepted)
        }
        Some(run_id) => {
            let Some(state) = operational(ledger.run_status(&run_id), "status")? else {
                eprintln!("relais status: unknown run {run_id}");
                return Ok(CliOutcome::UnknownRun);
            };
            let cost = operational(ledger.run_cost(&run_id), "status")?;
            let attempts = operational(ledger.attempt_count(&run_id), "status")?;
            println!("{run_id}: {state} (attempts: {attempts}, cost: {cost})");
            Ok(CliOutcome::Accepted)
        }
    }
}

fn explain_command(run_id: &str) -> Result<CliOutcome, CliError> {
    let ledger = open_ledger()?;
    let run = RunId::from_stored(run_id);
    let transitions = operational(ledger.transitions(&run), "explain")?;
    if transitions.is_empty() {
        eprintln!("relais explain: unknown run {run_id}");
        return Ok(CliOutcome::UnknownRun);
    }
    let run_dir = paths::runs_dir().map_err(CliError::Home)?.join(run_id);
    let route_file = run_dir.join("route.txt");
    match std::fs::read_to_string(&route_file) {
        Ok(route) => print!("{route}"),
        // The route note is written per run and is not part of the
        // ledger: an older run, or one that never got that far, simply
        // has none.
        Err(_) => println!("(no route note was recorded for this run)"),
    }
    println!("run: {run_id}");
    println!("artifacts: {}", run_dir.display());
    for transition in &transitions {
        println!(
            "  {} -> {} ({})",
            transition
                .from_state
                .map(|state| state.as_str().to_string())
                .unwrap_or_else(|| "-".into()),
            transition.to_state,
            transition.reason
        );
        if let Some(detail) = transition
            .detail
            .as_ref()
            .and_then(|detail| detail.get("detail"))
            .and_then(|value| value.as_str())
        {
            println!("      {detail}");
        }
    }
    let children = operational(ledger.child_runs(&run), "explain")?;
    if !children.is_empty() {
        println!("work packages (SPEC §19; each a run of its own, costed into this one):");
        for child in &children {
            println!(
                "  {}: {} {} (attempts: {}, cost: {})",
                child.package,
                child.run,
                child.status,
                operational(ledger.attempt_count(&child.run), "explain")?,
                operational(ledger.run_cost(&child.run), "explain")?
            );
        }
    }
    if let Some(decision) = operational(ledger.decision_of_run(&run), "explain")? {
        let resolved = decision
            .resolution
            .zip(decision.resolved_at.as_deref())
            .zip(decision.actor.as_deref());
        if let Some(((resolution, resolved_at), actor)) = resolved {
            println!(
                "decision: {} ({}) -> {} by {actor} at {resolved_at} (waited {}s){}",
                decision.raised_state,
                decision.raised_reason,
                resolution.as_str(),
                decision.waited_seconds,
                decision
                    .note
                    .as_deref()
                    .map(|note| format!(" — {note}"))
                    .unwrap_or_default()
            );
        } else {
            println!(
                "decision: open since {} ({}: {}), waiting {}s",
                decision.raised_at,
                decision.raised_state,
                decision.raised_reason,
                decision.waited_seconds
            );
        }
    }
    let cost = operational(ledger.run_cost(&run), "explain")?;
    let completeness = operational(ledger.run_cost_completeness(&run), "explain")?;
    println!("cost: {}", report::cost_line(cost, completeness));
    if let Some((receipt, _hash)) = operational(ledger.receipt(&run), "explain")? {
        println!(
            "receipt: candidate {} ({} attempt(s), [{}])",
            receipt["candidate_sha"].as_str().unwrap_or("?"),
            receipt["attempts"].as_i64().unwrap_or(0),
            receipt["models_used"]
                .as_array()
                .map(|models| models
                    .iter()
                    .filter_map(|model| model.as_str())
                    .collect::<Vec<_>>()
                    .join(","))
                .unwrap_or_default()
        );
    }
    let evidence = operational(ledger.evidence(&run), "explain")?;
    if !evidence.is_empty() {
        println!("evidence:");
        for row in &evidence {
            // Read through the typed row, not the positional tuple this
            // replaced — a caller here cannot print the kind where it
            // meant to print the path.
            print!("  {}: {}", row.kind, row.path);
            if let Some(tool) = &row.tool {
                print!(" via {tool}");
            }
            if let Some(criterion_id) = &row.criterion_id {
                print!(" for {criterion_id}");
            }
            println!();
        }
    }
    Ok(CliOutcome::Accepted)
}

/// Whether `resume` also retires the worktree once the run is terminal
/// and its dispatches provably dead. An enum, so the handler reads as
/// what was asked rather than as a bare flag (the clap edge is where the
/// `bool` lives).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Retire {
    Keep,
    Retire,
}

impl Retire {
    fn from_flag(retire: bool) -> Self {
        if retire {
            Self::Retire
        } else {
            Self::Keep
        }
    }
}

fn resume_command(run_id: &str, retire: Retire) -> Result<CliOutcome, CliError> {
    let ledger = open_ledger()?;
    let run = RunId::from_stored(run_id);
    let Some(state) = operational(ledger.run_status(&run), "resume")? else {
        eprintln!("relais resume: unknown run {run_id}");
        return Ok(CliOutcome::UnknownRun);
    };
    // An absent terminal result never means nothing executed (SPEC §12).
    // The policy is `resume::reconcile`, a pure function with a test per
    // row; this handler does the ledger write and the printing.
    let reconciliation = reconcile_run(&ledger, &run)?;
    if state.is_terminal() {
        println!("{run_id} is already terminal: {state}");
        return match retire {
            Retire::Keep => Ok(CliOutcome::Accepted),
            Retire::Retire => retire_run(&ledger, &run, state, &reconciliation),
        };
    }
    if !reconciliation.may_reconcile() {
        println!(
            "{run_id} is {state} with worker(s) still live: {}. Resume does not re-dispatch \
             while a worker may be running; `relais coordinator cancel --run {run_id}` stops \
             it, `relais explain {run_id}` shows the evidence",
            reconciliation.refusal()
        );
        return Ok(CliOutcome::ResumeRefusedLive);
    }
    for dispatch in reconciliation.provably_dead() {
        operational(
            ledger.finish_dispatch(dispatch, "reconciled_dead"),
            "resume",
        )?;
    }
    let detail = reconciliation.detail();
    operational(
        ledger.record_transition(&relais::ledger::Transition {
            run_id: run.clone(),
            attempt_id: None,
            from_state: Some(state),
            to_state: State::Interrupted,
            reason: Reason::ReconciledInterrupted.as_str().to_string(),
            detail: Some(serde_json::json!({ "detail": detail })),
            at: relais::ledger::now_rfc3339(),
        }),
        "resume",
    )?;
    println!(
        "{run_id}: {state} -> interrupted ({detail}). Changes are preserved under {}; nothing \
         was replayed. Start a NEW run with a revised contract if the task is still wanted",
        paths::runs_dir()
            .map_err(CliError::Home)?
            .join(run_id)
            .display()
    );
    match retire {
        Retire::Keep => {}
        // The worktree goes only once every dispatch that was live is
        // provably dead: an unknown outcome may still be writing it.
        Retire::Retire => {
            retire_run(&ledger, &run, State::Interrupted, &reconciliation)?;
        }
    }
    Ok(CliOutcome::ResumeReconciled)
}

/// Judge every dispatch the ledger still records as live for one run.
/// No coordinator answering is not evidence about the run, only the
/// absence of evidence: `reconcile` treats it as such.
fn reconcile_run(ledger: &Ledger, run: &RunId) -> Result<resume::Reconciliation, CliError> {
    let live: Vec<_> = operational(ledger.live_dispatches(), "resume")?
        .into_iter()
        .filter(|live| live.run == *run)
        .collect();
    let coordinator_view = relais::coordinator::socket_path()
        .ok()
        .map(relais::coordinator::Client::new)
        .and_then(|client| client.status().ok())
        .and_then(|snapshot| snapshot.runs.get(run.as_str()).cloned());
    Ok(resume::reconcile(
        &live,
        coordinator_view.as_ref(),
        &|pid| relais::coordinator::process_alive(pid.get()),
    ))
}

/// `relais resume --retire` with no run named: every terminal run that
/// still holds a worktree under the state directory — the current
/// `worktrees/<run>/…` layout and the legacy `runs/<run>/worktree` —
/// is retired, one line each. A run that is not terminal, not on
/// record, or whose dispatches are not provably dead is named and
/// kept. A retirement that fails is relais's own failure to report.
fn retire_all_command() -> Result<CliOutcome, CliError> {
    let ledger = open_ledger()?;
    let state_dir = paths::state_dir().map_err(CliError::Home)?;
    let retained = operational(
        workspace::retained_worktrees(&state_dir),
        "listing the retained worktrees",
    )?;
    if retained.is_empty() {
        println!("no run worktree is retained under {}", state_dir.display());
        return Ok(CliOutcome::Accepted);
    }
    let mut failed = false;
    for worktree in retained {
        let run = RunId::from_stored(worktree.run_id.clone());
        let Some(state) = operational(ledger.run_status(&run), "resume")? else {
            println!(
                "{}: kept, no such run on record ({})",
                worktree.run_id,
                worktree.path.display()
            );
            continue;
        };
        if !state.is_terminal() {
            println!(
                "{}: kept, the run is {state} ({})",
                worktree.run_id,
                worktree.path.display()
            );
            continue;
        }
        let reconciliation = reconcile_run(&ledger, &run)?;
        match retire_worktree(&ledger, &run, state, &reconciliation, &worktree.path)? {
            Retired::Retired => {}
            Retired::Kept => failed = true,
        }
    }
    Ok(if failed {
        CliOutcome::OperationalFailure
    } else {
        CliOutcome::Accepted
    })
}

/// Retire every worktree one terminal run still holds.
fn retire_run(
    ledger: &Ledger,
    run: &RunId,
    state: State,
    reconciliation: &resume::Reconciliation,
) -> Result<CliOutcome, CliError> {
    let state_dir = paths::state_dir().map_err(CliError::Home)?;
    let retained: Vec<_> = operational(
        workspace::retained_worktrees(&state_dir),
        "listing the retained worktrees",
    )?
    .into_iter()
    .filter(|worktree| worktree.run_id == run.as_str())
    .collect();
    if retained.is_empty() {
        println!("{run}: no worktree is retained");
        return Ok(CliOutcome::Accepted);
    }
    let mut failed = false;
    for worktree in retained {
        match retire_worktree(ledger, run, state, reconciliation, &worktree.path)? {
            Retired::Retired => {}
            Retired::Kept => failed = true,
        }
    }
    Ok(if failed {
        CliOutcome::OperationalFailure
    } else {
        CliOutcome::Accepted
    })
}

/// What one retirement attempt did, for the exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Retired {
    Retired,
    /// Not removed: a dispatch may still write it, the ledger lacks what
    /// the retirement needs, or the retirement itself failed (recorded
    /// as `worktree_not_released`).
    Kept,
}

/// One worktree of a terminal run: retired once its dispatches are
/// provably dead, with the transition the runner would have recorded,
/// and one line printed either way.
fn retire_worktree(
    ledger: &Ledger,
    run: &RunId,
    state: State,
    reconciliation: &resume::Reconciliation,
    path: &Path,
) -> Result<Retired, CliError> {
    if !reconciliation.all_provably_dead() {
        println!(
            "{run}: kept, a dispatch may still write it: {} ({})",
            reconciliation.refusal(),
            path.display()
        );
        return Ok(Retired::Kept);
    }
    let Some(workspace) = operational(ledger.run_workspace(run), "resume")? else {
        println!("{run}: kept, the run is not on record ({})", path.display());
        return Ok(Retired::Kept);
    };
    let Some(base_sha) = workspace.base_sha else {
        println!(
            "{run}: kept, the run never resolved a base to name its tree against ({})",
            path.display()
        );
        return Ok(Retired::Kept);
    };
    let worktree = workspace::TaskWorktree {
        path: path.to_path_buf(),
        base_sha,
    };
    let artifacts = paths::runs_dir()
        .map_err(CliError::Home)?
        .join(run.as_str());
    let (reason, detail, retired) = match workspace::retire(&worktree, run.as_str(), &artifacts) {
        Ok(retirement) => {
            match &retirement.exported {
                Some(exported) => println!(
                    "{run}: retired {} — {} written, patch {}, {} bytes reclaimed",
                    path.display(),
                    exported.reference,
                    exported.patch_path.display(),
                    retirement.bytes_reclaimed
                ),
                None => println!(
                    "{run}: retired {} — its tree was already a named candidate, {} bytes \
                     reclaimed",
                    path.display(),
                    retirement.bytes_reclaimed
                ),
            }
            (
                Reason::WorktreeRetired,
                serde_json::json!({
                    "worktree": path.to_string_lossy(),
                    "reference": retirement.exported.as_ref().map(|e| e.reference.clone()),
                    "patch": retirement
                        .exported
                        .as_ref()
                        .map(|e| e.patch_path.to_string_lossy().into_owned()),
                    "bytes_reclaimed": retirement.bytes_reclaimed,
                    "by": "resume --retire",
                }),
                Retired::Retired,
            )
        }
        Err(e) => {
            eprintln!(
                "relais resume: {run}: kept, {} could not be retired: {e}",
                path.display()
            );
            (
                Reason::WorktreeNotReleased,
                serde_json::json!({
                    "worktree": path.to_string_lossy(),
                    "error": e.to_string(),
                    "by": "resume --retire",
                }),
                Retired::Kept,
            )
        }
    };
    operational(
        ledger.record_transition(&relais::ledger::Transition {
            run_id: run.clone(),
            attempt_id: None,
            from_state: Some(state),
            to_state: state,
            reason: reason.as_str().to_string(),
            detail: Some(detail),
            at: relais::ledger::now_rfc3339(),
        }),
        "resume",
    )?;
    Ok(retired)
}

fn report_command(
    since: Option<&str>,
    json: bool,
    by: Option<report::Dimension>,
) -> Result<CliOutcome, CliError> {
    let since = since.map(|since| since.to_string()).unwrap_or_else(|| {
        chrono::Utc::now()
            .format("%Y-%m-01T00:00:00+00:00")
            .to_string()
    });
    let ledger = open_ledger()?;
    let report = operational(report::runs_report(&ledger, &since, by), "report")?;
    if json {
        print_document(&report)?;
    } else {
        print!("{}", report.render());
    }
    Ok(CliOutcome::Accepted)
}

/// The run a `relais feedback` invocation is about: the one named
/// directly, or the task's accepted run when `--task` is given instead.
/// Exactly one of `run_id`/`task` is `Some` — clap's `conflicts_with` and
/// `required_unless_present` on the `Feedback` variant guarantee it.
fn feedback_run(
    ledger: &Ledger,
    run_id: Option<&str>,
    task: Option<&str>,
) -> Result<std::result::Result<RunId, CliOutcome>, CliError> {
    if let Some(run_id) = run_id {
        return Ok(Ok(RunId::from_stored(run_id)));
    }
    let task_id = TaskId::from_stored(task.expect("clap requires run_id or --task"));
    let runs = operational(ledger.runs_of_task(&task_id), "feedback")?;
    for candidate in runs.into_iter().rev() {
        if operational(ledger.run_status(&candidate), "feedback")? == Some(State::Accepted) {
            return Ok(Ok(candidate));
        }
    }
    eprintln!("relais feedback: task {task_id} has no accepted run on record");
    Ok(Err(CliOutcome::UnknownRun))
}

/// One `relais feedback` invocation's arguments, bundled so the handler
/// takes one value rather than seven positional ones.
struct FeedbackRequest {
    run_id: Option<String>,
    task: Option<String>,
    outcome: FeedbackOutcome,
    candidate: Option<String>,
    magnitude: Option<f64>,
    evidence: Vec<String>,
    actor: String,
}

fn feedback_command(request: FeedbackRequest) -> Result<CliOutcome, CliError> {
    let FeedbackRequest {
        run_id,
        task,
        outcome,
        candidate,
        magnitude,
        evidence,
        actor,
    } = request;
    let ledger = open_ledger()?;
    let run = match feedback_run(&ledger, run_id.as_deref(), task.as_deref())? {
        Ok(run) => run,
        Err(outcome) => return Ok(outcome),
    };
    let state = operational(ledger.run_status(&run), "feedback")?;
    let Some(state) = state else {
        eprintln!("relais feedback: unknown run {run}");
        return Ok(CliOutcome::UnknownRun);
    };
    // SPEC §20: feedback is attributed to the candidate; absence of
    // feedback is never a positive label. Only accepted runs have a
    // candidate whose later life is worth recording.
    if state != State::Accepted {
        eprintln!(
            "relais feedback: run {run} is {state}, not accepted — final outcome feedback \
             records what happened to an ACCEPTED change"
        );
        return Ok(CliOutcome::InvalidInput);
    }
    let Some(task_id) = operational(ledger.task_of_run(&run), "feedback")? else {
        eprintln!("relais feedback: run {run} carries no task identity");
        return Ok(CliOutcome::OperationalFailure);
    };
    // A run accepted through a person's approval (`relais decide --answer
    // approve`) on a contract interrupted before verification never wrote
    // a receipt — SPEC's decision spine reaches `accepted` without one.
    // Feedback about it is still worth recording; whenever a receipt DOES
    // exist, it is still the source of truth a `--candidate` is checked
    // against (SPEC §20: feedback is attributed to the candidate).
    let receipt = operational(ledger.receipt(&run), "feedback")?;
    let recorded_candidate = match receipt {
        Some((receipt, _hash)) => {
            let Some(sha) = receipt["candidate_sha"].as_str() else {
                eprintln!("relais feedback: run {run}'s receipt names no candidate");
                return Ok(CliOutcome::OperationalFailure);
            };
            Some(sha.to_string())
        }
        // No receipt, but the run may still have finished an attempt and
        // named what it built. That is the ledger's own answer, and it
        // stands in for the receipt here exactly as the receipt would.
        None => operational(ledger.latest_attempt_candidate(&run), "feedback")?,
    };
    match (&recorded_candidate, &candidate) {
        (Some(recorded), Some(candidate)) if candidate != recorded => {
            eprintln!(
                "relais feedback: --candidate {candidate} does not match run {run}'s accepted \
                 candidate {recorded}"
            );
            return Ok(CliOutcome::InvalidInput);
        }
        // Nothing in the ledger says what this run built, so nothing can
        // check the caller's claim. Storing it anyway would put an
        // unverifiable sha in the outcomes table as the candidate this
        // outcome is about (SPEC §20: the receipt, not the caller, is the
        // source of truth). The outcome itself is still recordable —
        // without a candidate it cannot vouch for.
        (None, Some(candidate)) => {
            eprintln!(
                "relais feedback: run {run} has no recorded candidate to check --candidate \
                 {candidate} against — record the outcome without it, or name the run that \
                 produced the candidate"
            );
            return Ok(CliOutcome::InvalidInput);
        }
        (Some(_), Some(_)) | (Some(_), None) | (None, None) => {}
    }
    let candidate_sha = recorded_candidate;
    // The strategy actually used is read off the ledger, never asked of
    // the caller (SPEC §20).
    let Some((_contract, tier)) = operational(ledger.run_contract_and_tier(&run), "feedback")?
    else {
        eprintln!("relais feedback: run {run} carries no recorded attempt");
        return Ok(CliOutcome::OperationalFailure);
    };
    let escalated = operational(ledger.escalation_attempted(&run), "feedback")?;
    let models = operational(ledger.models_used(&run), "feedback")?;
    let magnitude = match magnitude {
        Some(value) => match relais::outcome::CorrectionMagnitude::new(value) {
            Ok(magnitude) => Some(magnitude),
            Err(e) => {
                eprintln!("relais feedback: {e}");
                return Ok(CliOutcome::InvalidInput);
            }
        },
        None => None,
    };
    let kind = outcome.into();
    let detail = relais::outcome::OutcomeDetail {
        candidate_sha,
        strategy: relais::outcome::Strategy {
            tier,
            models,
            escalated,
        },
        correction_magnitude: magnitude,
        evidence,
        actor,
    };
    let recorded = match relais::outcome::Outcome::new(kind, detail) {
        Ok(outcome) => outcome,
        Err(e) => {
            eprintln!("relais feedback: {e}");
            return Ok(CliOutcome::InvalidInput);
        }
    };
    operational(ledger.record_outcome(&run, &task_id, &recorded), "feedback")?;
    println!("recorded {} for {run}", kind.as_str());
    Ok(CliOutcome::Accepted)
}

/// The verification gaps named by the transition that raised a run's
/// still-open wait, if any: the detail `Observation::VerificationGap`
/// records (SPEC's decision spine — `decide --answer approve` is refused
/// while these are non-empty, because a waiver is a policy or contract
/// change, never a CLI flag).
fn open_verification_gaps(ledger: &Ledger, run: &RunId) -> Result<Vec<String>, CliError> {
    // Read the transition that RAISED the open decision, found by the
    // row's own `raised_at`, not the last transition landing on an
    // awaiting state. A run keeps transitioning after it starts waiting:
    // the runner retires its worktree and records that as a same-state
    // row whose detail carries a reference and a patch and no gaps. The
    // newest awaiting transition is therefore usually that retirement,
    // and reading it would find no gaps and wave through exactly the run
    // this refusal exists for.
    let Some(decision) = operational(ledger.decision_of_run(run), "decide")? else {
        return Ok(Vec::new());
    };
    let transitions = operational(ledger.transitions(run), "decide")?;
    let raised: Vec<String> = transitions
        .iter()
        .find(|transition| {
            transition.at == decision.raised_at && transition.to_state.awaits_a_person()
        })
        .and_then(|transition| transition.detail.as_ref())
        .and_then(|detail| detail.get("gaps"))
        .and_then(|gaps| gaps.as_array())
        .map(|gaps| {
            gaps.iter()
                .filter_map(|gap| gap.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    // A human-sign-off gap this run's own transition still names is not
    // open any more once `relais decide --answer approve --criterion
    // <id>` has recorded that id's sign-off — the frozen detail never
    // changes, so what clears the gap is read fresh, here, rather than
    // by rewriting history. The id is taken out of the stored sentence
    // by `verify::sign_off_gap_criterion`, the inverse of the one
    // function that wrote it; matching the id anywhere in the prose
    // would also clear a CHECK gap that happened to name it.
    let signed_off: Vec<String> = operational(ledger.human_signoffs(run), "decide")?
        .into_iter()
        .map(|(criterion_id, _actor)| criterion_id)
        .collect();
    Ok(raised
        .into_iter()
        .filter(|gap| match relais::verify::sign_off_gap_criterion(gap) {
            Some(criterion_id) => !signed_off.iter().any(|signed| signed == criterion_id),
            None => true,
        })
        .collect())
}

fn decide_command(
    run_id: &str,
    answer: DecideAnswer,
    actor: &str,
    note: Option<&str>,
    successor: Option<&str>,
    criterion: Option<&str>,
) -> Result<CliOutcome, CliError> {
    let ledger = open_ledger()?;
    let run = RunId::from_stored(run_id);
    let Some(state) = operational(ledger.run_status(&run), "decide")? else {
        eprintln!("relais decide: unknown run {run_id}");
        return Ok(CliOutcome::UnknownRun);
    };
    if !state.awaits_a_person() {
        eprintln!(
            "relais decide: run {run_id} is {state}, not waiting on a person — nothing to decide"
        );
        return Ok(CliOutcome::InvalidInput);
    }
    if let Some(criterion_id) = criterion {
        if answer != DecideAnswer::Approve {
            eprintln!("relais decide: `--criterion` only applies to `--answer approve`");
            return Ok(CliOutcome::InvalidInput);
        }
        let Some((contract, _tier)) = operational(ledger.run_contract_and_tier(&run), "decide")?
        else {
            eprintln!("relais decide: run {run_id} carries no recorded contract");
            return Ok(CliOutcome::OperationalFailure);
        };
        let declared: Vec<String> = contract.acceptance.iter().map(|entry| entry.id()).collect();
        let Some(entry) = contract
            .acceptance
            .iter()
            .find(|entry| entry.id() == criterion_id)
        else {
            eprintln!(
                "relais decide: run {run_id} names no acceptance criterion `{criterion_id}` — \
                 this contract declares: {}",
                declared.join(", ")
            );
            return Ok(CliOutcome::InvalidInput);
        };
        // Signing off a criterion the contract settles some OTHER way is
        // the second acceptance path this whole mechanism exists to
        // refuse: it would re-seal the receipt claiming a person settled
        // what a check was declared to settle. The gap itself would
        // survive — `acceptance_gaps` consults the sign-off set only for
        // `HumanSignOff` evidence — but the receipt would say something
        // untrue about how the criterion was met, and a receipt nobody
        // can trust is worse than a run that stays blocked.
        if !matches!(entry.evidence(), Some(Evidence::HumanSignOff)) {
            eprintln!(
                "relais decide: acceptance criterion `{criterion_id}` is not settled by a human \
                 sign-off but by {} — a sign-off answers only a criterion that asked for one",
                match entry.evidence() {
                    Some(Evidence::Check { name }) => format!("check `{name}`"),
                    Some(Evidence::Test { authorship }) =>
                        format!("a test ({})", authorship_label(*authorship)),
                    Some(Evidence::LlmReview) => "an LLM review".to_string(),
                    Some(Evidence::AmontGate { gate }) => format!("amont gate `{gate}`"),
                    Some(Evidence::HumanSignOff) => unreachable!("guarded above"),
                    None => "the verification profile as a whole".to_string(),
                }
            );
            return Ok(CliOutcome::InvalidInput);
        }
        operational(
            ledger.record_human_signoff(&run, criterion_id, actor, note),
            "decide",
        )?;
        reseal_receipt_with_signoff(&ledger, &run, criterion_id)?;
    }
    if answer == DecideAnswer::Approve {
        let gaps = open_verification_gaps(&ledger, &run)?;
        if !gaps.is_empty() {
            eprintln!(
                "relais decide: run {run_id} still carries verification gaps ({}) — a waiver is \
                 a policy or contract change, never a CLI flag",
                gaps.join("; ")
            );
            return Ok(CliOutcome::InvalidInput);
        }
    }
    let successor_run = successor.map(RunId::from_stored);
    let resolution = answer.resolution();
    let resolved = operational(
        ledger.resolve_decision(
            &run,
            &relais::ledger::DecisionAnswer {
                resolution,
                actor,
                note,
                successor_run: successor_run.as_ref(),
                from_state: state,
                to_state: answer.terminal_state(),
            },
        ),
        "decide",
    )?;
    if !resolved {
        eprintln!("relais decide: run {run_id} has no open decision on record");
        return Ok(CliOutcome::InvalidInput);
    }
    println!("recorded {} for {run_id} by {actor}", resolution.as_str());
    Ok(CliOutcome::Accepted)
}

/// Record one piece of evidence produced by another tool, against a run
/// and, optionally, the acceptance criterion it answers. This decides
/// nothing: it never marks a criterion met, changes a run's state, or
/// clears a gap — only `relais decide` and the runner's own verification
/// do that.
fn evidence_attach_command(
    run_id: &str,
    path: &Path,
    tool: Option<&str>,
    external_id: Option<&str>,
    subject: Option<&str>,
    criterion: Option<&str>,
) -> Result<CliOutcome, CliError> {
    let ledger = open_ledger()?;
    let run = RunId::from_stored(run_id);
    if operational(ledger.run_status(&run), "evidence attach")?.is_none() {
        eprintln!("relais evidence attach: unknown run {run_id}");
        return Ok(CliOutcome::UnknownRun);
    }
    if let Some(criterion_id) = criterion {
        let Some((contract, _tier)) =
            operational(ledger.run_contract_and_tier(&run), "evidence attach")?
        else {
            eprintln!("relais evidence attach: run {run_id} carries no recorded contract");
            return Ok(CliOutcome::OperationalFailure);
        };
        let declared: Vec<String> = contract.acceptance.iter().map(|entry| entry.id()).collect();
        if !declared.iter().any(|id| id == criterion_id) {
            eprintln!(
                "relais evidence attach: run {run_id} names no acceptance criterion \
                 `{criterion_id}` — this contract declares: {}",
                declared.join(", ")
            );
            return Ok(CliOutcome::InvalidInput);
        }
    }
    let sha256 = match workspace::sha256_file(path) {
        Ok(sha256) => sha256,
        Err(cause) => {
            eprintln!(
                "relais evidence attach: {path} cannot be read: {cause}",
                path = path.display()
            );
            return Ok(CliOutcome::InvalidInput);
        }
    };
    // Every other writer of an evidence row stores an absolute path —
    // the runner joins its artifacts directory, `decide` joins the runs
    // directory — and `relais explain` prints them all side by side. A
    // relative `--path` recorded verbatim reads back as a path that
    // resolves from whatever directory the person happened to be in,
    // which is no directory at all by the time anyone looks. The sha256
    // pins the content; this pins where it was.
    let path = match path.canonicalize() {
        Ok(absolute) => absolute,
        Err(cause) => {
            eprintln!(
                "relais evidence attach: {path} cannot be resolved to an absolute path: {cause}",
                path = path.display()
            );
            return Ok(CliOutcome::InvalidInput);
        }
    };
    let path = path.as_path();
    operational(
        ledger.attach_evidence(
            &run,
            path,
            Some(&sha256),
            EvidenceOrigin {
                tool,
                external_id,
                subject,
                criterion_id: criterion,
            },
        ),
        "evidence attach",
    )?;
    println!("attached evidence for {run_id}: {}", path.display());
    Ok(CliOutcome::Accepted)
}

/// How a test criterion's authorship reads in a refusal.
fn authorship_label(authorship: relais::acceptance::TestAuthorship) -> &'static str {
    use relais::acceptance::TestAuthorship;
    match authorship {
        TestAuthorship::PreExisting => "pre-existing",
        TestAuthorship::HumanAdded => "human-added",
        TestAuthorship::ModelAdded => "model-added",
    }
}

/// Mark one criterion met by the sign-off `relais decide` just recorded,
/// and re-seal the run's receipt with it (SPEC §10, §12): the runner
/// already stored a receipt for this run when every check, test and
/// review passed and only a human sign-off gap remained
/// ([`relais::runner::RunEngine::store_pending_receipt`]) — this is the
/// one place that receipt is ever edited, never a second acceptance path
/// built alongside it. A run with no stored receipt yet (an older run,
/// or one whose gaps were never signoff-only) has nothing to re-seal;
/// `relais decide`'s own gap check further down still refuses it if
/// other gaps remain.
fn reseal_receipt_with_signoff(
    ledger: &Ledger,
    run: &RunId,
    criterion_id: &str,
) -> Result<(), CliError> {
    let Some((stored, _hash)) = operational(ledger.receipt(run), "decide")? else {
        return Ok(());
    };
    // A receipt this same binary wrote and now cannot parse back is
    // corrupt, not absent — `operational` refuses loudly rather than
    // this re-seal silently skipping it.
    let mut receipt: Receipt = operational(serde_json::from_value(stored), "decide")?;
    for criterion in &mut receipt.criteria {
        if criterion.id == criterion_id {
            criterion.met = true;
            // The criterion ALREADY declared a human sign-off — `decide`
            // refuses `--criterion` for any other evidence kind — so this
            // records which evidence settled it, never a rewrite of what
            // the contract asked for.
            criterion.evidence = Some(Evidence::HumanSignOff);
        }
    }
    receipt.mandatory_evidence_independence = independence_summary(&receipt.criteria);
    // The gap this sign-off answers is no longer open, so the receipt
    // stops naming it — and once no gap is left, the receipt says what
    // the run now is. A receipt still reading `needs_decision`, still
    // listing a gap a person has since answered, beside a run the very
    // same command moved to `accepted`, is two records of one fact
    // disagreeing: exactly the drift `resolve_decision`'s single
    // transaction exists to prevent one line further down.
    receipt
        .verification
        .gaps
        .retain(|gap| relais::verify::sign_off_gap_criterion(gap) != Some(criterion_id));
    if receipt.verification.accepted() {
        receipt.outcome = State::Accepted.as_str().to_string();
    }
    let receipt_hash = receipt.hash();
    operational(
        ledger.store_receipt(
            run,
            &serde_json::to_value(&receipt).expect("a receipt serializes"),
            &receipt_hash,
        ),
        "decide",
    )?;
    // The ledger row and the file are one receipt. Updating only the row
    // would leave `receipt.json` — the copy a person actually opens, and
    // the one the evidence row's hash names — reading `needs_decision`
    // for a run this command just accepted.
    let receipt_path = paths::runs_dir()
        .map_err(CliError::Home)?
        .join(run.as_str())
        .join("receipt.json");
    if receipt_path.exists() {
        operational(
            std::fs::write(
                &receipt_path,
                serde_json::to_string_pretty(&receipt).expect("a receipt serializes"),
            ),
            "decide",
        )?;
        operational(
            ledger.record_evidence(
                run,
                None,
                EvidenceKind::Receipt,
                &receipt_path,
                Some(&receipt_hash),
            ),
            "decide",
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use relais::ledger::{Ledger, Transition};

    fn temp_ledger(label: &str) -> (Ledger, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "relais-main-test-{label}-{}-{}",
            std::process::id(),
            relais::ledger::now_rfc3339().replace([':', '.', '+'], "-")
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger opens");
        (ledger, dir)
    }

    /// Walks every `DecideAnswer` and asserts the terminal state it
    /// assigns: `approve` accepts, every other answer cancels. Matched
    /// directly over `DecideAnswer` in `terminal_state`, so a sixth
    /// variant added to that enum without a case there fails to compile
    /// this test along with everything else in the crate.
    #[test]
    fn decide_answers_every_variant_assigns_a_projected_status() {
        for answer in [
            DecideAnswer::Approve,
            DecideAnswer::Reject,
            DecideAnswer::Revise,
            DecideAnswer::Decided,
            DecideAnswer::Abandon,
        ] {
            let expected = if answer == DecideAnswer::Approve {
                State::Accepted
            } else {
                State::Cancelled
            };
            assert_eq!(answer.terminal_state(), expected, "{answer:?}");
        }
    }

    /// `decide --answer approve` reads the gaps a `VerificationGap`
    /// transition recorded, not a fresh verification run: the run is
    /// already terminal, and the detail its OWN transition into
    /// `needs_decision` carried is the only record of what the gaps were.
    #[test]
    fn open_verification_gaps_reads_the_transition_that_raised_the_decision() {
        let (ledger, dir) = temp_ledger("gaps");
        let run = RunId::from_stored("run-gaps");
        let task = relais::ids::TaskId::from_stored("task-gaps");
        ledger
            .insert_run(&run, "/repo", None, &task, "rk")
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: run.clone(),
                attempt_id: None,
                from_state: Some(State::Verifying),
                to_state: State::NeedsDecision,
                reason: Reason::VerificationGap.as_str().into(),
                detail: Some(serde_json::json!({ "gaps": ["declared-check: status `inert`"] })),
                at: relais::ledger::now_rfc3339(),
            })
            .expect("transition");
        // What every real run does next: the runner retires the worktree
        // and records it as a SAME-STATE transition whose detail carries
        // a reference and a patch and no gaps. Reading the newest
        // awaiting transition would find that row and wave the run
        // through — the bug this test exists to keep out.
        ledger
            .record_transition(&Transition {
                run_id: run.clone(),
                attempt_id: None,
                from_state: Some(State::NeedsDecision),
                to_state: State::NeedsDecision,
                reason: Reason::WorktreeRetired.as_str().into(),
                detail: Some(serde_json::json!({
                    "worktree": "/state/worktrees/run-gaps/task",
                    "reference": "refs/relais/candidates/run-gaps/final",
                })),
                at: relais::ledger::now_rfc3339(),
            })
            .expect("retirement transition");
        let gaps = open_verification_gaps(&ledger, &run).expect("gaps read");
        assert_eq!(
            gaps,
            vec!["declared-check: status `inert`".to_string()],
            "the raising transition's gaps survive a later same-state row"
        );
        // Best effort: the fixture is a temp dir; a leftover costs
        // nothing but disk, and the next run pre-cleans it.
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A decision raised for a reason that carries no `gaps` detail —
    /// `scope_exceeded`, say — names none: `approve` is only refused when
    /// the run's OWN wait actually carries verification gaps.
    #[test]
    fn open_verification_gaps_is_empty_when_the_raising_transition_names_none() {
        let (ledger, dir) = temp_ledger("nogaps");
        let run = RunId::from_stored("run-nogaps");
        let task = relais::ids::TaskId::from_stored("task-nogaps");
        ledger
            .insert_run(&run, "/repo", None, &task, "rk")
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: run.clone(),
                attempt_id: None,
                from_state: Some(State::Verifying),
                to_state: State::NeedsDecision,
                reason: Reason::ScopeExceeded.as_str().into(),
                detail: Some(serde_json::json!({ "paths": ["docs/x.md"] })),
                at: relais::ledger::now_rfc3339(),
            })
            .expect("transition");
        let gaps = open_verification_gaps(&ledger, &run).expect("gaps read");
        assert!(gaps.is_empty(), "{gaps:?}");
        // Best effort: the fixture is a temp dir; a leftover costs
        // nothing but disk, and the next run pre-cleans it.
        std::fs::remove_dir_all(&dir).ok();
    }

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
    ];

    #[test]
    fn the_exit_code_table_is_total_and_injective() {
        let mut seen: Vec<i32> = ALL.iter().map(exit_code).collect();
        assert_eq!(seen.len(), 17, "every documented outcome is listed");
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

    /// Under `--json` the coordinator's state is a document in every
    /// case, including "nothing answered" — which used to be an English
    /// sentence on stdout with the client's error discarded (C3).
    #[test]
    fn coordinator_status_json_is_a_document_in_every_state() {
        let absent = CoordinatorStatusDocument::Absent {
            socket: "/run/relais.sock".into(),
            cause: "coordinator: endpoint /run/relais.sock: Connection refused".into(),
        };
        let value: serde_json::Value =
            serde_json::to_value(&absent).expect("the document serializes");
        assert_eq!(value["coordinator"], "absent");
        assert_eq!(value["socket"], "/run/relais.sock");
        assert!(
            value["cause"]
                .as_str()
                .expect("a cause")
                .contains("Connection refused"),
            "the client's error is carried, not discarded: {value}"
        );

        let serving = CoordinatorStatusDocument::Serving {
            socket: "/run/relais.sock".into(),
            snapshot: Box::new(relais::admission::StatusSnapshot::default()),
        };
        let value: serde_json::Value = serde_json::to_value(&serving).expect("serializes");
        assert_eq!(value["coordinator"], "serving");
        assert!(value["snapshot"].is_object(), "{value}");

        let skew = CoordinatorStatusDocument::VersionSkew {
            socket: "/run/relais.sock".into(),
            cause: "protocol 2 vs 3".into(),
            ours: 2,
            theirs: 3,
            daemon_version: "0.1.6".into(),
        };
        let value: serde_json::Value = serde_json::to_value(&skew).expect("serializes");
        assert_eq!(value["coordinator"], "version_skew");
        assert_eq!(value["ours"], 2);
        assert_eq!(value["theirs"], 3);
    }
}
