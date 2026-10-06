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
//! | 17 | `PromotionRefused` | `recipe promote` refused: the comparison does not clear every gate; nothing was written |
//!
//! README.md carries the same table for people who do not read source.

use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;

use relais::acceptance::Evidence;
use relais::backend::Backend;
use relais::contract::TaskContract;
use relais::ids::{IdSource, RunId, TaskId};
use relais::install::{InstallReport, InstallRequest, Mode, Scope};
use relais::learn::comparison::{
    evaluate_candidate, DEFAULT_BOOTSTRAP_RESAMPLES, MIN_PAIRED_TASKS,
};
use relais::learn::predict::RegistryPredictor;
use relais::learn::promote::{admit, Amendment};
use relais::ledger::OrchestrationUsageRow;
use relais::lifecycle::RunPurpose;
use relais::money::{CostCompleteness, MicroUsd};
use relais::orchestration::{self, PriceTable, TranscriptSource};
use relais::policy::{
    effective_authority, grant_key, EffectiveAuthority, HookAdmissionSettings, MachineSettings,
    RecipeSpec, RepoIdentity, RepoPolicy,
};
use relais::runner::{execute, live_trial, worktree_root, Reason, RunConfig, State, Terminal};
use relais::verify::{independence_summary, Receipt, SettledVia};
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
        #[arg(
            long = "probe-hooks",
            conflicts_with_all = ["effort_template", "verify_sandbox"]
        )]
        probe_hooks: bool,
        /// Verify the worker OS sandbox with two real `claude -p` probe
        /// sessions (a sandboxed worker's launch, and the allowlist
        /// launch), and record the pass under the configuration it was
        /// measured on; a sandboxed worker runs only on a recorded one.
        /// Costs money (a few cents) and touches the network; never part
        /// of `make check`.
        #[arg(
            long = "verify-sandbox",
            conflicts_with_all = ["probe_hooks", "effort_template"]
        )]
        verify_sandbox: bool,
        /// Print the machine.toml block that states the effort facts
        /// `doctor` reports unknown: the CLI-accepted list pre-filled from
        /// `--help`, the model-support and order lines left for you to
        /// confirm.
        #[arg(
            long = "effort-template",
            conflicts_with_all = ["json", "verify_sandbox"]
        )]
        effort_template: bool,
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
        /// Print the decision as one JSON object, the route with its rung
        #[arg(long)]
        json: bool,
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
        /// Launch each worker attempt as a native subagent the parent
        /// Claude Code session spawns, so Claude Code renders it (SPEC §23).
        /// Needs a parent Claude Code session
        #[arg(long)]
        native: bool,
        /// How long to wait for the parent session to spawn a requested
        /// native worker before the attempt ends `native_spawn_missing`
        #[arg(
            long = "native-spawn-wait",
            value_name = "SECONDS",
            requires = "native"
        )]
        native_spawn_wait: Option<u64>,
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
        /// Skip importing orchestration usage before rendering: the
        /// figure printed is whatever a previous `relais usage import`
        /// already put in the ledger, not a fresh pass over transcripts
        #[arg(long = "no-import")]
        no_import: bool,
    },
    /// The orchestrating Claude Code session's own usage (SPEC §11)
    Usage {
        #[command(subcommand)]
        cmd: UsageCommand,
    },
    /// Owned-dataset and learned-routing operations (SPEC §17)
    Dataset {
        #[command(subcommand)]
        cmd: DatasetCommand,
    },
    /// Inspect, evaluate, promote and roll back recipe revisions (SPEC
    /// §25-27). Only `promote --write` and `rollback --write` change a
    /// file, by appending to relais.toml; nothing here grants trust
    Recipe {
        #[command(subcommand)]
        cmd: RecipeCommand,
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
        /// Free-form context for this outcome
        #[arg(long = "note")]
        note: Option<String>,
    },
    /// Install the Claude Code integration; preview-first, --write applies
    /// (SPEC §3). User-level installation is explicit, not the default.
    Install {
        /// Install the Claude Code skill and agent definitions
        #[arg(long = "claude")]
        claude: bool,
        /// Also wire the live hook into settings.json (SPEC §23);
        /// requires --claude. settings.json is a file a person
        /// maintains by hand, so this is a separate, explicit ask.
        #[arg(long = "hooks", requires = "claude")]
        hooks: bool,
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
        /// Also remove relais's own commands from settings.json;
        /// requires --claude
        #[arg(long = "hooks", requires = "claude")]
        hooks: bool,
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
        /// The candidate a person finished and merged; required by, and
        /// only meaningful with, `--answer salvaged`
        #[arg(long = "candidate")]
        candidate: Option<String>,
    },
    /// Evidence operations (SPEC §12, §18)
    Evidence {
        #[command(subcommand)]
        cmd: EvidenceCommand,
    },
    /// Read one Claude Code hook payload on stdin and answer it (SPEC
    /// §23): silence, or a refusal a person can act on. With `--probe
    /// --record <dir>`, record the payload verbatim instead and decide
    /// nothing — the internal mode `relais doctor --probe-hooks` wires
    /// into its throwaway settings file, not something a person runs
    /// directly.
    Hook {
        /// Record-only mode: requires `--record`.
        #[arg(long)]
        probe: bool,
        /// Where `--probe` writes the payload
        #[arg(long = "record")]
        record: Option<PathBuf>,
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
    /// Re-run a task's already-accepted run under a candidate recipe, in a
    /// workspace where the accepted answer is genuinely absent (SPEC §24).
    /// Spends real money, subject to the ordinary ceilings, and produces
    /// exactly one arm's result: this compares, scores, ranks and
    /// promotes NOTHING on its own.
    Replay {
        /// The task whose accepted run to replay
        #[arg(long = "task")]
        task: String,
        /// Path to the candidate `relais.toml`, admitted through the same
        /// boundary a learner's proposal is (route::validate_candidate);
        /// a candidate it refuses is refused here with that same rejection
        #[arg(long = "recipe")]
        recipe: PathBuf,
        /// Report what would run and spend nothing: no worker dispatched,
        /// no trial recorded, no usage recorded
        #[arg(long = "dry-run")]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum UsageCommand {
    /// Import orchestration usage from Claude Code session transcripts
    /// (SPEC §11): every session that is the `root_session` of at least
    /// one run, unless `--session` narrows it to one. Idempotent —
    /// importing the same session twice inserts nothing the second time.
    Import {
        /// Import exactly this session instead of every session on
        /// record as a run's root
        #[arg(long)]
        session: Option<String>,
        /// Where Claude Code session transcripts live, instead of
        /// `~/.claude/projects` (or `$CLAUDE_CONFIG_DIR/projects`)
        #[arg(long = "projects-dir")]
        projects_dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum RecipeCommand {
    /// List every recipe the repository's relais.toml declares (SPEC
    /// §26). Read-only: nothing here replays or evaluates anything.
    List,
    /// Show every revision of one recipe (SPEC §26). Read-only.
    Show {
        /// The recipe's name
        name: String,
    },
    /// Compare a candidate `relais.toml`'s recipes against the
    /// repository's own, field by field, and report whether
    /// `route::validate_candidate` would admit it (SPEC §26). Read-only:
    /// nothing here grants, replays, evaluates or writes anything.
    Diff {
        /// Path to the candidate `relais.toml`
        candidate: PathBuf,
    },
    /// Read the settled trials for a candidate recipe and report a paired
    /// comparison against the incumbent (SPEC §25). Reads and reports
    /// only: nothing here promotes, writes a policy, or issues a grant.
    Evaluate {
        /// Path to the candidate `relais.toml`, admitted through the same
        /// boundary a learner's proposal is (route::validate_candidate)
        candidate: PathBuf,
    },
    /// Add a candidate's new recipe revision(s) to the repository's
    /// `relais.toml`, only when the recomputed comparison clears every
    /// gate (SPEC §27). Prints by default; `--write` appends the text.
    /// Never grants trust and never runs anything.
    Promote {
        /// Path to the candidate `relais.toml`
        candidate: PathBuf,
        /// Append the fragment(s) to relais.toml instead of printing
        #[arg(long)]
        write: bool,
    },
    /// Restore the revision below the effective one by appending a new
    /// revision that repeats it (SPEC §27). Needs no evaluation. Prints
    /// by default; `--write` appends the text. Never grants trust.
    Rollback {
        /// The recipe's name
        name: String,
        /// Append the fragment to relais.toml instead of printing
        #[arg(long)]
        write: bool,
    },
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
    /// The accepted candidate was abandoned for reasons unrelated to its
    /// quality and never shipped. Takes it out of the accepted count.
    NotShipped,
}

impl From<FeedbackOutcome> for relais::outcome::OutcomeKind {
    fn from(outcome: FeedbackOutcome) -> Self {
        match outcome {
            FeedbackOutcome::Accepted => Self::AcceptedUnchanged,
            FeedbackOutcome::Corrected => Self::Corrected,
            FeedbackOutcome::Reverted => Self::Reverted,
            FeedbackOutcome::Regression => Self::ConfirmedRegression,
            FeedbackOutcome::NotShipped => Self::NeverShipped,
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
    /// A person finished and merged specific work — the candidate this
    /// run's own worktree carried, whether the run ended on its own
    /// (terminal) or is still waiting on a person — for a reason
    /// unrelated to the work itself. `decided` resolves a question and
    /// says nothing about whether code shipped; `salvaged` is a person's
    /// acceptance of that specific candidate. `--candidate` is required,
    /// and the run's own reason for stopping is kept, not replaced.
    Salvaged,
}

impl DecideAnswer {
    fn resolution(self) -> Reason {
        match self {
            Self::Approve => Reason::DecisionApproved,
            Self::Reject => Reason::DecisionRejected,
            Self::Revise => Reason::DecisionRevised,
            Self::Decided => Reason::DecisionRecorded,
            Self::Abandon => Reason::DecisionAbandoned,
            Self::Salvaged => Reason::DecisionSalvaged,
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
            Self::Salvaged => State::AcceptedByPerson,
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
    /// `recipe promote`: the recomputed comparison failed a gate.
    PromotionRefused,
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
        CliOutcome::PromotionRefused => 17,
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
    fn outcome(&self) -> CliOutcome {
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
        Command::Doctor {
            json,
            probe_hooks,
            verify_sandbox,
            effort_template,
        } => match (probe_hooks, verify_sandbox, effort_template) {
            (true, _, _) => doctor_probe_hooks_command(),
            (false, true, _) => doctor_verify_sandbox_command(),
            (false, false, true) => doctor_effort_template_command(),
            (false, false, false) => doctor_command(json),
        },
        Command::Hook { probe, record } => match (probe, record) {
            (true, Some(dir)) => hook_command(&dir),
            (false, None) => Ok(hook_respond_command()),
            (true, None) => Err(CliError::Usage {
                detail: "hook --probe: needs --record <dir>".into(),
            }),
            (false, Some(_)) => Err(CliError::Usage {
                detail: "hook --record: needs --probe".into(),
            }),
        },
        Command::Init => init_command(),
        Command::Plan { task, revise, json } => {
            let format = if json {
                PlanFormat::Json
            } else {
                PlanFormat::Text
            };
            plan_command(&task, revise.as_deref(), format)
        }
        Command::Run {
            task,
            revise,
            native,
            native_spawn_wait,
        } => run_command(
            &task,
            revise.as_deref(),
            native.then(|| {
                std::time::Duration::from_secs(
                    native_spawn_wait.unwrap_or(DEFAULT_NATIVE_SPAWN_WAIT),
                )
            }),
        ),
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
        Command::Report {
            since,
            json,
            by,
            no_import,
        } => report_command(since.as_deref(), json, by, no_import),
        Command::Usage { cmd } => match cmd {
            UsageCommand::Import {
                session,
                projects_dir,
            } => usage_import_command(session.as_deref(), projects_dir.as_deref()),
        },
        Command::Dataset { cmd } => match cmd {
            DatasetCommand::Build => dataset_build_command(),
            DatasetCommand::Replay {
                task,
                recipe,
                dry_run,
            } => replay_command(&task, &recipe, dry_run),
        },
        Command::Recipe { cmd } => match cmd {
            RecipeCommand::List => recipe_list_command(),
            RecipeCommand::Show { name } => recipe_show_command(&name),
            RecipeCommand::Diff { candidate } => recipe_diff_command(&candidate),
            RecipeCommand::Evaluate { candidate } => recipe_evaluate_command(&candidate),
            RecipeCommand::Promote { candidate, write } => {
                recipe_promote_command(&candidate, AmendMode::from_flag(write))
            }
            RecipeCommand::Rollback { name, write } => {
                recipe_rollback_command(&name, AmendMode::from_flag(write))
            }
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
            note,
        } => feedback_command(FeedbackRequest {
            run_id,
            task,
            outcome,
            candidate,
            magnitude,
            evidence,
            actor,
            note,
        }),
        Command::Install {
            claude,
            hooks,
            write,
            user,
        } => match claude {
            true => install_command(write, user, Targets::from_flag(hooks)),
            false => Err(CliError::Usage {
                detail: "install: name what to install (--claude)".into(),
            }),
        },
        Command::Uninstall {
            claude,
            hooks,
            write,
            user,
        } => match claude {
            true => uninstall_command(write, user, Targets::from_flag(hooks)),
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
            candidate,
        } => decide_command(
            &run_id,
            answer,
            &actor,
            note.as_deref(),
            successor.as_deref(),
            criterion.as_deref(),
            candidate.as_deref(),
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

/// What an `install`/`uninstall --claude` invocation is aimed at. Not a
/// flag: `settings.json` is a file a person maintains by hand, so
/// touching it is a different target rather than a modifier of the same
/// one, and the two cases read as what they are at every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Targets {
    /// The owned files under `.claude/` only.
    OwnedFiles,
    /// The owned files, and then the hook wiring in `settings.json`.
    OwnedFilesAndHooks,
}

impl Targets {
    /// `--hooks` names the second target. The flag lives in the CLI
    /// surface, where clap parses it; nothing below this line takes a
    /// bool that means "and also do the other thing".
    fn from_flag(hooks: bool) -> Self {
        match hooks {
            true => Targets::OwnedFilesAndHooks,
            false => Targets::OwnedFiles,
        }
    }
}

fn install_command(write: bool, user: bool, targets: Targets) -> Result<CliOutcome, CliError> {
    let request = install_request(write, user)?;
    let home = install_home(&request.scope)?;
    let report = operational(relais::install::install(&request, &home), "install")?;
    let outcome = render_install("install", &request, &report);
    match targets {
        Targets::OwnedFiles => Ok(outcome),
        Targets::OwnedFilesAndHooks => Ok(worse_outcome(
            outcome,
            install_hooks_target(&request, &home)?,
        )),
    }
}

/// The settings.json half of `install --claude --hooks`, on its own so
/// the function above stays one thing: the owned files, then whatever
/// else was named.
fn install_hooks_target(request: &InstallRequest, home: &Path) -> Result<CliOutcome, CliError> {
    let relais_binary = operational(std::env::current_exe(), "install --hooks")?;
    let root = request.root(home);
    let queue_wait = admission_queue_wait();
    // Both roots the harness merges, regardless of which one this install
    // writes to: a `--user` install still has to see the project's files,
    // or it writes the second handler issue #94 is about. The project is
    // the directory the command was run in; `home` here is the install
    // scope's root, which for a project install is the project itself, so
    // the real home is resolved separately.
    let project = operational(std::env::current_dir(), "install --hooks")?;
    let real_home = relais::paths::home_dir().ok();
    let roots = relais::install::settings::MergedRoots {
        project: Some(project.as_path()),
        home: real_home.as_deref(),
    };
    match request.mode {
        Mode::Preview => Ok(render_hooks_preview(operational(
            root.plan_hooks(&relais_binary, queue_wait, roots),
            "install --hooks",
        )?)),
        Mode::Apply => {
            let applied = operational(
                root.apply_hooks(&relais_binary, queue_wait, roots),
                "install --hooks",
            )?;
            Ok(render_hooks_applied(applied))
        }
    }
}

/// The admission wait the installer must size the `PreToolUse` handler
/// timeout for — a missing or unreadable machine.toml is exactly the
/// default this crate ships (`hook_admission_settings` reads the same
/// way), never an error install itself should stop over.
fn admission_queue_wait() -> std::time::Duration {
    match hook_admission_settings().queue_behaviour() {
        relais::policy::QueueBehaviour::RefuseImmediately => std::time::Duration::ZERO,
        relais::policy::QueueBehaviour::WaitUpTo(wait) => wait,
    }
}

fn uninstall_command(write: bool, user: bool, targets: Targets) -> Result<CliOutcome, CliError> {
    let request = install_request(write, user)?;
    let home = install_home(&request.scope)?;
    let report = operational(relais::install::uninstall(&request, &home), "uninstall")?;
    let outcome = render_install("uninstall", &request, &report);
    match targets {
        Targets::OwnedFiles => Ok(outcome),
        Targets::OwnedFilesAndHooks => Ok(worse_outcome(
            outcome,
            uninstall_hooks_target(&request, &home)?,
        )),
    }
}

/// The settings.json half of `uninstall --claude --hooks`.
fn uninstall_hooks_target(request: &InstallRequest, home: &Path) -> Result<CliOutcome, CliError> {
    let relais_binary = operational(std::env::current_exe(), "uninstall --hooks")?;
    let root = request.root(home);
    match request.mode {
        Mode::Preview => Ok(render_hooks_removal_preview(operational(
            root.plan_hooks_removal(&relais_binary),
            "uninstall --hooks",
        )?)),
        Mode::Apply => {
            let removed = operational(
                root.apply_hooks_removal(&relais_binary),
                "uninstall --hooks",
            )?;
            Ok(render_hooks_removed(removed))
        }
    }
}

/// The worse of two outcomes from one combined `--claude --hooks`
/// invocation, so a hooks refusal is never masked by an otherwise clean
/// file install (or the reverse). `Accepted` is the only outcome neither
/// side reports as a problem, so anything else wins.
fn worse_outcome(a: CliOutcome, b: CliOutcome) -> CliOutcome {
    if a == CliOutcome::Accepted {
        b
    } else {
        a
    }
}

/// Print what wiring the hook would do, in preview mode.
fn render_hooks_preview(plan: relais::install::HooksPlan) -> CliOutcome {
    match plan {
        relais::install::HooksPlan::Ready { events } => {
            println!("relais install --claude --hooks (settings.json)");
            for event in &events {
                println!(
                    "  {:<8} {}",
                    match event.action {
                        relais::install::HookEventAction::Current => "keep",
                        relais::install::HookEventAction::CorrectTimeout => "retime",
                        relais::install::HookEventAction::MigrateMatcher => "migrate",
                        relais::install::HookEventAction::JoinExisting => "join",
                        relais::install::HookEventAction::NewEntry => "add",
                    },
                    event.event
                );
            }
            if events.iter().any(|e| e.action.changes_anything()) {
                println!("preview only: re-run with --write to apply");
            } else {
                println!("nothing to do");
            }
            CliOutcome::Accepted
        }
        relais::install::HooksPlan::Unrenderable {
            reason,
            paste_block,
        } => print_unrenderable("install", &reason, &paste_block),
    }
}

fn render_hooks_applied(applied: relais::install::HooksApplied) -> CliOutcome {
    match applied {
        relais::install::HooksApplied::AlreadyCurrent => {
            println!("relais install --claude --hooks: settings.json is already current");
            CliOutcome::Accepted
        }
        relais::install::HooksApplied::Applied(events) => {
            println!(
                "relais install --claude --hooks: wired {} event(s): {}",
                events.len(),
                events.join(", ")
            );
            CliOutcome::Accepted
        }
        relais::install::HooksApplied::Refused {
            reason,
            paste_block,
        } => print_unrenderable("install", &reason, &paste_block),
    }
}

fn render_hooks_removal_preview(plan: relais::install::settings::HooksRemovalPlan) -> CliOutcome {
    match plan {
        relais::install::settings::HooksRemovalPlan::Ready { events } => {
            println!("relais uninstall --claude --hooks (settings.json)");
            for event in &events {
                println!(
                    "  {:<8} {}",
                    match event.action {
                        relais::install::settings::HookRemovalAction::WouldRemove => "remove",
                        relais::install::settings::HookRemovalAction::NotPresent => "keep",
                    },
                    event.event
                );
            }
            if events
                .iter()
                .any(|e| e.action == relais::install::settings::HookRemovalAction::WouldRemove)
            {
                println!("preview only: re-run with --write to apply");
            } else {
                println!("nothing to do");
            }
            CliOutcome::Accepted
        }
        relais::install::settings::HooksRemovalPlan::Unrenderable { reason } => {
            eprintln!("relais uninstall --hooks: refused — {reason}");
            CliOutcome::Blocked
        }
    }
}

fn render_hooks_removed(removed: relais::install::HooksRemoved) -> CliOutcome {
    match removed {
        relais::install::HooksRemoved::AlreadyAbsent => {
            println!("relais uninstall --claude --hooks: nothing of relais's was on settings.json");
            CliOutcome::Accepted
        }
        relais::install::HooksRemoved::Removed(events) => {
            println!(
                "relais uninstall --claude --hooks: removed {} event(s): {}",
                events.len(),
                events.join(", ")
            );
            CliOutcome::Accepted
        }
        relais::install::HooksRemoved::Refused { reason } => {
            eprintln!("relais uninstall --hooks: refused — {reason}");
            CliOutcome::Blocked
        }
    }
}

/// A settings.json that failed the round-trip check: nothing was
/// written, so the exit is `NotFullyApplied` rather than `Blocked` —
/// the file install half of a combined `--claude --hooks` run may well
/// have succeeded, and this is "part of the plan could not be applied",
/// not "nothing ran at all".
fn print_unrenderable(verb: &str, reason: &str, paste_block: &str) -> CliOutcome {
    eprintln!("relais {verb} --hooks: refused — {reason}");
    println!("paste this into settings.json's \"hooks\" key by hand:\n{paste_block}");
    CliOutcome::NotFullyApplied
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
fn harness_identity() -> HarnessProbe {
    let unknown = HarnessProbe {
        identity: None,
        efforts: None,
    };
    let backend = match relais::adapter::claude::ClaudeBackend::discover() {
        Ok(backend) => backend,
        Err(e) => {
            eprintln!("relais plan: the harness is unknown ({e})");
            return unknown;
        }
    };
    let capabilities = match backend.probe_report() {
        Ok(capabilities) => capabilities,
        Err(failure) => {
            eprintln!("relais plan: the harness is unknown ({failure})");
            return unknown;
        }
    };
    HarnessProbe {
        identity: Some(format!(
            "{} {}",
            backend.name(),
            capabilities.version.as_deref().unwrap_or("?")
        )),
        efforts: Some(capabilities.accepted_efforts),
    }
}

/// What one probe of the harness told `plan`: who it is, and which efforts
/// its CLI accepts. Both `None` when it could not be probed.
struct HarnessProbe {
    identity: Option<String>,
    efforts: Option<relais::catalog::EffortSet>,
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
                // The one situation this whole line exists for. With no
                // daemon answering, a hook under `carry_on` refuses
                // nothing — and saying nothing here is how that went
                // unnoticed until it was measured.
                println!(
                    "{}",
                    relais::admission::enforcement_line(
                        relais::admission::Enforcement::Observed,
                        &relais::admission::StatusSnapshot::default(),
                    )
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
    if snapshot.adopted_pre_migration > 0 {
        println!(
            "adopted from a pre-migration ledger row: {} — session and reservation are real, \
             parent and depth are unrecorded and enforced as root/zero",
            snapshot.adopted_pre_migration
        );
    }
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
    println!(
        "{}",
        relais::admission::enforcement_line(
            // This snapshot came back, so a coordinator is serving and
            // its caps bind across every tab of this user. Read off the
            // answer rather than named as a constant here: the absent
            // branch above reaches the same function with `Observed`,
            // and the two must not be able to drift into agreeing.
            relais::admission::Enforcement::Coordinator,
            &snapshot,
        )
    );
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
            // The machine's `binding_lease_secs` governs the lease a
            // hook-admitted agent is held on (SPEC §23), read from the
            // same file and the same parse as the concurrency limits
            // above it, rather than left at `AdmissionState`'s own
            // fallback regardless of what machine.toml states.
            let (limits, agent_lease_ttl) = match std::fs::read_to_string(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
                    coordinator::effective_limits(&Default::default()),
                    Duration::from_secs(HookAdmissionSettings::default().binding_lease_secs),
                ),
                Err(cause) => {
                    return Err(CliError::Read {
                        what: "the machine settings",
                        path,
                        cause,
                    })
                }
                Ok(text) => match MachineSettings::from_toml_str(&text) {
                    Ok(machine) => (
                        coordinator::effective_limits(&machine.concurrency),
                        Duration::from_secs(machine.admission.binding_lease_secs),
                    ),
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
            match coordinator::run_daemon(&socket, limits, ledger.as_ref(), agent_lease_ttl) {
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

/// The bounds a candidate recipe is admitted under: the incumbent's own,
/// with effort catalogs resolved as routing resolves them — from the
/// harness probe and machine.toml. A machine.toml that is absent or invalid
/// states no effort fact, and a harness that does not answer accepts none:
/// both leave the catalogs unknown, which admits only the incumbent's
/// configured efforts, never more.
fn tuning_bounds(incumbent: &RepoPolicy) -> route::TuningBounds {
    let settings = paths::machine_settings_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| MachineSettings::from_toml_str(&text).ok())
        .map(|machine| {
            (
                machine.efforts,
                machine.routing.max_effort,
                machine.allowed_models,
            )
        });
    let (efforts, max_effort, allowed_models) = settings.unwrap_or_else(|| {
        (
            relais::policy::EffortSettings::default(),
            relais::policy::RoutingSettings::default().max_effort,
            None,
        )
    });
    let cli = relais::adapter::claude::ClaudeBackend::discover()
        .ok()
        .and_then(|backend| backend.probe())
        .map_or(relais::catalog::Fact::Unknown, |capabilities| {
            capabilities.accepted_efforts
        });
    let catalogs = relais::catalog::EffortCatalogs::resolve_all(
        &cli,
        &efforts,
        &max_effort,
        incumbent.models.values().map(|profile| profile.id.as_str()),
    );
    route::default_tuning_bounds(incumbent, allowed_models.as_deref(), &catalogs)
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

/// Say on doctor's `effort` finding which models a `raise` repair WOULD hold
/// because their effort order is unknown. Doctor has no task or budget, so
/// the line is conditional and lists every model a repair could run at;
/// `relais plan` states the models one run's ladder actually holds. Holding
/// the effort never changes the finding's level — nothing is blocked. A
/// repository or machine.toml that cannot be read says nothing here: doctor
/// reports those on findings of their own.
fn note_held_repair_effort(report: &mut doctor::DoctorReport) {
    let (Ok((_, repo)), Ok(machine)) = (load_repo_policy(), load_machine()) else {
        return;
    };
    let cli = relais::adapter::claude::ClaudeBackend::discover()
        .ok()
        .and_then(|backend| backend.probe())
        .map_or(relais::catalog::Fact::Unknown, |capabilities| {
            capabilities.accepted_efforts
        });
    let catalogs = relais::catalog::EffortCatalogs::resolve_all(
        &cli,
        &machine.efforts,
        &machine.routing.max_effort,
        repo.models.values().map(|profile| profile.id.as_str()),
    );
    let held = route::held_repair_models(&repo.models, &catalogs, repo.execution.repair_effort);
    if held.is_empty() {
        return;
    }
    if let Some(finding) = report
        .findings
        .iter_mut()
        .find(|finding| finding.component == "effort")
    {
        finding.detail.push('\n');
        finding.detail.push_str(&route::held_would_text(&held));
    }
}

fn doctor_command(json: bool) -> Result<CliOutcome, CliError> {
    let mut report = doctor::doctor(&project_dir()?);
    note_held_repair_effort(&mut report);
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

/// `relais doctor --effort-template`: just the block to paste.
fn doctor_effort_template_command() -> Result<CliOutcome, CliError> {
    let (_, policy) = load_repo_policy()?;
    print!("{}", doctor::effort_template_for(&policy));
    Ok(CliOutcome::Accepted)
}

/// `relais doctor --verify-sandbox`: two real probe sessions, and the
/// record of a pass. With `[sandbox]` off nothing is launched and the
/// answer is the exit-0 statement of that; a sandbox that cannot be relied
/// on blocks (exit 3); a probe that fails or cannot run exits 1.
fn doctor_verify_sandbox_command() -> Result<CliOutcome, CliError> {
    let settings = match load_machine() {
        Ok(machine) => machine.sandbox,
        // No machine.toml: the sandbox is off, as for a run.
        Err(CliError::Read { cause, .. }) if cause.kind() == std::io::ErrorKind::NotFound => {
            relais::policy::SandboxSettings::default()
        }
        Err(other) => return Err(other),
    };
    match doctor::verify_sandbox_here(&settings, &project_dir()?) {
        Ok(doctor::SandboxVerification::Off) => {
            println!("the OS sandbox is off ([sandbox] enabled = false); nothing was launched");
            Ok(CliOutcome::Accepted)
        }
        Ok(doctor::SandboxVerification::Blocked(blocker)) => {
            println!(
                "blocked ({}): {}; nothing was launched",
                blocker.code, blocker.detail
            );
            Ok(CliOutcome::Blocked)
        }
        Ok(doctor::SandboxVerification::Ran(outcome)) => {
            print!("{}", outcome.render());
            match outcome.passed() {
                true => Ok(CliOutcome::Accepted),
                false => Ok(CliOutcome::OperationalFailure),
            }
        }
        Err(why) => {
            eprintln!("relais doctor --verify-sandbox: {why}");
            Ok(CliOutcome::OperationalFailure)
        }
    }
}

/// `relais hook --probe --record <dir>`: the record-only handler
/// `relais doctor --probe-hooks` wires into its throwaway settings file.
/// Reads one payload from stdin, writes it verbatim, decides nothing,
/// and always accepts — the handler cannot fail the session it is
/// watching.
fn hook_command(dir: &Path) -> Result<CliOutcome, CliError> {
    let order = relais::hook::arrival_nanos();
    relais::hook::record(dir, order, std::io::stdin().lock());
    Ok(CliOutcome::Accepted)
}

/// `relais hook`, run with no flags: read one payload on stdin, decide
/// what to say about it, and say it (SPEC §23). Exits 0 except when the
/// answer itself asks otherwise: a `WorktreeCreate` relais could not make
/// a tree for exits 1 with the reason, because Claude Code refuses that
/// agent either way and the reason is the only thing worth adding. For
/// everything else a non-zero exit is reported to the session as a
/// failure of the tool call it was watching, so a relais that cannot
/// answer must be indistinguishable from a relais that had nothing to
/// say. Every path that could fail (an unreadable settings file, an
/// unreachable coordinator, a payload that is not JSON, a panic anywhere
/// inside) is swallowed rather than surfaced; `relais doctor` is where
/// any of that is reported as a finding.
fn hook_respond_command() -> CliOutcome {
    // The whole body, not just `respond::handle`: reading stdin,
    // loading settings and journalling all run here too, and none of
    // them may take the process down with them any more than the
    // decision itself may.
    // An answer may ask for a non-zero exit (a worktree that could not be
    // made); a panic still exits 0, as every hook failure always has.
    let code = std::panic::catch_unwind(std::panic::AssertUnwindSafe(hook_respond)).unwrap_or(0);
    if code != 0 {
        std::process::exit(code);
    }
    CliOutcome::Accepted
}

/// Print an answer: stdout as rendered (one trailing newline), stderr
/// when it has something to say. Returns the exit status it asks for.
fn write_answer(answer: &relais::hook::decide::HookAnswer) -> i32 {
    if let Some(mut text) = answer.stdout_payload() {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        print!("{text}");
    }
    if let Some(reason) = answer.stderr_payload() {
        eprintln!("{reason}");
    }
    answer.exit_code()
}

fn hook_respond() -> i32 {
    use std::io::Read;
    let mut payload = Vec::new();
    // A truncated read leaves `payload` with whatever arrived so far;
    // `event::parse` treats that the same as any other input it cannot
    // make sense of (`HookEvent::NotOurs`), so the hook still answers.
    let _ = std::io::stdin().lock().read_to_end(&mut payload);

    let settings = hook_admission_settings();

    let Ok(socket) = relais::coordinator::socket_path() else {
        // No home directory: nothing to connect to and nowhere to
        // journal. The hook still owes an answer for "the coordinator
        // could not be reached" under this machine's own stance, and a
        // `WorktreeCreate` still owes a path or a reason.
        let answer = relais::hook::respond::answer_without_home(&payload, &settings);
        return write_answer(&answer);
    };
    let gate = relais::coordinator::RemoteGate::new(socket);
    let handled = relais::hook::respond::handle(&payload, &settings, &gate);
    let code = write_answer(&handled.answer);
    if let Ok(path) = paths::hook_journal_path() {
        let entry = relais::hook::respond::journal_entry(&payload, &handled);
        // Best effort, like every other step here: a journal write
        // failure is not the tool call's to report, and it already
        // answered above.
        let _ = relais::hook::respond::append_journal(&path, &entry);
    }
    code
}

/// The machine's hook-admission settings, or the stated defaults when
/// machine.toml is missing or will not parse. A hook cannot refuse to
/// answer over a settings problem any more than over anything else it
/// might hit — that failure belongs to `relais doctor`, not to a hook
/// answer holding a tool call open.
fn hook_admission_settings() -> relais::policy::HookAdmissionSettings {
    paths::machine_settings_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| MachineSettings::from_toml_str(&text).ok())
        .map(|machine| machine.admission)
        .unwrap_or_default()
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

/// How `plan` prints its decision.
#[derive(Clone, Copy)]
enum PlanFormat {
    Text,
    Json,
}

/// `plan --json`: the decision as one JSON object — the route with its
/// rung, or every blocker with its code.
fn plan_json(
    decision: &route::Routed,
    authority: &EffectiveAuthority,
    contract: &TaskContract,
) -> Result<CliOutcome, CliError> {
    let (value, outcome) = match decision {
        route::Routed::Route(routed) => (
            serde_json::json!({
                "contract_hash": contract.hash(),
                "policy_hash": authority.authority_hash,
                "route": {
                    "tier": routed.tier.as_str(),
                    "rung": routed.rung,
                    "ladder": routed.ladder,
                    "routed_by": routed.routed_by.as_str(),
                    "escalation_tier": routed.escalation_tier.map(|tier| tier.as_str()),
                    "reasons": routed.reasons.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
                },
            }),
            CliOutcome::Accepted,
        ),
        route::Routed::Blocked(blocked) => (
            serde_json::json!({
                "contract_hash": contract.hash(),
                "policy_hash": authority.authority_hash,
                "blocked": blocked
                    .blockers()
                    .iter()
                    .map(|b| serde_json::json!({"code": b.code.as_str(), "detail": b.detail}))
                    .collect::<Vec<_>>(),
            }),
            CliOutcome::Blocked,
        ),
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&value).expect("a plan decision serializes")
    );
    Ok(outcome)
}

fn plan_command(
    task: &Path,
    revise: Option<&str>,
    format: PlanFormat,
) -> Result<CliOutcome, CliError> {
    let (root, repo) = load_repo_policy()?;
    let machine = load_machine()?;
    let contract = load_contract(task)?;
    // Only a contract that NAMES a task needs the ledger, and `plan`
    // writes nothing: opening it unconditionally would create the state
    // directory, the database and its migrations as a side effect of a
    // preflight whose whole promise is that nothing happens yet.
    let mut task_override = None;
    if contract.task_id.is_some() || revise.is_some() {
        let ledger = open_ledger()?;
        match resolve_task_override(&ledger, &contract, revise) {
            Ok(resolved) => task_override = resolved,
            Err(detail) => {
                eprintln!("relais plan: {detail}");
                return Ok(CliOutcome::Blocked);
            }
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
    // The same check `run` performs in `assemble_context`: a read hint
    // that does not resolve at the base revision points the worker at a
    // path this tree does not have, and `plan` should refuse it exactly
    // as `run` does rather than route a candidate `run` would then block.
    if let Err(unresolvable) = relais::context::fingerprint_hints(
        &relais::workspace::SystemGit,
        &root,
        &base_sha,
        &contract.read_hints,
    ) {
        eprintln!(
            "relais plan: blocked (read_hint_unresolvable): {unresolvable} (base {base_sha})"
        );
        return Ok(CliOutcome::Blocked);
    }
    let repo_identity = relais::repo::identity(&root);
    let authority = effective_authority(&repo, &machine, &contract, &repo_identity);
    let probe = harness_identity();
    let harness = probe.identity;
    let registry = learned_registry(&machine);
    let predictor = registry
        .as_ref()
        .map(|registry| RegistryPredictor::new(registry, &repo, harness.as_deref()));
    let catalogs = route::resolve_catalogs(&repo, &authority, &machine, probe.efforts.as_ref());
    let decision = route::route(route::RouteInputs {
        contract: &contract,
        repo: &repo,
        machine: &machine,
        authority: &authority,
        predictor: predictor
            .as_ref()
            .map(|predictor| predictor as &dyn route::RoutePredictor),
        catalogs: &catalogs,
    });
    if let PlanFormat::Json = format {
        return plan_json(&decision, &authority, &contract);
    }
    let session = relais::coordinator::resolve_session();
    println!("contract hash: {}", contract.hash());
    println!("policy hash: {}", authority.authority_hash);
    println!("repository: {repo_identity}");
    println!("base: {} ({})", base_sha, contract.base_ref);
    // Stderr, not stdout: `plan`'s stdout stays what it was (see below),
    // and session attribution is not part of that contract.
    eprintln!("session: {} ({})", session.id, session.source);
    if session.source.is_fallback() {
        eprintln!(
            "relais plan: warning: no CLAUDE_CODE_SESSION_ID or RELAIS_SESSION_ID in the \
             environment; attributing this call to {}",
            session.source
        );
    }
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
            "{}",
            missing_grant_text(&authority.grant_key, &repo_identity)?
        );
    }
    match &decision {
        route::Routed::Route(routed) => {
            print!("{}", routed.explain());
            // Stderr, like the other advisories: the route line and the
            // ladder on stdout are unchanged by holding the effort.
            for model in routed.ladder.held() {
                eprintln!("{}", route::held_text(model));
            }
            for line in plan_trial_lines(
                &root,
                &repo,
                &machine,
                &repo_identity,
                &contract,
                task_override,
            )? {
                println!("{line}");
            }
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

/// The task identity a run of this contract will have: the one already
/// confirmed on record, or the one derived from the repository and the
/// contract, exactly as the runner derives it at preflight.
fn task_identity(
    task_override: Option<TaskId>,
    repo_identity: &RepoIdentity,
    contract: &TaskContract,
) -> TaskId {
    task_override.unwrap_or_else(|| {
        relais::ids::derive_task_id(&relais::policy::repo_key(repo_identity), &contract.hash())
    })
}

/// What `relais plan` prints about live trials (SPEC §28): the assignment
/// it WOULD make, and every candidate dropped and why. Nothing when the
/// envelope is off, so plan's output is unchanged. Writes nothing: with no
/// ledger on disk yet it does not create one to learn that today's usage
/// is zero.
fn plan_trial_lines(
    root: &Path,
    repo: &RepoPolicy,
    machine: &MachineSettings,
    repo_identity: &RepoIdentity,
    contract: &TaskContract,
    task_override: Option<TaskId>,
) -> Result<Vec<String>, CliError> {
    if !machine.trials.enabled {
        return Ok(Vec::new());
    }
    let ledger_path = paths::ledger_path().map_err(CliError::Home)?;
    let ledger = if ledger_path.exists() {
        Some(open_ledger()?)
    } else {
        None
    };
    let task_id = task_identity(task_override, repo_identity, contract);
    let decision = operational(
        live_trial::decide(&live_trial::Environment {
            root,
            repo,
            machine,
            identity: repo_identity,
            contract,
            task_id: &task_id,
            ledger: ledger.as_ref(),
        }),
        "plan trial assignment",
    )?;
    Ok(decision.lines())
}

/// Seconds `relais run --native` waits for the parent session to spawn a
/// requested worker.
const DEFAULT_NATIVE_SPAWN_WAIT: u64 = 120;

/// Why a `--native` run cannot start under this session, if it cannot: a
/// native worker is spawned by a parent Claude Code session, which a
/// session id guessed from the parent process is not.
fn native_session_refusal(
    native: Option<std::time::Duration>,
    session: &relais::coordinator::ResolvedSessionId,
) -> Option<String> {
    (native.is_some() && session.source.is_fallback()).then(|| {
        "native workers need to run inside Claude Code: no CLAUDE_CODE_SESSION_ID in the \
         environment names a parent session to spawn them"
            .to_string()
    })
}

/// `native` is the spawn wait of a `--native` run, `None` for a headless one.
fn run_command(
    task: &Path,
    revise: Option<&str>,
    native: Option<std::time::Duration>,
) -> Result<CliOutcome, CliError> {
    let session = relais::coordinator::resolve_session();
    if let Some(detail) = native_session_refusal(native, &session) {
        eprintln!("relais run: {detail}");
        return Ok(CliOutcome::Blocked);
    }
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
    // The worker attempts of a `--native` run go to the parent session;
    // everything else stays on the Claude backend.
    let prices = match native {
        Some(_) => Some(load_price_table()?),
        None => None,
    };
    let native_backend = native.map(|spawn_wait| {
        relais::adapter::native::NativeBackend::new(
            backend.as_ref(),
            &gate,
            session.id.clone(),
            spawn_wait,
            prices,
        )
    });
    let run_backend: &dyn relais::backend::Backend = match &native_backend {
        Some(native_backend) => native_backend,
        None => backend.as_ref(),
    };
    let worker_presentation = match native {
        Some(_) => relais::backend::Presentation::Native,
        None => relais::backend::Presentation::Headless,
    };
    // Printed here, before `execute`, so it is visible on every path —
    // including the `Err` arm below that returns early — and only once:
    // a run can take minutes, and deferring this to the end read as
    // silence for the whole time it ran.
    eprintln!("session: {} ({})", session.id, session.source);
    if session.source.is_fallback() {
        eprintln!(
            "relais run: warning: no CLAUDE_CODE_SESSION_ID or RELAIS_SESSION_ID in the \
             environment; attributing this run to {}",
            session.source
        );
    }
    // Live trials (SPEC §28). With the envelope off `decide` answers
    // without reading a candidate or touching the ledger, `lines` is
    // empty and `record` writes nothing: the run below is the run it
    // always was.
    let repo_identity = relais::repo::identity(&root);
    let task_id = task_identity(task_override.clone(), &repo_identity, &contract);
    let trial = operational(
        live_trial::decide(&live_trial::Environment {
            root: &root,
            repo: &repo,
            machine: &machine,
            identity: &repo_identity,
            contract: &contract,
            task_id: &task_id,
            ledger: Some(&ledger),
        }),
        "trial assignment",
    )?;
    for line in trial.lines() {
        eprintln!("{line}");
    }
    // A row is written before the run, so a draw is on record even if the
    // run dies. Without a resolvable base there is nothing to record, and
    // then nothing is drawn: the incumbent runs and reports the base
    // itself. Never a candidate policy without its row.
    let trial_base = trial
        .draws()
        .then(|| workspace::resolve_base(&root, &contract.base_ref).ok())
        .flatten();
    let run_id = match ids.run_id() {
        Ok(run_id) => run_id,
        Err(e) => {
            eprintln!("relais run: {e}");
            return Ok(CliOutcome::OperationalFailure);
        }
    };
    let trial_id = match &trial_base {
        Some(base_sha) => {
            let contract_hash = contract.hash();
            let profile_hash = repo
                .verification
                .profiles
                .get(&contract.verification_profile)
                .map(|profile| profile.hash())
                .unwrap_or_default();
            match trial.record(
                &ledger,
                &ids,
                &live_trial::TrialFacts {
                    task_id: &task_id,
                    base_sha,
                    contract_hash: &contract_hash,
                    verification_profile_hash: &profile_hash,
                },
                &run_id,
            ) {
                Ok(trial_id) => trial_id,
                Err(e) => {
                    eprintln!("relais run: {e}");
                    return Ok(CliOutcome::OperationalFailure);
                }
            }
        }
        None => None,
    };
    let run_policy: &RepoPolicy = match &trial_id {
        Some(_) => trial.candidate_policy().unwrap_or(&repo),
        None => &repo,
    };
    let outcome = match execute(&RunConfig {
        repo_dir: &root,
        contract: &contract,
        repo_policy: run_policy,
        machine: &machine,
        ledger: &ledger,
        ids: &ids,
        backend: run_backend,
        git: &git,
        hooks: &hooks,
        attest: &attest,
        worker_env,
        sandbox_host: &relais::sandbox::RealSandboxHost,
        artifacts_dir: artifacts_dir.clone(),
        // Where a sandboxed worker's short temp-dir link goes (unused off unix).
        tmp_link_root: std::path::PathBuf::from("/tmp"),
        aval_resolver: &aval_resolver,
        predictor: predictor
            .as_ref()
            .map(|predictor| predictor as &dyn route::RoutePredictor),
        gate: Some(&gate),
        session_id: session.id.clone(),
        heartbeat_every: std::time::Duration::from_secs(30),
        task_override: task_override.as_ref(),
        // A run a live trial row names is a trial arm on the run's own
        // record too — Control included — so `runs.purpose` and
        // `trials.source_run_id` never disagree about it (SPEC §28).
        purpose: trial_id.as_ref().map(|_| RunPurpose::TrialArm),
        run_id: Some(run_id.clone()),
        worker_presentation,
    }) {
        Ok(outcome) => outcome,
        Err(e) => {
            if let Some(trial_id) = &trial_id {
                if let Err(settle_error) = live_trial::settle_errored(&ledger, trial_id) {
                    eprintln!("relais run: could not settle trial {trial_id}: {settle_error}");
                }
            }
            eprintln!("relais run: {e}");
            return Ok(CliOutcome::OperationalFailure);
        }
    };
    if let Some(trial_id) = &trial_id {
        if let Err(e) = live_trial::settle(&ledger, trial_id, &outcome) {
            eprintln!("relais run: could not settle trial {trial_id}: {e}");
        }
    }
    let run_dir = artifacts_dir.join(outcome.run_id());
    let result = match &outcome.terminal {
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
    };
    result
}

/// `relais dataset replay` (SPEC §24): re-run a task's already-accepted
/// run under a candidate recipe, in a workspace where the accepted answer
/// is genuinely absent, and record that arm as one settled trial. This is
/// NOT a promotion mechanism and does not compare, score, rank or promote
/// anything — it produces exactly one arm's result for one task, and it
/// spends real money, subject to the ordinary ceilings ordinary work goes
/// through (the same `execute` path `relais run` takes).
fn replay_command(task: &str, recipe: &Path, dry_run: bool) -> Result<CliOutcome, CliError> {
    let (root, incumbent) = load_repo_policy()?;
    let candidate_text = std::fs::read_to_string(recipe).map_err(|cause| CliError::Read {
        what: "the candidate recipe",
        path: recipe.to_path_buf(),
        cause,
    })?;
    let candidate_policy =
        RepoPolicy::from_toml_str(&candidate_text).map_err(|cause| CliError::Invalid {
            what: "the candidate recipe",
            path: recipe.to_path_buf(),
            cause: Box::new(cause),
        })?;
    // Admitted through the same door a learner's proposal is — never a
    // second, looser one for replay.
    let bounds = tuning_bounds(&incumbent);
    let candidate = match route::validate_candidate(&incumbent, &candidate_policy, &bounds) {
        Ok(candidate) => candidate,
        Err(rejection) => {
            eprintln!("relais dataset replay: candidate refused: {rejection}");
            return Ok(CliOutcome::Blocked);
        }
    };

    let ledger = open_ledger()?;
    let task_id = TaskId::from_stored(task);
    let runs = operational(ledger.runs_of_task(&task_id), "dataset replay")?;
    // `runs_of_task` comes back oldest-first, so `.rev()` walks newest
    // first — and needs the `break` it was missing: without one the
    // assignment kept overwriting and the EARLIEST accepted run won,
    // the opposite of what reversing the list was for. An older receipt
    // is also likelier to carry an empty verification profile hash, so
    // the bug made the comparability refusal fire more often too.
    //
    // A replay is never its own source. Replay runs execute under the
    // task's own id, so an accepted replay would otherwise be eligible as
    // the "accepted run" of the next replay, and an arm would be compared
    // against another arm instead of against the work that actually
    // shipped.
    let mut source_run = None;
    for run in runs.into_iter().rev() {
        // Exactly `Accepted` (SPEC §24) — not `AcceptedByPerson`: a
        // person's salvage never went through the runner's own
        // verification, so there is no receipt with a verification
        // profile hash to check a replay's comparability against.
        if operational(ledger.run_status(&run), "dataset replay")? != Some(State::Accepted) {
            continue;
        }
        if operational(ledger.run_purpose(&run), "dataset replay")?.is_some() {
            continue;
        }
        source_run = Some(run);
        break;
    }
    let Some(source_run) = source_run else {
        eprintln!("relais dataset replay: task {task_id} has no accepted run on record");
        return Ok(CliOutcome::UnknownRun);
    };

    let Some((source_contract, _tier)) =
        operational(ledger.run_contract_and_tier(&source_run), "dataset replay")?
    else {
        eprintln!("relais dataset replay: run {source_run} carries no recorded contract");
        return Ok(CliOutcome::OperationalFailure);
    };
    let Some((receipt, _hash)) = operational(ledger.receipt(&source_run), "dataset replay")? else {
        eprintln!("relais dataset replay: run {source_run} has no receipt to replay from");
        return Ok(CliOutcome::OperationalFailure);
    };
    let (Some(base_sha), Some(source_profile_hash), Some(contract_hash)) = (
        receipt["base_sha"].as_str(),
        receipt["verification_profile_hash"].as_str(),
        receipt["contract_hash"].as_str(),
    ) else {
        eprintln!("relais dataset replay: run {source_run}'s receipt is missing a base SHA, a contract hash, or a verification profile hash");
        return Ok(CliOutcome::OperationalFailure);
    };
    let base_sha = base_sha.to_string();
    let source_profile_hash = source_profile_hash.to_string();
    let contract_hash = contract_hash.to_string();

    // The profile hash that would judge THIS replay: the candidate's own
    // verification is byte-identical to the incumbent's
    // (`fixed_fields_match` never lets it move), so this is really asking
    // whether the REPOSITORY's verification has moved since the source run
    // — a comparison judged by different checks is not a comparison.
    let profile_name = &source_contract.verification_profile;
    let Some(profile) = candidate.policy().verification.profiles.get(profile_name) else {
        eprintln!(
            "relais dataset replay: verification profile `{profile_name}` is not declared in \
             the candidate policy"
        );
        return Ok(CliOutcome::InvalidInput);
    };
    let replay_profile_hash = profile.hash();
    if replay_profile_hash != source_profile_hash {
        eprintln!(
            "relais dataset replay: verification profile hash mismatch — the source run {} was \
             judged by `{source_profile_hash}`, but a replay would be judged by \
             `{replay_profile_hash}`; a comparison judged by different checks is not a \
             comparison",
            source_run
        );
        return Ok(CliOutcome::Blocked);
    }

    let incumbent_eligible = route::eligible_tiers(&source_contract, &incumbent, &incumbent.models);
    let incumbent_recipe_id =
        route::covering_recipe_id(&source_contract, &incumbent, &incumbent_eligible.tiers)
            .unwrap_or_default();
    let candidate_eligible = route::eligible_tiers(
        &source_contract,
        candidate.policy(),
        &candidate.policy().models,
    );
    let arm_recipe = route::covering_recipe_id(
        &source_contract,
        candidate.policy(),
        &candidate_eligible.tiers,
    );

    println!(
        "this replay is one arm's result; it does not compare, score, rank or promote anything"
    );
    if dry_run {
        println!("dry run: nothing was dispatched, no trial was recorded, no usage was spent");
        println!("would replay source run: {source_run} (task {task_id})");
        println!(
            "candidate recipe: {}",
            arm_recipe
                .as_deref()
                .unwrap_or("(no recipe covers this task under the candidate)")
        );
        println!("verification profile hash checked: {replay_profile_hash}");
        return Ok(CliOutcome::Accepted);
    }
    let Some(arm_recipe) = arm_recipe else {
        eprintln!("relais dataset replay: no recipe in the candidate policy covers task {task_id}");
        return Ok(CliOutcome::Blocked);
    };

    let machine = load_machine()?;
    let artifacts_dir = paths::runs_dir().map_err(CliError::Home)?;
    let ids = id_source();
    let replay_id = match ids.mint_replay_trial() {
        Ok(id) => id,
        Err(e) => {
            eprintln!("relais dataset replay: {e}");
            return Ok(CliOutcome::OperationalFailure);
        }
    };

    // The isolated workspace (SPEC §24): a fresh checkout at the source
    // run's base SHA, never a worktree of the live repository — so the
    // accepted answer this task already earned is not merely hidden from
    // the tree, it was never fetched into this repository's object store.
    let checkout_dir = worktree_root(&artifacts_dir)
        .join("replay")
        .join(replay_id.as_str());
    if let Err(e) = workspace::create_replay_checkout(&root, &base_sha, &checkout_dir) {
        eprintln!("relais dataset replay: {e}");
        return Ok(CliOutcome::OperationalFailure);
    }
    let replay_contract = TaskContract {
        base_ref: base_sha.clone(),
        ..source_contract.clone()
    };

    let backend = match relais::adapter::claude::ClaudeBackend::discover() {
        Ok(backend) => std::sync::Arc::from(backend),
        Err(e) => {
            eprintln!("relais dataset replay: {e}");
            return Ok(CliOutcome::Blocked);
        }
    };
    let git = relais::workspace::SystemGit;
    let aval_resolver = relais::context::AvalCli::new(checkout_dir.clone());
    let hooks = relais::verify::AmontCli::new();
    let attest = relais::verify::AmontCli::new();
    let worker_env = relais::backend::LaunchEnv::from_process_env();
    let socket = relais::coordinator::socket_path().map_err(CliError::Home)?;
    if let Err(e) = relais::coordinator::ensure_running(&socket) {
        eprintln!("relais dataset replay: blocked (admission_unavailable): {e}");
        return Ok(CliOutcome::Blocked);
    }
    let gate = relais::coordinator::RemoteGate::new(socket);

    let session = relais::coordinator::resolve_session();
    // Same placement as `relais run`: before `execute`, so it is visible
    // on every path rather than deferred to the end of a run that spends
    // real money and can take minutes.
    eprintln!("session: {} ({})", session.id, session.source);
    if session.source.is_fallback() {
        eprintln!(
            "relais dataset replay: warning: no CLAUDE_CODE_SESSION_ID or RELAIS_SESSION_ID in \
             the environment; attributing this replay to {}",
            session.source
        );
    }
    let outcome = execute(&RunConfig {
        repo_dir: &checkout_dir,
        contract: &replay_contract,
        repo_policy: candidate.policy(),
        machine: &machine,
        ledger: &ledger,
        ids: &ids,
        backend: backend.as_ref(),
        git: &git,
        hooks: &hooks,
        attest: &attest,
        worker_env,
        sandbox_host: &relais::sandbox::RealSandboxHost,
        artifacts_dir: artifacts_dir.clone(),
        // Where a sandboxed worker's short temp-dir link goes (unused off unix).
        tmp_link_root: std::path::PathBuf::from("/tmp"),
        aval_resolver: &aval_resolver,
        predictor: None,
        gate: Some(&gate),
        session_id: session.id.clone(),
        heartbeat_every: std::time::Duration::from_secs(30),
        task_override: Some(&task_id),
        purpose: Some(RunPurpose::Replay),
        run_id: None,
        worker_presentation: relais::backend::Presentation::Headless,
    });
    // Bring the replay's own refs into the live repository BEFORE the
    // checkout goes. The runner names its candidate and snapshot refs in
    // `repo_dir`, which here is the scratch checkout, and the receipt's
    // `candidate_sha` and the run's `repo_path` point into it — so
    // deleting it first left the ledger describing objects that no longer
    // existed, and an arm that "earned its result" had nothing to show.
    let fetched = relais::workspace::fetch_candidate_refs(&root, &checkout_dir);
    if let Err(e) = &fetched {
        eprintln!(
            "relais dataset replay: could not bring the replay's refs into {}: {e}. Leaving the \
             checkout at {} so the result is not lost.",
            root.display(),
            checkout_dir.display()
        );
    }
    // Only once the refs are safe. A failure to remove is logged, never
    // fatal; a checkout kept because its refs could not be saved is the
    // better outcome of the two.
    if fetched.is_ok() {
        if let Err(e) = std::fs::remove_dir_all(&checkout_dir) {
            eprintln!("relais dataset replay: could not remove the replay checkout: {e}");
        }
    }
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(e) => {
            eprintln!("relais dataset replay: {e}");
            return Ok(CliOutcome::OperationalFailure);
        }
    };

    // The purpose was written with the run row (see `RunConfig::purpose`),
    // so there is nothing to stamp here.

    let (trial_outcome, accepted_without_escalation) =
        live_trial::trial_outcome_of(&outcome.terminal);
    let (trial_cost, duration_ms) = operational(
        live_trial::settled_figures(&ledger, &outcome.run_id),
        "dataset replay",
    )?;

    operational(
        ledger.insert_replay_trial(&relais::ledger::NewReplayTrial {
            trial_id: &replay_id,
            task_id: &task_id,
            source_run_id: &source_run,
            incumbent_recipe_id: &incumbent_recipe_id,
            arm_recipe_id: &arm_recipe,
            base_sha: &base_sha,
            contract_hash: &contract_hash,
            verification_profile_hash: &replay_profile_hash,
            workspace_isolation: "fresh_checkout_no_accepted_answer",
            arm_run_id: &outcome.run_id,
        }),
        "dataset replay",
    )?;
    operational(
        ledger.settle_trial(
            &replay_id,
            trial_outcome,
            accepted_without_escalation,
            trial_cost,
            duration_ms,
        ),
        "dataset replay",
    )?;

    println!("replay run: {}", outcome.run_id());
    println!("replay record: {replay_id}");
    println!("outcome:       {trial_outcome}");
    println!(
        "this replay is one arm's result; it does not compare, score, rank or promote anything"
    );
    Ok(CliOutcome::Accepted)
}

/// `kind` as `list`/`show` print it — never `Debug`'s capitalised
/// spelling, and named `unset` rather than empty when the recipe leaves
/// it unspecified.
fn kind_label(kind: Option<relais::contract::Kind>) -> &'static str {
    match kind {
        None => "unset",
        Some(relais::contract::Kind::Change) => "change",
        Some(relais::contract::Kind::Inspect) => "inspect",
    }
}

fn scope_label(scope_within: &[String]) -> String {
    if scope_within.is_empty() {
        "**".to_string()
    } else {
        scope_within.join(",")
    }
}

/// List every recipe `relais.toml` declares, one per line, in
/// declaration order (SPEC §26). Read-only: opens nothing but the
/// repository's own policy file.
fn recipe_list_command() -> Result<CliOutcome, CliError> {
    let (_root, policy) = load_repo_policy()?;
    if policy.recipes.is_empty() {
        println!("relais.toml declares no recipes");
        return Ok(CliOutcome::Accepted);
    }
    for recipe in &policy.recipes {
        println!(
            "{name}\trevision={revision}\tenabled={enabled}\ttier={tier}\tkind={kind}\t\
             scope={scope}\trecipe_id={id}",
            name = recipe.name,
            revision = recipe.revision,
            enabled = recipe.enabled,
            tier = recipe.tier.as_str(),
            kind = kind_label(recipe.kind),
            scope = scope_label(&recipe.scope_within),
            id = recipe.recipe_id(),
        );
    }
    Ok(CliOutcome::Accepted)
}

/// Show every revision of one recipe, oldest first (SPEC §26). An
/// unknown name is refused, naming the recipes that do exist, rather
/// than printing nothing.
fn recipe_show_command(name: &str) -> Result<CliOutcome, CliError> {
    let (_root, policy) = load_repo_policy()?;
    let mut revisions: Vec<&RecipeSpec> =
        policy.recipes.iter().filter(|r| r.name == name).collect();
    if revisions.is_empty() {
        let known: Vec<&str> = policy.recipes.iter().map(|r| r.name.as_str()).collect();
        let detail = if known.is_empty() {
            format!("relais recipe show: no recipe named `{name}`; relais.toml declares no recipes")
        } else {
            format!(
                "relais recipe show: no recipe named `{name}`; declared recipes: {}",
                known.join(", ")
            )
        };
        return Err(CliError::Usage { detail });
    }
    revisions.sort_by_key(|r| r.revision);
    for recipe in revisions {
        println!(
            "{name} revision {revision} (recipe_id={id}, enabled={enabled}, tier={tier}, \
             kind={kind}, scope={scope})",
            name = recipe.name,
            revision = recipe.revision,
            id = recipe.recipe_id(),
            enabled = recipe.enabled,
            tier = recipe.tier.as_str(),
            kind = kind_label(recipe.kind),
            scope = scope_label(&recipe.scope_within),
        );
        // `models` is read by routing. `execution`, `context` and `review`
        // are DECLARED AND HASHED, NOT YET READ by routing: the exact
        // caveat `RecipeSpec::execution`'s field doc carries, repeated
        // here rather than left implicit, so this command cannot be read
        // as saying those blocks take effect.
        if let Some(models) = &recipe.models {
            println!(
                "  models: {}",
                serde_json::to_string(models).unwrap_or_default()
            );
        }
        for (label, json) in [
            (
                "execution",
                recipe
                    .execution
                    .as_ref()
                    .map(|v| serde_json::to_string(v).unwrap_or_default()),
            ),
            (
                "context",
                recipe
                    .context
                    .as_ref()
                    .map(|v| serde_json::to_string(v).unwrap_or_default()),
            ),
            (
                "review",
                recipe
                    .review
                    .as_ref()
                    .map(|v| serde_json::to_string(v).unwrap_or_default()),
            ),
        ] {
            if let Some(json) = json {
                println!("  {label}: {json} — DECLARED AND HASHED, NOT YET READ by routing");
            }
        }
    }
    Ok(CliOutcome::Accepted)
}

/// Mirrors `RecipeSpec` field for field, but with no `skip_serializing_if`:
/// every field always appears in the JSON, holding the recipe's EFFECTIVE
/// value, never omitted just because it equals a default. `RecipeSpec`'s
/// own `Serialize` skips default-valued fields (so a `relais.toml` written
/// before a field existed keeps hashing the same); comparing that compact
/// form directly would print a defaulted field as `null` instead of the
/// value it actually resolves to.
///
/// Built by destructuring `RecipeSpec` with no `..`, so this fails to
/// compile — not silently drops the new field — the day `RecipeSpec`
/// gains one.
#[derive(Serialize)]
struct RecipeSpecFull {
    name: String,
    kind: Option<relais::contract::Kind>,
    scope_within: Vec<String>,
    tier: relais::policy::Tier,
    revision: u32,
    enabled: bool,
    models: Option<std::collections::BTreeMap<relais::policy::Tier, relais::policy::ModelProfile>>,
    execution: Option<relais::policy::ExecutionPolicy>,
    context: Option<relais::policy::ContextPolicy>,
    review: Option<relais::contract::Review>,
}

impl From<&RecipeSpec> for RecipeSpecFull {
    fn from(spec: &RecipeSpec) -> Self {
        let RecipeSpec {
            name,
            kind,
            scope_within,
            tier,
            revision,
            enabled,
            models,
            execution,
            context,
            review,
        } = spec.clone();
        Self {
            name,
            kind,
            scope_within,
            tier,
            revision,
            enabled,
            models,
            execution,
            context,
            review,
        }
    }
}

/// Every field the two recipes' EFFECTIVE values disagree on, name and
/// JSON value on each side. Derived from [`RecipeSpecFull`]'s serialized
/// form rather than a hand-picked field list, so a field added to
/// `RecipeSpec` later appears here without anyone remembering to add it.
fn recipe_field_diffs(before: &RecipeSpec, after: &RecipeSpec) -> Vec<(String, String, String)> {
    let before_value =
        serde_json::to_value(RecipeSpecFull::from(before)).expect("RecipeSpecFull serializes");
    let after_value =
        serde_json::to_value(RecipeSpecFull::from(after)).expect("RecipeSpecFull serializes");
    let (serde_json::Value::Object(before_map), serde_json::Value::Object(after_map)) =
        (&before_value, &after_value)
    else {
        unreachable!("RecipeSpec always serializes to a JSON object")
    };
    let mut keys: std::collections::BTreeSet<&String> = before_map.keys().collect();
    keys.extend(after_map.keys());
    let mut diffs = Vec::new();
    for key in keys {
        let a = before_map
            .get(key)
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let b = after_map
            .get(key)
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        if a != b {
            diffs.push((key.clone(), a.to_string(), b.to_string()));
        }
    }
    diffs
}

/// Compare a candidate `relais.toml`'s recipes against the repository's
/// own, per recipe and revision, naming every field that differs; then
/// state whether `route::validate_candidate` would admit the candidate
/// (SPEC §26). Read-only throughout: nothing here grants, replays,
/// evaluates or writes anything.
fn recipe_diff_command(candidate_path: &Path) -> Result<CliOutcome, CliError> {
    let (_root, incumbent) = load_repo_policy()?;
    let candidate_text =
        std::fs::read_to_string(candidate_path).map_err(|cause| CliError::Read {
            what: "the candidate recipe",
            path: candidate_path.to_path_buf(),
            cause,
        })?;
    let candidate_policy =
        RepoPolicy::from_toml_str(&candidate_text).map_err(|cause| CliError::Invalid {
            what: "the candidate recipe",
            path: candidate_path.to_path_buf(),
            cause: Box::new(cause),
        })?;

    let incumbent_by_key: std::collections::BTreeMap<(String, u32), &RecipeSpec> = incumbent
        .recipes
        .iter()
        .map(|r| ((r.name.clone(), r.revision), r))
        .collect();
    let candidate_by_key: std::collections::BTreeMap<(String, u32), &RecipeSpec> = candidate_policy
        .recipes
        .iter()
        .map(|r| ((r.name.clone(), r.revision), r))
        .collect();
    let mut keys: std::collections::BTreeSet<(String, u32)> =
        incumbent_by_key.keys().cloned().collect();
    keys.extend(candidate_by_key.keys().cloned());

    let mut any_difference = false;
    for key @ (name, revision) in &keys {
        match (incumbent_by_key.get(key), candidate_by_key.get(key)) {
            (Some(before), Some(after)) => {
                let diffs = recipe_field_diffs(before, after);
                if diffs.is_empty() {
                    continue;
                }
                any_difference = true;
                println!("{name} revision {revision}:");
                for (field, before_json, after_json) in diffs {
                    println!("  {field}: {before_json} -> {after_json}");
                }
            }
            (None, Some(_)) => {
                any_difference = true;
                println!("{name} revision {revision}: only in the candidate");
            }
            (Some(_), None) => {
                any_difference = true;
                println!("{name} revision {revision}: only in the repository");
            }
            (None, None) => unreachable!("every key comes from one side or the other"),
        }
    }
    if !any_difference {
        println!("no differences between the repository's recipes and the candidate's");
    }

    let bounds = tuning_bounds(&incumbent);
    match route::validate_candidate(&incumbent, &candidate_policy, &bounds) {
        Ok(_) => println!(
            "admissible: route::validate_candidate would ADMIT this candidate — admissible is \
             not approved: it still needs its own trust grant, and nothing here grants, \
             replays, evaluates or writes anything"
        ),
        Err(rejection) => println!(
            "not admissible: route::validate_candidate would REJECT this candidate: \
             {rejection} — admissible is not approved either way: nothing here grants, \
             replays, evaluates or writes anything"
        ),
    }
    Ok(CliOutcome::Accepted)
}

/// Read the settled trials for a candidate recipe and print a paired
/// comparison against the incumbent (SPEC §25). This command reads and
/// reports; it never promotes, writes a policy, or issues a grant —
/// `relais promote` is the only thing in this crate that activates
/// anything, and it takes a learned artifact id, not a recipe.
fn recipe_evaluate_command(candidate_path: &Path) -> Result<CliOutcome, CliError> {
    let (_root, incumbent) = load_repo_policy()?;
    let candidate_text =
        std::fs::read_to_string(candidate_path).map_err(|cause| CliError::Read {
            what: "the candidate recipe",
            path: candidate_path.to_path_buf(),
            cause,
        })?;
    let candidate_policy =
        RepoPolicy::from_toml_str(&candidate_text).map_err(|cause| CliError::Invalid {
            what: "the candidate recipe",
            path: candidate_path.to_path_buf(),
            cause: Box::new(cause),
        })?;
    // Admitted through the same door a learner's proposal, or a replay's
    // candidate, is — never a second, looser one for evaluation.
    let bounds = tuning_bounds(&incumbent);
    let candidate = match route::validate_candidate(&incumbent, &candidate_policy, &bounds) {
        Ok(candidate) => candidate,
        Err(rejection) => {
            eprintln!("relais recipe evaluate: candidate refused: {rejection}");
            return Ok(CliOutcome::Blocked);
        }
    };

    let ledger = open_ledger()?;
    let report = operational(
        evaluate_candidate(
            &ledger,
            &incumbent,
            candidate.policy(),
            MIN_PAIRED_TASKS,
            DEFAULT_BOOTSTRAP_RESAMPLES,
        ),
        "recipe evaluate",
    )?;
    print!("{}", report.render());
    Ok(CliOutcome::Accepted)
}

/// What `relais plan` prints for a repository whose policy has no trust
/// grant yet: the block a person reviews and pastes into machine.toml.
/// `recipe promote` and `recipe rollback` print the same block for the
/// policy they would produce, so it is one text, not two.
fn missing_grant_text(grant_key: &str, repo_identity: &RepoIdentity) -> Result<String, CliError> {
    Ok(format!(
        "trust grant: MISSING for {grant_key}. Review the execution declaration, then paste this \n\
         into {}:\n\n\
         [trust.\"{grant_key}\"]\n\
         granted_at = \"{}\"\n\
         reviewed_by = \"<your name>\"\n\
         repo = \"{}\"\n\
         note = \"<what you reviewed>\"\n",
        paths::machine_settings_path()
            .map_err(CliError::Home)?
            .display(),
        chrono::Utc::now().format("%Y-%m-%d"),
        repo_identity.label(),
    ))
}

/// Whether an amendment is shown or appended. A parameter of its own
/// rather than a `bool`, so a call site says which it means.
#[derive(Clone, Copy)]
enum AmendMode {
    Print,
    Write,
}

impl AmendMode {
    fn from_flag(write: bool) -> Self {
        if write {
            Self::Write
        } else {
            Self::Print
        }
    }
}

/// Show an amendment: the fragment(s) it appends, then the trust block
/// for the policy that would result; with [`AmendMode::Write`], append
/// the fragment(s) to relais.toml as text. Never rewrites the file,
/// never touches machine.toml, never issues a grant (SPEC §27).
fn present_amendment(
    root: &Path,
    incumbent: &RepoPolicy,
    amendment: &Amendment,
    mode: AmendMode,
) -> Result<CliOutcome, CliError> {
    let path = root.join("relais.toml");
    let subject = path.display().to_string();
    let fragment = amendment.fragment(&subject).map_err(CliError::Amend)?;
    println!("recipe revision(s) to append to {subject}:\n\n{fragment}");
    let resulting = amendment.resulting_policy(incumbent);
    let authority_hash = resulting.authority_hash();
    let repo_identity = relais::repo::identity(root);
    println!(
        "policy hash after this change: {authority_hash}\n{}",
        missing_grant_text(&grant_key(&authority_hash, &repo_identity), &repo_identity)?
    );
    match mode {
        AmendMode::Print => {
            println!("nothing was written; re-run with --write to append the fragment");
        }
        AmendMode::Write => {
            let existing = std::fs::read_to_string(&path).map_err(|cause| CliError::Read {
                what: "the repository policy",
                path: path.clone(),
                cause,
            })?;
            let appendix = amendment
                .appendix(&existing, &subject)
                .map_err(CliError::Amend)?;
            let append_failed = |cause| CliError::AppendPolicy {
                operation: amendment.operation(),
                path: path.clone(),
                cause,
            };
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .map_err(append_failed)?;
            std::io::Write::write_all(&mut file, appendix.as_bytes()).map_err(append_failed)?;
            println!(
                "appended to {subject}; no trust grant was issued, so the next `relais plan` \
                 reports missing_trust_grant until the block above is pasted into machine.toml"
            );
        }
    }
    Ok(CliOutcome::Accepted)
}

/// Add a candidate's new recipe revision(s) to the repository's policy,
/// only when the recomputed comparison clears every gate (SPEC §27).
fn recipe_promote_command(candidate_path: &Path, mode: AmendMode) -> Result<CliOutcome, CliError> {
    let (root, incumbent) = load_repo_policy()?;
    let candidate_text =
        std::fs::read_to_string(candidate_path).map_err(|cause| CliError::Read {
            what: "the candidate recipe",
            path: candidate_path.to_path_buf(),
            cause,
        })?;
    let candidate_policy =
        RepoPolicy::from_toml_str(&candidate_text).map_err(|cause| CliError::Invalid {
            what: "the candidate recipe",
            path: candidate_path.to_path_buf(),
            cause: Box::new(cause),
        })?;
    let bounds = tuning_bounds(&incumbent);
    let candidate = match route::validate_candidate(&incumbent, &candidate_policy, &bounds) {
        Ok(candidate) => candidate,
        Err(rejection) => {
            eprintln!("relais recipe promote: candidate refused: {rejection}");
            return Ok(CliOutcome::Blocked);
        }
    };

    // Recomputed here, from the ledger's settled trials — never read from
    // a verdict an earlier `recipe evaluate` printed.
    let ledger = open_ledger()?;
    let report = operational(
        evaluate_candidate(
            &ledger,
            &incumbent,
            candidate.policy(),
            MIN_PAIRED_TASKS,
            DEFAULT_BOOTSTRAP_RESAMPLES,
        ),
        "recipe promote",
    )?;
    let subject = candidate_path.display().to_string();
    let proof = match admit(&report, &subject) {
        Ok(proof) => proof,
        Err(refused) => {
            eprintln!("{refused}");
            eprint!("{}", report.render_evidence());
            eprintln!("nothing was written");
            return Ok(CliOutcome::PromotionRefused);
        }
    };
    let amendment = Amendment::for_promotion(proof, &incumbent, candidate.policy(), &subject)
        .map_err(CliError::Amend)?;
    println!("evaluation this rests on:");
    print!("{}", report.render_evidence());
    println!();
    present_amendment(&root, &incumbent, &amendment, mode)
}

/// Restore the revision below the effective one by appending a new
/// revision that repeats it (SPEC §27). Removing a change needs no
/// evaluation.
fn recipe_rollback_command(name: &str, mode: AmendMode) -> Result<CliOutcome, CliError> {
    let (root, incumbent) = load_repo_policy()?;
    let amendment = Amendment::for_rollback(&incumbent, name).map_err(CliError::Amend)?;
    present_amendment(&root, &incumbent, &amendment, mode)
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
    // A per-run query (`outcome_of_run`), not the task-scoped
    // `latest_outcome`: a later run of the same task recording its own
    // feedback must not hide an earlier run's outcome when THAT run is
    // the one being explained (SPEC §20, issue #98).
    if let Some(stored) = operational(ledger.outcome_of_run(&run), "explain")? {
        println!(
            "outcome: {}{}",
            stored.outcome.kind.as_str(),
            stored
                .outcome
                .detail
                .note
                .as_deref()
                .map(|note| format!(" — {note}"))
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
            if row.kind == EvidenceKind::SandboxDenials {
                println!("{}", sandbox_denials_text(&row.path));
            }
        }
    }
    Ok(CliOutcome::Accepted)
}

/// The denial report a sandboxed attempt recorded, rendered; a file that
/// cannot be read or parsed is said so rather than skipped.
fn sandbox_denials_text(path: &str) -> String {
    let report = std::fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|body| {
            serde_json::from_str::<relais::sandbox::DenialReport>(&body).map_err(|e| e.to_string())
        });
    match report {
        Ok(report) => report.render(),
        Err(why) => relais::sandbox::DenialReport::render_unreadable(&why),
    }
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
    no_import: bool,
) -> Result<CliOutcome, CliError> {
    let since = since.map(|since| since.to_string()).unwrap_or_else(|| {
        chrono::Utc::now()
            .format("%Y-%m-01T00:00:00+00:00")
            .to_string()
    });
    let ledger = open_ledger()?;
    let mut not_imported = NotImported::default();
    if !no_import {
        let projects_dir = paths::claude_projects_dir().map_err(CliError::Home)?;
        let price_table = load_price_table()?;
        not_imported = import_for_report(&ledger, &projects_dir, &price_table, &since)?;
    }
    let mut report = operational(report::runs_report(&ledger, &since, by), "report")?;
    report.orchestration.unattributable = not_imported.unattributable;
    report.orchestration.transcript_missing = not_imported.transcript_missing;
    // Best-effort: a report is still a report with no coordinator
    // reachable, and `EnforcementReport::observed` already says so
    // honestly rather than this command failing over it.
    report.enforcement = relais::coordinator::socket_path()
        .ok()
        .map(relais::coordinator::Client::new)
        .and_then(|client| client.status().ok())
        .map(|snapshot| {
            // A snapshot came back: a coordinator is serving. The
            // fallback below is the other half of the same question, so
            // neither is a free-standing claim about enforcement.
            report::EnforcementReport::from_snapshot(
                relais::admission::Enforcement::Coordinator,
                &snapshot,
            )
        })
        .unwrap_or_else(report::EnforcementReport::observed);
    if json {
        print_document(&report)?;
    } else {
        print!("{}", report.render());
    }
    Ok(CliOutcome::Accepted)
}

/// `~/.config/relais/machine.toml`'s `[pricing]` table, or an empty one
/// when the file is absent — the same "absent settings means the
/// defaults" posture `coordinator daemon` already takes reading this
/// file, not an error just because nobody has priced anything yet.
fn load_price_table() -> Result<PriceTable, CliError> {
    let path = paths::machine_settings_path().map_err(CliError::Home)?;
    match std::fs::read_to_string(&path) {
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => Ok(PriceTable::empty()),
        Err(cause) => Err(CliError::Read {
            what: "the machine settings",
            path,
            cause,
        }),
        Ok(text) => match MachineSettings::from_toml_str(&text) {
            Ok(machine) => Ok(machine.pricing.unwrap_or_else(PriceTable::empty)),
            Err(e) => Err(CliError::Invalid {
                what: "the machine settings",
                path,
                cause: Box::new(e),
            }),
        },
    }
}

/// Every run recorded before the session-identity fix (#119) keyed a run
/// by the bare shell PID relais fell back to — a value that was never a
/// Claude Code session id, so it can never have a transcript. Reported
/// as `unattributable`, counted, never guessed at (SPEC §11); distinct
/// from a session that SHOULD have a transcript but none was found,
/// which is `transcript missing` instead.
fn is_bare_shell_pid(session_id: &str) -> bool {
    !session_id.is_empty() && session_id.chars().all(|c| c.is_ascii_digit())
}

/// One session's message count, token totals and cost, imported.
struct ImportSummary {
    messages: usize,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_5m_tokens: u64,
    cache_write_1h_tokens: u64,
    cost: MicroUsd,
    completeness: CostCompleteness,
}

/// Sessions `relais report` could not import, counted only for sessions
/// that started a run inside the report window.
#[derive(Debug, Default, PartialEq, Eq)]
struct NotImported {
    unattributable: usize,
    transcript_missing: usize,
}

/// What `relais report` imports before it renders. EVERY orchestrating
/// session is imported, not only those whose run started inside the
/// window: a session that started a run on the 30th and kept reviewing
/// and landing work on the 1st and 2nd spends inside a window its run
/// predates, and that spend is attributed by each message's own
/// timestamp (SPEC §11) — but only once it has been imported. Import is
/// idempotent (`message_id` is unique), so re-reading a session costs a
/// parse and inserts nothing. The window narrows only the COUNTS of
/// sessions that could not be imported, so a pre-fix PID session from
/// months ago is not reported as missing on every later report.
fn import_for_report(
    ledger: &Ledger,
    projects_dir: &Path,
    price_table: &PriceTable,
    since: &str,
) -> Result<NotImported, CliError> {
    let in_window: std::collections::BTreeSet<String> =
        operational(ledger.distinct_root_sessions_since(since), "usage import")?
            .into_iter()
            .collect();
    let mut not_imported = NotImported::default();
    for session_id in operational(ledger.distinct_root_sessions(), "usage import")? {
        let counted = in_window.contains(&session_id);
        match import_session(ledger, projects_dir, price_table, &session_id)? {
            SessionImportOutcome::Imported(_) => {}
            SessionImportOutcome::Unattributable if counted => not_imported.unattributable += 1,
            SessionImportOutcome::TranscriptMissing if counted => {
                not_imported.transcript_missing += 1
            }
            SessionImportOutcome::Unattributable | SessionImportOutcome::TranscriptMissing => {}
        }
    }
    Ok(not_imported)
}

enum SessionImportOutcome {
    Imported(ImportSummary),
    Unattributable,
    TranscriptMissing,
}

/// Import one session's orchestration usage: its main transcript and
/// every subagent file beside it (SPEC §11). Idempotent — every insert
/// goes through `record_orchestration_usage`'s `message_id` uniqueness,
/// so importing the same session twice inserts nothing the second time.
fn import_session(
    ledger: &Ledger,
    projects_dir: &Path,
    price_table: &PriceTable,
    session_id: &str,
) -> Result<SessionImportOutcome, CliError> {
    if is_bare_shell_pid(session_id) {
        return Ok(SessionImportOutcome::Unattributable);
    }
    let Some(main_path) = paths::find_transcript(projects_dir, session_id) else {
        return Ok(SessionImportOutcome::TranscriptMissing);
    };
    let mut files = vec![(TranscriptSource::Main, main_path.clone())];
    for path in paths::subagent_transcripts(&main_path) {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("subagent")
            .to_string();
        files.push((TranscriptSource::Subagent(name), path));
    }
    let mut summary = ImportSummary {
        messages: 0,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_5m_tokens: 0,
        cache_write_1h_tokens: 0,
        cost: MicroUsd::ZERO,
        completeness: CostCompleteness::Actual,
    };
    let mut parts = Vec::new();
    for (source, path) in files {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(cause) => {
                return Err(CliError::Read {
                    what: "a transcript file",
                    path,
                    cause,
                })
            }
        };
        for record in orchestration::parse_transcript(&text) {
            let priced = orchestration::price(&record, price_table);
            summary.messages += 1;
            summary.input_tokens += record.input_tokens;
            summary.output_tokens += record.output_tokens;
            summary.cache_read_tokens += record.cache_read_input_tokens;
            summary.cache_write_5m_tokens += record.cache_writes.ephemeral_5m_input_tokens;
            summary.cache_write_1h_tokens += record.cache_writes.ephemeral_1h_input_tokens;
            if let Some(cost) = priced.cost {
                summary.cost = summary.cost.saturating_add(cost);
            }
            parts.push(priced.completeness);
            let row = OrchestrationUsageRow {
                message_id: record.message_id,
                session_id: session_id.to_string(),
                transcript: source.clone(),
                model: record.model,
                speed: record.speed,
                input_tokens: record.input_tokens,
                output_tokens: record.output_tokens,
                cache_read_tokens: record.cache_read_input_tokens,
                cache_write_5m_tokens: record.cache_writes.ephemeral_5m_input_tokens,
                cache_write_1h_tokens: record.cache_writes.ephemeral_1h_input_tokens,
                cost: priced.cost,
                pricing_version: if priced.pricing_version.is_empty() {
                    None
                } else {
                    Some(priced.pricing_version)
                },
                completeness: priced.completeness,
                at: record.timestamp,
            };
            operational(ledger.record_orchestration_usage(&row), "usage import")?;
        }
    }
    summary.completeness = CostCompleteness::worst(parts);
    Ok(SessionImportOutcome::Imported(summary))
}

fn usage_import_command(
    session: Option<&str>,
    projects_dir: Option<&Path>,
) -> Result<CliOutcome, CliError> {
    let ledger = open_ledger()?;
    let projects_dir = match projects_dir {
        Some(dir) => dir.to_path_buf(),
        None => paths::claude_projects_dir().map_err(CliError::Home)?,
    };
    let price_table = load_price_table()?;
    let sessions = match session {
        Some(id) => vec![id.to_string()],
        None => operational(ledger.distinct_root_sessions(), "usage import")?,
    };
    if sessions.is_empty() {
        println!("relais usage import: no session is the root of any run on record");
    }
    for session_id in sessions {
        match import_session(&ledger, &projects_dir, &price_table, &session_id)? {
            SessionImportOutcome::Imported(summary) => {
                println!(
                    "{session_id}: {} message(s), {} input + {} output tokens: {}",
                    summary.messages,
                    summary.input_tokens,
                    summary.output_tokens,
                    report::cost_line(summary.cost, summary.completeness),
                );
            }
            SessionImportOutcome::Unattributable => {
                println!(
                    "{session_id}: unattributable (root_session is a bare shell PID from \
                     before the session-identity fix; it never had a transcript)"
                );
            }
            SessionImportOutcome::TranscriptMissing => {
                println!("{session_id}: transcript missing");
            }
        }
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
        if operational(ledger.run_status(&candidate), "feedback")?.is_some_and(State::is_accepted) {
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
    note: Option<String>,
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
        note,
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
    // feedback is never a positive label. Only a run that reached an
    // accepted candidate — the runner's own verification, or a person's
    // salvage of a terminal run's work — has one whose later life is
    // worth recording.
    if !state.is_accepted() {
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
    // A salvaged run's candidate is never relais's own: it is whatever a
    // person named with `--candidate` on `relais decide --answer
    // salvaged`, read back from the same transition that recorded it —
    // never the receipt (a salvaged run never wrote one) and never the
    // latest attempt's own candidate (that is the work relais discarded,
    // not what the person actually merged).
    let recorded_candidate = if state == State::AcceptedByPerson {
        operational(ledger.salvaged_candidate(&run), "feedback")?
    } else {
        // A run accepted through a person's approval (`relais decide
        // --answer approve`) on a contract interrupted before
        // verification never wrote a receipt — SPEC's decision spine
        // reaches `accepted` without one. Feedback about it is still
        // worth recording; whenever a receipt DOES exist, it is still
        // the source of truth a `--candidate` is checked against (SPEC
        // §20: feedback is attributed to the candidate).
        match operational(ledger.receipt(&run), "feedback")? {
            Some((receipt, _hash)) => {
                let Some(sha) = receipt["candidate_sha"].as_str() else {
                    eprintln!("relais feedback: run {run}'s receipt names no candidate");
                    return Ok(CliOutcome::OperationalFailure);
                };
                Some(sha.to_string())
            }
            // No receipt, but the run may still have finished an attempt
            // and named what it built. That is the ledger's own answer,
            // and it stands in for the receipt here exactly as the
            // receipt would.
            None => operational(ledger.latest_attempt_candidate(&run), "feedback")?,
        }
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
        note,
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
    candidate: Option<&str>,
) -> Result<CliOutcome, CliError> {
    let ledger = open_ledger()?;
    let run = RunId::from_stored(run_id);
    let Some(state) = operational(ledger.run_status(&run), "decide")? else {
        eprintln!("relais decide: unknown run {run_id}");
        return Ok(CliOutcome::UnknownRun);
    };
    // `salvaged` answers a run no other answer can reach: one whose
    // candidate a person finished and merged themselves, whether the run
    // ended on its own (never awaiting a person, so it opened no decision
    // row for `resolve_decision` to close) or is still waiting on one (an
    // OPEN row of its own). It takes its own path below rather than
    // falling into the `awaits_a_person` gate every other answer shares.
    if answer == DecideAnswer::Salvaged {
        return salvage_command(
            &ledger,
            &run,
            state,
            &SalvageAnswer {
                actor,
                note,
                criterion,
                successor,
                candidate,
            },
        );
    }
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
                    Some(Evidence::Test { authorship, .. }) =>
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

/// The CLI flags `--answer salvaged` reads, grouped because they answer
/// one decision together (mirrors [`relais::ledger::DecisionAnswer`]).
struct SalvageAnswer<'a> {
    actor: &'a str,
    note: Option<&'a str>,
    criterion: Option<&'a str>,
    successor: Option<&'a str>,
    candidate: Option<&'a str>,
}

/// `relais decide --answer salvaged --candidate <sha>`: a run relais
/// itself never accepted, whose candidate a person finished and merged
/// anyway (SPEC's decision spine) — terminal, or still waiting on a
/// person. Distinct from every other answer in `decide_command`: a run
/// that never awaited a person opened no decision row for
/// `resolve_decision` to close, and even a run that IS waiting is
/// answered here rather than there, because this is about what happened
/// to the candidate, not about resolving the question the wait raised.
/// Either way the run's own reason for stopping is kept, not overwritten,
/// by [`Ledger::record_salvage`].
fn salvage_command(
    ledger: &Ledger,
    run: &RunId,
    state: State,
    answer: &SalvageAnswer<'_>,
) -> Result<CliOutcome, CliError> {
    let Some(candidate_sha) = answer.candidate else {
        eprintln!("relais decide: `--answer salvaged` requires `--candidate <sha>`");
        return Ok(CliOutcome::InvalidInput);
    };
    if answer.criterion.is_some() {
        eprintln!("relais decide: `--criterion` does not apply to `--answer salvaged`");
        return Ok(CliOutcome::InvalidInput);
    }
    if answer.successor.is_some() {
        eprintln!("relais decide: `--successor` does not apply to `--answer salvaged`");
        return Ok(CliOutcome::InvalidInput);
    }
    // Salvage answers a run that has stopped running, without relais
    // itself producing an accepted candidate: never one still in flight,
    // never one already accepted the ordinary way or already salvaged
    // once.
    if !state.is_terminal() || state.is_accepted() {
        eprintln!(
            "relais decide: run {run} is {state} — `salvaged` answers a run that has already \
             stopped without an accepted candidate, not one still in flight or already accepted"
        );
        return Ok(CliOutcome::InvalidInput);
    }
    let Some(original_reason) = operational(
        ledger.record_salvage(run, candidate_sha, answer.actor, answer.note),
        "decide",
    )?
    else {
        eprintln!("relais decide: run {run} already carries a decision on record");
        return Ok(CliOutcome::InvalidInput);
    };
    println!(
        "recorded {} for {run} by {} (originally ended: {})",
        Reason::DecisionSalvaged.as_str(),
        answer.actor,
        original_reason.as_str()
    );
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
    let runs_dir = paths::runs_dir().map_err(CliError::Home)?;
    reseal_receipt_in(ledger, &runs_dir, run, criterion_id)
}

/// [`reseal_receipt_with_signoff`] against an explicit `runs_dir`, the one
/// place a run's `receipt.json` lives.
fn reseal_receipt_in(
    ledger: &Ledger,
    runs_dir: &Path,
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
            criterion.settled_via = Some(SettledVia::HumanSignOff);
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
    if receipt.accepted_by_settlement() {
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
    let receipt_path = runs_dir.join(run.as_str()).join("receipt.json");
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

    fn temp_ledger(label: &str) -> (Ledger, relais::test_support::TempDir) {
        // Through the shared helper, not a hand-rolled path: this one
        // built its own `relais-main-test-*` directory under the
        // per-user temp root with no guard at all, which is the fourth
        // creator this change had to find — and a grep for
        // `from("/tmp")` does not see it.
        let dir = relais::test_support::short_temp_dir(&format!("main-{label}"));
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger opens");
        (ledger, dir)
    }

    fn session(
        source: relais::coordinator::SessionSource,
    ) -> relais::coordinator::ResolvedSessionId {
        relais::coordinator::ResolvedSessionId {
            id: "s1".into(),
            source,
        }
    }

    /// A native worker is spawned by a parent Claude Code session: a
    /// session id guessed from the parent process is refused, and a
    /// headless run never asks.
    #[test]
    fn a_native_run_needs_a_parent_claude_code_session() {
        use relais::coordinator::SessionSource;
        let wait = Some(std::time::Duration::from_secs(120));
        let refusal = native_session_refusal(wait, &session(SessionSource::ParentPidFallback))
            .expect("a fallback session is refused");
        assert!(refusal.contains("inside Claude Code"), "{refusal}");
        assert_eq!(
            native_session_refusal(wait, &session(SessionSource::ClaudeCode)),
            None
        );
        assert_eq!(
            native_session_refusal(None, &session(SessionSource::ParentPidFallback)),
            None
        );
    }

    #[test]
    fn the_native_spawn_wait_is_only_accepted_with_native() {
        let parse = |args: &[&str]| {
            Cli::try_parse_from(["relais", "run", "--task", "t.json"].iter().chain(args))
        };
        assert!(parse(&["--native", "--native-spawn-wait", "30"]).is_ok());
        assert!(parse(&["--native"]).is_ok());
        assert!(parse(&[]).is_ok());
        assert!(
            parse(&["--native-spawn-wait", "30"]).is_err(),
            "the wait means nothing without --native"
        );
    }

    /// `relais explain` says a report it cannot parse in one line within 80
    /// columns, however long the parse error is.
    #[test]
    fn an_unreadable_denial_report_is_one_line_within_80_columns() {
        let dir = relais::test_support::short_temp_dir("main-denials-text");
        let path = dir.join("sandbox-denials-1.json");
        let coverage = "x".repeat(300);
        std::fs::write(
            &path,
            format!(r#"{{"verified":[],"suspected":[],"coverage":"{coverage}"}}"#),
        )
        .expect("report written");
        let text = sandbox_denials_text(&path.to_string_lossy());
        assert!(text.starts_with("sandbox denials: unreadable ("), "{text}");
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(text.chars().count() <= 80, "{text}");
    }

    /// A session that started its run BEFORE the report window and kept
    /// orchestrating inside it: its in-window spend must be imported and
    /// counted by `relais report`'s own import path, not only by a
    /// hand-run `relais usage import`. Session S starts a run on 09-30,
    /// then spends on 10-02; the window opens on 10-01.
    ///
    /// FALSIFIED: with `import_for_report` iterating
    /// `distinct_root_sessions_since` (the reviewed patch), the in-window
    /// cost read back as $0 and this test failed; restored.
    #[test]
    fn report_imports_in_window_spend_of_a_session_whose_run_predates_the_window() {
        let dir = relais::test_support::short_temp_dir("main-orch-window");
        let ledger = Ledger::open_with_clock(
            &dir.join("ledger.sqlite"),
            Box::new(relais::ledger::FixedClock::new([
                "2026-09-30T23:00:00+00:00",
            ])),
        )
        .expect("ledger opens");
        ledger
            .insert_run(
                &relais::ids::RunId::from_stored("run-s"),
                "/repo",
                Some("sess-s"),
                &relais::ids::TaskId::from_stored("task-s"),
                "rk",
            )
            .expect("run before the window");
        let projects = dir.join("projects").join("-slug");
        std::fs::create_dir_all(&projects).expect("projects dir");
        std::fs::write(
            projects.join("sess-s.jsonl"),
            r#"{"type":"assistant","timestamp":"2026-10-02T10:00:00Z","message":{"id":"msg-oct","model":"claude-opus-5","usage":{"input_tokens":1000000,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":0}}}}"#,
        )
        .expect("transcript");
        let table = PriceTable {
            version: "test".into(),
            models: vec![relais::orchestration::ModelPrice {
                ids: vec!["claude-opus-5".into()],
                input: 5_000_000,
                output: 25_000_000,
                cache_read: 500_000,
                cache_write_5m: 6_250_000,
                cache_write_1h: 10_000_000,
                fast_input: None,
                fast_output: None,
            }],
        };
        let since = "2026-10-01T00:00:00+00:00";
        let not_imported =
            import_for_report(&ledger, &dir.join("projects"), &table, since).expect("imports");
        assert_eq!(not_imported, NotImported::default());
        let (cost, _) = ledger
            .orchestration_spend_since(since)
            .expect("spend in the window");
        assert_eq!(cost, MicroUsd::from_micros(5_000_000));
    }

    /// Walks every `DecideAnswer` and asserts the terminal state it
    /// assigns: `approve` accepts, `salvaged` reaches its own distinct
    /// accepted state, every other answer cancels. Matched directly over
    /// `DecideAnswer` in `terminal_state`, so a seventh variant added to
    /// that enum without a case there fails to compile this test along
    /// with everything else in the crate.
    #[test]
    fn decide_answers_every_variant_assigns_a_projected_status() {
        for answer in [
            DecideAnswer::Approve,
            DecideAnswer::Reject,
            DecideAnswer::Revise,
            DecideAnswer::Decided,
            DecideAnswer::Abandon,
            DecideAnswer::Salvaged,
        ] {
            let expected = match answer {
                DecideAnswer::Approve => State::Accepted,
                DecideAnswer::Salvaged => State::AcceptedByPerson,
                DecideAnswer::Reject
                | DecideAnswer::Revise
                | DecideAnswer::Decided
                | DecideAnswer::Abandon => State::Cancelled,
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
        CliOutcome::PromotionRefused,
    ];

    #[test]
    fn the_exit_code_table_is_total_and_injective() {
        let mut seen: Vec<i32> = ALL.iter().map(exit_code).collect();
        assert_eq!(seen.len(), 18, "every documented outcome is listed");
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

    fn probe_recipe_spec() -> RecipeSpec {
        RecipeSpec {
            name: "probe".to_string(),
            kind: None,
            scope_within: Vec::new(),
            tier: relais::policy::Tier::Implementation,
            revision: 0,
            enabled: true,
            models: None,
            execution: None,
            context: None,
            review: None,
        }
    }

    /// Changes exactly one field of `RecipeSpec` at a time, on the same
    /// `(name, revision)`, and asserts `recipe_field_diffs` names that
    /// field and nothing else. The destructure (no `..`) fails to compile
    /// when `RecipeSpec` gains a field; the key-set assertion then fails
    /// the test until that field also has a case, because a `new: _`
    /// binding alone would otherwise let it pass uncovered.
    ///
    /// FALSIFIED (key set): the `review` case was removed and the key-set
    /// assertion turned red before it was restored.
    ///
    /// FALSIFIED: `recipe_field_diffs` was changed to build its diff set
    /// from a struct literal that left `review` out of `RecipeSpecFull`,
    /// and the `review` case below turned red (`diffs.is_empty()`) before
    /// the omission was restored.
    #[test]
    fn recipe_field_diffs_names_exactly_the_changed_field() {
        let base = probe_recipe_spec();
        let RecipeSpec {
            name: _,
            kind: base_kind,
            scope_within: base_scope_within,
            tier: base_tier,
            revision: _,
            enabled: base_enabled,
            models: base_models,
            execution: base_execution,
            context: base_context,
            review: base_review,
        } = base.clone();

        let cases: Vec<(&str, RecipeSpec)> = vec![
            (
                "kind",
                RecipeSpec {
                    kind: Some(relais::contract::Kind::Change),
                    ..base.clone()
                },
            ),
            (
                "scope_within",
                RecipeSpec {
                    scope_within: vec!["src/**".to_string()],
                    ..base.clone()
                },
            ),
            (
                "tier",
                RecipeSpec {
                    tier: relais::policy::Tier::Escalation,
                    ..base.clone()
                },
            ),
            (
                "enabled",
                RecipeSpec {
                    enabled: !base_enabled,
                    ..base.clone()
                },
            ),
            (
                "models",
                RecipeSpec {
                    models: Some(std::collections::BTreeMap::from([(
                        relais::policy::Tier::Implementation,
                        relais::policy::ModelProfile {
                            id: "sonnet".to_string(),
                            effort: None,
                            max_effort: None,
                        },
                    )])),
                    ..base.clone()
                },
            ),
            (
                "execution",
                RecipeSpec {
                    execution: Some(relais::policy::ExecutionPolicy::default()),
                    ..base.clone()
                },
            ),
            (
                "context",
                RecipeSpec {
                    context: Some(relais::policy::ContextPolicy::default()),
                    ..base.clone()
                },
            ),
            (
                "review",
                RecipeSpec {
                    review: Some(relais::contract::Review::Required),
                    ..base.clone()
                },
            ),
        ];
        // The destructure above stands for the whole field list; assert
        // against it so the base used in every case really is the base.
        assert_eq!(base_kind, None);
        assert_eq!(base_scope_within, Vec::<String>::new());
        assert_eq!(base_tier, relais::policy::Tier::Implementation);
        assert!(base_enabled);
        assert_eq!(base_models, None);
        assert_eq!(base_execution, None);
        assert_eq!(base_context, None);
        assert_eq!(base_review, None);

        // The case list must name every field the diff compares. The
        // destructure above only forces a binding for a new field, not a
        // case, so tie the two together through the diff's own key set.
        let compared: std::collections::BTreeSet<String> =
            match serde_json::to_value(RecipeSpecFull::from(&base)) {
                Ok(serde_json::Value::Object(map)) => map
                    .keys()
                    .filter(|key| !matches!(key.as_str(), "name" | "revision"))
                    .cloned()
                    .collect(),
                other => panic!("RecipeSpecFull must serialize to an object, got {other:?}"),
            };
        let covered: std::collections::BTreeSet<String> = cases
            .iter()
            .map(|(field, _)| (*field).to_string())
            .collect();
        assert_eq!(
            covered, compared,
            "every field recipe_field_diffs compares needs a case here"
        );

        for (field, changed) in cases {
            assert_ne!(changed, base, "{field} case must actually change the spec");
            let diffs = recipe_field_diffs(&base, &changed);
            assert_eq!(
                diffs.len(),
                1,
                "{field}: expected exactly one changed field, got {diffs:?}"
            );
            assert_eq!(diffs[0].0, field, "{field}: {diffs:?}");
        }
    }

    /// `enabled`/`revision` skip serialization when they hold their
    /// default, so a naive diff over the two RAW serialized forms reads a
    /// defaulted field on one side as JSON `null` — never the value it
    /// actually resolves to. `recipe_field_diffs` must compare EFFECTIVE
    /// values instead: revision 1 `enabled = false` against a candidate
    /// that omits `enabled` (so it defaults to `true`) prints
    /// `enabled: false -> true`, never `-> null`.
    #[test]
    fn recipe_field_diffs_compares_effective_values_not_omitted_defaults() {
        let before = RecipeSpec {
            enabled: false,
            ..probe_recipe_spec()
        };
        let after = probe_recipe_spec();
        assert!(after.enabled, "the candidate leaves enabled at its default");

        let diffs = recipe_field_diffs(&before, &after);
        assert_eq!(diffs.len(), 1, "{diffs:?}");
        assert_eq!(diffs[0].0, "enabled");
        assert_eq!(diffs[0].1, "false");
        assert_eq!(
            diffs[0].2, "true",
            "the omitted default must compare as its effective value, never null: {diffs:?}"
        );
    }

    /// #118: `feedback --task` used to look for a run in `State::Accepted`
    /// only, so a task whose sole acceptance was a person's salvage
    /// (`State::AcceptedByPerson`) refused with "no accepted run on
    /// record" — even though `relais feedback <run-id>` on that very run
    /// accepts it. The task here carries two runs: the first reaches
    /// `needs_review` and is answered `revise` (cancelled, no
    /// acceptance), the second ends `failed` and is then salvaged. The
    /// `--task` lookup must land on the salvaged run.
    ///
    /// FALSIFIED: restricting `feedback_run`'s loop back to
    /// `Some(State::Accepted)` (the pre-fix condition) makes this test
    /// fail with `Err(CliOutcome::UnknownRun)`; restored to
    /// `State::is_accepted`.
    #[test]
    fn feedback_task_finds_a_run_the_only_acceptance_was_a_persons_salvage() {
        let (ledger, dir) = temp_ledger("salvage-task");
        let task = relais::ids::TaskId::from_stored("task-salvaged");

        let revised = RunId::from_stored("run-revised");
        ledger
            .insert_run(&revised, "/repo", None, &task, "rk")
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: revised.clone(),
                attempt_id: None,
                from_state: Some(State::Verifying),
                to_state: State::NeedsReview,
                reason: Reason::VerificationGap.as_str().into(),
                detail: None,
                at: ledger.now(),
            })
            .expect("transition");
        assert!(ledger
            .resolve_decision(
                &revised,
                &relais::ledger::DecisionAnswer {
                    resolution: Reason::DecisionRevised,
                    actor: "a person",
                    note: None,
                    successor_run: None,
                    from_state: State::NeedsReview,
                    to_state: State::Cancelled,
                },
            )
            .expect("resolve"));

        let salvaged = RunId::from_stored("run-salvaged");
        ledger
            .insert_run(&salvaged, "/repo", None, &task, "rk")
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: salvaged.clone(),
                attempt_id: None,
                from_state: Some(State::Verifying),
                to_state: State::Failed,
                reason: Reason::BehavioralFailure.as_str().into(),
                detail: None,
                at: ledger.now(),
            })
            .expect("transition");
        ledger
            .record_salvage(&salvaged, "deadbeef", "a person", None)
            .expect("salvage")
            .expect("a terminal run with no prior decision salvages");

        let found = feedback_run(&ledger, None, Some(task.as_str()))
            .expect("feedback_run")
            .expect("a salvaged run counts as accepted");
        assert_eq!(found, salvaged);

        std::fs::remove_dir_all(&dir).ok();
    }

    const SIGN_OFF_GAP: &str =
        "acceptance criterion `read` names a human sign-off, which nothing has recorded";

    /// A pending receipt as the runner stores it for a run waiting only on
    /// a sign-off: one criterion settled by the report review, one
    /// waiting on `read`.
    fn pending_receipt(kind: relais::contract::Kind, extra_gap: bool) -> Receipt {
        let criterion = |id: &str, evidence: Option<Evidence>, met: bool, via| {
            relais::verify::CriterionOutcome {
                id: id.into(),
                statement: id.into(),
                mandatory: true,
                met,
                evidence,
                settled_by: None,
                settled_via: Some(via),
            }
        };
        let mut gaps = vec![SIGN_OFF_GAP.to_string()];
        if extra_gap {
            gaps.push("another gap".into());
        }
        Receipt {
            run_id: "run-seal".into(),
            candidate_sha: "abc".into(),
            base_sha: "def".into(),
            contract_hash: "ch".into(),
            policy_hash: "ph".into(),
            outcome: State::NeedsDecision.as_str().into(),
            verification: relais::verify::VerificationReport {
                candidate_sha: "abc".into(),
                base_sha: "def".into(),
                contract_hash: "ch".into(),
                policy_hash: "ph".into(),
                checks: Vec::new(),
                gaps,
                gaps_not_judged: vec!["amont gap, not judged".into()],
                notes: Vec::new(),
                baseline_failures: vec!["base@1".into()],
                amont_bypasses: Vec::new(),
                amont_downgrades: Vec::new(),
                verification_inputs_changed: Vec::new(),
                integration_gaps: Vec::new(),
                baseline_cached: false,
                baseline_cache_refused: None,
            },
            kind,
            models_used: vec!["haiku".into()],
            attempts: 1,
            cost_completeness: relais::money::CostCompleteness::Actual,
            cost: MicroUsd::from_micros(1),
            criteria: vec![
                criterion("report", None, true, SettledVia::ReportReview),
                criterion(
                    "read",
                    Some(Evidence::HumanSignOff),
                    false,
                    SettledVia::HumanSignOff,
                ),
            ],
            mandatory_evidence_independence: Some(
                relais::verify::IndependenceSummary::PartlyIndependent,
            ),
            recipe: relais::policy::RecipeRecord::NotRecorded,
            verification_profile_hash: "vph".into(),
            review: relais::verify::ReviewRecord::NoReview,
            ladder: relais::verify::LadderRecord::NotRecorded,
            efforts_used: Vec::new(),
        }
    }

    /// Store `stored` as the pending receipt of a fresh run, beside its
    /// `receipt.json`, re-seal it with the sign-off on `read`, and read
    /// back the ledger's row and the file's outcome.
    fn resealed(stored: &serde_json::Value) -> serde_json::Value {
        let (ledger, dir) = temp_ledger("reseal");
        let run = RunId::from_stored("run-seal");
        let task = relais::ids::TaskId::from_stored("t");
        ledger
            .insert_run(&run, "/repo", None, &task, "rk")
            .expect("run");
        ledger
            .store_receipt(&run, stored, "pending")
            .expect("stored");
        let runs = dir.join("runs");
        let file_path = runs.join("run-seal").join("receipt.json");
        std::fs::create_dir_all(runs.join("run-seal")).expect("run dir");
        std::fs::write(&file_path, stored.to_string()).expect("receipt.json");
        reseal_receipt_in(&ledger, &runs, &run, "read").expect("reseal");
        let (row, _) = ledger.receipt(&run).expect("read").expect("row");
        let file = std::fs::read_to_string(&file_path).expect("receipt.json");
        let file: serde_json::Value = serde_json::from_str(&file).expect("json");
        assert_eq!(
            row["outcome"], file["outcome"],
            "the row and the file agree"
        );
        std::fs::remove_dir_all(&dir).ok();
        row
    }

    #[test]
    fn a_signed_off_inspection_is_accepted_by_its_recorded_settlements() {
        let receipt = pending_receipt(relais::contract::Kind::Inspect, false);
        let row = resealed(&serde_json::to_value(&receipt).expect("receipt"));
        assert_eq!(row["outcome"], "accepted", "{row}");
        let read = &row["criteria"][1];
        assert_eq!(read["met"], true, "{read}");
        assert_eq!(read["settled_via"], "human_sign_off", "{read}");
        assert_eq!(row["criteria"][0]["settled_via"], "report_review");
        assert_eq!(
            row["mandatory_evidence_independence"], "partly_independent",
            "a re-seal recomputes what the runner computed: {row}"
        );
    }

    #[test]
    fn a_signed_off_change_still_needs_every_gap_closed() {
        let open = pending_receipt(relais::contract::Kind::Change, true);
        let row = resealed(&serde_json::to_value(&open).expect("receipt"));
        assert_eq!(row["outcome"], "needs_decision", "{row}");
        let closed = pending_receipt(relais::contract::Kind::Change, false);
        let row = resealed(&serde_json::to_value(&closed).expect("receipt"));
        assert_eq!(row["outcome"], "accepted", "{row}");
    }

    /// A receipt written before `kind` and `settled_via` existed carries
    /// neither: it parses, is a change, and re-seals as it always did.
    #[test]
    fn a_receipt_without_kind_or_settled_via_reseals_as_a_change() {
        let receipt = pending_receipt(relais::contract::Kind::Change, true);
        let mut stored = serde_json::to_value(&receipt).expect("receipt");
        stored.as_object_mut().expect("object").remove("kind");
        for criterion in stored["criteria"].as_array_mut().expect("criteria") {
            criterion
                .as_object_mut()
                .expect("object")
                .remove("settled_via");
        }
        stored["criteria"][0]["settled_by"] = serde_json::json!({
            "test": "suite::case",
            "command": "cargo-test@1",
            "outcome": "passed",
        });
        stored["mandatory_evidence_independence"] = serde_json::json!("all_independent");
        let parsed: Receipt =
            serde_json::from_value(stored.clone()).expect("an old receipt parses");
        assert_eq!(parsed.kind, relais::contract::Kind::Change);
        assert_eq!(parsed.criteria[1].settled_via, None);
        let row = resealed(&stored);
        assert_eq!(
            row["outcome"], "needs_decision",
            "the other gap is still open: {row}"
        );
        assert_eq!(
            row["mandatory_evidence_independence"], "all_independent",
            "no settled_via: today's evidence rule: {row}"
        );
    }
}
