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

use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;

use relais::acceptance::Evidence;
use relais::admission::{BoundOutcome, Gate, GateError, StoppedOutcome, StoppedReport};
use relais::backend::Harness;
use relais::contract::TaskContract;
use relais::ids::{IdSource, RunId, TaskId};
use relais::install::{InstallReport, InstallRequest, Mode, Scope};
use relais::learn::comparison::{
    evaluate_candidate, DEFAULT_BOOTSTRAP_RESAMPLES, MIN_PAIRED_TASKS,
};
use relais::learn::predict::RegistryPredictor;
use relais::learn::promote::{admit, Amendment};
use relais::learn::typed_gate;
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
        /// Print the machine.toml block that states the effort facts
        /// `doctor` reports unknown: the CLI-accepted list pre-filled from
        /// `--help`, the model-support and order lines left for you to
        /// confirm.
        #[arg(
            long = "effort-template",
            conflicts_with_all = ["json"]
        )]
        effort_template: bool,
    },
    /// Create a relais.toml policy for this repository
    Init {
        /// Propose a policy from the repository's own files (Makefile
        /// targets, manifests, lockfiles) instead of writing the
        /// template. Only reads text; runs nothing.
        #[arg(long = "detect")]
        detect: bool,
        /// Print the proposal as JSON: `{"proposal": {...}}`, or
        /// `{"proposal": null}` when nothing was detected.
        #[arg(long = "json", requires = "detect")]
        json: bool,
        /// Write the proposal to relais.toml. Never overwrites.
        #[arg(long = "write", requires = "detect")]
        write: bool,
        /// Use this command instead of the detected ones: split on
        /// whitespace and quotes, and refused if it holds a shell
        /// operator.
        #[arg(long = "command", value_name = "TEXT", requires = "detect")]
        command: Option<String>,
    },
    /// Watch the runs that amont and amont-agent keep quiet (entry:
    /// amont docs/quiet-runs.md)
    Quiet {
        #[command(subcommand)]
        cmd: QuietCommand,
    },
    /// Show what a trust grant for this repository's policy would let
    /// relais run, or record one in machine.toml (SPEC §5)
    Trust {
        #[command(subcommand)]
        cmd: TrustCommand,
    },
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
        #[arg(long = "task", required_unless_present = "intent")]
        task: Option<PathBuf>,
        /// Run a successor intent a `decide revise` recorded: claims it
        /// atomically (`pending -> claimed`) and runs its contract as a
        /// revision of the decided run's task. A second claim of an intent
        /// that is already running is refused `intent_already_claimed`,
        /// naming the run that holds it
        #[arg(long = "intent", conflicts_with_all = ["task", "revise"])]
        intent: Option<String>,
        /// Revise an existing task: this run's cost joins its total
        /// rather than starting a new one. Refused if the contract
        /// declares a different task, or if this id names no task on
        /// record.
        #[arg(long = "revise")]
        revise: Option<String>,
        /// Keep stdout for protocol lines (one JSON object per line, each
        /// with a `relais` key) and send everything else to stderr
        /// (SPEC §29). Not supported on Windows yet. A run starts only from
        /// Claude Code with the relais plugin, which passes this
        #[arg(long)]
        protocol: bool,
        /// Where the run works: `auto` (default: edit a linked worktree in
        /// place, otherwise fork it with copy-on-write dependencies),
        /// `fork`, `in-place` (refused outside a linked worktree) or
        /// `isolated` (a fresh worktree and every setup step, as before).
        /// `RELAIS_WORKSPACE` sets the same; `CI` forces `isolated`
        #[arg(long)]
        workspace: Option<String>,
    },
    /// What the relais plugin calls: short processes that tell the
    /// coordinator what its agents did (SPEC §23)
    Native {
        #[command(subcommand)]
        cmd: NativeCommand,
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
        /// Append this command's output to a file instead of the terminal;
        /// for a detached sweep that has no terminal
        #[arg(long)]
        log: Option<PathBuf>,
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
        /// like task classes rather than blending every task together.
        /// `task-type` groups by the task's type (what kind of work it
        /// is); it is not `task-class`, which is the verification profile
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
    /// The learning loop: a cycle that builds, trains, evaluates and (inside
    /// the `[learning]` envelope) promotes; and its last result (SPEC §17)
    Learn {
        #[command(subcommand)]
        cmd: LearnCommand,
    },
    /// The session router's data for training a local classifier
    /// (SPEC §30, relais ADR-0002)
    Router {
        #[command(subcommand)]
        cmd: RouterCommand,
    },
    /// Return to the previous task artifact; the person's own rollback,
    /// recorded as a learning cycle row with source `person` (SPEC §17)
    Rollback {
        /// Print the result as JSON
        #[arg(long)]
        json: bool,
    },
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
        /// Who is recording this feedback: `orchestrator`, `person` or
        /// `person-via-orchestrator` (the same three as `decide`)
        #[arg(long = "actor")]
        actor: String,
        /// Free-form context for this outcome
        #[arg(long = "note")]
        note: Option<String>,
        /// Print one JSON object: `{run, task, outcome, actor, source}`, or
        /// `{error, exit}` on a refusal
        #[arg(long)]
        json: bool,
        /// Read `{"session", "nonce"}` from stdin: the plugin's proof that
        /// its feedback tool made this call (ADR-0004). A live nonce stores
        /// the outcome's `source` as `orchestrator`; any other, as `cli`
        #[arg(long = "proof-stdin")]
        proof_stdin: bool,
    },
    /// Install the Claude Code integration; preview-first, --write applies
    /// (SPEC §3). User-level installation is explicit, not the default.
    Install {
        /// Install the relais Claude Code plugin (the run tools, the pane, the `/relais` skill and the agents it dispatches) and the advisory agents and skills
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
        /// Remove the relais Claude Code plugin and the advisory agents and skills
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
        /// Who is deciding: `orchestrator` (the Claude Code session on its
        /// own authority), `person` (the person, typing in their own
        /// terminal) or `person-via-orchestrator` (the person said it, the
        /// orchestrator typed it; needs `--person-quote`)
        #[arg(long = "actor")]
        actor: String,
        /// The person's own words; required by, and only meaningful with,
        /// `--actor person-via-orchestrator`
        #[arg(long = "person-quote")]
        person_quote: Option<String>,
        /// Print one JSON object: `{run, resolution, actor, from_state,
        /// to_state, advice}`, or `{error, exit}` on a refusal
        #[arg(long)]
        json: bool,
        /// Read `{"session", "nonce"}` from stdin: the plugin's proof that
        /// its decide tool made this call (ADR-0004). A live nonce records
        /// the answer `recorded_via = plugin`; any other, `cli`
        #[arg(long = "proof-stdin")]
        proof_stdin: bool,
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
        /// With `--answer revise` on a `scope_exceeded` decision: record a
        /// successor intent for a `kind: verify` of this run's candidate
        /// whose write scope adds exactly the out-of-scope paths. The
        /// plugin launches it; `--json` returns `{successor_intent}`
        #[arg(
            long = "widen-scope",
            conflicts_with = "successor_contract",
            conflicts_with = "successor"
        )]
        widen_scope: bool,
        /// With `--answer revise`: record a successor intent for the task
        /// contract in this file. The plugin launches it
        #[arg(long = "successor-contract", conflicts_with = "successor")]
        successor_contract: Option<PathBuf>,
        /// With `--answer revise` on a `resource_exhausted` decision: record
        /// a successor intent for a `kind: verify` rerun of this same
        /// candidate, once the machine has room. The plugin launches it;
        /// `--json` returns `{successor_intent}`
        #[arg(
            long = "rerun-verify",
            conflicts_with = "successor_contract",
            conflicts_with = "widen_scope",
            conflicts_with = "successor"
        )]
        rerun_verify: bool,
    },
    /// Evidence operations (SPEC §12, §18)
    Evidence {
        #[command(subcommand)]
        cmd: EvidenceCommand,
    },
    /// Read one Claude Code hook payload on stdin and answer it (SPEC
    /// §23): silence, or a refusal a person can act on. With `--probe
    /// --record <dir>`, record the payload verbatim instead and decide
    /// nothing — the mode that captures payloads as fixtures, not something
    /// a person runs directly.
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
enum QuietCommand {
    /// Poll the active-run directory and print one JSON line per event
    /// until stdout closes or the parent process changes. Reads no stdin.
    Watch {
        /// Print the protocol lines (required: there is no other format)
        #[arg(long, required = true)]
        protocol: bool,
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
enum RouterCommand {
    /// Write the kept classifier calls that parsed as JSON lines: a header
    /// with the capability table and its version, then one line per call
    /// `{call_id, at, prompt_version, input, answer, split}`, split by time
    /// (newest 20% test, next 10% calibration, the rest train). The input
    /// text is the person's own prompts; nothing leaves the machine except
    /// by this command
    #[command(name = "export-classifier")]
    ExportClassifier {
        /// The file to create (mode 0600, through a temporary file in its
        /// directory, then renamed), or `-` for stdout
        #[arg(long)]
        out: PathBuf,
        /// Only calls made on or after this date (YYYY-MM-DD, UTC)
        #[arg(long)]
        since: Option<String>,
    },
}

#[derive(Subcommand)]
enum LearnCommand {
    /// Build the dataset, train, evaluate and promote when allowed. Takes
    /// the Training lease without queueing: a second cycle exits 20.
    /// Not idle (`training_when_idle`) exits 0 as `deferred`
    Cycle {
        /// Do nothing (exit 0, one line) when the last cycle started less
        /// than 20 hours ago or no run reached a terminal state since it
        #[arg(long = "if-due")]
        if_due: bool,
        /// Print the cycle as JSON
        #[arg(long)]
        json: bool,
        /// Append output to this file (created 0600, rotated at 1 MiB with
        /// one old file kept) instead of stdout and stderr
        #[arg(long)]
        log: Option<PathBuf>,
        /// No progress lines on stderr
        #[arg(short, long)]
        quiet: bool,
    },
    /// The most recent learning cycle row, read without migrating the ledger
    Last {
        /// Print the row as JSON
        #[arg(long)]
        json: bool,
    },
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
        /// A program design (markdown, at most 16 KiB) handed to the
        /// worker's context package only: the recorded contract and its
        /// hash stay the source run's, and the trial records the design's
        /// sha-256. Read before anything runs; a missing or oversized file
        /// is refused
        #[arg(long)]
        design: Option<PathBuf>,
        /// Speak the protocol of `relais run --protocol` on stdout; how
        /// the relais plugin starts a replay that spends
        #[arg(long)]
        protocol: bool,
    },
    /// Write one JSON line per recorded task, for a classifier: task id,
    /// repo, kind, objective, write scope and acceptance, with no file
    /// contents. relais links no model; the orchestrator labels the
    /// export and `import-task-types` reads the labels back
    #[command(name = "export-tasks")]
    ExportTasks {
        /// The file to create (mode 0600, through a temporary file in its
        /// directory, then renamed), or `-` for stdout
        #[arg(long)]
        out: PathBuf,
    },
    /// Read classified task types, one JSON line each
    /// (`{"task_id": ..., "task_type": ...}`), and store them as
    /// `classified`. Every row is checked first: a file with an unknown
    /// task id or type is refused whole (exit 2, each bad row printed as
    /// `<file>:<line>: <reason>`) and writes nothing. A declared type is
    /// never overwritten, and importing the same file again changes nothing
    #[command(name = "import-task-types")]
    ImportTaskTypes {
        /// The file of labels
        file: PathBuf,
    },
    /// Record a ground-truth verdict on a review-rejected replay trial:
    /// `pass` when the source work has the same defect the review found
    /// (the typed-research gate then counts the trial as accepted), `fail`
    /// when it does not. Refused (exit 2, nothing written) for an unknown
    /// trial, a trial that was not rejected, an empty note, or a trial
    /// already judged the other way; the same verdict again is a no-op
    #[command(name = "judge-trial")]
    JudgeTrial {
        /// The trial to judge
        trial_id: String,
        /// The verdict
        #[arg(long, value_enum)]
        verdict: JudgeVerdict,
        /// The evidence, e.g. which source commit has the same defect
        #[arg(long)]
        note: String,
        /// Print the verdict as JSON
        #[arg(long)]
        json: bool,
    },
}

/// The verdicts `relais dataset judge-trial` takes.
#[derive(Clone, Copy, clap::ValueEnum)]
enum JudgeVerdict {
    Pass,
    Fail,
}

#[derive(Subcommand)]
enum TrustCommand {
    /// The grant key, the repository, where machine.toml is, and every
    /// command a grant would authorize. Writes nothing.
    Show {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Trust this repository once, in machine.toml (`[trust_repos]`): every
    /// policy of it is then admitted, a change to its commands warns. With
    /// the deprecated --key, record a legacy grant for the current policy
    /// alone. Never reads stdin; refused when --key is not the current
    /// policy's.
    Grant {
        /// Deprecated: the key `relais trust show` printed, for a legacy
        /// grant of this exact policy. Omit it to trust the repository.
        #[arg(long)]
        key: Option<String>,
        /// Who reviewed the commands
        #[arg(long = "reviewed-by")]
        reviewed_by: String,
        /// What was reviewed
        #[arg(long)]
        note: Option<String>,
        /// Who asked the person: `cli` (typed in a shell) or `plugin`
        #[arg(long, value_enum, default_value_t = GrantSource::Cli)]
        source: GrantSource,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum GrantSource {
    Cli,
    Plugin,
}

impl GrantSource {
    fn as_str(self) -> &'static str {
        match self {
            GrantSource::Cli => "cli",
            GrantSource::Plugin => "plugin",
        }
    }
}

/// What a `relais trust grant` trusts: the repository once (ADR-0007), or
/// one exact policy through the deprecated `--key`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GrantScope {
    Repository,
    Legacy,
}

impl GrantScope {
    fn as_str(self) -> &'static str {
        match self {
            GrantScope::Repository => "repository",
            GrantScope::Legacy => "legacy",
        }
    }
}

/// Exit 0 acknowledges; non-zero asks the plugin to retry.
#[derive(Subcommand)]
enum NativeCommand {
    /// Mint the session's decide nonce and print it (ADR-0004); with
    /// `--end`, forget it. The plugin calls this at session start and end
    DecideNonce {
        #[arg(long)]
        session: String,
        #[arg(long)]
        end: bool,
    },
    /// The session's plugin is alive
    Hello {
        #[arg(long)]
        session: String,
    },
    /// The plugin spawned the dispatch's agent
    Bound {
        #[arg(long)]
        dispatch: String,
        #[arg(long)]
        agent: String,
    },
    /// The dispatch's agent ended; `{"agent", "status", "usage", "answer"}`
    /// on stdin
    Stopped {
        #[arg(long)]
        dispatch: String,
    },
    /// The dispatch's agent finished a turn; `{"agent", "turns", "usage",
    /// "message_ids"}` on stdin, the usage cumulative over the dispatch.
    /// Books only what the ledger does not hold yet for the dispatch
    Usage {
        #[arg(long)]
        dispatch: String,
    },
    /// Cancel one run, from the session that started it. A run with no
    /// live driver is marked cancelled at once, from any session; a run
    /// with a live driver of another session is refused (11)
    Cancel {
        /// The run
        #[arg(long)]
        run: String,
        /// The Claude Code session id asking; required, so a session never
        /// cancels another's live run
        #[arg(long)]
        session: Option<String>,
    },
    /// Print a run's timeline as one JSON object: phases, decisions, cost,
    /// outcome and the last 40 lines of check output
    Status {
        /// The run; the latest on this machine when omitted
        #[arg(long)]
        run: Option<String>,
        /// Every run started from this Claude Code session id, newest
        /// first (at most 20), as {"runs":[...]}
        #[arg(long, conflicts_with = "run")]
        session: Option<String>,
    },
    /// A task contract on stdin, validated and saved under the
    /// repository's `.relais/tasks/`; prints the path `relais run --task`
    /// takes. What the plugin's run tool calls with the model's contract.
    Contract,
    /// What the session router needs to decide (SPEC §30): mode, tiers,
    /// capability table, rates, priors, pins, checks, hold-out and seed,
    /// as one JSON object. Reads only; never creates the ledger
    #[command(name = "router-state")]
    RouterState {
        /// The Claude Code session id
        #[arg(long)]
        session: String,
    },
    /// What amont's pre-push gate stamped on the repository's current tree
    /// (SPEC §30), as JSON: `{"stamped":false}` when there is no stamp.
    /// Reads only; exits non-zero only when git or the directory fails
    #[command(name = "gate-stamp")]
    GateStamp {
        /// The repository
        #[arg(long)]
        cwd: PathBuf,
    },
    /// The session router's records on stdin (decisions, usage,
    /// reassessments, tasks), written in one transaction; 2 when the
    /// payload is invalid, and then nothing is written
    #[command(name = "router-observe")]
    RouterObserve,
    /// Ingest amont-agent's plan-review outcomes ledger: label each
    /// reviewer clean, miss or unknown into the relais ledger. Refuses
    /// (3) a ledger that is a symlink, not ours, or open to group or
    /// other; never writes machine.toml
    #[command(name = "review-outcome")]
    ReviewOutcome {
        /// The outcomes ledger (default: amont-agent's, under the Claude
        /// config directory)
        #[arg(long)]
        ledger: Option<PathBuf>,
        /// Where the repositories' docs/plans live (default:
        /// $AMONT_AGENT_PLAN_ROOT, else ~/Developer/Perso)
        #[arg(long)]
        plan_root: Option<PathBuf>,
        /// Print the labels that would be assigned; write nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Record the person's routing envelope in machine.toml (SPEC §30),
    /// or with `--off` remove it and record that routing is off. Run by
    /// the plugin after its own question; typed in a shell it is recorded
    /// as unattributed
    #[command(name = "router-envelope")]
    RouterEnvelope {
        /// Who granted it (or turned routing off)
        #[arg(long)]
        by: String,
        /// The highest exploration rate allowed, 0 to 1
        #[arg(
            long = "epsilon-max",
            required_unless_present = "off",
            conflicts_with = "off"
        )]
        epsilon_max: Option<f64>,
        /// Turn routing off: remove the envelope, and keep `relais install`
        /// from recording it again
        #[arg(long)]
        off: bool,
        /// `plugin-ask` when the plugin asked the person
        #[arg(long, value_enum)]
        source: Option<RouterSource>,
    },
    /// Record the person's automatic-promotion grant in machine.toml
    /// (`[learning] auto_promote`, SPEC §17), or with `--off` remove it and
    /// record that promotion is off. Run by the plugin after its own
    /// question; typed in a shell it is recorded as unattributed
    #[command(name = "learning-envelope")]
    LearningEnvelope {
        /// Who granted it (or turned automatic promotion off)
        #[arg(long)]
        by: String,
        /// Turn automatic promotion off and keep the installer from
        /// recording it again
        #[arg(long)]
        off: bool,
        /// `plugin-ask` when the plugin asked the person
        #[arg(long, value_enum)]
        source: Option<RouterSource>,
    },
}

/// Who wrote a session-routing record: only the plugin's own question
/// is attributed (`relais install` attributes its own write).
#[derive(Clone, Copy, ValueEnum)]
enum RouterSource {
    #[value(name = "plugin-ask")]
    PluginAsk,
}

fn router_source(source: Option<RouterSource>) -> relais::router::Source {
    match source {
        Some(RouterSource::PluginAsk) => relais::router::Source::PluginAsk,
        None => relais::router::Source::Cli,
    }
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
    /// The name a `done` line gives this failure when it ends a
    /// `relais run --protocol` before any run exists (SPEC §29).
    fn wire_code(&self) -> &'static str {
        match self {
            CliError::Locate(relais::repo::LocateError::RepoWithoutPolicy(_)) => "no_policy",
            CliError::Locate(relais::repo::LocateError::NotInRepository(_)) => "not_in_repository",
            CliError::Read { what, .. } | CliError::Invalid { what, .. } => match *what {
                "the repository policy" => "invalid_policy",
                "the machine settings" => "invalid_machine_settings",
                _ => "invalid_input",
            },
            CliError::Home(_) | CliError::Cwd(_) => "environment",
            CliError::Usage { .. } => "refused",
            CliError::NoDataset { .. }
            | CliError::Amend(_)
            | CliError::AppendPolicy { .. }
            | CliError::Operational { .. } => "operational_failure",
        }
    }

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
    // Before anything runs, or prints: `--protocol` takes stdout over.
    let channel = match open_protocol(&cli.command) {
        Ok(channel) => channel,
        Err(e) => {
            eprintln!("relais: {e}");
            std::process::exit(exit_code(&e.outcome()));
        }
    };
    // A panic's message is written to fd 2, which `--protocol` points at
    // the mirror's pipe; the channel is closed (the pipe drained into the
    // real stderr and `events.jsonl`) before the panic carries on, so the
    // message is never lost to an exit that beat the mirror thread.
    let dispatched =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dispatch(cli.command)));
    let outcome = match dispatched {
        Ok(Ok(outcome)) => {
            if channel.is_some() {
                announce_unstarted(outcome, None);
            }
            outcome
        }
        Ok(Err(e)) => {
            eprintln!("relais: {e}");
            if channel.is_some() {
                announce_unstarted(e.outcome(), Some(&e));
            }
            e.outcome()
        }
        Err(panic) => {
            if let Some(channel) = channel {
                channel.close();
            }
            std::panic::resume_unwind(panic);
        }
    };
    if let Some(channel) = channel {
        channel.close();
    }
    std::process::exit(exit_code(&outcome));
}

thread_local! {
    /// The refusal a `relais run --protocol` printed before any run
    /// existed, for the `done` line `main` sends in its place.
    static REFUSAL: std::cell::RefCell<Option<(&'static str, String)>> =
        const { std::cell::RefCell::new(None) };
}

/// Print a refusal that ends `relais run` before a run exists, and keep
/// it for the `done` line (see [`announce_unstarted`]).
fn refuse_run(code: &'static str, detail: String, outcome: CliOutcome) -> CliOutcome {
    eprintln!("relais run: blocked ({code}): {detail}");
    REFUSAL.with(|refusal| *refusal.borrow_mut() = Some((code, detail)));
    outcome
}

/// Every `relais run --protocol` (and `dataset replay --protocol`) that
/// exits sends one `done` line: the runner sends it for a run that
/// started; this sends it, with `run: null`, for one refused before it
/// could — no policy, unreadable settings, a bad contract, admission.
/// Without it the plugin had only an exit code to tell the model.
fn announce_unstarted(outcome: CliOutcome, error: Option<&CliError>) {
    if relais::protocol::done_sent() || outcome == CliOutcome::Accepted {
        return;
    }
    let kept = REFUSAL.with(|refusal| refusal.borrow_mut().take());
    let (code, detail) = match (error, kept) {
        (Some(e), _) => (e.wire_code(), e.to_string()),
        (None, Some((code, detail))) => (code, detail),
        (None, None) => (
            "refused",
            format!("relais exited with code {}", exit_code(&outcome)),
        ),
    };
    let _ = relais::protocol::Wire::process().send(&relais::protocol::Request::Done {
        run: None,
        outcome: "blocked",
        receipt: None,
        summary: None,
        trial: None,
        code: Some(code),
        detail: Some(&detail),
    });
}

/// The protocol channel of `relais run --protocol` (SPEC §29): installed
/// before the run starts, or refused on a platform that cannot.
fn open_protocol(command: &Command) -> Result<Option<relais::protocol::Channel>, CliError> {
    let run = matches!(command, Command::Run { protocol: true, .. });
    let replay = matches!(
        command,
        Command::Dataset {
            cmd: DatasetCommand::Replay { protocol: true, .. },
        }
    );
    if !run && !replay {
        return Ok(None);
    }
    let name = if replay { "dataset replay" } else { "run" };
    relais::protocol::install()
        .map(Some)
        .map_err(|e| CliError::Usage {
            detail: format!("{name} --protocol: {e}"),
        })
}

/// Route one parsed invocation to its handler. Every handler returns the
/// same pair, so the exit code is decided in exactly one place.
fn dispatch(command: Command) -> Result<CliOutcome, CliError> {
    match command {
        Command::Doctor {
            json,
            effort_template,
        } => {
            if effort_template {
                doctor_effort_template_command()
            } else {
                doctor_command(json)
            }
        }
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
        Command::Init {
            detect,
            json,
            write,
            command,
        } => {
            if detect {
                init_detect_command(json, write, command.as_deref())
            } else {
                init_command()
            }
        }
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
            intent,
            revise,
            protocol,
            workspace,
        } => {
            let host = std::env::var("RELAIS_HOST").ok();
            let origin = relais::native::RunOrigin {
                protocol,
                host: host.as_deref(),
            };
            let source = match (&task, &intent) {
                (Some(task), None) => RunSource::Task(task),
                (None, Some(intent)) => RunSource::Intent(intent),
                _ => {
                    return Err(CliError::Usage {
                        detail: "run: name exactly one of --task and --intent".into(),
                    })
                }
            };
            run_command(source, revise.as_deref(), &origin, workspace.as_deref())
        }
        Command::Native { cmd } => native_command(cmd),
        Command::Quiet { cmd } => quiet_command(cmd),
        Command::Trust { cmd } => trust_command(cmd),
        Command::Status { run_id } => status_command(run_id.as_deref()),
        Command::Explain { run_id } => explain_command(&run_id),
        Command::Resume {
            run_id,
            retire,
            all: _,
            log,
        } => match (log, run_id) {
            (Some(log), _) => resume_logged(&log),
            (None, Some(run_id)) => resume_command(&run_id, Retire::from_flag(retire)),
            (None, None) => retire_all_command(),
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
            DatasetCommand::ExportTasks { out } => export_tasks_command(&out),
            DatasetCommand::ImportTaskTypes { file } => import_task_types_command(&file),
            DatasetCommand::JudgeTrial {
                trial_id,
                verdict,
                note,
                json,
            } => judge_trial_command(&trial_id, verdict, &note, json),
            DatasetCommand::Replay {
                task,
                recipe,
                dry_run,
                design,
                protocol,
            } => {
                let host = std::env::var("RELAIS_HOST").ok();
                let origin = relais::native::RunOrigin {
                    protocol,
                    host: host.as_deref(),
                };
                replay_command(&task, &recipe, dry_run, design.as_deref(), &origin)
            }
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
        Command::Learn { cmd } => match cmd {
            LearnCommand::Cycle {
                if_due,
                json,
                log,
                quiet,
            } => learn_cycle_command(if_due, json, log.as_deref(), quiet),
            LearnCommand::Last { json } => learn_last_command(json),
        },
        Command::Router { cmd } => match cmd {
            RouterCommand::ExportClassifier { out, since } => {
                export_classifier_command(&out, since.as_deref())
            }
        },
        Command::Rollback { json } => rollback_command(json),
        Command::Feedback {
            run_id,
            task,
            outcome,
            candidate,
            magnitude,
            evidence,
            actor,
            note,
            json,
            proof_stdin,
        } => feedback_command(FeedbackRequest {
            run_id,
            task,
            outcome,
            candidate,
            magnitude,
            evidence,
            actor,
            note,
            json,
            proof_stdin,
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
            person_quote,
            json,
            proof_stdin,
            note,
            successor,
            criterion,
            candidate,
            widen_scope,
            successor_contract,
            rerun_verify,
        } => decide_command(DecideRequest {
            widen_scope,
            rerun_verify,
            successor_contract: successor_contract.as_deref(),
            run_id: &run_id,
            answer,
            actor: &actor,
            person_quote: person_quote.as_deref(),
            note: note.as_deref(),
            successor: successor.as_deref(),
            criterion: criterion.as_deref(),
            candidate: candidate.as_deref(),
            json,
            proof_stdin,
        }),
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
fn render_install(
    verb: &str,
    request: &InstallRequest,
    report: &InstallReport,
    plugin: &relais::install::plugin::PluginReport,
) -> CliOutcome {
    println!(
        "relais {verb} --claude ({})\n{}{}",
        request.scope_label(),
        report.plan.render(),
        plugin.render()
    );
    let plugin_outcome = match &plugin.failure {
        None => CliOutcome::Accepted,
        Some(failure) => {
            eprintln!("relais {verb}: the plugin step failed — {failure}");
            CliOutcome::NotFullyApplied
        }
    };
    let files_outcome = match report.mode {
        Mode::Preview => {
            if report.plan.applicable_count() == 0 && plugin.steps.is_empty() {
                println!("nothing to do");
            } else {
                println!("preview only: re-run with --write to apply");
            }
            CliOutcome::Accepted
        }
        Mode::Apply => {
            println!("applied {} change(s)", report.applied.len());
            for action in &report.not_applied {
                eprintln!(
                    "relais {verb}: could not apply {} — it changed between the preview and \
                     --write; re-run to see what it is now",
                    action.relative().display()
                );
            }
            match report.not_applied.is_empty() {
                true => CliOutcome::Accepted,
                false => CliOutcome::NotFullyApplied,
            }
        }
    };
    worse_outcome(files_outcome, plugin_outcome)
}

/// The directory marketplace the plugin is installed from.
fn marketplace_dir() -> Result<PathBuf, CliError> {
    paths::claude_marketplace_dir().map_err(CliError::Home)
}

/// What an `install`/`uninstall --claude` invocation is aimed at. Not a
/// flag: `settings.json` is a file a person maintains by hand, so
/// touching it is a different target rather than a modifier of the same
/// one, and the two cases read as what they are at every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Targets {
    /// The owned files under `.claude/` and the plugin.
    OwnedFiles,
    /// The owned files and the plugin, and then the hook wiring in
    /// `settings.json`.
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
    let plugin = relais::install::plugin::install(request.mode, &marketplace_dir()?);
    let outcome = render_install("install", &request, &report, &plugin);
    if request.mode == Mode::Apply && plugin.failure.is_none() {
        install_routing_envelope();
        install_learning_envelope();
    }
    match targets {
        Targets::OwnedFiles => Ok(outcome),
        Targets::OwnedFilesAndHooks => Ok(worse_outcome(
            outcome,
            install_hooks_target(&request, &home)?,
        )),
    }
}

/// Installing the plugin is the person's authorization to route the
/// session (plan, decision 1, R1b): record the envelope when machine.toml
/// has neither one nor an `off` tombstone, and say how to turn it off. A
/// failure here is said and does not fail the install: routing then stays
/// in shadow.
fn install_routing_envelope() {
    let Ok(machine_path) = paths::machine_settings_path() else {
        eprintln!("relais install: session routing not recorded: no config directory");
        return;
    };
    let envelope = relais::policy::RoutingEnvelope {
        granted_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        by: "relais install".to_string(),
        epsilon_max: 0.1,
        source: relais::router::Source::Install.as_str().to_string(),
    };
    match relais::router::write_envelope_if_absent(&machine_path, &envelope) {
        Ok(true) => {
            let recorded = open_ledger().and_then(|ledger| {
                operational(
                    ledger.record_router_provenance(&relais::ledger::RouterProvenance::Envelope {
                        by: envelope.by.clone(),
                        epsilon_max: envelope.epsilon_max,
                        source: envelope.source.clone(),
                        at: envelope.granted_at.clone(),
                    }),
                    "recording the envelope in the ledger",
                )
            });
            if let Err(e) = recorded {
                eprintln!("relais install: warning: the ledger did not record the envelope ({e})");
            }
            println!(
                "session routing is on; turn it off with /relais-routing off or RELAIS_SESSION_ROUTING=off"
            );
        }
        Ok(false) => {}
        Err(e) => eprintln!("relais install: session routing not recorded: {e}"),
    }
}

/// Installing the plugin is also the person's authorization for automatic
/// promotion of a task artifact that passed every gate (relais ADR-0003):
/// record the grant when machine.toml has neither one nor an `off`
/// tombstone, and say how to turn it off. A failure is said and does not
/// fail the install: the cycle then stops at `awaiting_promotion`.
fn install_learning_envelope() {
    let Ok(machine_path) = paths::machine_settings_path() else {
        eprintln!("relais install: automatic promotion not recorded: no config directory");
        return;
    };
    let grant = relais::policy::AutoPromote {
        granted_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        by: "relais install".to_string(),
        source: relais::router::Source::Install.as_str().to_string(),
    };
    match relais::learn::envelope::write_auto_promote_if_absent(&machine_path, &grant) {
        Ok(true) => println!("automatic promotion is on; turn it off with /relais-learning off"),
        Ok(false) => {}
        Err(e) => eprintln!("relais install: automatic promotion not recorded: {e}"),
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
    let plugin = relais::install::plugin::uninstall(request.mode, &marketplace_dir()?);
    let outcome = render_install("uninstall", &request, &report, &plugin);
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
                        relais::install::HookEventAction::UpdatePath => "repoint",
                        relais::install::HookEventAction::MigrateMatcher => "migrate",
                        relais::install::HookEventAction::RemoveRetired => "remove",
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
    // Nothing to paste when the reason already says what to change by hand.
    if !paste_block.is_empty() {
        println!("paste this into settings.json's \"hooks\" key by hand:\n{paste_block}");
    }
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
    // Salvages recorded before containment was computed get it once, while
    // their repository and candidate patch are still on disk; the rest
    // stay unknown and are excluded.
    let backfill = operational(relais::learn::salvage::backfill(&ledger), "dataset build")?;
    // Then the reverts git shows, before the dataset is read: one flips its
    // task's label to a failure.
    let detection =
        relais::learn::revert::RevertDetection::from_settings(&load_machine()?.learning);
    let reverts = operational(
        relais::learn::revert::detect_if(detection, &ledger, chrono::Utc::now()),
        "dataset build",
    )?;
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
    let count_of = |source| {
        dataset
            .records
            .iter()
            .filter(|record| record.label_source == source)
            .count()
    };
    let by_person = count_of(relais::learn::features::LabelSource::PersonSalvageContained);
    let by_orchestrator =
        count_of(relais::learn::features::LabelSource::OrchestratorSalvageContained);
    println!(
        "label sources: runner {}, person_salvage_contained {by_person}, \
         orchestrator_salvage_contained {by_orchestrator}",
        dataset.records.len() - by_person - by_orchestrator
    );
    let origin_count = |origin| {
        dataset
            .records
            .iter()
            .filter(|record| record.origin == origin)
            .count()
    };
    println!(
        "examples by origin: live {}, replay {}",
        origin_count(relais::learn::features::Origin::Live),
        origin_count(relais::learn::features::Origin::Replay)
    );
    for line in dataset.counterfactual_gap.render_lines() {
        println!("{line}");
    }
    println!("{}", reverts.summary());
    for line in reverts.unknown_lines() {
        eprintln!("{line}");
    }
    println!(
        "salvage containment backfill: {} computed, {} unreachable (left unknown, excluded)",
        backfill.recorded, backfill.unreachable
    );
    for (reason, count) in &backfill.unreachable_reasons {
        println!("  unreachable x{count}: {reason}");
    }
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

/// Where a learning cycle's lines go: stdout and stderr, or, with `--log`,
/// one append-only file relais opens itself (0600, rotated at 1 MiB, one old
/// file kept). `-q` silences progress lines only.
struct CycleOut {
    log: Option<relais::learn::cycle::CycleLog>,
    quiet: bool,
}

impl CycleOut {
    fn open(log: Option<&Path>, quiet: bool) -> Result<Self, CliError> {
        let log = match log {
            Some(path) => Some(relais::learn::cycle::CycleLog::open(path).map_err(|cause| {
                CliError::Read {
                    what: "the learning cycle log",
                    path: path.to_path_buf(),
                    cause,
                }
            })?),
            None => None,
        };
        Ok(Self { log, quiet })
    }

    /// A result line: stdout.
    fn say(&mut self, line: &str) {
        match &mut self.log {
            Some(log) => log.line(line),
            None => println!("{line}"),
        }
    }

    /// A diagnostic: stderr.
    fn complain(&mut self, line: &str) {
        match &mut self.log {
            Some(log) => log.line(line),
            None => eprintln!("{line}"),
        }
    }

    /// One stage's progress line: stderr, unless `-q`.
    fn progress(&mut self, line: &str) {
        if !self.quiet {
            self.complain(line);
        }
    }
}

/// The Training lease, released when this drops. The coordinator also frees
/// it when this process is gone (pid liveness), so a crash leaves nothing.
struct TrainingGuard {
    client: relais::coordinator::Client,
    lease: relais::coordinator::TrainingLease,
}

impl Drop for TrainingGuard {
    fn drop(&mut self) {
        if let Err(e) = self.client.end_training(&self.lease) {
            eprintln!("relais learn cycle: the Training lease was not released ({e}); it expires with this process");
        }
    }
}

/// `relais learn cycle [--if-due] [--json] [--log <path>] [-q]` (SPEC §17).
///
/// Order matters: the Training lease comes first, before the due check and
/// before the cycle's row exists, so concurrent session starts cannot both
/// pass a 20-hour check.
fn learn_cycle_command(
    if_due: bool,
    json: bool,
    log: Option<&Path>,
    quiet: bool,
) -> Result<CliOutcome, CliError> {
    use relais::admission::TrainingDecision;
    let mut out = CycleOut::open(log, quiet)?;
    let ledger = open_ledger()?;
    let machine = load_machine()?;
    let socket = relais::coordinator::socket_path().map_err(CliError::Home)?;
    let client = match relais::coordinator::ensure_running(&socket) {
        Ok(client) => client,
        Err(e) => {
            out.complain(&format!("relais learn cycle: {e}"));
            return Ok(CliOutcome::OperationalFailure);
        }
    };
    let session = relais::coordinator::resolve_session();
    let started_at = relais::ledger::now_rfc3339();
    let lease = match client.acquire_training(&session.id, std::process::id(), &started_at) {
        Ok(lease) => lease,
        Err(e) => {
            out.complain(&format!("relais learn cycle: {e}"));
            return Ok(CliOutcome::OperationalFailure);
        }
    };
    let due = |ledger: &Ledger| -> Result<relais::learn::cycle::Due, CliError> {
        let last = operational(ledger.last_cycle_for_due_check(), "learn cycle")?;
        let terminal = match &last {
            Some(row) => operational(
                ledger.terminal_root_runs_since(&row.started_at),
                "learn cycle",
            )?,
            None => 0,
        };
        Ok(relais::learn::cycle::due(
            last.as_ref().map(|row| row.started_at.as_str()),
            terminal,
            chrono::Utc::now(),
        ))
    };
    match lease.decision.clone() {
        TrainingDecision::Granted => {}
        TrainingDecision::Busy { holder } => {
            let who = match holder {
                Some(holder) => format!(
                    "pid {}, started {}",
                    holder
                        .pid
                        .map_or_else(|| "unknown".to_string(), |pid| pid.to_string()),
                    if holder.started_at.is_empty() {
                        "at an unrecorded time"
                    } else {
                        &holder.started_at
                    }
                ),
                None => "its holder is not recorded".to_string(),
            };
            out.complain(&format!(
                "relais learn cycle: another learning cycle holds the Training lease ({who}); \
                 not queued"
            ));
            return Ok(CliOutcome::TrainingLeaseHeld);
        }
        TrainingDecision::NotIdle { agents } => {
            // A cycle that is not due has nothing to defer.
            if if_due {
                if let Some(line) = due(&ledger)?.line() {
                    out.say(&line);
                    return Ok(CliOutcome::Accepted);
                }
            }
            let line = format!("deferred: not idle ({agents} agents active)");
            // With `--log` the one line goes to the file, stamped (it is
            // appended to a log read days later); otherwise to stdout.
            match &mut out.log {
                Some(log) => log.line(&format!(
                    "{} {line}",
                    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                )),
                None => println!("{line}"),
            }
            return Ok(CliOutcome::Accepted);
        }
        TrainingDecision::Refused { code, detail } => {
            out.complain(&format!(
                "relais learn cycle: the coordinator refused the Training lease ({}): {detail}",
                code.as_str()
            ));
            return Ok(CliOutcome::OperationalFailure);
        }
    }
    let _guard = TrainingGuard { client, lease };
    if if_due {
        if let Some(line) = due(&ledger)?.line() {
            out.say(&line);
            return Ok(CliOutcome::Accepted);
        }
    }
    let repo = match load_repo_policy() {
        Ok((_, repo)) => repo,
        // A cycle started from a directory with no relais.toml still has a
        // ledger to learn from; task features then read the default policy.
        // Any other failure (a parse error, a permission error) is surfaced.
        Err(CliError::Read { cause, .. }) if cause.kind() == std::io::ErrorKind::NotFound => {
            default_repo_policy()?
        }
        Err(CliError::Locate(_)) => default_repo_policy()?,
        // Said where the other cycle complaints go (the `--log` file), and
        // recorded as an error row so `learn last` can note it.
        Err(other) => {
            let message = other.to_string();
            out.complain(&format!("relais learn cycle: {message}"));
            let id = operational(ledger.begin_learning_cycle(), "learn cycle")?;
            operational(
                ledger.finish_learning_cycle(
                    id,
                    &relais::ledger::LearningCycleFinish {
                        action: Some(relais::ledger::LearningAction::Error),
                        error: Some(message),
                        ..Default::default()
                    },
                ),
                "learn cycle",
            )?;
            return Ok(CliOutcome::OperationalFailure);
        }
    };
    let registry = registry()?;
    let datasets = datasets_dir()?;
    let inputs = relais::learn::cycle::CycleInputs {
        ledger: &ledger,
        registry: &registry,
        repo: &repo,
        datasets_dir: &datasets,
        gates: relais::learn::cycle::GateSettings {
            quality_floor: machine
                .routing
                .quality_floor
                .unwrap_or(relais::learn::evaluate::DEFAULT_QUALITY_FLOOR),
            min_supported_test_records: machine.routing.min_supported_test_records,
            max_abstention_rate: machine.routing.max_abstention_rate,
        },
        auto_promote: machine.learning.auto_promote.is_some(),
        revert_detection: relais::learn::revert::RevertDetection::from_settings(&machine.learning),
        classifier_dir: paths::state_dir().ok().map(|dir| dir.join("classifier")),
        review_outcomes: Some(
            relais::review_outcome::Settings::resolve(None, None).map_err(|e| e.to_string()),
        ),
        now: chrono::Utc::now(),
    };
    let mut stage_number = 0;
    let report = {
        let out = &mut out;
        operational(
            relais::learn::cycle::run_cycle(&inputs, &mut |stage| {
                stage_number += 1;
                out.progress(&format!(
                    "relais learn cycle: [{stage_number}/4] {}",
                    stage.as_str()
                ));
            }),
            "learn cycle",
        )?
    };
    if json {
        let text = operational(serde_json::to_string_pretty(&report), "rendering JSON")?;
        out.say(&text);
    } else {
        out.say(&format!(
            "learning cycle {}: {}",
            report.cycle_id,
            report.action.as_str()
        ));
        if let Some(artifact) = &report.artifact_id {
            out.say(&format!("  artifact: {artifact}"));
        }
        if let Some(fingerprint) = &report.dataset_fingerprint {
            let classes = report
                .examples_by_class
                .iter()
                .map(|(class, count)| format!("{class} {count}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.say(&format!(
                "  dataset: {} ({classes})",
                fingerprint.get(..12).unwrap_or(fingerprint)
            ));
        }
        if let Some(verdict) = &report.verdict {
            if verdict.passed {
                out.say("  evaluate: every gate passed");
            } else {
                out.say("  evaluate: gates failed");
                for gate in &verdict.failed_gates {
                    out.say(&format!("    {gate}"));
                }
            }
        }
        if let Some(note) = &report.note {
            out.say(&format!("  {note}"));
        }
        if let Some(error) = &report.error {
            out.complain(&format!("relais learn cycle: {error}"));
        }
        if let Some(hint) = &report.promote_hint {
            out.say(&format!("  {hint}"));
        }
    }
    Ok(match report.action {
        relais::ledger::LearningAction::Error => CliOutcome::OperationalFailure,
        _ => CliOutcome::Accepted,
    })
}

/// `relais learn last [--json]`: the most recent `learning_cycles` row, read
/// without creating or migrating the ledger (as `native status` reads it).
fn learn_last_command(json: bool) -> Result<CliOutcome, CliError> {
    let path = paths::ledger_path().map_err(CliError::Home)?;
    let last = if path.is_file() {
        match Ledger::open_read_only(&path).and_then(|ledger| ledger.last_learning_cycle()) {
            Ok(last) => last,
            Err(e) => {
                eprintln!("relais learn last: {e}");
                return Ok(CliOutcome::OperationalFailure);
            }
        }
    } else {
        None
    };
    if json {
        print_document(&serde_json::json!({ "schema": 1, "last": last }))?;
    } else {
        match last {
            Some(row) => println!(
                "{} {}{}",
                row.started_at,
                row.action.as_str(),
                row.artifact_id
                    .map(|id| format!(" {id}"))
                    .unwrap_or_default()
            ),
            None => println!("no learning cycle has run yet"),
        }
    }
    Ok(CliOutcome::Accepted)
}

/// `relais router export-classifier --out <file|-> [--since <date>]`.
fn export_classifier_command(out: &Path, since: Option<&str>) -> Result<CliOutcome, CliError> {
    let since_at = match since {
        None => None,
        Some(text) => match chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d") {
            Ok(day) => Some(format!("{day}T00:00:00Z")),
            Err(_) => {
                return Err(CliError::Usage {
                    detail: format!("--since `{text}` is not a date (YYYY-MM-DD)"),
                })
            }
        },
    };
    let ledger = open_ledger()?;
    let (all, parsed) = operational(ledger.router_classification_counts(), "export-classifier")?;
    let rows = operational(
        ledger.router_classifications_parsed(since_at.as_deref()),
        "export-classifier",
    )?;
    let dir = paths::state_dir()
        .map_err(CliError::Home)?
        .join("classifier");
    let (lines, skipped) = operational(
        relais::router::capture::read_lines(&dir),
        "export-classifier",
    )?;
    if skipped > 0 {
        eprintln!("relais router export-classifier: {skipped} unreadable day-file line(s) skipped");
    }
    let export = relais::router::export::build(
        rows,
        &lines,
        &relais::router::table::default_table(),
        all,
        parsed,
    );
    let text = export.text();
    if out == Path::new("-") {
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        if let Err(e) = stdout
            .write_all(text.as_bytes())
            .and_then(|()| stdout.flush())
        {
            eprintln!("relais router export-classifier: stdout: {e}");
            return Ok(CliOutcome::OperationalFailure);
        }
    } else if let Err(e) = relais::fsutil::write_atomic_with_mode(out, &text, Some(0o600)) {
        eprintln!("relais router export-classifier: {}: {e}", out.display());
        return Ok(CliOutcome::OperationalFailure);
    }
    eprintln!("{}", export.counts.summary());
    Ok(CliOutcome::Accepted)
}

/// `relais dataset export-tasks --out <file|->`. Exit 0 written, 1 on a
/// ledger or IO failure. A closed stdout (`| head -1`) is the reader
/// finishing, not a failure: it ends quietly with exit 0.
fn export_tasks_command(out: &Path) -> Result<CliOutcome, CliError> {
    let ledger = open_ledger()?;
    let exported = operational(ledger.export_tasks(), "export-tasks")?;
    let rows = exported.rows;
    let mut text = String::new();
    for row in &rows {
        text.push_str(&serde_json::to_string(row).expect("an export row serializes"));
        text.push('\n');
    }
    if out == Path::new("-") {
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        match stdout
            .write_all(text.as_bytes())
            .and_then(|()| stdout.flush())
        {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                return Ok(CliOutcome::Accepted)
            }
            Err(e) => {
                eprintln!("relais dataset export-tasks: stdout: {e}");
                return Ok(CliOutcome::OperationalFailure);
            }
        }
    } else if let Err(e) = relais::fsutil::write_atomic_with_mode(out, &text, Some(0o600)) {
        eprintln!("relais dataset export-tasks: {}: {e}", out.display());
        return Ok(CliOutcome::OperationalFailure);
    }
    eprintln!(
        "relais dataset export-tasks: {} task(s) written",
        rows.len()
    );
    if exported.unreadable_contracts > 0 {
        eprintln!(
            "relais dataset export-tasks: {} task(s) with an unreadable contract exported without one",
            exported.unreadable_contracts
        );
    }
    Ok(CliOutcome::Accepted)
}

/// `relais dataset import-task-types <file>`. Every row is validated
/// before anything is written; one bad row refuses the file (exit 2).
fn import_task_types_command(file: &Path) -> Result<CliOutcome, CliError> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("relais dataset import-task-types: {}: {e}", file.display());
            return Ok(CliOutcome::OperationalFailure);
        }
    };
    let ledger = open_ledger()?;
    let known = operational(ledger.task_id_set(), "import-task-types")?;
    let labels = match relais::task_labels::parse_labels(&text, &known) {
        Ok(labels) => labels,
        Err(problems) => {
            for problem in &problems {
                eprintln!("{}:{}: {}", file.display(), problem.line, problem.reason);
            }
            eprintln!(
                "relais dataset import-task-types: {} bad row(s); nothing was written",
                problems.len()
            );
            return Ok(CliOutcome::InvalidInput);
        }
    };
    let imported = operational(ledger.import_task_types(&labels), "import-task-types")?;
    let by_type: Vec<String> = imported
        .applied
        .iter()
        .map(|(task_type, count)| format!("{task_type} {count}"))
        .collect();
    let by_type = if by_type.is_empty() {
        String::new()
    } else {
        format!(" ({})", by_type.join(", "))
    };
    eprintln!(
        "relais dataset import-task-types: {} labelled{by_type}, {} unchanged, {} kept as declared",
        imported.applied.values().sum::<u32>(),
        imported.unchanged,
        imported.kept_declared
    );
    Ok(CliOutcome::Accepted)
}

/// `relais dataset judge-trial <TRIAL_ID> --verdict pass|fail --note <TEXT>`.
fn judge_trial_command(
    trial_id: &str,
    verdict: JudgeVerdict,
    note: &str,
    json: bool,
) -> Result<CliOutcome, CliError> {
    use relais::ledger::{GroundTruthVerdict, JudgeRefusal, Judged};
    let verdict = match verdict {
        JudgeVerdict::Pass => GroundTruthVerdict::Pass,
        JudgeVerdict::Fail => GroundTruthVerdict::Fail,
    };
    let ledger = open_ledger()?;
    let id = relais::ids::TrialId::from_stored(trial_id);
    let judged = operational(ledger.judge_trial(&id, verdict, note), "judge-trial")?;
    let refuse = |detail: String| CliError::Usage {
        detail: format!("relais dataset judge-trial: trial {trial_id}: {detail}"),
    };
    let (truth, already) = match judged {
        Ok(Judged::Recorded(truth)) => (truth, false),
        Ok(Judged::AlreadyRecorded(truth)) => (truth, true),
        Err(JudgeRefusal::UnknownTrial) => return Err(refuse("no such trial".into())),
        Err(JudgeRefusal::NotRejected(outcome)) => {
            return Err(refuse(format!(
                "its outcome is {}, and only a review-rejected trial is judged",
                outcome.map_or("not settled", |outcome| outcome.as_str())
            )))
        }
        Err(JudgeRefusal::EmptyNote) => {
            return Err(refuse(
                "the note is empty; it must carry the evidence for the verdict".into(),
            ))
        }
        Err(JudgeRefusal::Conflict(existing)) => {
            return Err(refuse(format!(
                "already judged {} ({}); a verdict is final",
                existing.verdict.as_str(),
                existing.note
            )))
        }
    };
    if json {
        println!(
            "{}",
            serde_json::json!({
                "trial_id": trial_id,
                "verdict": truth.verdict.as_str(),
                "note": truth.note,
                "at": truth.at,
                "already_recorded": already,
            })
        );
    } else if already {
        println!(
            "relais dataset judge-trial: trial {trial_id} is already judged {}; nothing changed",
            truth.verdict.as_str()
        );
    } else {
        println!(
            "relais dataset judge-trial: trial {trial_id} judged {} at {}",
            truth.verdict.as_str(),
            truth.at
        );
    }
    Ok(CliOutcome::Accepted)
}

/// `relais rollback [--json]`: the previous task artifact becomes active,
/// recorded as a `learning_cycles` row from a person.
fn rollback_command(json: bool) -> Result<CliOutcome, CliError> {
    let reg = registry()?;
    let ledger = open_ledger()?;
    let result = operational(
        relais::learn::cycle::roll_back_active(
            &ledger,
            &reg,
            relais::ledger::LearningSource::Person,
            "rolled back by a person",
            None,
        ),
        "rollback",
    )?;
    match result {
        Ok(rolled) => {
            if json {
                print_document(&serde_json::json!({ "schema": 1, "rolled_back": rolled }))?;
            } else {
                println!("rolled back {} to {}", rolled.from, rolled.to);
            }
            Ok(CliOutcome::Accepted)
        }
        Err(nothing) => {
            if json {
                print_document(
                    &serde_json::json!({ "schema": 1, "rolled_back": null, "reason": nothing.to_string() }),
                )?;
            } else {
                eprintln!("relais rollback: {nothing}");
            }
            Ok(CliOutcome::Blocked)
        }
    }
}

/// `relais native learning-envelope`: the automatic-promotion grant in
/// machine.toml (the same locked, comment-keeping writer as the routing
/// envelope).
fn learning_envelope_command(
    by: &str,
    source: relais::router::Source,
) -> Result<CliOutcome, CliError> {
    if by.trim().is_empty() {
        eprintln!("relais native learning-envelope: --by is empty; name who granted it");
        return Ok(CliOutcome::InvalidInput);
    }
    let machine_path = paths::machine_settings_path().map_err(CliError::Home)?;
    let grant = relais::policy::AutoPromote {
        granted_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        by: by.to_string(),
        source: source.as_str().to_string(),
    };
    if let Err(e) = relais::learn::envelope::write_auto_promote(&machine_path, &grant) {
        eprintln!("relais native learning-envelope: {e}");
        return Ok(CliOutcome::OperationalFailure);
    }
    print_document(&serde_json::json!({ "schema": 1, "auto_promote": grant }))?;
    Ok(CliOutcome::Accepted)
}

/// `relais native learning-envelope --off`: the grant removed and the
/// `off` tombstone recorded.
fn learning_off_command(by: &str, source: relais::router::Source) -> Result<CliOutcome, CliError> {
    if by.trim().is_empty() {
        eprintln!("relais native learning-envelope: --by is empty; name who turned it off");
        return Ok(CliOutcome::InvalidInput);
    }
    let machine_path = paths::machine_settings_path().map_err(CliError::Home)?;
    let off = relais::policy::AutoPromoteOff {
        at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        by: by.to_string(),
        source: source.as_str().to_string(),
    };
    let had_grant = match relais::learn::envelope::write_off(&machine_path, &off) {
        Ok(had) => had,
        Err(e) => {
            eprintln!("relais native learning-envelope: {e}");
            return Ok(CliOutcome::OperationalFailure);
        }
    };
    print_document(
        &serde_json::json!({ "schema": 1, "auto_promote": null, "off": off, "removed": had_grant }),
    )?;
    Ok(CliOutcome::Accepted)
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

/// `relais quiet watch --protocol`: the watcher ends when its reader goes
/// away, which is how it is meant to stop, so that is success.
fn quiet_command(cmd: QuietCommand) -> Result<CliOutcome, CliError> {
    let QuietCommand::Watch { .. } = cmd;
    let Some(dir) = relais::quiet::active_dir(|name| std::env::var_os(name)) else {
        return Err(CliError::Usage {
            detail: "quiet watch: set QUIET_ACTIVE_DIR (no home or state directory found)".into(),
        });
    };
    let _ = relais::quiet::watch(&dir, &mut std::io::stdout());
    Ok(CliOutcome::Accepted)
}

fn trust_command(cmd: TrustCommand) -> Result<CliOutcome, CliError> {
    let (root, policy) = load_repo_policy()?;
    let machine = load_machine()?;
    let machine_path = paths::machine_settings_path().map_err(CliError::Home)?;
    let identity = relais::repo::identity(&root);
    let repo_key = relais::policy::repo_key(&identity);
    match cmd {
        TrustCommand::Show { json } => {
            // Read-only: the ledger is consulted only if it already
            // exists, so `show` never creates the state directory.
            let previous = previous_grant_steps(&repo_key);
            let mut shown = relais::trust::show(
                &policy,
                &identity,
                &machine,
                machine_path,
                previous.as_deref(),
            );
            shown.pending_change = pending_command_change("trust show", &repo_key, &policy);
            if json {
                print_document(&shown)?;
            } else {
                print!("{}", trust_show_text(&shown));
            }
            Ok(CliOutcome::Accepted)
        }
        TrustCommand::Grant {
            key,
            reviewed_by,
            note,
            source,
            json,
        } => {
            let shown =
                relais::trust::show(&policy, &identity, &machine, machine_path.clone(), None);
            if reviewed_by.trim().is_empty() {
                eprintln!(
                    "relais trust grant: --reviewed-by is empty; name who reviewed the commands"
                );
                return Ok(CliOutcome::InvalidInput);
            }
            let record = relais::policy::TrustGrant {
                granted_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                reviewed_by: reviewed_by.clone(),
                note,
                repo: Some(identity.label().to_string()),
            };
            let (granted, recorded_key, scope) = match key {
                None => (
                    relais::trust::grant_repository(&machine_path, &repo_key, &record),
                    repo_key.clone(),
                    GrantScope::Repository,
                ),
                Some(key) => {
                    if key != shown.grant_key {
                        if json {
                            print_document(&serde_json::json!({
                                "code": "stale_grant_key",
                                "current_key": shown.grant_key,
                            }))?;
                        }
                        eprintln!(
                            "relais trust grant: refused (stale_grant_key): {key} is not this \
                             policy's key; relais.toml changed since it was shown. The current \
                             key is {}; review it with `relais trust show`",
                            shown.grant_key
                        );
                        return Ok(CliOutcome::StaleGrantKey);
                    }
                    eprintln!(
                        "relais trust grant: --key is deprecated: it grants this exact policy \
                         only. `relais trust grant --reviewed-by <name>` trusts the repository \
                         once"
                    );
                    (
                        relais::trust::grant(&machine_path, &key, &record),
                        key,
                        GrantScope::Legacy,
                    )
                }
            };
            let granted = match granted {
                Ok(granted) => granted,
                Err(e) => {
                    eprintln!("relais trust grant: {e}");
                    return Ok(CliOutcome::OperationalFailure);
                }
            };
            // The grant is in machine.toml now; the ledger row is its audit
            // trail and the base of the next question's change marks, and a
            // repository grant also starts the declaration the command
            // warning compares against. A ledger that cannot take it is
            // said, not turned into a failed grant: the grant stands
            // either way.
            if granted == relais::trust::Granted::Written {
                if let Err(e) = record_grant(
                    &shown,
                    &recorded_key,
                    &repo_key,
                    &identity,
                    &reviewed_by,
                    source,
                    scope,
                    &policy,
                ) {
                    eprintln!(
                        "relais trust grant: warning: the grant is in machine.toml, but the ledger \
                         did not record it ({e}); the next question will not mark what changed"
                    );
                }
            }
            let written = granted == relais::trust::Granted::Written;
            if json {
                print_document(&serde_json::json!({
                    "code": if written { "granted" } else { "already_granted" },
                    "scope": scope.as_str(),
                    "grant_key": recorded_key,
                    "machine_settings": machine_path,
                }))?;
            } else if written {
                println!(
                    "trust grant: {} {recorded_key} recorded in {}",
                    scope.as_str(),
                    machine_path.display()
                );
            } else {
                println!(
                    "trust grant: {} {recorded_key} was already in {}",
                    scope.as_str(),
                    machine_path.display()
                );
            }
            Ok(CliOutcome::Accepted)
        }
    }
}

/// The steps the latest recorded grant for this repository authorized,
/// when the ledger exists and holds one. Best effort: a ledger that
/// cannot be read only costs the change marks.
fn previous_grant_steps(repo_key: &str) -> Option<Vec<relais::trust::Step>> {
    let path = paths::ledger_path().ok()?;
    if !path.exists() {
        return None;
    }
    let read = Ledger::open(&path)
        .map_err(|e| e.to_string())
        .and_then(|ledger| {
            ledger
                .latest_trust_grant_steps(repo_key)
                .map_err(|e| e.to_string())
        })
        .and_then(|json| match json {
            Some(json) => serde_json::from_str(&json)
                .map(Some)
                .map_err(|e| e.to_string()),
            None => Ok(None),
        });
    match read {
        Ok(steps) => steps,
        Err(e) => {
            eprintln!(
                "relais trust show: warning: the last grant could not be read from the ledger ({e}); \
                 changes are not marked"
            );
            None
        }
    }
}

/// The ledger row of a grant just written: who, from where, and the steps
/// it authorized.
#[allow(clippy::too_many_arguments)]
fn record_grant(
    shown: &relais::trust::Shown,
    key: &str,
    repo_key: &str,
    identity: &RepoIdentity,
    reviewed_by: &str,
    source: GrantSource,
    scope: GrantScope,
    policy: &RepoPolicy,
) -> Result<(), CliError> {
    let steps = operational(
        serde_json::to_string(&shown.declaration.steps),
        "rendering the granted steps",
    )?;
    let ledger = open_ledger()?;
    operational(
        ledger.record_trust_grant(&relais::ledger::TrustGrantRecord {
            grant_key: key,
            repo_key,
            repo: identity.label(),
            reviewed_by,
            source: source.as_str(),
            steps_json: &steps,
        }),
        "recording the grant in the ledger",
    )?;
    match scope {
        // A repository grant starts the declaration the command warning
        // compares against, so the first change after it is the first said.
        GrantScope::Repository => {
            use relais::trust::DeclarationStore;
            operational(
                relais::trust::Recorder(&ledger)
                    .record(repo_key, &relais::trust::commands_of(policy))
                    .map_err(std::io::Error::other),
                "recording the starting declaration in the ledger",
            )
        }
        // A legacy grant trusts one exact policy; the commands it covers
        // are its key, so there is no declaration to start.
        GrantScope::Legacy => Ok(()),
    }
}

/// A change to the repository's commands that no `relais run` has said
/// yet, read without consuming it (`plan`, `trust show`): from the ledger
/// when there is one, nothing otherwise. Best effort — a ledger that
/// cannot be read costs the report, never the command, and is said on
/// stderr under `command`'s name.
fn pending_command_change(
    command: &str,
    repo_key: &str,
    policy: &RepoPolicy,
) -> Option<relais::trust::CommandsChanged> {
    let ledger = match paths::ledger_path()
        .ok()
        .filter(|path| path.exists())
        .map(|path| Ledger::open(&path))
        .transpose()
    {
        Ok(ledger) => ledger,
        Err(e) => {
            eprintln!(
                "relais {command}: warning: the ledger could not be opened ({e}); a pending \
                 change to the commands is not reported"
            );
            return None;
        }
    };
    let seen = match &ledger {
        Some(ledger) => relais::trust::observe(&relais::trust::Reader(ledger), repo_key, policy),
        None => relais::trust::observe(&relais::trust::NoHistory, repo_key, policy),
    };
    match seen {
        Ok(change) => change,
        Err(e) => {
            eprintln!(
                "relais {command}: warning: the commands this repository last ran under could \
                 not be read ({e}); a pending change to them is not reported"
            );
            None
        }
    }
}

/// `relais trust show` for a person: the key on a line of its own, then
/// the commands with their change marks, the models and integrations.
fn trust_show_text(shown: &relais::trust::Shown) -> String {
    use relais::trust::{display_argv, escape_display, Change};
    let mut out = String::new();
    match &shown.repository_grant {
        Some(grant) => out.push_str(&format!(
            "repository: granted {} by {}\n",
            escape_display(&grant.granted_at),
            escape_display(&grant.reviewed_by)
        )),
        None => out.push_str("repository: not granted\n"),
    }
    out.push_str(&format!(
        "identity: {}\n",
        escape_display(&shown.repository)
    ));
    out.push_str(&format!("grant key (legacy): {}\n", shown.grant_key));
    out.push_str(&format!(
        "status: {}\n",
        match (&shown.repository_grant, shown.granted) {
            (Some(_), _) => "granted (repository)",
            (None, true) => "granted (legacy grant for this exact policy)",
            (None, false) => "not granted",
        }
    ));
    out.push_str(&format!(
        "machine.toml: {} ($RELAIS_CONFIG_DIR, else ~/.config/relais)\n",
        shown.machine_settings.display()
    ));
    out.push_str("commands relais would run:\n");
    let width = shown
        .declaration
        .steps
        .iter()
        .map(|step| step.kind.as_str().len())
        .max()
        .unwrap_or(5);
    for (i, step) in shown.declaration.steps.iter().enumerate() {
        let mark = match shown.changes.as_ref().map(|changes| changes[i]) {
            Some(Change::New) => '+',
            Some(Change::Changed) => '~',
            _ => ' ',
        };
        out.push_str(&format!(
            "{mark} {:width$}  {}  [{}]\n",
            step.kind.as_str(),
            display_argv(&step.argv),
            escape_display(&step.profile),
        ));
    }
    let models: Vec<String> = shown
        .declaration
        .models
        .iter()
        .map(|model| format!("{} {}", model.tier, escape_display(&model.id)))
        .collect();
    out.push_str(&format!("models: {}\n", models.join(" · ")));
    let integrations: Vec<String> = shown
        .declaration
        .integrations
        .iter()
        .map(|line| format!("{} ({})", line.name, line.mode))
        .collect();
    out.push_str(&format!(
        "integrations: {}\n",
        if integrations.is_empty() {
            "none".to_string()
        } else {
            integrations.join(" · ")
        }
    ));
    if let Some(change) = &shown.pending_change {
        out.push_str(&format!("{}\n", change.text()));
    }
    out
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
            let (limits, agent_lease_ttl, disk_floor) = match std::fs::read_to_string(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
                    coordinator::effective_limits(&Default::default()),
                    Duration::from_secs(HookAdmissionSettings::default().binding_lease_secs),
                    relais::disk::DEFAULT_MIN_FREE_BYTES,
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
                        machine.resources.min_free_bytes(),
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
            match coordinator::run_daemon(
                &socket,
                limits,
                ledger.as_ref(),
                agent_lease_ttl,
                disk_floor,
            ) {
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
            let cancelled_run = run.clone();
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
                    if let (Some(run_id), true) = (cancelled_run, dispatches.is_empty()) {
                        return finish_workerless_cancel(&run_id);
                    }
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

/// `coordinator cancel --run` that signalled no worker: the same path as
/// `native cancel`, with the operator's authority (no session to match).
fn finish_workerless_cancel(run_id: &str) -> Result<CliOutcome, CliError> {
    let ledger = open_ledger()?;
    let run = RunId::from_stored(run_id);
    match cancel_standing(&ledger, &run)? {
        CancelStanding::Unknown => {
            eprintln!("relais coordinator cancel: run {run_id} is not on record");
            Ok(CliOutcome::UnknownRun)
        }
        CancelStanding::Settled(line) => {
            println!("{line}");
            Ok(CliOutcome::Accepted)
        }
        CancelStanding::Live { .. } => {
            println!("{}", await_cancel(&ledger, &run)?);
            Ok(CliOutcome::Accepted)
        }
    }
}

/// Where a run stands when a cancel arrives.
enum CancelStanding {
    /// The ledger has no such run.
    Unknown,
    /// Nothing is left to cancel: the line to print (already ended, or
    /// reaped by this call because its driver is gone).
    Settled(String),
    /// A live driver; `session` is the session that started the run.
    Live { session: Option<String> },
}

/// How long `native cancel` waits for a live driver to end its run.
const CANCEL_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Judge a run for cancelling: an ended run is reported as it is, one whose
/// driver is gone or silent is reaped as cancelled (any session may clear
/// it), and any other is live.
fn cancel_standing(ledger: &Ledger, run: &RunId) -> Result<CancelStanding, CliError> {
    let Some(state) = operational(ledger.run_status(run), "native cancel")? else {
        return Ok(CancelStanding::Unknown);
    };
    if state.is_terminal() {
        return Ok(CancelStanding::Settled(format!(
            "run {run} already ended: {state}"
        )));
    }
    match reap_if_driverless(ledger, run, state)? {
        Some(line) => Ok(CancelStanding::Settled(line)),
        None => Ok(CancelStanding::Live {
            session: operational(ledger.run_root_session(run), "native cancel")?,
        }),
    }
}

/// Reap a non-terminal run as cancelled when its driver is Gone or Silent.
/// `None` when the driver may be alive, or a dispatch of the run is.
fn reap_if_driverless(
    ledger: &Ledger,
    run: &RunId,
    state: State,
) -> Result<Option<String>, CliError> {
    let runs_dir = paths::runs_dir().map_err(CliError::Home)?;
    let silent = operational(
        relais::reap::run_silence(ledger, &runs_dir, run),
        "native cancel",
    )?;
    let verdict = operational(
        relais::reap::orphan_verdict_of_run(ledger, run, silent),
        "native cancel",
    )?;
    if verdict == resume::OrphanVerdict::Alive {
        return Ok(None);
    }
    let events_dir = runs_dir.join(run.as_str());
    let reaped = relais::reap::reap(
        ledger,
        &relais::reap::Reap {
            run,
            from: state,
            ending: relais::reap::Ending::Cancelled,
            detail: relais::reap::orphan_detail(ledger, run, verdict, silent),
            extra: serde_json::json!({ "verdict": format!("{verdict:?}") }),
            events_dir: &events_dir,
        },
        &|pid| relais::coordinator::process_alive(pid.get()),
    );
    match reaped {
        Ok(relais::reap::Reaped::Moved(_)) => {}
        // The ledger holds the reap; only the events file missed it.
        Err(e @ relais::reap::ReapError::EventsNotWritten { .. }) => {
            eprintln!("relais native cancel: {e}");
        }
        Ok(relais::reap::Reaped::AlreadyMoved) => {
            let now = operational(ledger.run_status(run), "native cancel")?.unwrap_or(state);
            return Ok(Some(format!("run {run} already ended: {now}")));
        }
        Ok(relais::reap::Reaped::KeptAlive(_)) => return Ok(None),
        Err(e) => {
            return Err(CliError::Operational {
                operation: "native cancel",
                cause: Box::new(e),
            })
        }
    }
    Ok(Some(format!(
        "run {run} had no live driver; marked cancelled"
    )))
}

/// After a cancel was asked of the coordinator: wait for the driver to end
/// the run, reaping it as cancelled if the driver dies meanwhile. The line
/// to print.
fn await_cancel(ledger: &Ledger, run: &RunId) -> Result<String, CliError> {
    let until = std::time::Instant::now() + CANCEL_WAIT;
    loop {
        let state = operational(ledger.run_status(run), "native cancel")?;
        match state {
            Some(State::Cancelled) => return Ok(format!("run {run} cancelled")),
            Some(ended) if ended.is_terminal() => {
                return Ok(format!("run {run} already ended: {ended}"))
            }
            Some(open) => {
                if let Some(line) = reap_if_driverless(ledger, run, open)? {
                    return Ok(line);
                }
            }
            None => return Ok(format!("run {run} is not on record")),
        }
        if std::time::Instant::now() >= until {
            return Ok("cancel requested; the driver has not ended yet".to_string());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// `relais native cancel --run X --session S` (SPEC §23): end a run.
fn native_cancel_command(run_id: &str, session: Option<&str>) -> Result<CliOutcome, CliError> {
    let Some(session) = session else {
        return Err(CliError::Usage {
            detail: "native cancel: --session is required".into(),
        });
    };
    let ledger = open_ledger()?;
    let run = RunId::from_stored(run_id);
    match cancel_standing(&ledger, &run)? {
        CancelStanding::Unknown => {
            eprintln!("relais native cancel: run {run_id} is not on record");
            Ok(CliOutcome::UnknownRun)
        }
        CancelStanding::Settled(line) => {
            println!("{line}");
            Ok(CliOutcome::Accepted)
        }
        CancelStanding::Live { session: owner } if owner.as_deref() != Some(session) => {
            println!(
                "run {run_id} belongs to live session {}; cancel it from that session",
                owner.as_deref().unwrap_or("(none recorded)")
            );
            Ok(CliOutcome::ResumeRefusedLive)
        }
        CancelStanding::Live { .. } => {
            let socket = relais::coordinator::socket_path().map_err(CliError::Home)?;
            let client = relais::coordinator::Client::new(socket);
            match client.request(&relais::coordinator::Request::CancelRun {
                run_id: run_id.to_string(),
            }) {
                Ok(relais::coordinator::Response::Cancelled { .. }) => {
                    println!("{}", await_cancel(&ledger, &run)?);
                    Ok(CliOutcome::Accepted)
                }
                Ok(other) => {
                    eprintln!("relais native cancel: unexpected reply {other:?}");
                    Ok(CliOutcome::OperationalFailure)
                }
                Err(e) => {
                    eprintln!("relais native cancel: {e}");
                    Ok(CliOutcome::Blocked)
                }
            }
        }
    }
}

/// Run this same `relais resume` again with its output appended to `log`
/// (a timestamp line first), and report whether it succeeded. A detached
/// sweep has no terminal to write to.
fn resume_logged(log: &Path) -> Result<CliOutcome, CliError> {
    use std::io::Write;
    let open = || -> std::io::Result<std::fs::File> {
        if let Some(parent) = log.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
    };
    let attempt = || -> std::io::Result<std::process::ExitStatus> {
        let mut file = open()?;
        writeln!(file, "--- {} relais resume", relais::ledger::now_rfc3339())?;
        let mut args = std::env::args_os().skip(1);
        let mut kept = Vec::new();
        while let Some(arg) = args.next() {
            if arg == "--log" {
                args.next();
            } else if !arg.to_string_lossy().starts_with("--log=") {
                kept.push(arg);
            }
        }
        std::process::Command::new(std::env::current_exe()?)
            .args(kept)
            .stdout(file.try_clone()?)
            .stderr(file)
            .status()
    };
    match attempt() {
        Ok(status) if status.success() => Ok(CliOutcome::Accepted),
        Ok(_) => Ok(CliOutcome::OperationalFailure),
        Err(e) => {
            eprintln!("relais resume: cannot write the log {}: {e}", log.display());
            Ok(CliOutcome::OperationalFailure)
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

/// The repository policy a cycle reads when there is no `relais.toml`.
fn default_repo_policy() -> Result<RepoPolicy, CliError> {
    RepoPolicy::from_toml_str(&format!(
        "schema_version = {}\n",
        relais::policy::POLICY_SCHEMA_VERSION
    ))
    .map_err(|cause| CliError::Operational {
        operation: "learn cycle",
        cause: Box::new(cause),
    })
}

fn load_machine() -> Result<MachineSettings, CliError> {
    let path = paths::machine_settings_path().map_err(CliError::Home)?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        // Absent is empty: no ceilings, no grants. A fresh machine then
        // reaches the trust check and is told `missing_trust_grant`,
        // which names the way out, instead of failing on a read before
        // any run exists (SPEC §5).
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => {
            format!(
                "schema_version = {}\n",
                relais::policy::MACHINE_SCHEMA_VERSION
            )
        }
        // Unreadable is not absent: ceilings and grants the file states
        // would silently not apply.
        Err(cause) => {
            return Err(CliError::Read {
                what: "the machine settings",
                path,
                cause,
            })
        }
    };
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

/// `relais native contract`: the model writes a task as JSON, the plugin
/// pipes it here. Refused, by its schema error, unless it is a contract —
/// so a sentence of prose never becomes a run. Saved as
/// `<root>/.relais/tasks/<hash>.json`, under the `.relais/` the dirty-tree
/// check already leaves alone, with a `.gitignore` that keeps it out of
/// `git status`.
fn native_contract_command() -> Result<CliOutcome, CliError> {
    use std::io::Read;
    let mut text = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut text) {
        eprintln!("relais native contract: stdin could not be read: {e}");
        return Ok(CliOutcome::InvalidInput);
    }
    let contract = match TaskContract::from_json_str(&text) {
        Ok(contract) => contract,
        Err(e) => {
            eprintln!(
                "relais native contract: not a task contract: {e}. Required: schema_version, kind, \
                 objective, base_ref, write_scope, acceptance, verification_profile"
            );
            return Ok(CliOutcome::InvalidInput);
        }
    };
    let root = project_dir()?;
    let dir = root.join(".relais");
    let tasks = dir.join("tasks");
    let written = (|| -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(&tasks)?;
        let ignore = dir.join(".gitignore");
        if !ignore.exists() {
            std::fs::write(&ignore, "*\n")?;
        }
        let hash = contract.hash();
        let path = tasks.join(format!("{}.json", &hash[..hash.len().min(16)]));
        std::fs::write(&path, &text)?;
        Ok(path)
    })();
    match written {
        Ok(path) => {
            println!("{}", path.display());
            Ok(CliOutcome::Accepted)
        }
        Err(e) => {
            eprintln!(
                "relais native contract: {} could not be written: {e}",
                tasks.display()
            );
            Ok(CliOutcome::OperationalFailure)
        }
    }
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

/// `relais hook --probe --record <dir>`: the record-only handler
/// for capturing hook payloads as fixtures. Reads one payload from
/// stdin, writes it verbatim, decides nothing, and always accepts — the
/// handler cannot fail the session it is watching.
fn hook_command(dir: &Path) -> Result<CliOutcome, CliError> {
    let order = relais::hook::arrival_nanos();
    relais::hook::record(dir, order, std::io::stdin().lock());
    Ok(CliOutcome::Accepted)
}

/// `relais hook`, run with no flags: read one payload on stdin, decide
/// what to say about it, and say it (SPEC §23). Always accepts — a
/// non-zero exit from a hook is reported to the session as a failure of
/// the tool call it was watching, so a relais that cannot answer must be
/// indistinguishable from a relais that had nothing to say. Every path
/// that could fail (an unreadable settings file, an unreachable
/// coordinator, a payload that is not JSON, a panic anywhere inside) is
/// swallowed rather than surfaced; `relais doctor` is where any of that
/// is reported as a finding.
fn hook_respond_command() -> CliOutcome {
    // The whole body, not just `respond::handle`: reading stdin,
    // loading settings and journalling all run here too, and none of
    // them may take the process down with them any more than the
    // decision itself may.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(hook_respond));
    CliOutcome::Accepted
}

fn hook_respond() {
    use std::io::Read;
    let mut payload = Vec::new();
    // A truncated read leaves `payload` with whatever arrived so far;
    // `event::parse` treats that the same as any other input it cannot
    // make sense of (`HookEvent::NotOurs`), so the hook still answers.
    let _ = std::io::stdin().lock().read_to_end(&mut payload);

    let settings = hook_admission_settings();

    let Ok(socket) = relais::coordinator::socket_path() else {
        // No home directory: nothing to connect to and nowhere to
        // journal. `decide_or_silent` still owes an answer for "the
        // coordinator could not be reached" under this machine's own
        // stance, so build one directly rather than skip the decision
        // entirely.
        let event = relais::hook::event::parse(&payload);
        let answer = relais::hook::decide::decide_or_silent(
            &event,
            &settings,
            None,
            std::time::Duration::ZERO,
        );
        if let Some(text) = answer.stdout_payload() {
            println!("{text}");
        }
        return;
    };
    let gate = relais::coordinator::RemoteGate::new(socket);
    let handled = relais::hook::respond::handle(&payload, &settings, &gate);
    if let Some(text) = handled.answer.stdout_payload() {
        println!("{text}");
    }
    if let Ok(path) = paths::hook_journal_path() {
        let entry = relais::hook::respond::journal_entry(&payload, &handled);
        // Best effort, like every other step here: a journal write
        // failure is not the tool call's to report, and it already
        // answered above.
        let _ = relais::hook::respond::append_journal(&path, &entry);
    }
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

fn init_command() -> Result<CliOutcome, CliError> {
    // At the repository root, not the shell's cwd: a policy written in a
    // subdirectory would govern nothing.
    let path = project_dir()?.join("relais.toml");
    match relais::repo::write_init_template(&path) {
        Ok(true) => {
            println!("{}", init_written_hint(&path));
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

/// What a person does after init wrote a policy: review it, then grant.
fn init_written_hint(path: &Path) -> String {
    format!(
        "wrote {} (review it, then: relais trust show; relais trust grant --key <key> --reviewed-by <name>)",
        path.display()
    )
}

/// `relais init --detect`: propose a policy from the repository's files,
/// print it, and with `--write` write it — never over an existing one.
/// Nothing detected and nothing typed is its own exit code (19), with no
/// file written, so a caller can ask a person for the command.
fn init_detect_command(
    json: bool,
    write: bool,
    command: Option<&str>,
) -> Result<CliOutcome, CliError> {
    let root = project_dir()?;
    // Detection reads the root's listing; one that cannot be read is not
    // "nothing detected", which would send a person to type a command.
    if let Err(e) = std::fs::read_dir(&root) {
        eprintln!(
            "relais init --detect: {} could not be read: {e}",
            root.display()
        );
        return Ok(CliOutcome::OperationalFailure);
    }
    let mut proposal = relais::repo::detect_policy(&root);
    // Diagnostics, on stderr: stdout stays the proposal alone.
    for note in &proposal.notes {
        eprintln!(
            "relais init --detect: {}",
            relais::repo::escape_control(note)
        );
    }
    if let Some(text) = command {
        match relais::repo::parse_command_text(text) {
            Ok(argv) => proposal = proposal.with_command(argv, "--command"),
            Err(e) => {
                eprintln!("relais init --command: {e}");
                return Ok(CliOutcome::InvalidInput);
            }
        }
    }
    if proposal.is_empty() {
        if json {
            println!("{}", serde_json::json!({ "proposal": null }));
        } else {
            for line in skipped_lines(&proposal) {
                println!("{line}");
            }
        }
        eprintln!(
            "relais init --detect: no verification command found in {} \
             (no Makefile check/test target, Cargo.toml, go.mod, package.json \
             test script or pytest); name one with --command",
            root.display()
        );
        return Ok(CliOutcome::NothingDetected);
    }
    let path = root.join("relais.toml");
    if write {
        match relais::repo::write_new_policy(&path, &proposal.toml) {
            Ok(true) => {}
            Ok(false) => {
                eprintln!(
                    "relais init: {} already exists; init never overwrites",
                    path.display()
                );
                return Ok(CliOutcome::InvalidInput);
            }
            Err(e) => {
                eprintln!("relais init: {e}");
                return Ok(CliOutcome::OperationalFailure);
            }
        }
    }
    if json {
        println!("{}", serde_json::json!({ "proposal": proposal }));
    } else {
        for line in proposal_lines(&proposal) {
            println!("{line}");
        }
    }
    if write {
        // A diagnostic: stdout carries the proposal alone, so it stays
        // one JSON document, or lines of at most 80 columns.
        eprintln!("{}", init_written_hint(&path));
    }
    Ok(CliOutcome::Accepted)
}

/// The proposal as plain lines: one per setup step and command, with its
/// source, then what was seen and not proposed. At most 80 columns where
/// the argv allows it: a source that does not fit goes on its own line.
/// Repository text is escaped, so a crafted name cannot draw a line.
fn proposal_lines(proposal: &relais::repo::Proposal) -> Vec<String> {
    const WIDTH: usize = 80;
    let mut lines = Vec::new();
    let rows = proposal
        .setup
        .iter()
        .map(|step| ("setup", step))
        .chain(proposal.commands.iter().map(|check| ("check", check)));
    for (kind, item) in rows {
        let argv = relais::repo::escape_control(&item.argv.join(" "));
        let source = relais::repo::escape_control(&item.source);
        let head = format!("  {kind}  {argv}");
        let one_line = format!("{head}   {source}");
        if one_line.chars().count() <= WIDTH {
            lines.push(one_line);
        } else {
            lines.push(head);
            lines.push(format!("         {source}"));
        }
    }
    lines.extend(skipped_lines(proposal));
    lines
}

fn skipped_lines(proposal: &relais::repo::Proposal) -> Vec<String> {
    proposal
        .skipped
        .iter()
        .map(|skip| {
            format!(
                "Skipped: {} ({})",
                relais::repo::escape_control(&skip.what),
                relais::repo::escape_control(&skip.reason)
            )
        })
        .collect()
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
    // And the design: `run` refuses a contract whose design is missing,
    // oversized or not text at the base revision, so `plan` does too.
    if let Some(design) = &contract.design {
        if let Err(e) =
            relais::context::read_design(&relais::workspace::SystemGit, &root, &base_sha, design)
        {
            eprintln!("relais plan: blocked (design_unreadable): {e} (base {base_sha})");
            return Ok(CliOutcome::Blocked);
        }
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
    let task_type = match (&task_override, contract.task_type) {
        (_, Some(declared)) => Some(declared),
        (Some(task), None) => operational(open_ledger()?.routing_type(task, None), "plan")?,
        (None, None) => None,
    };
    let decision = route::route(route::RouteInputs {
        draw_key: None,
        task_type,
        has_design: contract.design.is_some(),
        admission: route::Admission::Live,
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
    match authority.trust_status {
        relais::policy::TrustStatus::Repository => {
            println!(
                "trust grant: repository {} (in machine.toml)",
                relais::policy::repo_key(&repo_identity)
            );
        }
        relais::policy::TrustStatus::LegacyExact => {
            println!("trust grant: {} (in machine.toml)", authority.grant_key);
        }
        relais::policy::TrustStatus::Missing => {
            println!(
                "{}",
                missing_grant_text(&authority.grant_key, &repo_identity)?
            );
        }
    }
    // A pending change to the commands is reported here and consumed only
    // by `run` (ADR-0007). On stderr: `plan`'s stdout stays what it was.
    if let Some(change) =
        pending_command_change("plan", &relais::policy::repo_key(&repo_identity), &repo)
    {
        eprintln!("{}", change.text());
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

/// `relais native …`: what the relais plugin tells the coordinator, and
/// the run timeline it reads back (SPEC §23, §29). Exit 0 acknowledges.
fn native_command(cmd: NativeCommand) -> Result<CliOutcome, CliError> {
    match cmd {
        NativeCommand::Status { run, session } => {
            native_status_command(run.as_deref(), session.as_deref())
        }
        NativeCommand::Cancel { run, session } => native_cancel_command(&run, session.as_deref()),
        NativeCommand::Usage { dispatch } => native_usage_command(&dispatch),
        NativeCommand::Contract => native_contract_command(),
        NativeCommand::RouterState { session } => Ok(router_state_command(&session)),
        NativeCommand::RouterObserve => Ok(router_observe_command()),
        NativeCommand::ReviewOutcome {
            ledger,
            plan_root,
            dry_run,
        } => Ok(if dry_run {
            review_outcome_preview(ledger, plan_root)
        } else {
            review_outcome_ingest(ledger, plan_root)
        }),
        NativeCommand::GateStamp { cwd } => Ok(gate_stamp_command(&cwd)),
        NativeCommand::RouterEnvelope {
            by,
            off: true,
            source,
            ..
        } => router_off_command(&by, router_source(source)),
        NativeCommand::RouterEnvelope {
            by,
            epsilon_max,
            source,
            ..
        } => router_envelope_command(&by, epsilon_max.unwrap_or(f64::NAN), router_source(source)),
        NativeCommand::LearningEnvelope {
            by,
            off: true,
            source,
        } => learning_off_command(&by, router_source(source)),
        NativeCommand::LearningEnvelope { by, source, .. } => {
            learning_envelope_command(&by, router_source(source))
        }
        NativeCommand::DecideNonce { session, end } => {
            let ledger = open_ledger()?;
            if end {
                operational(ledger.end_decide_nonce(&session), "native decide-nonce")?;
            } else {
                println!(
                    "{}",
                    operational(ledger.mint_decide_nonce(&session), "native decide-nonce")?
                );
            }
            Ok(CliOutcome::Accepted)
        }
        NativeCommand::Hello { session } => {
            let socket = relais::coordinator::socket_path().map_err(CliError::Home)?;
            // The first call of a session may find no daemon: start one, as
            // `relais run` would.
            if let Err(e) = relais::coordinator::ensure_running(&socket) {
                eprintln!("relais native hello: {e}");
                return Ok(CliOutcome::OperationalFailure);
            }
            acknowledged(
                "hello",
                relais::coordinator::RemoteGate::new(socket).native_hello(&session),
            )
        }
        NativeCommand::Bound { dispatch, agent } => {
            let gate = native_gate()?;
            match gate.native_bound(&dispatch, &agent) {
                Ok(BoundOutcome::Bound) => Ok(CliOutcome::Accepted),
                Ok(BoundOutcome::Refused { reason }) => {
                    eprintln!("relais native bound: refused: {reason}");
                    Ok(CliOutcome::Blocked)
                }
                Ok(BoundOutcome::NotNative) => {
                    eprintln!("relais native bound: no native dispatch {dispatch} on record");
                    Ok(CliOutcome::UnknownRun)
                }
                Err(e) => {
                    eprintln!("relais native bound: {e}");
                    Ok(CliOutcome::OperationalFailure)
                }
            }
        }
        NativeCommand::Stopped { dispatch } => {
            let report = match read_stopped_report(&read_stdin()?) {
                Ok(report) => report,
                Err(e) => {
                    return Err(CliError::Usage {
                        detail: format!(
                            "native stopped: the payload on stdin is not a report: {e}"
                        ),
                    })
                }
            };
            let gate = native_gate()?;
            match gate.native_stopped(&dispatch, &report) {
                Ok(StoppedOutcome::Recorded | StoppedOutcome::AlreadyStopped) => {
                    Ok(CliOutcome::Accepted)
                }
                Ok(StoppedOutcome::WrongAgent) => {
                    eprintln!(
                        "relais native stopped: dispatch {dispatch} is bound to another agent \
                         than {}",
                        report.agent_id
                    );
                    Ok(CliOutcome::Blocked)
                }
                Ok(StoppedOutcome::NotNative) => {
                    eprintln!("relais native stopped: no native dispatch {dispatch} on record");
                    Ok(CliOutcome::UnknownRun)
                }
                Err(e) => {
                    eprintln!("relais native stopped: {e}");
                    Ok(CliOutcome::OperationalFailure)
                }
            }
        }
    }
}

/// The coordinator the plugin's callbacks talk to. A callback never starts
/// one: with none running there is no dispatch to tell it about.
/// `relais native router-state` (SPEC §30). Its own exits, per the wire
/// contract: 0 with the document on stdout, 1 when it cannot answer (the
/// plugin then runs in shadow); clap's 2 covers bad arguments.
fn router_state_command(session: &str) -> CliOutcome {
    if session.trim().is_empty() {
        eprintln!("relais native router-state: --session is empty");
        return CliOutcome::InvalidInput;
    }
    let answered = cwd()
        .map_err(|e| e.to_string())
        .and_then(|cwd| relais::router::router_state(session, &cwd).map_err(|e| e.to_string()))
        .and_then(|state| serde_json::to_string(&state).map_err(|e| e.to_string()));
    match answered {
        Ok(text) => {
            println!("{text}");
            CliOutcome::Accepted
        }
        Err(e) => {
            eprintln!("relais native router-state: {e}");
            CliOutcome::OperationalFailure
        }
    }
}

/// `relais native gate-stamp`: 0 with the JSON document on stdout, 1 when
/// git fails, 2 when `--cwd` is not a directory.
fn gate_stamp_command(repo: &std::path::Path) -> CliOutcome {
    if !repo.is_dir() {
        eprintln!(
            "relais native gate-stamp: {} is not a directory",
            repo.display()
        );
        return CliOutcome::InvalidInput;
    }
    match relais::router::gate::read(repo)
        .map_err(|e| e.to_string())
        .and_then(|stamp| serde_json::to_string(&stamp).map_err(|e| e.to_string()))
    {
        Ok(text) => {
            println!("{text}");
            CliOutcome::Accepted
        }
        Err(e) => {
            eprintln!("relais native gate-stamp: {e}");
            CliOutcome::OperationalFailure
        }
    }
}

/// `relais native review-outcome`: 0 done (or no ledger to read), 1 the
/// ledger file or the relais ledger failed, 2 the usage, 3 the ledger file
/// crosses the trust boundary. `Err` carries the exit for a run that stops
/// before labelling; `Ok(None)` is no ledger at all.
fn review_outcome_labels(
    ledger: Option<PathBuf>,
    plan_root: Option<PathBuf>,
) -> Result<Option<relais::review_outcome::Labelled>, CliOutcome> {
    use relais::review_outcome::{read, IngestError, Settings};
    let settings = Settings::resolve(ledger, plan_root).map_err(|e| {
        eprintln!("relais native review-outcome: {e}");
        CliOutcome::InvalidInput
    })?;
    match read(&settings) {
        Ok(Some(labelled)) => Ok(Some(labelled)),
        Ok(None) => {
            println!("no reviewer outcomes at {}", settings.ledger.display());
            Ok(None)
        }
        Err(e) => {
            eprintln!("relais native review-outcome: {e}");
            Err(match e {
                IngestError::Refused(_) => CliOutcome::Blocked,
                IngestError::Unreadable(_) | IngestError::Ledger(_) => {
                    CliOutcome::OperationalFailure
                }
            })
        }
    }
}

/// `--dry-run`: each label and the summary, and nothing written.
fn review_outcome_preview(ledger: Option<PathBuf>, plan_root: Option<PathBuf>) -> CliOutcome {
    let labelled = match review_outcome_labels(ledger, plan_root) {
        Ok(Some(labelled)) => labelled,
        Ok(None) => return CliOutcome::Accepted,
        Err(outcome) => return outcome,
    };
    for line in labelled.lines() {
        println!("{line}");
    }
    println!("{}", labelled.summary());
    CliOutcome::Accepted
}

/// The labels, written to the `review_outcomes` table.
fn review_outcome_ingest(ledger: Option<PathBuf>, plan_root: Option<PathBuf>) -> CliOutcome {
    let labelled = match review_outcome_labels(ledger, plan_root) {
        Ok(Some(labelled)) => labelled,
        Ok(None) => return CliOutcome::Accepted,
        Err(outcome) => return outcome,
    };
    let written = open_ledger()
        .map_err(|e| e.to_string())
        .and_then(|ledger| labelled.record(&ledger).map_err(|e| e.to_string()));
    let unmatched = match written {
        Ok(unmatched) => unmatched,
        Err(e) => {
            eprintln!("relais native review-outcome: {e}");
            return CliOutcome::OperationalFailure;
        }
    };
    println!("{}", labelled.summary_after(unmatched));
    CliOutcome::Accepted
}

/// `relais native router-observe`: 0 recorded (duplicates included), 2
/// the payload is invalid and nothing was written, 1 the ledger failed
/// (the plugin retries).
fn router_observe_command() -> CliOutcome {
    use std::io::Read;
    let mut payload = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut payload) {
        // Not a bad payload: nothing was read, so the plugin retries.
        eprintln!("relais native router-observe: stdin could not be read: {e}");
        return CliOutcome::OperationalFailure;
    }
    let ledger_path = match paths::ledger_path() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("relais native router-observe: {e}");
            return CliOutcome::OperationalFailure;
        }
    };
    let policy = match (paths::machine_settings_path(), paths::state_dir()) {
        (Ok(machine_path), Ok(state_dir)) => relais::router::capture_policy(
            &machine_path,
            &state_dir,
            std::env::var(relais::router::mode::MODE_ENV)
                .ok()
                .as_deref(),
            chrono::Utc::now(),
        ),
        // The ledger path resolved from the same home, so this does not
        // happen; were it to, nothing is kept.
        _ => relais::router::capture::CapturePolicy::Off,
    };
    match relais::router::observe(&ledger_path, &payload, &policy) {
        Ok(_) => CliOutcome::Accepted,
        Err(e @ relais::router::ObserveError::Payload(_)) => {
            eprintln!("relais native router-observe: {e}");
            CliOutcome::InvalidInput
        }
        Err(e) => {
            eprintln!("relais native router-observe: {e}");
            CliOutcome::OperationalFailure
        }
    }
}

/// `relais native router-envelope`: machine.toml first (the envelope is
/// authority), then the ledger's provenance row, which is its audit
/// trail; a ledger that cannot take the row is said, not a failed grant.
fn router_envelope_command(
    by: &str,
    epsilon_max: f64,
    source: relais::router::Source,
) -> Result<CliOutcome, CliError> {
    if by.trim().is_empty() {
        eprintln!("relais native router-envelope: --by is empty; name who granted it");
        return Ok(CliOutcome::InvalidInput);
    }
    if !(epsilon_max.is_finite() && (0.0..=1.0).contains(&epsilon_max)) {
        eprintln!(
            "relais native router-envelope: --epsilon-max {epsilon_max} is not between 0 and 1"
        );
        return Ok(CliOutcome::InvalidInput);
    }
    let machine_path = paths::machine_settings_path().map_err(CliError::Home)?;
    let envelope = relais::policy::RoutingEnvelope {
        granted_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        by: by.to_string(),
        epsilon_max,
        source: source.as_str().to_string(),
    };
    if let Err(e) = relais::router::write_envelope(&machine_path, &envelope) {
        eprintln!("relais native router-envelope: {e}");
        return Ok(CliOutcome::OperationalFailure);
    }
    let recorded = open_ledger().and_then(|ledger| {
        operational(
            ledger.record_router_provenance(&relais::ledger::RouterProvenance::Envelope {
                by: envelope.by.clone(),
                epsilon_max,
                source: envelope.source.clone(),
                at: envelope.granted_at.clone(),
            }),
            "recording the envelope in the ledger",
        )
    });
    if let Err(e) = recorded {
        eprintln!(
            "relais native router-envelope: warning: the envelope is in machine.toml, but the \
             ledger did not record it ({e})"
        );
    }
    print_document(&serde_json::json!({ "schema": 1, "envelope": envelope }))?;
    Ok(CliOutcome::Accepted)
}

/// `relais native router-envelope --off`: the envelope removed and the
/// `off` tombstone recorded in machine.toml, then the provenance row.
fn router_off_command(by: &str, source: relais::router::Source) -> Result<CliOutcome, CliError> {
    if by.trim().is_empty() {
        eprintln!("relais native router-envelope: --by is empty; name who turned routing off");
        return Ok(CliOutcome::InvalidInput);
    }
    let machine_path = paths::machine_settings_path().map_err(CliError::Home)?;
    let off = relais::policy::RoutingOff {
        at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        by: by.to_string(),
        source: source.as_str().to_string(),
    };
    let had_envelope = match relais::router::write_off(&machine_path, &off) {
        Ok(had) => had,
        Err(e) => {
            eprintln!("relais native router-envelope: {e}");
            return Ok(CliOutcome::OperationalFailure);
        }
    };
    let recorded = open_ledger().and_then(|ledger| {
        operational(
            ledger.record_router_provenance(&relais::ledger::RouterProvenance::EnvelopeRemoved {
                source: off.source.clone(),
                at: off.at.clone(),
            }),
            "recording the removal in the ledger",
        )
    });
    if let Err(e) = recorded {
        eprintln!(
            "relais native router-envelope: warning: routing is off in machine.toml, but the \
             ledger did not record it ({e})"
        );
    }
    print_document(
        &serde_json::json!({ "schema": 1, "envelope": null, "off": off, "removed": had_envelope }),
    )?;
    Ok(CliOutcome::Accepted)
}

/// `relais native usage`: books the dispatch's cumulative usage as the
/// delta over what is already booked, and tells the run's events. Empty or
/// terminal stdin is a usage error (2), never a wait.
fn native_usage_command(dispatch: &str) -> Result<CliOutcome, CliError> {
    use relais::adapter::native::{book_turn_usage, TurnBooked, TurnReport};
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        return Err(CliError::Usage {
            detail: "native usage: the report goes on stdin, which is a terminal".into(),
        });
    }
    let payload = read_stdin()?;
    if payload.trim().is_empty() {
        return Err(CliError::Usage {
            detail: "native usage: stdin is empty; expected the dispatch's usage report".into(),
        });
    }
    let report: TurnReport = serde_json::from_str(&payload).map_err(|e| CliError::Usage {
        detail: format!("native usage: the payload on stdin is not a usage report: {e}"),
    })?;
    let prices = load_machine_pricing()?;
    let ledger = open_ledger()?;
    match operational(
        book_turn_usage(&ledger, dispatch, &report, prices.as_ref()),
        "native usage",
    )? {
        TurnBooked::NoSuchDispatch => {
            eprintln!("relais native usage: no dispatch {dispatch} on record");
            Ok(CliOutcome::UnknownRun)
        }
        TurnBooked::Nothing => Ok(CliOutcome::Accepted),
        TurnBooked::Booked {
            run_id,
            cost,
            completeness,
            unpriced_reason,
        } => {
            let artifacts = paths::runs_dir()
                .map_err(CliError::Home)?
                .join(run_id.as_str());
            let event = relais::protocol::Event::Cost {
                booked: cost,
                completeness,
                unpriced_reason,
            };
            // The ledger holds the booking; a run that cannot be told of it
            // is said, not undone.
            if let Err(e) = relais::protocol::append_event(run_id.as_str(), &artifacts, &event) {
                eprintln!("relais native usage: booked, but the run's events are not written: {e}");
            }
            Ok(CliOutcome::Accepted)
        }
    }
}

fn native_gate() -> Result<relais::coordinator::RemoteGate, CliError> {
    let socket = relais::coordinator::socket_path().map_err(CliError::Home)?;
    Ok(relais::coordinator::RemoteGate::new(socket))
}

fn acknowledged(what: &str, answer: Result<(), GateError>) -> Result<CliOutcome, CliError> {
    match answer {
        Ok(()) => Ok(CliOutcome::Accepted),
        Err(e) => {
            eprintln!("relais native {what}: {e}");
            Ok(CliOutcome::OperationalFailure)
        }
    }
}

fn read_stdin() -> Result<String, CliError> {
    std::io::read_to_string(std::io::stdin()).map_err(|cause| CliError::Operational {
        operation: "reading stdin",
        cause: Box::new(cause),
    })
}

fn read_stopped_report(payload: &str) -> Result<StoppedReport, serde_json::Error> {
    serde_json::from_str(payload)
}

/// The most runs `relais native status --session` lists.
const SESSION_RUNS_LIMIT: usize = 20;

/// The stall threshold in force: [`resume::STALLED_AFTER`], unless
/// `RELAIS_STALLED_AFTER_SECS` names another (the seam tests inject through).
fn stalled_after() -> std::time::Duration {
    std::env::var("RELAIS_STALLED_AFTER_SECS")
        .ok()
        .and_then(|seconds| seconds.parse::<u64>().ok())
        .map_or(resume::STALLED_AFTER, std::time::Duration::from_secs)
}

/// `relais native status`: the run's timeline from its `events.jsonl`, and
/// never the whole file — plus `current`, what the ledger holds for the run
/// now (SPEC §23). With `--session`, `{"runs":[...]}`: one such entry for
/// each run that session started, newest first. Without either flag, the
/// latest run on this machine (a CLI convenience; the plugin passes
/// `--session`).
fn native_status_command(run: Option<&str>, session: Option<&str>) -> Result<CliOutcome, CliError> {
    let artifacts = paths::runs_dir().map_err(CliError::Home)?;
    let opened = open_status_ledger();
    if let Some(session) = session {
        return native_status_of_session(&artifacts, opened, session);
    }
    let Some(run) = run
        .map(str::to_string)
        .or_else(|| relais::protocol::latest_run(&artifacts))
    else {
        eprintln!("relais native status: no run has written events yet");
        return Ok(CliOutcome::UnknownRun);
    };
    // The ledger first: a reap appends the run's end to its events, which
    // the timeline below must then show.
    let current = match &opened {
        Ok((ledger, access)) => current_of_run(ledger, *access, &run, stalled_after()),
        Err(e) => Err(CurrentUnknown::Unavailable(e.to_string())),
    };
    let path = artifacts.join(&run).join("events.jsonl");
    match std::fs::read_to_string(&path) {
        Ok(events) => {
            let mut timeline = relais::protocol::timeline(&run, &events);
            match current {
                Ok(Some(current)) => timeline["current"] = current,
                // A run the ledger never recorded: one refused before it
                // started. Nothing to say, and nothing wrong.
                Ok(None) => {}
                Err(e) => eprintln!("relais native status: no current state for {run}: {e}"),
            }
            println!("{timeline}");
            Ok(CliOutcome::Accepted)
        }
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "relais native status: run {run} has no events at {}",
                path.display()
            );
            Ok(CliOutcome::UnknownRun)
        }
        Err(cause) => Err(CliError::Read {
            what: "the run's events",
            path,
            cause,
        }),
    }
}

/// `relais native status --session`: `{"runs":[...]}`, and `{"runs":[]}`
/// with exit 0 when the session started none (or there is no ledger).
fn native_status_of_session(
    artifacts: &Path,
    opened: Result<(Ledger, StatusAccess), CurrentUnknown>,
    session: &str,
) -> Result<CliOutcome, CliError> {
    let mut intents = Vec::new();
    let entries = match opened {
        Ok((ledger, access)) => {
            intents = listed_intents(&ledger)
                .iter()
                .map(|intent| intent_json(&ledger, intent))
                .collect();
            let runs = operational(
                ledger.runs_of_session(session, SESSION_RUNS_LIMIT),
                "native status",
            )?;
            let mut entries = Vec::new();
            for run in runs {
                let current = current_of_run(&ledger, access, run.as_str(), stalled_after());
                let path = artifacts.join(run.as_str()).join("events.jsonl");
                // A run refused before it wrote anything has no events: an
                // empty timeline. Any other failure to read them is said.
                let events = match std::fs::read_to_string(&path) {
                    Ok(events) => events,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                    Err(e) => {
                        eprintln!(
                            "relais native status: {run}'s events at {} cannot be read: {e}",
                            path.display()
                        );
                        String::new()
                    }
                };
                let mut timeline = relais::protocol::timeline(run.as_str(), &events);
                match current {
                    Ok(Some(current)) => timeline["current"] = current,
                    Ok(None) => {}
                    Err(e) => eprintln!("relais native status: no current state for {run}: {e}"),
                }
                entries.push(timeline);
            }
            entries
        }
        Err(CurrentUnknown::NoLedger(_)) => Vec::new(),
        Err(e) => {
            eprintln!("relais native status: the ledger cannot be read: {e}");
            Vec::new()
        }
    };
    // `intents` appears only when there is something to launch, so a
    // session with none prints the shape it always has.
    let mut printed = serde_json::json!({ "runs": entries });
    if !intents.is_empty() {
        printed["intents"] = serde_json::Value::Array(intents);
    }
    println!("{printed}");
    Ok(CliOutcome::Accepted)
}

/// The ledger for `native status`, and whether it may be written: read
/// only unless it exists at this binary's schema, and never created or
/// migrated here.
/// What `native status` may do with an orphan it finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusAccess {
    /// The ledger is writable: an orphan is reaped.
    Reaping,
    /// The ledger is not: an orphan is only reported (`orphaned: true`).
    ReadOnly,
}

fn open_status_ledger() -> Result<(Ledger, StatusAccess), CurrentUnknown> {
    let path = paths::ledger_path().map_err(CurrentUnknown::Home)?;
    if !path.is_file() {
        return Err(CurrentUnknown::NoLedger(path));
    }
    let read_only = Ledger::open_read_only(&path).map_err(CurrentUnknown::Ledger)?;
    // `Ledger::open` on a ledger it cannot write falls back to a read-only
    // connection, whatever the cause (mode, owner, mount, ACL): SQLite's
    // own answer decides, not a guess from permission bits.
    Ok(match Ledger::open(&path) {
        Ok(ledger) if ledger.is_read_only().is_ok_and(|ro| !ro) => (ledger, StatusAccess::Reaping),
        Ok(_) | Err(_) => (read_only, StatusAccess::ReadOnly),
    })
}

/// Why `relais native status` cannot say what holds for a run now.
#[derive(Debug)]
enum CurrentUnknown {
    Home(paths::HomeUnset),
    NoLedger(std::path::PathBuf),
    Ledger(relais::ledger::LedgerError),
    /// The ledger was not opened, for the reason already given.
    Unavailable(String),
    Other(String),
}

impl std::fmt::Display for CurrentUnknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Home(e) => write!(f, "{e}"),
            Self::NoLedger(path) => write!(f, "no ledger at {}", path.display()),
            Self::Ledger(e) => write!(f, "{e}"),
            Self::Unavailable(why) | Self::Other(why) => write!(f, "{why}"),
        }
    }
}

/// What the ledger holds for `run` now: its state, its decision (open or
/// answered), who started it and how long it has been silent. `outcome` in
/// the timeline is how the run ended, from its events; a decision answered
/// since is recorded only in the ledger, so this is the one place a reader
/// learns it. `Ok(None)` for a run the ledger does not know.
///
/// A non-terminal run whose driver is provably gone (or silent past the
/// silence rule) is reaped under `StatusAccess::Reaping`; otherwise it is reported
/// `orphaned: true` and nothing is written. A non-terminal run silent for
/// `stalled_after` or more is `stalled: true`.
fn current_of_run(
    ledger: &Ledger,
    access: StatusAccess,
    run: &str,
    stalled_after: std::time::Duration,
) -> Result<Option<serde_json::Value>, CurrentUnknown> {
    let run = RunId::from_stored(run);
    let ledgered = |e| CurrentUnknown::Ledger(e);
    let Some(mut state) = ledger.run_status(&run).map_err(ledgered)? else {
        return Ok(None);
    };
    let runs_dir = paths::runs_dir().map_err(|e| CurrentUnknown::Other(e.to_string()))?;
    let silent = relais::reap::run_silence(ledger, &runs_dir, &run)
        .map_err(|e| CurrentUnknown::Other(e.to_string()))?;
    let mut orphaned = false;
    let mut verdict_seen = None;
    if !state.is_terminal() {
        let mut verdict = relais::reap::orphan_verdict_of_run(ledger, &run, silent)
            .map_err(|e| CurrentUnknown::Other(e.to_string()))?;
        if verdict != resume::OrphanVerdict::Alive {
            if access == StatusAccess::Reaping {
                state = reap_for_status(ledger, &runs_dir, &run, state, verdict, silent)
                    .map_err(|e| CurrentUnknown::Other(e.to_string()))?;
                // Not reaped (a dispatch lives): the run is alive, and
                // the verdict says so rather than contradicting the state.
                if !state.is_terminal() {
                    verdict = resume::OrphanVerdict::Alive;
                }
            } else {
                orphaned = true;
            }
        }
        // A verdict describes a run still open; a reaped one has its state.
        if !state.is_terminal() {
            verdict_seen = Some(verdict);
        }
    }
    let decision = ledger.decision_of_run(&run).map_err(ledgered)?;
    let session = ledger.run_root_session(&run).map_err(ledgered)?;
    let stalled =
        !state.is_terminal() && silent.is_some_and(|s| resume::is_stalled(s, stalled_after));
    let cost_micros = ledger.run_cost(&run).map_err(ledgered)?;
    let cost_completeness = ledger.run_cost_completeness(&run).map_err(ledgered)?;
    let mut current = serde_json::json!({
        "state": state,
        "session": session,
        // The ledger's total, which the plugin puts in place of whatever
        // it summed from events: replaying events cannot drift from it.
        "cost_total": {
            "micros": cost_micros.to_micros(),
            "completeness": cost_completeness,
        },
        "silent_seconds": silent.map(|s| s.as_secs()),
        "decision": decision.map(|d| serde_json::json!({
            "raised_state": d.raised_state,
            "raised_reason": d.raised_reason,
            "resolution": d.resolution,
            "actor": d.actor,
            "resolved_at": d.resolved_at,
        })),
    });
    if stalled {
        current["stalled"] = true.into();
    }
    if orphaned {
        current["orphaned"] = true.into();
    }
    if let Some(verdict) = verdict_seen {
        current["verdict"] = match verdict {
            resume::OrphanVerdict::Alive => "alive",
            resume::OrphanVerdict::Silent => "silent",
            resume::OrphanVerdict::Gone => "gone",
        }
        .into();
    }
    Ok(Some(current))
}

/// Reap a run `native status` found orphaned; the state it is in after.
fn reap_for_status(
    ledger: &Ledger,
    runs_dir: &Path,
    run: &RunId,
    state: State,
    verdict: resume::OrphanVerdict,
    silent: Option<std::time::Duration>,
) -> Result<State, CliError> {
    let detail = relais::reap::orphan_detail(ledger, run, verdict, silent);
    let reaped = relais::reap::reap_orphan(ledger, runs_dir, run, state, verdict, detail);
    match reaped {
        Ok(relais::reap::Reaped::Moved(message)) => eprintln!("relais native status: {message}"),
        Ok(relais::reap::Reaped::AlreadyMoved | relais::reap::Reaped::KeptAlive(_)) => {}
        // The ledger holds the reap; only the events file missed it.
        Err(e @ relais::reap::ReapError::EventsNotWritten { .. }) => {
            eprintln!("relais native status: {e}");
        }
        Err(e) => {
            return Err(CliError::Operational {
                operation: "native status",
                cause: Box::new(e),
            })
        }
    }
    Ok(operational(ledger.run_status(run), "native status")?.unwrap_or(state))
}

/// Why this `relais run` must not start, if it must not: it comes from the
/// relais plugin, whose session said hello recently, on a Claude Code the
/// plugin supports. `installed` is what `claude --version` printed;
/// `probe_failure` is why the probe could not say, when it could not. A
/// probe that merely timed out leaves the version unchecked: one warning
/// on stderr under `command`'s name, and the run goes on if the hello is
/// fresh.
fn run_refusal(
    command: &str,
    gate: &dyn Gate,
    session: &str,
    installed: Option<&str>,
    probe_failure: Option<&relais::adapter::claude::ProbeFailure>,
) -> Result<Option<String>, GateError> {
    use relais::adapter::claude::{claude_code_support, Support};
    let unchecked = match claude_code_support(installed, probe_failure) {
        Support::Unsupported(detail) => return Ok(Some(detail)),
        Support::Unchecked(reason) => Some(reason),
        Support::Supported => None,
    };
    let refusal = relais::native::hello_refusal(gate.native_hello_age(session)?);
    if refusal.is_none() {
        if let Some(reason) = unchecked {
            eprintln!(
                "{command}: warning: Claude Code version unchecked: {reason}; continuing \
                 (supported: {})",
                relais::native::SUPPORTED_CLAUDE_CODE
            );
        }
    }
    Ok(refusal)
}

/// A `relais run` starts only from Claude Code with the relais plugin.
/// Where this run works (ADR-0008): the flag, `RELAIS_WORKSPACE`, `CI`, the
/// machine setting and git's own answer about the checkout, through the one
/// pure decision. `Ok(Err(..))` is a refusal that blocks the run with its
/// remedy; an unknown value is a usage error.
fn run_workspace(
    flag: Option<&str>,
    root: &Path,
    machine: &MachineSettings,
    repo: &RepoPolicy,
) -> Result<Result<relais::workspace::reuse::Workspace, String>, CliError> {
    use relais::workspace::reuse::{resolve, ModeInputs, ModeRefusal, Requested};
    let flag = flag
        .map(Requested::parse)
        .transpose()
        .map_err(|refusal| CliError::Usage {
            detail: format!(
                "relais run: --workspace takes auto, fork, in-place or isolated ({})",
                match refusal {
                    ModeRefusal::Unknown(value) => format!("not `{value}`"),
                    _ => "unknown value".to_string(),
                }
            ),
        })?;
    let env_workspace = std::env::var("RELAIS_WORKSPACE").ok();
    let ci = std::env::var("CI").ok();
    let inputs = ModeInputs {
        flag,
        env_workspace: env_workspace.as_deref(),
        ci: ci.as_deref(),
        machine: machine.workspace.mode,
        repo_opts_out_of_in_place: repo.workspace.in_place == Some(false),
        linked_worktree: relais::repo::is_linked_worktree(root),
    };
    Ok(match resolve(inputs) {
        Ok(workspace) => Ok(workspace),
        Err(ModeRefusal::Unknown(value)) => {
            return Err(CliError::Usage {
                detail: format!(
                    "relais run: RELAIS_WORKSPACE takes auto, fork, in-place or isolated, not `{value}`"
                ),
            })
        }
        Err(ModeRefusal::WidenedPastTheEnvironment) => Err(
            "relais run: RELAIS_WORKSPACE=isolated or CI forces an isolated workspace, and --workspace cannot widen it; unset the variable or drop the flag".to_string(),
        ),
        Err(ModeRefusal::PrimaryCheckout) => Err(format!(
            "relais run: {} is the primary checkout; in-place needs a linked worktree (git worktree add …), or drop --workspace to fork",
            root.display()
        )),
        Err(ModeRefusal::InPlaceOptedOut) => Err(
            "relais run: this repository's relais.toml sets [workspace] in_place = false; drop --workspace to fork".to_string(),
        ),
    })
}

/// Where a run's contract comes from.
#[derive(Clone, Copy)]
enum RunSource<'a> {
    /// A contract file.
    Task(&'a Path),
    /// A successor intent in the ledger (plan A5).
    Intent(&'a str),
}

/// The contract of a successor intent and the task its run belongs to: the
/// decided run's, so the successor's cost joins that task's total.
fn intent_contract(ledger: &Ledger, intent_id: &str) -> Result<(TaskContract, String), String> {
    let intent = ledger
        .successor_intent(intent_id)
        .map_err(|e| format!("ledger: {e}"))?
        .ok_or_else(|| format!("unknown successor intent {intent_id}"))?;
    let contract = TaskContract::from_json_str(&intent.contract_json)
        .map_err(|e| format!("the contract of intent {intent_id} is invalid: {e}"))?;
    if contract.hash() != intent.contract_digest {
        return Err(format!(
            "the contract of intent {intent_id} no longer has the digest recorded with it"
        ));
    }
    let task = ledger
        .task_of_run(&intent.predecessor)
        .map_err(|e| format!("ledger: {e}"))?
        .ok_or_else(|| format!("run {} has no task on record", intent.predecessor))?;
    Ok((contract, task.to_string()))
}

/// What the host says of a driver pid, with the start time of the process
/// that holds it now: the ledger compares it with the one recorded.
fn successor_process(pid: u32) -> relais::ledger::SuccessorProcess {
    if relais::coordinator::process_alive(pid) {
        relais::ledger::SuccessorProcess::Alive {
            started: relais::procs::start_time(pid),
        }
    } else {
        relais::ledger::SuccessorProcess::Gone
    }
}

/// A launch of a successor intent was refused before its run existed: hand
/// the claim back to pending with the refusal on the intent, so a later
/// launch can take it instead of finding a claim with no run row.
fn release_intent(ledger: &Ledger, source: RunSource<'_>, run_id: &RunId, refusal: &str) {
    if let RunSource::Intent(intent) = source {
        if let Err(e) = ledger.release_claim(intent, run_id, refusal) {
            eprintln!("relais run: intent {intent} could not be released: {e}");
        }
    }
}

/// A run that failed operationally hands its claim back only when it left
/// no row: a run that exists holds the claim, and recovery reads its state.
fn release_intent_without_run(
    ledger: &Ledger,
    source: RunSource<'_>,
    run_id: &RunId,
    refusal: &str,
) {
    match ledger.run_exists(run_id) {
        Ok(false) => release_intent(ledger, source, run_id, refusal),
        Ok(true) => {}
        Err(e) => eprintln!("relais run: could not tell whether run {run_id} exists: {e}"),
    }
}

fn run_command(
    source: RunSource<'_>,
    revise: Option<&str>,
    origin: &relais::native::RunOrigin<'_>,
    workspace_flag: Option<&str>,
) -> Result<CliOutcome, CliError> {
    if let Some(detail) = relais::native::origin_refusal(origin) {
        return Err(CliError::Usage { detail });
    }
    let session = relais::coordinator::resolve_session();
    let (root, repo) = load_repo_policy()?;
    let machine = load_machine()?;
    let workspace = match run_workspace(workspace_flag, &root, &machine, &repo)? {
        Ok(workspace) => workspace,
        Err(refusal) => {
            eprintln!("{refusal}");
            return Ok(CliOutcome::Blocked);
        }
    };
    let from_file = match source {
        RunSource::Task(task) => Some(load_contract(task)?),
        RunSource::Intent(_) => None,
    };
    let ledger = open_ledger()?;
    let (contract, revise) = match (source, from_file) {
        (_, Some(contract)) => (contract, revise.map(str::to_string)),
        (RunSource::Intent(intent), None) => match intent_contract(&ledger, intent) {
            Ok((contract, task)) => (contract, Some(task)),
            Err(detail) => {
                return Ok(refuse_run(
                    "intent_unavailable",
                    detail,
                    CliOutcome::Blocked,
                ))
            }
        },
        (RunSource::Task(_), None) => unreachable!("a task source loaded its contract above"),
    };
    let task_override = match resolve_task_override(&ledger, &contract, revise.as_deref()) {
        Ok(task_override) => task_override,
        Err(detail) => return Ok(refuse_run("task_refused", detail, CliOutcome::Blocked)),
    };
    let artifacts_dir = paths::runs_dir().map_err(CliError::Home)?;
    let backend = match relais::adapter::claude::ClaudeBackend::discover() {
        Ok(backend) => std::sync::Arc::from(backend),
        Err(e) => {
            return Ok(refuse_run(
                "backend_unavailable",
                e.to_string(),
                CliOutcome::Blocked,
            ))
        }
    };
    // The three tools a run talks to, each behind the port this crate
    // owns: git, the decision corpus and the check inventory.
    let git = relais::workspace::SystemGit;
    let aval_resolver = relais::context::AvalCli::new(root.clone());
    let hooks = relais::verify::AmontCli::new();
    let attest = relais::verify::AmontCli::new();
    // Managed dispatch is the only path `relais run` takes: a missing
    // coordinator blocks the run rather than launching unmanaged
    // (SPEC §23).
    let socket = relais::coordinator::socket_path().map_err(CliError::Home)?;
    if let Err(e) = relais::coordinator::ensure_running(&socket) {
        return Ok(refuse_run(
            "admission_unavailable",
            e.to_string(),
            CliOutcome::Blocked,
        ));
    }
    let gate = relais::coordinator::RemoteGate::new(socket);
    // Before anything is dispatched: the plugin of this session is alive,
    // and the Claude Code it runs on is one the plugin supports.
    let report = backend.probe_report();
    let installed = report
        .as_ref()
        .ok()
        .and_then(|capabilities| capabilities.version.as_deref());
    match run_refusal(
        "relais run",
        &gate,
        &session.id,
        installed,
        report.as_ref().err(),
    ) {
        Ok(None) => {}
        Ok(Some(detail)) => return Err(CliError::Usage { detail }),
        Err(e) => {
            return Ok(refuse_run(
                "admission_unavailable",
                e.to_string(),
                CliOutcome::Blocked,
            ))
        }
    }
    // Learned routing reads the registry's active artifact, pinned for
    // this run (SPEC §17); disabled routing leaves everything else intact.
    let harness = report.as_ref().ok().map(|capabilities| {
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
    let run_id = match ids.run_id() {
        Ok(run_id) => run_id,
        Err(e) => {
            eprintln!("relais run: {e}");
            return Ok(CliOutcome::OperationalFailure);
        }
    };
    // A successor intent is claimed by the run id minted for it: atomic,
    // so a second launch of the same intent finds the first run instead of
    // making another (plan A5).
    if let RunSource::Intent(intent) = source {
        match ledger.claim_intent(intent, &run_id, &successor_process) {
            Ok(relais::ledger::Claim::Claimed(_)) => {}
            Ok(relais::ledger::Claim::AlreadyClaimed { run }) => {
                return Ok(refuse_run(
                    "intent_already_claimed",
                    format!("intent {intent} is already claimed by run {run}"),
                    CliOutcome::Blocked,
                ))
            }
            Ok(relais::ledger::Claim::Unknown) => {
                return Ok(refuse_run(
                    "intent_unavailable",
                    format!("unknown successor intent {intent}"),
                    CliOutcome::Blocked,
                ))
            }
            Err(e) => {
                eprintln!("relais run: {e}");
                return Ok(CliOutcome::OperationalFailure);
            }
        }
    }
    // The worker attempts go to the relais plugin of the parent session;
    // everything else stays on the Claude backend.
    let prices = match load_price_table() {
        Ok(prices) => prices,
        Err(e) => {
            release_intent(
                &ledger,
                source,
                &run_id,
                &format!("prices unavailable: {e}"),
            );
            return Err(e);
        }
    };
    let refusals = match pricing_refusals(&repo, &prices, &ledger) {
        Ok(refusals) => refusals,
        Err(e) => {
            release_intent(
                &ledger,
                source,
                &run_id,
                &format!("pricing preflight failed: {e}"),
            );
            return Err(e);
        }
    };
    if !refusals.is_empty() {
        for refusal in &refusals {
            eprintln!("relais run: {refusal}");
        }
        release_intent(&ledger, source, &run_id, &refusals.join("; "));
        return Ok(CliOutcome::Blocked);
    }
    let native_backend = relais::adapter::native::NativeBackend::new(
        backend.as_ref(),
        &gate,
        relais::adapter::native::Link {
            run_id: run_id.as_str().to_string(),
            session_id: session.id.clone(),
            wire: relais::protocol::Wire::process(),
            // Without a home there is no transcript to read: the run books
            // its usage and says `rollback_ids_missing`.
            projects_dir: paths::claude_projects_dir().ok(),
        },
        relais::adapter::native::Waits::DEFAULT,
        Some(prices),
    );
    // The advisor calls an agent's transcript holds are booked into the
    // ledger beside the plugin's figures, which never contain them.
    let native_backend = match paths::ledger_path() {
        Ok(path) => native_backend.with_advisor_ledger(path),
        Err(e) => {
            eprintln!("advisor usage will not be booked: the ledger path is unknown: {e}");
            native_backend
        }
    };
    let run_backend: &dyn relais::backend::Backend = &native_backend;
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
    let trial = match operational(
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
    ) {
        Ok(trial) => trial,
        Err(e) => {
            release_intent(
                &ledger,
                source,
                &run_id,
                &format!("trial assignment failed: {e}"),
            );
            return Err(e);
        }
    };
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
                    release_intent(
                        &ledger,
                        source,
                        &run_id,
                        &format!("trial record failed: {e}"),
                    );
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
    // The ballast is made at start when space allows, and made again on
    // the next start after a full disk freed it (SPEC §10).
    keep_ballast(&artifacts_dir);
    let outcome = match execute(&RunConfig {
        workspace,
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
        artifacts_dir: artifacts_dir.clone(),
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
        wire: relais::protocol::Wire::process(),
        design: relais::runner::DesignSource::Contract,
        // Only `run` consumes a pending change to the repository's
        // commands (ADR-0007).
        declarations: &relais::trust::Recorder(&ledger),
    }) {
        Ok(outcome) => outcome,
        Err(e) => {
            if let Some(trial_id) = &trial_id {
                if let Err(settle_error) = live_trial::settle_errored(&ledger, trial_id) {
                    eprintln!("relais run: could not settle trial {trial_id}: {settle_error}");
                }
            }
            if let relais::runner::RunError::StorageFull { volume } = &e {
                // One line, naming the volume; the run's last durable
                // ledger state is what `resume` reconciles from.
                eprintln!(
                    "relais run: storage full on {volume}; `relais resume {run_id}` reconciles the run"
                );
                release_intent_without_run(&ledger, source, &run_id, &format!("run failed: {e}"));
                return Ok(CliOutcome::StorageFull);
            }
            eprintln!("relais run: {e}");
            release_intent_without_run(&ledger, source, &run_id, &format!("run failed: {e}"));
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
        Terminal::BlockedBy { reason, detail } => {
            eprintln!("blocked ({reason}): {detail}");
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

/// Make the state directory's ballast file when it is missing and the
/// volume has room for it. A ballast that cannot be made is said once and
/// never stops the run: it is a cushion, not a requirement.
fn keep_ballast(artifacts_dir: &Path) {
    use relais::storage_full::{ballast_path, ensure_ballast, BallastState, BALLAST_BYTES};
    let Some(state_dir) = artifacts_dir.parent() else {
        return;
    };
    let probe = relais::disk::StatvfsProbe;
    if let BallastState::Failed(why) =
        ensure_ballast(&ballast_path(state_dir), BALLAST_BYTES, &probe)
    {
        eprintln!("relais run: the 64 MiB ballast file could not be made: {why}");
    }
}

/// `relais dataset replay` (SPEC §24): re-run a task's already-accepted
/// run under a candidate recipe, in a workspace where the accepted answer
/// is genuinely absent, and record that arm as one settled trial. This is
/// NOT a promotion mechanism and does not compare, score, rank or promote
/// anything — it produces exactly one arm's result for one task, and it
/// spends real money, subject to the ordinary ceilings ordinary work goes
/// through (the same `execute` path `relais run` takes).
/// Refuses, before anything is dispatched, a policy naming a model the
/// price table cannot price and no observation resolves (N5); the warnings
/// go to stderr either way.
fn pricing_preflight(
    who: &str,
    policy: &relais::policy::RepoPolicy,
    prices: &relais::orchestration::PriceTable,
    ledger: &relais::ledger::Ledger,
) -> Result<Option<CliOutcome>, CliError> {
    let refusals = pricing_refusals(policy, prices, ledger)?;
    if refusals.is_empty() {
        return Ok(None);
    }
    for refusal in &refusals {
        eprintln!("{who}: {refusal}");
    }
    Ok(Some(CliOutcome::Blocked))
}

/// The pricing preflight's refusals, warnings printed; empty when the run
/// may proceed.
fn pricing_refusals(
    policy: &relais::policy::RepoPolicy,
    prices: &relais::orchestration::PriceTable,
    ledger: &relais::ledger::Ledger,
) -> Result<Vec<String>, CliError> {
    let mut aliases: Vec<String> = policy.models.values().map(|m| m.id.clone()).collect();
    aliases.sort();
    aliases.dedup();
    let observations = operational(ledger.model_observations(), "model observations")?;
    let machine_toml = paths::machine_settings_path().map_err(CliError::Home)?;
    let decided = relais::native_pricing::preflight(&aliases, prices, &observations, &machine_toml);
    for warning in &decided.warnings {
        eprintln!("{warning}");
    }
    Ok(decided.refusals.iter().map(|r| r.to_string()).collect())
}

/// The `done` line of a replay, sent once its trial is recorded (or could
/// not be): the plugin's verdict names the trial, and never precedes it.
fn announce_replay_done(artifacts_dir: &Path, run: &str, outcome: &str, trial: Option<&str>) {
    let artifacts = artifacts_dir.join(run);
    let written = artifacts.join("receipt.json");
    let receipt = written
        .is_file()
        .then(|| written.to_string_lossy().into_owned());
    // Dropped on failure: the plugin that was reading is gone, and the
    // replay's result is in the ledger.
    let _ = relais::protocol::Wire::process().send(&relais::protocol::Request::Done {
        run: Some(run),
        outcome,
        receipt: receipt.as_deref(),
        summary: relais::protocol::Summary::of_candidate(&artifacts),
        trial,
        code: None,
        detail: None,
    });
}

fn replay_command(
    task: &str,
    recipe: &Path,
    dry_run: bool,
    design: Option<&Path>,
    origin: &relais::native::RunOrigin<'_>,
) -> Result<CliOutcome, CliError> {
    // The design file first, before anything else is read or run: a
    // missing or oversized one refuses the replay outright.
    let given_design = match design.map(relais::context::read_design_file).transpose() {
        Ok(given) => given,
        Err(e) => {
            eprintln!("relais dataset replay: refused (design_unreadable): {e}");
            return Ok(CliOutcome::InvalidInput);
        }
    };
    // A dry run spends nothing and is allowed anywhere; a spending replay
    // starts only the way a run does.
    if !dry_run {
        if let Some(detail) = relais::native::origin_refusal(origin) {
            return Err(CliError::Usage { detail });
        }
    }
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
    let candidate = match route::validate_candidate(
        &incumbent,
        &candidate_policy,
        &bounds,
        route::CandidateAdmission::Replay,
    ) {
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
    // The design the arm is handed: `--design`, else whatever the source
    // contract named, read here at its base so routing, the package and
    // the trial row all see the one design.
    let design_input = match (given_design, &source_contract.design) {
        (Some(given), _) => Some(given),
        (None, None) => None,
        (None, Some(path)) => {
            match relais::context::read_design(
                &relais::workspace::SystemGit,
                &root,
                &base_sha,
                path,
            ) {
                Ok(read) => Some(read),
                Err(e) => {
                    eprintln!("relais dataset replay: blocked (design_unreadable): {e}");
                    return Ok(CliOutcome::Blocked);
                }
            }
        }
    };
    let design_sha256 = design_input
        .as_ref()
        .map(|design| relais::ids::sha256_hex(design.text.as_bytes()));

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

    // The recorded contract carries no type; the ledger's (declared or
    // classified) one comes in through the facts.
    let task_type = operational(
        ledger.routing_type(&task_id, source_contract.task_type),
        "dataset replay",
    )?;
    // The incumbent is what routed the source run, with its own design;
    // the arm is routed with the design it is handed.
    let incumbent_facts = route::TaskFacts {
        task_type,
        has_design: source_contract.design.is_some(),
        admission: route::Admission::Live,
    };
    let replay_facts = route::TaskFacts {
        has_design: design_input.is_some(),
        admission: route::Admission::Replay,
        ..incumbent_facts
    };
    let incumbent_eligible = route::eligible_tiers(
        &source_contract,
        &incumbent,
        &incumbent.models,
        incumbent_facts,
    );
    let incumbent_recipe_id = route::covering_recipe_id(
        &source_contract,
        &incumbent,
        &incumbent_eligible.tiers,
        incumbent_facts,
    )
    .unwrap_or_default();
    let candidate_eligible = route::eligible_tiers(
        &source_contract,
        candidate.policy(),
        &candidate.policy().models,
        replay_facts,
    );
    let arm_recipe = route::covering_recipe_id(
        &source_contract,
        candidate.policy(),
        &candidate_eligible.tiers,
        replay_facts,
    );

    println!(
        "this replay is one arm's result; it does not compare, score, rank or promote anything"
    );
    let arm = arm_recipe
        .as_deref()
        .and_then(|id| ReplayArm::of(candidate.policy(), id));
    if dry_run {
        println!("dry run: nothing was dispatched, no trial was recorded, no usage was spent");
        println!("would replay source run: {source_run} (task {task_id})");
        println!(
            "task type: {}",
            task_type.map_or("none", relais::contract::TaskType::as_str)
        );
        println!(
            "candidate recipe: {}",
            arm_recipe
                .as_deref()
                .unwrap_or("(no recipe covers this task under the candidate)")
        );
        if let Some(arm) = &arm {
            println!(
                "arm: {} revision {}, tier {}, model {}",
                arm.spec.name,
                arm.spec.revision,
                arm.spec.tier.as_str(),
                arm.model.unwrap_or("(none configured)")
            );
        }
        println!("design: {}", design_sha256.as_deref().unwrap_or("none"));
        println!("verification profile hash checked: {replay_profile_hash}");
        return Ok(CliOutcome::Accepted);
    }
    let (Some(arm_recipe), Some(arm)) = (arm_recipe, arm) else {
        eprintln!("relais dataset replay: no recipe in the candidate policy covers task {task_id}");
        return Ok(CliOutcome::Blocked);
    };

    let machine = load_machine()?;
    let artifacts_dir = paths::runs_dir().map_err(CliError::Home)?;
    let backend = match relais::adapter::claude::ClaudeBackend::discover() {
        Ok(backend) => std::sync::Arc::from(backend),
        Err(e) => {
            eprintln!("relais dataset replay: {e}");
            return Ok(CliOutcome::Blocked);
        }
    };
    let socket = relais::coordinator::socket_path().map_err(CliError::Home)?;
    if let Err(e) = relais::coordinator::ensure_running(&socket) {
        eprintln!("relais dataset replay: blocked (admission_unavailable): {e}");
        return Ok(CliOutcome::Blocked);
    }
    let gate = relais::coordinator::RemoteGate::new(socket);
    let session = relais::coordinator::resolve_session();
    // Before the checkout and before anything is dispatched: the plugin of
    // this session is alive, on a Claude Code it supports.
    let report = backend.probe_report();
    let installed = report
        .as_ref()
        .ok()
        .and_then(|capabilities| capabilities.version.as_deref());
    match run_refusal(
        "relais dataset replay",
        &gate,
        &session.id,
        installed,
        report.as_ref().err(),
    ) {
        Ok(None) => {}
        Ok(Some(detail)) => return Err(CliError::Usage { detail }),
        Err(e) => {
            eprintln!("relais dataset replay: blocked (admission_unavailable): {e}");
            return Ok(CliOutcome::Blocked);
        }
    }
    let ids = id_source();
    let run_id = match ids.run_id() {
        Ok(run_id) => run_id,
        Err(e) => {
            eprintln!("relais dataset replay: {e}");
            return Ok(CliOutcome::OperationalFailure);
        }
    };
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

    let git = relais::workspace::SystemGit;
    let aval_resolver = relais::context::AvalCli::new(checkout_dir.clone());
    let hooks = relais::verify::AmontCli::new();
    let attest = relais::verify::AmontCli::new();
    let prices = load_price_table()?;
    if let Some(blocked) = pricing_preflight(
        "relais dataset replay",
        candidate.policy(),
        &prices,
        &ledger,
    )? {
        return Ok(blocked);
    }
    let native_backend = relais::adapter::native::NativeBackend::new(
        backend.as_ref(),
        &gate,
        relais::adapter::native::Link {
            run_id: run_id.as_str().to_string(),
            session_id: session.id.clone(),
            wire: relais::protocol::Wire::process(),
            projects_dir: paths::claude_projects_dir().ok(),
        },
        relais::adapter::native::Waits::DEFAULT,
        Some(prices),
    );
    // The advisor calls an agent's transcript holds are booked into the
    // ledger beside the plugin's figures, which never contain them.
    let native_backend = match paths::ledger_path() {
        Ok(path) => native_backend.with_advisor_ledger(path),
        Err(e) => {
            eprintln!("advisor usage will not be booked: the ledger path is unknown: {e}");
            native_backend
        }
    };
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
    let log = ReplayLog {
        task_type,
        task: &task_id,
        arm_model: arm.model.unwrap_or("none"),
        load: relais::procs::load_average(),
        started: std::time::Instant::now(),
    };
    eprintln!("{}", log.start_line());
    let outcome = execute(&RunConfig {
        workspace: relais::workspace::reuse::Workspace::Isolated,
        repo_dir: &checkout_dir,
        contract: &replay_contract,
        repo_policy: candidate.policy(),
        machine: &machine,
        ledger: &ledger,
        ids: &ids,
        backend: &native_backend,
        git: &git,
        hooks: &hooks,
        attest: &attest,
        artifacts_dir: artifacts_dir.clone(),
        aval_resolver: &aval_resolver,
        predictor: None,
        gate: Some(&gate),
        session_id: session.id.clone(),
        heartbeat_every: std::time::Duration::from_secs(30),
        task_override: Some(&task_id),
        purpose: Some(RunPurpose::Replay),
        run_id: Some(run_id.clone()),
        wire: relais::protocol::Wire::process(),
        design: design_input
            .as_ref()
            .map_or(relais::runner::DesignSource::Contract, |design| {
                relais::runner::DesignSource::Given(design)
            }),
        // A replay neither says nor records a pending command change: the
        // runner skips it for a replay, and `plan`, `doctor` and `trust
        // show` report it.
        declarations: &relais::trust::NoHistory,
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
            eprintln!("{}", log.end_line(&ReplayEnd::unrecorded("interrupted")));
            announce_replay_done(&artifacts_dir, run_id.as_str(), "interrupted", None);
            return Ok(CliOutcome::OperationalFailure);
        }
    };

    // The purpose was written with the run row (see `RunConfig::purpose`),
    // so there is nothing to stamp here.
    let recorded = record_replay_trial(
        &ledger,
        &outcome,
        &relais::ledger::NewReplayTrial {
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
            design_sha256: design_sha256.as_deref(),
        },
        &arm,
    );
    let trial_outcome = match recorded {
        Ok(settled) => {
            if let Some(mismatch) = &settled.verdict.mismatch {
                eprintln!("relais dataset replay: recorded invalid: {mismatch}");
            }
            eprintln!("{}", log.end_line(&settled.end()));
            announce_replay_done(
                &artifacts_dir,
                outcome.run_id(),
                outcome.state().as_str(),
                Some(replay_id.as_str()),
            );
            settled.verdict.outcome
        }
        Err(e) => {
            eprintln!(
                "{}",
                log.end_line(&ReplayEnd::unrecorded(outcome.state().as_str()))
            );
            announce_replay_done(
                &artifacts_dir,
                outcome.run_id(),
                outcome.state().as_str(),
                None,
            );
            return Err(e);
        }
    };

    println!("replay run: {}", outcome.run_id());
    println!("replay record: {replay_id}");
    println!("outcome:       {trial_outcome}");
    println!(
        "this replay is one arm's result; it does not compare, score, rank or promote anything"
    );
    Ok(CliOutcome::Accepted)
}

/// The recipe a replay's arm runs, and the model its first attempt must
/// dispatch: the recipe's own `models[tier]`, else the policy's (the
/// profile the router's ladder resolves for a covering recipe).
struct ReplayArm<'a> {
    spec: &'a RecipeSpec,
    model: Option<&'a str>,
}

impl<'a> ReplayArm<'a> {
    fn of(policy: &'a RepoPolicy, recipe_id: &str) -> Option<Self> {
        let spec = policy
            .recipes
            .iter()
            .find(|spec| spec.recipe_id() == recipe_id)?;
        let model = typed_gate::arm_model(policy, spec);
        Some(Self { spec, model })
    }
}

/// A replay's trial as written: the self-check's verdict and the figures
/// it settled with.
struct SettledReplay {
    verdict: live_trial::ReplayVerdict,
    dispatched_model: Option<String>,
    cost: relais::ledger::TrialCost,
}

impl SettledReplay {
    fn end(&self) -> ReplayEnd<'_> {
        ReplayEnd {
            outcome: self.verdict.outcome.as_str(),
            dispatched_model: self.dispatched_model.as_deref(),
            cost: Some(self.cost),
        }
    }
}

/// Writes the replay's trial and settles it from the arm run's outcome,
/// after the replay's self-check: a first attempt that ran another model
/// than the arm's, or a design that disagrees with the arm's recipe, is
/// recorded `invalid`, never as the arm's result.
fn record_replay_trial(
    ledger: &relais::ledger::Ledger,
    outcome: &relais::runner::RunOutcome,
    trial: &relais::ledger::NewReplayTrial<'_>,
    arm: &ReplayArm<'_>,
) -> Result<SettledReplay, CliError> {
    let dispatched_model = operational(
        ledger.first_dispatch_intent(&outcome.run_id),
        "dataset replay",
    )?
    .and_then(|intent| {
        intent
            .get("model")
            .and_then(|m| m.as_str())
            .map(str::to_string)
    });
    let verdict = live_trial::ReplayCheck {
        arm_model: arm.model.unwrap_or_default(),
        dispatched_model: dispatched_model.as_deref(),
        requires_design: arm.spec.requires_design,
        design_sha256: trial.design_sha256,
    }
    .settle(live_trial::trial_outcome_of(&outcome.terminal));
    let (cost, duration_ms) = operational(
        live_trial::settled_figures(ledger, &outcome.run_id),
        "dataset replay",
    )?;
    operational(ledger.insert_replay_trial(trial), "dataset replay")?;
    operational(
        ledger.settle_trial(
            trial.trial_id,
            verdict.outcome,
            verdict.accepted_without_escalation,
            cost,
            duration_ms,
        ),
        "dataset replay",
    )?;
    Ok(SettledReplay {
        verdict,
        dispatched_model,
        cost,
    })
}

/// The two lines every replay prints to stderr, one as its arm starts and
/// one as it ends; the plan's record is copied from them
/// (docs/plans/2026-10-12-task-types.md).
struct ReplayLog<'a> {
    task_type: Option<relais::contract::TaskType>,
    task: &'a TaskId,
    /// The model the arm's first attempt is meant to dispatch.
    arm_model: &'a str,
    /// The one-minute load average when the replay started.
    load: Option<f64>,
    started: std::time::Instant,
}

/// How a replay ended, for its end line.
struct ReplayEnd<'a> {
    outcome: &'a str,
    /// What the first attempt dispatched; `None` when nothing was.
    dispatched_model: Option<&'a str>,
    /// `None` when no trial was settled to carry a figure.
    cost: Option<relais::ledger::TrialCost>,
}

impl ReplayEnd<'_> {
    /// A replay whose trial could not be written: its run's state, and no
    /// figures.
    fn unrecorded(outcome: &str) -> ReplayEnd<'_> {
        ReplayEnd {
            outcome,
            dispatched_model: None,
            cost: None,
        }
    }
}

impl ReplayLog<'_> {
    fn head(&self, event: &str) -> String {
        format!(
            "relais dataset replay: {event} type={} task={}",
            self.task_type
                .map_or("none", relais::contract::TaskType::as_str),
            self.task
        )
    }

    fn load_text(&self) -> String {
        self.load
            .map_or_else(|| "unknown".to_string(), |load| format!("{load:.2}"))
    }

    fn start_line(&self) -> String {
        format!(
            "{} model={} load={}",
            self.head("start"),
            self.arm_model,
            self.load_text()
        )
    }

    fn end_line(&self, end: &ReplayEnd<'_>) -> String {
        self.end_line_after(end, self.started.elapsed())
    }

    fn end_line_after(&self, end: &ReplayEnd<'_>, wall: std::time::Duration) -> String {
        let cost = end
            .cost
            .and_then(|cost| cost.cost())
            .map_or_else(|| "unknown".to_string(), |cost| cost.to_string());
        format!(
            "{} outcome={} model={} cost={cost} wall={:.1}s load={}",
            self.head("end"),
            end.outcome,
            end.dispatched_model.unwrap_or("none"),
            wall.as_secs_f64(),
            self.load_text()
        )
    }
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
    task_type: Vec<relais::contract::TaskType>,
    requires_design: bool,
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
            task_type,
            requires_design,
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
            task_type,
            requires_design,
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
    match route::validate_candidate(
        &incumbent,
        &candidate_policy,
        &bounds,
        route::CandidateAdmission::Promote,
    ) {
        Ok(_) => println!(
            "admissible: route::validate_candidate would ADMIT this candidate — admissible is \
             not approved: it runs only under the repository's grant, or a legacy grant for \
             its exact hash, and nothing here grants, \
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
    let ledger = open_ledger()?;
    let gate = operational(
        typed_gate::assess(&ledger, &incumbent, &candidate_policy),
        "recipe evaluate",
    )?;
    let candidate = match route::validate_candidate(
        &incumbent,
        &candidate_policy,
        &bounds,
        route::CandidateAdmission::PromoteGated(&gate.evidence),
    ) {
        Ok(candidate) => candidate,
        Err(rejection) => {
            eprintln!("relais recipe evaluate: candidate refused: {rejection}");
            return Ok(CliOutcome::Blocked);
        }
    };

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
        "trust grant: MISSING for {grant_key}. Review the commands with `relais trust show`, then \n\
         run `relais trust grant --reviewed-by <your name>` once to trust this repository. The \n\
         deprecated legacy grant for this exact policy is `relais trust grant --key {grant_key} \n\
         --reviewed-by <your name>`, or paste this into {}:\n\n\
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
    // Recomputed here, from the ledger's settled trials — never read from
    // a verdict an earlier `recipe evaluate` printed. The typed-research
    // gate comes first: a research-tier change is admitted only on its
    // count, and the generic gates below cannot stand in for it.
    let ledger = open_ledger()?;
    let gate = operational(
        typed_gate::assess(&ledger, &incumbent, &candidate_policy),
        "recipe promote",
    )?;
    let candidate = match route::validate_candidate(
        &incumbent,
        &candidate_policy,
        &bounds,
        route::CandidateAdmission::PromoteGated(&gate.evidence),
    ) {
        Ok(candidate) => candidate,
        Err(rejection) => {
            eprintln!("relais recipe promote: candidate refused: {rejection}");
            return Ok(CliOutcome::Blocked);
        }
    };
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
    let amendment = Amendment::for_promotion(
        proof,
        &gate.evidence,
        &incumbent,
        candidate.policy(),
        &subject,
    )
    .map_err(CliError::Amend)?;
    println!("evaluation this rests on:");
    print!("{}", report.render_evidence());
    println!();
    let outcome = present_amendment(&root, &incumbent, &amendment, mode)?;
    match mode {
        AmendMode::Print => {}
        AmendMode::Write => {
            operational(typed_gate::record(&ledger, &gate), "recipe promote")?;
            for authorisation in &gate.authorisations {
                println!(
                    "recorded in the ledger: {} revision {} ({}) was authorised by {} trials",
                    authorisation.name,
                    authorisation.revision,
                    authorisation.task_type.as_str(),
                    authorisation.trial_ids.len()
                );
            }
        }
    }
    Ok(outcome)
}

/// Restore the revision below the effective one by appending a new
/// revision that repeats it (SPEC §27). Removing a change needs no
/// evaluation.
fn recipe_rollback_command(name: &str, mode: AmendMode) -> Result<CliOutcome, CliError> {
    let (root, incumbent) = load_repo_policy()?;
    let amendment = Amendment::for_rollback(&incumbent, name).map_err(CliError::Amend)?;
    present_amendment(&root, &incumbent, &amendment, mode)
}

/// The successor intents a status shows: every pending one (a refused one
/// carries its refusal and retry time, and the plugin waits for that time),
/// and every claimed one whose claim was abandoned (plan A5). `status` and
/// `resume` only list them; the plugin is the one launcher.
fn listed_intents(ledger: &Ledger) -> Vec<relais::ledger::SuccessorIntent> {
    ledger
        .listed_intents(&successor_process)
        .unwrap_or_else(|e| {
            eprintln!("relais: successor intents cannot be listed: {e}");
            Vec::new()
        })
}

/// One line per listed intent, for `status` and `resume`.
fn print_pending_intents(ledger: &Ledger) {
    for intent in listed_intents(ledger) {
        println!(
            "pending successor intent {} (decided run {}, {}): launch with `relais run --intent {}`",
            intent.intent_id,
            intent.predecessor,
            intent.state.as_str(),
            intent.intent_id
        );
        if let (Some(refusal), Some(retry)) = (&intent.refusal, intent.retry_after()) {
            println!("  last launch refused: {refusal} (retry after {retry})");
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
            print_pending_intents(&ledger);
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
            print_pending_intents(&ledger);
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
    print_pending_intents(&ledger);
    let run = RunId::from_stored(run_id);
    let Some(state) = operational(ledger.run_status(&run), "resume")? else {
        eprintln!("relais resume: unknown run {run_id}");
        return Ok(CliOutcome::UnknownRun);
    };
    // An absent terminal result never means nothing executed (SPEC §12).
    // The policy is `resume::reconcile`, a pure function with a test per
    // row; this handler does the ledger write and the printing.
    if !state.is_terminal() {
        if let Some(orphan) = orphan_of_run(&ledger, &run)? {
            match interrupt_orphan(&ledger, &run, state, &orphan)? {
                OrphanOutcome::Ended(ended) => {
                    return match retire {
                        Retire::Keep => Ok(CliOutcome::ResumeReconciled),
                        Retire::Retire => {
                            let reconciliation = reconcile_run(&ledger, &run)?;
                            retire_run(&ledger, &run, ended, &reconciliation)?;
                            Ok(CliOutcome::ResumeReconciled)
                        }
                    };
                }
                OrphanOutcome::KeptAlive(why) => {
                    println!(
                        "{run_id}: kept, silent past its threshold but {why}; resume does not \
                         re-dispatch while a worker may be running, `relais coordinator cancel \
                         --run {run_id}` stops it"
                    );
                    return Ok(CliOutcome::ResumeRefusedLive);
                }
            }
        }
    }
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
    record_resume_interruption(&ledger, &run, state, &detail)?;
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

/// The resume's own transition: the resuming process re-enters the run,
/// so the same write records it as the run's driver.
fn record_resume_interruption(
    ledger: &Ledger,
    run: &RunId,
    from: State,
    detail: &str,
) -> Result<(), CliError> {
    operational(
        ledger.record_transition_as_driver(&relais::ledger::Transition {
            run_id: run.clone(),
            attempt_id: None,
            from_state: Some(from),
            to_state: State::Interrupted,
            reason: Reason::ReconciledInterrupted.as_str().to_string(),
            detail: Some(serde_json::json!({ "detail": detail })),
            at: relais::ledger::now_rfc3339(),
        }),
        "resume",
    )
}

/// Whether a non-terminal run has lost its driver (SPEC §12): silent for
/// longer than its attempt wall time plus a margin.
fn orphan_of_run(ledger: &Ledger, run: &RunId) -> Result<Option<resume::Orphan>, CliError> {
    let runs_dir = paths::runs_dir().map_err(CliError::Home)?;
    let Some(silent) = operational(relais::reap::run_silence(ledger, &runs_dir, run), "resume")?
    else {
        return Ok(None);
    };
    Ok(resume::orphan_of(
        silent,
        relais::reap::attempt_wall_of_run(ledger, run),
    ))
}

/// What became of an orphaned run.
enum OrphanOutcome {
    /// The run is no longer running; this is the state it is in now (the
    /// interrupted state this call wrote, or the one a racing process did).
    Ended(State),
    /// A dispatch of the run is provably alive: nothing was changed.
    KeptAlive(String),
}

/// Move an orphaned run to `interrupted` through the library's reap (a
/// compare-and-set that closes its dispatches and ends its events only when
/// it wins), and print what it returns.
fn interrupt_orphan(
    ledger: &Ledger,
    run: &RunId,
    state: State,
    orphan: &resume::Orphan,
) -> Result<OrphanOutcome, CliError> {
    let events_dir = paths::runs_dir()
        .map_err(CliError::Home)?
        .join(run.as_str());
    let reaped = relais::reap::reap(
        ledger,
        &relais::reap::Reap {
            run,
            from: state,
            ending: relais::reap::Ending::Interrupted,
            detail: orphan.detail(),
            extra: serde_json::json!({
                "silent_seconds": orphan.silent.as_secs(),
                "threshold_seconds": orphan.threshold.as_secs(),
            }),
            events_dir: &events_dir,
        },
        &|pid| relais::coordinator::process_alive(pid.get()),
    );
    let reaped = match reaped {
        Ok(reaped) => reaped,
        // The ledger holds the reap; only the events file missed it.
        Err(e @ relais::reap::ReapError::EventsNotWritten { .. }) => {
            eprintln!("relais resume: {e}");
            return Ok(OrphanOutcome::Ended(State::Interrupted));
        }
        Err(e) => {
            return Err(CliError::Operational {
                operation: "resume",
                cause: Box::new(e),
            })
        }
    };
    match reaped {
        relais::reap::Reaped::Moved(message) => {
            println!("{message}");
            Ok(OrphanOutcome::Ended(State::Interrupted))
        }
        relais::reap::Reaped::AlreadyMoved => {
            let now = operational(ledger.run_status(run), "resume")?.unwrap_or(state);
            Ok(OrphanOutcome::Ended(now))
        }
        relais::reap::Reaped::KeptAlive(why) => Ok(OrphanOutcome::KeptAlive(why)),
    }
}

/// The dispatches the ledger still records as live for one run.
fn run_live_dispatches(
    ledger: &Ledger,
    run: &RunId,
) -> Result<Vec<relais::ledger::LiveDispatch>, CliError> {
    Ok(operational(ledger.live_dispatches(), "resume")?
        .into_iter()
        .filter(|live| live.run == *run)
        .collect())
}

/// Judge every dispatch the ledger still records as live for one run.
/// No coordinator answering is not evidence about the run, only the
/// absence of evidence: `reconcile` treats it as such.
fn reconcile_run(ledger: &Ledger, run: &RunId) -> Result<resume::Reconciliation, CliError> {
    let live = run_live_dispatches(ledger, run)?;
    let coordinator_view = relais::coordinator::socket_path()
        .ok()
        .map(relais::coordinator::Client::new)
        .and_then(|client| client.status().ok())
        .and_then(|snapshot| snapshot.runs.get(run.as_str()).cloned());
    // A native agent lives and dies with its Claude Code session, which
    // is live while its relais plugin says hello.
    let gate = relais::coordinator::socket_path()
        .ok()
        .map(relais::coordinator::RemoteGate::new);
    let session_live = |session: &str| {
        let gate = gate.as_ref()?;
        match gate.native_hello_age(session) {
            Ok(Some(age)) => Some(age < relais::native::HELLO_FRESH),
            // Never heard since the coordinator started (it may have been
            // restarted): no evidence either way.
            Ok(None) | Err(_) => None,
        }
    };
    Ok(resume::reconcile_with_sessions(
        &live,
        coordinator_view.as_ref(),
        &|pid| relais::coordinator::process_alive(pid.get()),
        &session_live,
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
    let retained = retained_with_verification(&state_dir)?;
    let InPlaceScan {
        runs: in_place,
        unreadable,
    } = in_place_leftovers();
    if retained.is_empty() && in_place.is_empty() {
        if unreadable > 0 {
            // Each was named on its own line: this is not an empty scan.
            return Ok(CliOutcome::OperationalFailure);
        }
        println!("no run worktree is retained under {}", state_dir.display());
        return Ok(CliOutcome::Accepted);
    }
    // A marker relais cannot read is kept for the person: not a success.
    let mut failed = unreadable > 0;
    let mut seen = std::collections::BTreeSet::new();
    for (worktree, kind) in retained {
        let run = RunId::from_stored(worktree.run_id.clone());
        seen.insert(worktree.run_id.clone());
        let Some(state) = settle_state(&ledger, &run, &worktree.path)? else {
            continue;
        };
        let reconciliation = reconcile_run(&ledger, &run)?;
        match retire_any(&ledger, &run, state, &reconciliation, &worktree.path, kind)? {
            Retired::Retired => {}
            Retired::Kept => failed = true,
        }
    }
    // Interrupted in-place runs live in the person's own worktree, which the
    // state directory never lists: their markers are found in the worktrees
    // of the repository this is run from. A run found by both sources is
    // handled once.
    for (run, path) in in_place {
        if !seen.insert(run.as_str().to_string()) {
            continue;
        }
        let Some(state) = settle_state(&ledger, &run, &path)? else {
            continue;
        };
        let reconciliation = reconcile_run(&ledger, &run)?;
        match retire_in_place(&ledger, &run, state, &reconciliation)? {
            Some(CliOutcome::Accepted) => {}
            Some(_) => failed = true,
            None => {
                println!(
                    "{run}: kept, {} has a marker the ledger does not match",
                    path.display()
                );
                failed = true;
            }
        }
    }
    Ok(if failed {
        CliOutcome::OperationalFailure
    } else {
        CliOutcome::Accepted
    })
}

/// The run's state, settled for a retirement: `None` (after saying why on
/// one line) when the run is not on record, is still alive, or is silent but
/// not provably dead. A non-terminal run that has gone silent is ended as
/// interrupted first.
fn settle_state(ledger: &Ledger, run: &RunId, path: &Path) -> Result<Option<State>, CliError> {
    let Some(state) = operational(ledger.run_status(run), "resume")? else {
        println!("{run}: kept, no such run on record ({})", path.display());
        return Ok(None);
    };
    if state.is_terminal() {
        return Ok(Some(state));
    }
    if let Some(orphan) = orphan_of_run(ledger, run)? {
        return match interrupt_orphan(ledger, run, state, &orphan)? {
            OrphanOutcome::Ended(ended) => Ok(Some(ended)),
            OrphanOutcome::KeptAlive(why) => {
                println!(
                    "{run}: kept, silent past its threshold but {why} ({})",
                    path.display()
                );
                Ok(None)
            }
        };
    }
    println!("{run}: kept, the run is {state} ({})", path.display());
    Ok(None)
}

/// The interrupted in-place runs of the worktrees of the repository the
/// command runs from, as `(run, worktree)`. Outside a repository, or where a
/// worktree's git directory cannot be read, it says so in one line and
/// carries on: this is a sweep, and one unreadable worktree (a pruned one,
/// say) must not hide the others.
/// What the worktrees hold from interrupted in-place runs: the runs to
/// retire, and how many marker files could not be read (each named on a line
/// of its own already).
#[derive(Default)]
struct InPlaceScan {
    runs: Vec<(RunId, PathBuf)>,
    unreadable: usize,
}

fn in_place_leftovers() -> InPlaceScan {
    let here = match project_dir() {
        Ok(here) => here,
        Err(e) => {
            println!("interrupted in-place runs are not looked for: {e}");
            return InPlaceScan::default();
        }
    };
    let worktrees = match workspace::worktree_paths(&here) {
        Ok(worktrees) => worktrees,
        Err(e) => {
            println!("interrupted in-place runs are not looked for: {e}");
            return InPlaceScan::default();
        }
    };
    let mut found = InPlaceScan::default();
    for path in worktrees {
        match relais::workspace::inplace::git_dir(&path)
            .and_then(|git_dir| relais::workspace::inplace::leftover_markers(&git_dir))
        {
            Ok(left) => {
                found.runs.extend(
                    left.markers
                        .into_iter()
                        .map(|marker| (RunId::from_stored(marker.run), path.clone())),
                );
                for bad in left.unreadable {
                    found.unreadable += 1;
                    println!(
                        "kept, {}: {} cannot be read ({}); look at the tree, and delete the file once you have",
                        path.display(),
                        bad.path.display(),
                        bad.why
                    );
                }
            }
            Err(e) => {
                found.unreadable += 1;
                println!("kept, {}: {e}", path.display());
            }
        }
    }
    found
}

/// What a retained checkout is: a task worktree (tracked work exported
/// first) or a verification checkout (holds no work, removed as is).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckoutKind {
    Task,
    Verification,
}

/// Every retained task worktree and verification checkout, in that order.
fn retained_with_verification(
    state_dir: &Path,
) -> Result<Vec<(workspace::RetainedWorktree, CheckoutKind)>, CliError> {
    let tasks = operational(
        workspace::retained_worktrees(state_dir),
        "listing the retained worktrees",
    )?;
    let verifications = operational(
        workspace::retained_verify_worktrees(state_dir),
        "listing the retained verification checkouts",
    )?;
    Ok(tasks
        .into_iter()
        .map(|w| (w, CheckoutKind::Task))
        .chain(
            verifications
                .into_iter()
                .map(|w| (w, CheckoutKind::Verification)),
        )
        .collect())
}

fn retire_any(
    ledger: &Ledger,
    run: &RunId,
    state: State,
    reconciliation: &resume::Reconciliation,
    path: &Path,
    kind: CheckoutKind,
) -> Result<Retired, CliError> {
    match kind {
        CheckoutKind::Task => retire_worktree(ledger, run, state, reconciliation, path),
        CheckoutKind::Verification => Ok(retire_verification_checkout(run, reconciliation, path)),
    }
}

/// One verification checkout of a terminal run: removed once its
/// dispatches are provably dead, unless a tracked file was modified.
fn retire_verification_checkout(
    run: &RunId,
    reconciliation: &resume::Reconciliation,
    path: &Path,
) -> Retired {
    if !reconciliation.all_provably_dead() {
        println!(
            "{run}: kept, a dispatch may still write it: {} ({})",
            reconciliation.refusal(),
            path.display()
        );
        return Retired::Kept;
    }
    match workspace::retire_verification(path) {
        Ok(workspace::VerifyRetirement::Removed { bytes_reclaimed }) => {
            println!(
                "{run}: removed verification checkout {} — {bytes_reclaimed} bytes reclaimed",
                path.display()
            );
            Retired::Retired
        }
        Ok(workspace::VerifyRetirement::Modified { files }) => {
            println!(
                "{run}: kept, verification checkout {} has tracked modifications: {}",
                path.display(),
                files.join("; ")
            );
            Retired::Kept
        }
        Err(e) => {
            eprintln!(
                "relais resume: {run}: kept, {} could not be removed: {e}",
                path.display()
            );
            Retired::Kept
        }
    }
}

/// Finish an in-place run that died, if `run` was one (ADR-0008): the
/// worktree is the caller's own and is never removed. Its marker names the
/// tree it must end as; the work no candidate holds is exported first, the
/// target applied, and the marker removed last. `None` = not an in-place run.
fn retire_in_place(
    ledger: &Ledger,
    run: &RunId,
    state: State,
    reconciliation: &resume::Reconciliation,
) -> Result<Option<CliOutcome>, CliError> {
    use relais::workspace::inplace::{self, Lease, LeaseRefusal, Outcome, Target, TargetKind};
    let Some(workspace) = operational(ledger.run_workspace(run), "resume")? else {
        return Ok(None);
    };
    let root = workspace.repo_path;
    let Ok(git_dir) = inplace::git_dir(&root) else {
        return Ok(None);
    };
    let Ok(marker) = inplace::read_marker(&git_dir, run.as_str()) else {
        return Ok(None);
    };
    if !reconciliation.all_provably_dead() {
        println!(
            "{run}: kept, a dispatch may still write {} ({})",
            root.display(),
            reconciliation.detail()
        );
        return Ok(Some(CliOutcome::OperationalFailure));
    }
    let lease = match Lease::reclaim(&root, run.as_str()) {
        Ok(lease) => lease,
        Err(LeaseRefusal::Busy { holder }) => {
            println!(
                "{run}: kept, run {} holds {}",
                holder.as_deref().unwrap_or("?"),
                root.display()
            );
            return Ok(Some(CliOutcome::OperationalFailure));
        }
        Err(LeaseRefusal::Interrupted { .. } | LeaseRefusal::UnreadableMarker { .. }) => {
            return Ok(None)
        }
        Err(LeaseRefusal::Io(e)) => {
            println!("{run}: kept, {} could not be taken: {e}", root.display());
            return Ok(Some(CliOutcome::OperationalFailure));
        }
    };
    let tree_of = |rev: &str| -> Result<String, CliError> {
        let output = workspace::git_command(&root)
            .args(["rev-parse", &format!("{rev}^{{tree}}")])
            .output()
            .map_err(|e| CliError::Usage {
                detail: format!("git could not be launched: {e}"),
            })?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    };
    // An accepted run is left holding its change, also when it died before
    // the marker said so: the ledger and the receipt know.
    let artifacts = paths::runs_dir()
        .map_err(CliError::Home)?
        .join(run.as_str());
    let accepted = (state == State::Accepted)
        .then(|| std::fs::read_to_string(artifacts.join("receipt.json")).ok())
        .flatten()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|receipt| receipt["candidate_sha"].as_str().map(str::to_string));
    let target = match accepted {
        Some(candidate) => Target {
            kind: TargetKind::Accepted,
            tree: tree_of(&candidate)?,
        },
        None => Target {
            kind: TargetKind::Start,
            tree: tree_of(&marker.start)?,
        },
    };
    let worktree = workspace::TaskWorktree {
        path: root.clone(),
        base_sha: marker.start.clone(),
        index: Some(lease.relais_index()),
    };
    let mut export = || {
        workspace::export_unkept(&worktree, run.as_str(), &artifacts)
            .map(|exported| {
                exported
                    .map(|exported| exported.reference)
                    .unwrap_or_else(|| "none".to_string())
            })
            .map_err(std::io::Error::other)
    };
    let keep = inplace::IndexPolicy::Keep;
    Ok(Some(
        match inplace::finalize(lease, target, keep, &mut export) {
            Ok(Outcome::Applied { index }) => {
                println!(
                    "{run}: restored to {} in {} (directory kept)",
                    inplace::short(&marker.start),
                    root.display()
                );
                if matches!(index, inplace::IndexOutcome::Changed(_)) {
                    println!(
                    "{run}: your git index changed during the run; the original is kept at {}/relais-inplace/{run}.index.orig",
                    git_dir.display()
                );
                }
                CliOutcome::Accepted
            }
            Ok(Outcome::HeadMoved { was, now }) => {
                println!(
                "{run}: kept, HEAD moved from {} to {} during the run; the tree and the branch were not touched, review them yourself",
                inplace::short(&was),
                inplace::short(&now)
            );
                CliOutcome::OperationalFailure
            }
            Err(e) => {
                println!("{run}: kept, {e}");
                CliOutcome::OperationalFailure
            }
        },
    ))
}

/// Retire every worktree one terminal run still holds.
fn retire_run(
    ledger: &Ledger,
    run: &RunId,
    state: State,
    reconciliation: &resume::Reconciliation,
) -> Result<CliOutcome, CliError> {
    if let Some(done) = retire_in_place(ledger, run, state, reconciliation)? {
        return Ok(done);
    }
    let state_dir = paths::state_dir().map_err(CliError::Home)?;
    let retained: Vec<_> = retained_with_verification(&state_dir)?
        .into_iter()
        .filter(|(worktree, _)| worktree.run_id == run.as_str())
        .collect();
    if retained.is_empty() {
        println!("{run}: no worktree is retained");
        return Ok(CliOutcome::Accepted);
    }
    let mut failed = false;
    for (worktree, kind) in retained {
        match retire_any(ledger, run, state, reconciliation, &worktree.path, kind)? {
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
        index: None,
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
    report.session_routing = operational(
        relais::router::report_section(&ledger, &since, &load_price_table()?),
        "the session-routing section",
    )?;
    // Best-effort: a registry that will not open reads as no active artifact.
    let registry_for_report = paths::registry_dir()
        .ok()
        .and_then(|dir| relais::learn::registry::Registry::open(&dir).ok());
    report.learning = Some(operational(
        relais::learn::summary::learning_report(&ledger, registry_for_report.as_ref()),
        "the learning section",
    )?);
    report.review_outcomes = Some(report::reviewer_report(&ledger));
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
    Ok(load_machine_pricing()?.unwrap_or_else(PriceTable::empty))
}

/// The machine's `[pricing]` table; `None` when it has none, so a cost can
/// say that no table is the reason it is unknown.
fn load_machine_pricing() -> Result<Option<PriceTable>, CliError> {
    let path = paths::machine_settings_path().map_err(CliError::Home)?;
    match std::fs::read_to_string(&path) {
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(cause) => Err(CliError::Read {
            what: "the machine settings",
            path,
            cause,
        }),
        Ok(text) => match MachineSettings::from_toml_str(&text) {
            Ok(machine) => Ok(machine.pricing),
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
    /// Records left out because a relais run booked them (a native
    /// worker's messages, SPEC §11); not part of any other figure.
    skipped_native: usize,
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
/// every subagent file beside it, except what a relais run booked: the
/// message ids in `native_usage_messages` and the whole file of an agent
/// a dispatch recorded (SPEC §11). Idempotent — every insert
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
        skipped_native: 0,
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
        // A relais dispatch's agent is the run's, whatever message ids its
        // transcript carries: a dispatch whose ids could not be recorded
        // (`rollback_ids_missing`) is still never booked twice.
        let relais_agent = match paths::subagent_agent_id(&path) {
            Some(agent_id) => operational(ledger.is_relais_agent(agent_id), "usage import")?,
            None => false,
        };
        for record in orchestration::parse_transcript(&text) {
            if relais_agent
                || operational(ledger.is_native_booked(&record.message_id), "usage import")?
            {
                summary.skipped_native += 1;
                continue;
            }
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

/// The summary line's tail when records were left to the run that booked them.
fn skipped_note(skipped: usize) -> String {
    match skipped {
        0 => String::new(),
        n => format!("; {n} record(s) skipped, already booked to a relais run"),
    }
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
                    "{session_id}: {} message(s), {} input + {} output tokens: {}{}",
                    summary.messages,
                    summary.input_tokens,
                    summary.output_tokens,
                    report::cost_line(summary.cost, summary.completeness),
                    skipped_note(summary.skipped_native),
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
) -> Result<std::result::Result<RunId, Refusal>, CliError> {
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
    Ok(refusal(
        CliOutcome::UnknownRun,
        format!("task {task_id} has no accepted run on record"),
    ))
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
    json: bool,
    proof_stdin: bool,
}

/// What a recorded outcome reports, for both the sentence and `--json`.
struct Recorded {
    run: RunId,
    task: TaskId,
    outcome: &'static str,
    actor: relais::ledger::Actor,
    source: &'static str,
}

fn feedback_command(request: FeedbackRequest) -> Result<CliOutcome, CliError> {
    let json = request.json;
    match feedback_inner(request) {
        Ok(Ok(done)) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "run": done.run.as_str(),
                        "task": done.task.as_str(),
                        "outcome": done.outcome,
                        "actor": done.actor.as_str(),
                        "source": done.source,
                    })
                );
            } else {
                println!("recorded {} for {}", done.outcome, done.run);
            }
            Ok(CliOutcome::Accepted)
        }
        Ok(Err(refused)) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "error": refused.message,
                        "exit": exit_code(&refused.outcome),
                    })
                );
            } else {
                eprintln!("relais feedback: {}", refused.message);
            }
            Ok(refused.outcome)
        }
        Err(e) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "error": e.to_string(),
                        "exit": exit_code(&e.outcome()),
                    })
                );
            }
            Err(e)
        }
    }
}

/// The `source` an outcome is stored with: `orchestrator` only when the
/// plugin's nonce verified, `cli` for every other call.
fn feedback_source(via: relais::ledger::RecordedVia) -> &'static str {
    match via {
        relais::ledger::RecordedVia::Plugin => relais::outcome::SOURCE_ORCHESTRATOR,
        relais::ledger::RecordedVia::Cli => relais::outcome::SOURCE_CLI,
    }
}

fn feedback_inner(request: FeedbackRequest) -> Result<Result<Recorded, Refusal>, CliError> {
    let FeedbackRequest {
        run_id,
        task,
        outcome,
        candidate,
        magnitude,
        evidence,
        actor,
        note,
        json: _,
        proof_stdin,
    } = request;
    let actor = match relais::ledger::Actor::parse(&actor) {
        Ok(actor) => actor,
        Err(message) => return Ok(refusal(CliOutcome::InvalidInput, message)),
    };
    let proof = if proof_stdin {
        match read_decide_proof() {
            Ok(proof) => Some(proof),
            Err(refused) => return Ok(Err(refused)),
        }
    } else {
        None
    };
    let ledger = open_ledger()?;
    let via = match &proof {
        Some(proof) => operational(
            ledger.verify_decide_nonce(&proof.session, &proof.nonce),
            "feedback",
        )?,
        None => relais::ledger::RecordedVia::Cli,
    };
    let run = match feedback_run(&ledger, run_id.as_deref(), task.as_deref())? {
        Ok(run) => run,
        Err(refused) => return Ok(Err(refused)),
    };
    let state = operational(ledger.run_status(&run), "feedback")?;
    let Some(state) = state else {
        return Ok(refusal(
            CliOutcome::UnknownRun,
            format!("unknown run {run}"),
        ));
    };
    // SPEC §20: feedback is attributed to the candidate; absence of
    // feedback is never a positive label. Only a run that reached an
    // accepted candidate — the runner's own verification, or a person's
    // salvage of a terminal run's work — has one whose later life is
    // worth recording.
    if !state.is_accepted() {
        return Ok(refusal(
            CliOutcome::InvalidInput,
            format!(
                "run {run} is {state}, not accepted — final outcome feedback \
                 records what happened to an ACCEPTED change"
            ),
        ));
    }
    let Some(task_id) = operational(ledger.task_of_run(&run), "feedback")? else {
        return Ok(refusal(
            CliOutcome::OperationalFailure,
            format!("run {run} carries no task identity"),
        ));
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
                    return Ok(refusal(
                        CliOutcome::OperationalFailure,
                        format!("run {run}'s receipt names no candidate"),
                    ));
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
            return Ok(refusal(
                CliOutcome::InvalidInput,
                format!(
                    "--candidate {candidate} does not match run {run}'s accepted \
                     candidate {recorded}"
                ),
            ));
        }
        // Nothing in the ledger says what this run built, so nothing can
        // check the caller's claim. Storing it anyway would put an
        // unverifiable sha in the outcomes table as the candidate this
        // outcome is about (SPEC §20: the receipt, not the caller, is the
        // source of truth). The outcome itself is still recordable —
        // without a candidate it cannot vouch for.
        (None, Some(candidate)) => {
            return Ok(refusal(
                CliOutcome::InvalidInput,
                format!(
                    "run {run} has no recorded candidate to check --candidate \
                     {candidate} against — record the outcome without it, or name the run that \
                     produced the candidate"
                ),
            ));
        }
        (Some(_), Some(_)) | (Some(_), None) | (None, None) => {}
    }
    let candidate_sha = recorded_candidate;
    // The strategy actually used is read off the ledger, never asked of
    // the caller (SPEC §20).
    let Some((_contract, tier)) = operational(ledger.run_contract_and_tier(&run), "feedback")?
    else {
        return Ok(refusal(
            CliOutcome::OperationalFailure,
            format!("run {run} carries no recorded attempt"),
        ));
    };
    let escalated = operational(ledger.escalation_attempted(&run), "feedback")?;
    let models = operational(ledger.models_used(&run), "feedback")?;
    let magnitude = match magnitude {
        Some(value) => match relais::outcome::CorrectionMagnitude::new(value) {
            Ok(magnitude) => Some(magnitude),
            Err(e) => return Ok(refusal(CliOutcome::InvalidInput, e.to_string())),
        },
        None => None,
    };
    let kind = outcome.into();
    let source = feedback_source(via);
    let detail = relais::outcome::OutcomeDetail {
        candidate_sha,
        strategy: relais::outcome::Strategy {
            tier,
            models,
            escalated,
        },
        correction_magnitude: magnitude,
        evidence,
        actor: actor.as_str().to_string(),
        note,
        source: Some(source.to_string()),
    };
    let recorded = match relais::outcome::Outcome::new(kind, detail) {
        Ok(outcome) => outcome,
        Err(e) => return Ok(refusal(CliOutcome::InvalidInput, e.to_string())),
    };
    operational(ledger.record_outcome(&run, &task_id, &recorded), "feedback")?;
    Ok(Ok(Recorded {
        run,
        task: task_id,
        outcome: kind.as_str(),
        actor,
        source,
    }))
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

/// One `relais decide` invocation's arguments, bundled because they
/// answer one decision together.
struct DecideRequest<'a> {
    run_id: &'a str,
    answer: DecideAnswer,
    actor: &'a str,
    person_quote: Option<&'a str>,
    note: Option<&'a str>,
    successor: Option<&'a str>,
    criterion: Option<&'a str>,
    candidate: Option<&'a str>,
    widen_scope: bool,
    rerun_verify: bool,
    successor_contract: Option<&'a Path>,
    json: bool,
    proof_stdin: bool,
}

/// A refusal of a decision: the exit outcome and the sentence that says
/// why. Printed to stderr, or as `{error, exit}` under `--json`.
#[derive(Debug)]
struct Refusal {
    outcome: CliOutcome,
    message: String,
}

fn refusal<T>(outcome: CliOutcome, message: impl Into<String>) -> Result<T, Refusal> {
    Err(Refusal {
        outcome,
        message: message.into(),
    })
}

/// What a recorded decision reports, for both the sentence and `--json`.
struct Decided {
    run: String,
    resolution: Reason,
    actor: relais::ledger::Actor,
    from_state: State,
    to_state: State,
    advice: serde_json::Value,
    sentence: String,
    /// The successor intent a `revise --widen-scope` or `--successor-
    /// contract` answer wrote (or, repeated, found).
    successor_intent: Option<serde_json::Value>,
}

/// An intent as `decide --json` and `native status` print it.
fn intent_json(ledger: &Ledger, intent: &relais::ledger::SuccessorIntent) -> serde_json::Value {
    // Where the plugin starts the run: the repository the decided run
    // worked in.
    let repo = ledger
        .run_workspace(&intent.predecessor)
        .ok()
        .flatten()
        .map(|workspace| workspace.repo_path.to_string_lossy().into_owned());
    serde_json::json!({
        "repo": repo,
        "intent_id": intent.intent_id,
        "decision_id": intent.decision_id,
        "predecessor": intent.predecessor.as_str(),
        "contract_digest": intent.contract_digest,
        "state": intent.state.as_str(),
        "run_id": intent.run_id.as_ref().map(RunId::as_str),
        "scope_delta": intent.scope_delta,
        "refusal": intent.refusal,
        "refusal_count": intent.refusal_count,
        "retry_after": intent.retry_after(),
    })
}

type Decision = Result<Decided, Refusal>;

fn decide_command(request: DecideRequest<'_>) -> Result<CliOutcome, CliError> {
    let json = request.json;
    match decide_inner(&request) {
        Ok(Ok(done)) => {
            if json {
                let mut printed = serde_json::json!({
                    "run": done.run,
                    "resolution": done.resolution.as_str(),
                    "actor": done.actor.as_str(),
                    "from_state": done.from_state.as_str(),
                    "to_state": done.to_state.as_str(),
                    "advice": done.advice,
                });
                if let Some(intent) = &done.successor_intent {
                    printed["successor_intent"] = intent.clone();
                }
                println!("{printed}");
            } else {
                println!("{}", done.sentence);
            }
            Ok(CliOutcome::Accepted)
        }
        Ok(Err(refused)) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "error": refused.message,
                        "exit": exit_code(&refused.outcome),
                    })
                );
            } else {
                eprintln!("relais decide: {}", refused.message);
            }
            Ok(refused.outcome)
        }
        Err(e) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "error": e.to_string(),
                        "exit": exit_code(&e.outcome()),
                    })
                );
            }
            Err(e)
        }
    }
}

/// Why a `person-via-orchestrator` sign-off without a live plugin nonce is
/// refused: only the plugin's proved call counts as the person's
/// (ADR-0004).
const UNPROVED_SIGNOFF: &str = "`--criterion` is refused for `--actor person-via-orchestrator` \
     without the plugin's proof: only an answer recorded through the decide tool counts as the \
     person's sign-off";

/// The plugin's proof on stdin (`--proof-stdin`).
#[derive(serde::Deserialize)]
struct DecideProof {
    session: String,
    nonce: String,
}

/// Read the `--proof-stdin` frame. A malformed frame is a refusal, never a
/// silent `cli`: the plugin wrote it, so a bad one is a bug to see.
fn read_decide_proof() -> Result<DecideProof, Refusal> {
    use std::io::Read;
    let mut text = String::new();
    if let Err(e) = std::io::stdin().take(64 * 1024).read_to_string(&mut text) {
        return refusal(
            CliOutcome::InvalidInput,
            format!("`--proof-stdin`: could not read stdin: {e}"),
        );
    }
    serde_json::from_str(text.trim()).or_else(|e| {
        refusal(
            CliOutcome::InvalidInput,
            format!("`--proof-stdin` expects {{\"session\", \"nonce\"}} on stdin: {e}"),
        )
    })
}

/// The checks that need only the arguments, run before the ledger is
/// opened so a refused invocation touches nothing.
fn check_decide_arguments(
    request: &DecideRequest<'_>,
    stdin_is_terminal: bool,
) -> Result<relais::ledger::Actor, Refusal> {
    use relais::ledger::Actor;
    let actor = match Actor::parse(request.actor) {
        Ok(actor) => actor,
        Err(message) => return refusal(CliOutcome::InvalidInput, message),
    };
    let quote = request.person_quote.map(str::trim);
    match (actor, quote) {
        (Actor::PersonViaOrchestrator, None | Some("")) => {
            return refusal(
                CliOutcome::InvalidInput,
                "`--actor person-via-orchestrator` requires `--person-quote <text>`: the \
                 person's own words",
            );
        }
        (Actor::PersonViaOrchestrator, Some(_)) => {}
        (_, Some(_)) => {
            return refusal(
                CliOutcome::InvalidInput,
                "`--person-quote` only applies to `--actor person-via-orchestrator`",
            );
        }
        (_, None) => {}
    }
    if actor == Actor::Person && !stdin_is_terminal {
        return refusal(
            CliOutcome::InvalidInput,
            "`--actor person` is for the person at their own terminal, and stdin is not one — \
             run it from a terminal, or record it with `--actor person-via-orchestrator \
             --person-quote <their words>`",
        );
    }
    if request.criterion.is_some() {
        match actor {
            Actor::Orchestrator => {
                return refusal(
                    CliOutcome::InvalidInput,
                    "`--criterion` signs off a human criterion and an orchestrator never does: \
                     the person signs it off from their own terminal (`--actor person`)",
                );
            }
            Actor::PersonViaOrchestrator if !request.proof_stdin => {
                return refusal(CliOutcome::InvalidInput, UNPROVED_SIGNOFF);
            }
            Actor::PersonViaOrchestrator => {}
            Actor::Person => {}
        }
    }
    Ok(actor)
}

fn decide_inner(request: &DecideRequest<'_>) -> Result<Decision, CliError> {
    use std::io::IsTerminal;
    let actor = match check_decide_arguments(request, std::io::stdin().is_terminal()) {
        Ok(actor) => actor,
        Err(refused) => return Ok(Err(refused)),
    };
    let run_id = request.run_id;
    let (answer, note, successor, criterion, candidate) = (
        request.answer,
        request.note,
        request.successor,
        request.criterion,
        request.candidate,
    );
    let person_quote = request.person_quote.map(str::trim);
    let proof = if request.proof_stdin {
        match read_decide_proof() {
            Ok(proof) => Some(proof),
            Err(refused) => return Ok(Err(refused)),
        }
    } else {
        None
    };
    let ledger = open_ledger()?;
    let recorded_via = match &proof {
        Some(proof) => operational(
            ledger.verify_decide_nonce(&proof.session, &proof.nonce),
            "decide",
        )?,
        None => relais::ledger::RecordedVia::Cli,
    };
    if criterion.is_some()
        && actor == relais::ledger::Actor::PersonViaOrchestrator
        && recorded_via != relais::ledger::RecordedVia::Plugin
    {
        return Ok(refusal(CliOutcome::InvalidInput, UNPROVED_SIGNOFF));
    }
    let run = RunId::from_stored(run_id);
    let Some(state) = operational(ledger.run_status(&run), "decide")? else {
        return Ok(refusal(
            CliOutcome::UnknownRun,
            format!("unknown run {run_id}"),
        ));
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
                person_quote,
                recorded_via,
                note,
                criterion,
                successor,
                candidate,
            },
        );
    }
    if request.widen_scope || request.rerun_verify || request.successor_contract.is_some() {
        if answer != DecideAnswer::Revise {
            return Ok(refusal(
                CliOutcome::InvalidInput,
                "`--widen-scope`, `--rerun-verify` and `--successor-contract` only apply to \
                 `--answer revise`",
            ));
        }
        if criterion.is_some() {
            return Ok(refusal(
                CliOutcome::InvalidInput,
                "`--criterion` only applies to `--answer approve`",
            ));
        }
        return successor_decide(
            &ledger,
            &run,
            state,
            request,
            relais::ledger::Answerer {
                actor,
                person_quote,
                recorded_via,
            },
        );
    }
    if !state.awaits_a_person() {
        return Ok(refusal(
            CliOutcome::InvalidInput,
            format!("run {run_id} is {state}, not waiting on a person — nothing to decide"),
        ));
    }
    if let Some(criterion_id) = criterion {
        if answer != DecideAnswer::Approve {
            return Ok(refusal(
                CliOutcome::InvalidInput,
                "`--criterion` only applies to `--answer approve`",
            ));
        }
        let Some((contract, _tier)) = operational(ledger.run_contract_and_tier(&run), "decide")?
        else {
            return Ok(refusal(
                CliOutcome::OperationalFailure,
                format!("run {run_id} carries no recorded contract"),
            ));
        };
        let declared: Vec<String> = contract.acceptance.iter().map(|entry| entry.id()).collect();
        let Some(entry) = contract
            .acceptance
            .iter()
            .find(|entry| entry.id() == criterion_id)
        else {
            return Ok(refusal(
                CliOutcome::InvalidInput,
                format!(
                    "run {run_id} names no acceptance criterion `{criterion_id}` — \
                     this contract declares: {}",
                    declared.join(", ")
                ),
            ));
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
            return Ok(refusal(
                CliOutcome::InvalidInput,
                format!(
                    "acceptance criterion `{criterion_id}` is not settled by a human \
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
                ),
            ));
        }
        operational(
            ledger.record_human_signoff(&run, criterion_id, actor, recorded_via, note),
            "decide",
        )?;
        reseal_receipt_with_signoff(&ledger, &run, criterion_id)?;
    }
    if answer == DecideAnswer::Approve {
        let gaps = open_verification_gaps(&ledger, &run)?;
        if !gaps.is_empty() {
            return Ok(refusal(
                CliOutcome::InvalidInput,
                format!(
                    "run {run_id} still carries verification gaps ({}) — a waiver is \
                     a policy or contract change, never a CLI flag",
                    gaps.join("; ")
                ),
            ));
        }
    }
    let successor_run = successor.map(RunId::from_stored);
    let resolution = answer.resolution();
    let to_state = answer.terminal_state();
    let resolved = operational(
        ledger.resolve_decision(
            &run,
            &relais::ledger::DecisionAnswer {
                resolution,
                actor,
                person_quote,
                recorded_via,
                note,
                successor_run: successor_run.as_ref(),
                from_state: state,
                to_state,
            },
        ),
        "decide",
    )?;
    if !resolved {
        return Ok(refusal(
            CliOutcome::InvalidInput,
            format!("run {run_id} has no open decision on record"),
        ));
    }
    Ok(Ok(Decided {
        run: run_id.to_string(),
        resolution,
        actor,
        from_state: state,
        to_state,
        advice: operational(ledger.decision_advice(&run), "decide")?,
        sentence: format!("recorded {} for {run_id} by {actor}", resolution.as_str()),
        successor_intent: None,
    }))
}

/// `relais decide --answer revise --widen-scope | --successor-contract
/// <file>`: the answer and the successor intent that carries it out are one
/// ledger transaction (plan A5). Repeated on an answered decision it returns
/// the intent the first answer wrote.
fn successor_decide(
    ledger: &Ledger,
    run: &RunId,
    state: State,
    request: &DecideRequest<'_>,
    answerer: relais::ledger::Answerer<'_>,
) -> Result<Decision, CliError> {
    use relais::runner::successor;
    let supplied = match request.successor_contract {
        Some(path) => Some(load_contract(path)?),
        None => None,
    };
    let decided =
        |intent: relais::ledger::SuccessorIntent, sentence: String| -> Result<Decision, CliError> {
            Ok(Ok(Decided {
                run: run.to_string(),
                resolution: Reason::DecisionRevised,
                actor: answerer.actor,
                from_state: state,
                to_state: DecideAnswer::Revise.terminal_state(),
                advice: operational(ledger.decision_advice(run), "decide")?,
                sentence,
                successor_intent: Some(intent_json(ledger, &intent)),
            }))
        };
    // Repeated: the decision is answered and its intent exists.
    if let Some(existing) = operational(ledger.intent_of_decision(run), "decide")? {
        if let Some(contract) = &supplied {
            if contract.hash() != existing.contract_digest {
                return Ok(refusal(
                    CliOutcome::InvalidInput,
                    format!(
                        "run {run} was already answered with successor intent {} for another \
                         contract ({}); a decision has one successor",
                        existing.intent_id, existing.contract_digest
                    ),
                ));
            }
        }
        let sentence = format!(
            "run {run} was already answered; successor intent {} is {}",
            existing.intent_id,
            existing.state.as_str()
        );
        return decided(existing, sentence);
    }
    if !state.awaits_a_person() {
        return Ok(refusal(
            CliOutcome::InvalidInput,
            format!("run {run} is {state}, not waiting on a person — nothing to decide"),
        ));
    }
    let planned = (|| -> Result<_, successor::PlanError> {
        let (contract, scope_delta) = match supplied {
            Some(contract) => (contract, Vec::new()),
            None if request.rerun_verify => {
                successor::require_resource_exhausted(ledger, run)?;
                let (original, _tier) = ledger.run_contract_and_tier(run)?.ok_or_else(|| {
                    successor::PlanError::Missing(format!("run {run} carries no recorded contract"))
                })?;
                let rerun = successor::widened_contract(&original, run, &[])
                    .map_err(successor::PlanError::Contract)?;
                (rerun, Vec::new())
            }
            None => {
                let paths = successor::out_of_scope_paths(ledger, run)?;
                let (original, _tier) = ledger.run_contract_and_tier(run)?.ok_or_else(|| {
                    successor::PlanError::Missing(format!("run {run} carries no recorded contract"))
                })?;
                let widened = successor::widened_contract(&original, run, &paths)
                    .map_err(successor::PlanError::Contract)?;
                (widened, paths)
            }
        };
        let runs_dir =
            paths::runs_dir().map_err(|e| successor::PlanError::Missing(e.to_string()))?;
        successor::plan(ledger, run, &contract, scope_delta, &runs_dir)
    })();
    let intent = match planned {
        Ok(intent) => intent,
        Err(e) => return Ok(refusal(CliOutcome::InvalidInput, e.to_string())),
    };
    let written = operational(
        ledger.resolve_decision_with_intent(
            run,
            &relais::ledger::DecisionAnswer {
                resolution: Reason::DecisionRevised,
                actor: answerer.actor,
                person_quote: answerer.person_quote,
                recorded_via: answerer.recorded_via,
                note: request.note,
                successor_run: None,
                from_state: state,
                to_state: DecideAnswer::Revise.terminal_state(),
            },
            &intent,
        ),
        "decide",
    )?;
    let Some(stored) = written else {
        return Ok(refusal(
            CliOutcome::InvalidInput,
            format!("run {run} has no open decision on record"),
        ));
    };
    let sentence = format!(
        "recorded {} for {run} by {}; successor intent {} is pending",
        Reason::DecisionRevised.as_str(),
        answerer.actor,
        stored.intent_id
    );
    decided(stored, sentence)
}

/// The CLI flags `--answer salvaged` reads, grouped because they answer
/// one decision together (mirrors [`relais::ledger::DecisionAnswer`]).
struct SalvageAnswer<'a> {
    actor: relais::ledger::Actor,
    person_quote: Option<&'a str>,
    recorded_via: relais::ledger::RecordedVia,
    note: Option<&'a str>,
    criterion: Option<&'a str>,
    successor: Option<&'a str>,
    candidate: Option<&'a str>,
}

/// `relais decide --answer salvaged --candidate <sha>`: a run relais
/// itself never accepted, whose candidate a person finished and merged
/// anyway (SPEC's decision spine) — terminal, or still waiting on a
/// person. Distinct from every other answer in `decide_inner`: a run
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
) -> Result<Decision, CliError> {
    let Some(candidate_sha) = answer.candidate else {
        return Ok(refusal(
            CliOutcome::InvalidInput,
            "`--answer salvaged` requires `--candidate <sha>`",
        ));
    };
    if answer.criterion.is_some() {
        return Ok(refusal(
            CliOutcome::InvalidInput,
            "`--criterion` does not apply to `--answer salvaged`",
        ));
    }
    if answer.successor.is_some() {
        return Ok(refusal(
            CliOutcome::InvalidInput,
            "`--successor` does not apply to `--answer salvaged`",
        ));
    }
    // Salvage answers a run that has stopped running, without relais
    // itself producing an accepted candidate: never one still in flight,
    // never one already accepted the ordinary way or already salvaged
    // once.
    if !state.is_terminal() || state.is_accepted() {
        return Ok(refusal(
            CliOutcome::InvalidInput,
            format!(
                "run {run} is {state} — `salvaged` answers a run that has already \
                 stopped without an accepted candidate, not one still in flight or already accepted"
            ),
        ));
    }
    // Whether the verified candidate patch survives intact in the commit a
    // person landed is computed here, once, from the run's own evidence;
    // when the repository or the patch is gone it is recorded as unknown
    // (NULL) and said so, never guessed.
    let contained = salvage_containment(ledger, run, candidate_sha)?;
    let Some(original_reason) = operational(
        ledger.record_salvage_contained(
            run,
            candidate_sha,
            relais::ledger::Answerer {
                actor: answer.actor,
                person_quote: answer.person_quote,
                recorded_via: answer.recorded_via,
            },
            answer.note,
            contained,
        ),
        "decide",
    )?
    else {
        return Ok(refusal(
            CliOutcome::InvalidInput,
            format!("run {run} already carries a decision on record"),
        ));
    };
    // The decision is saved: a landing that cannot be recorded is logged and
    // never fails the command; detection finds the landing itself later.
    if contained == Some(true) {
        let recorded = ledger.run_workspace(run).and_then(|workspace| {
            workspace
                .map(|w| {
                    relais::learn::salvage::record_salvage_landing(
                        ledger,
                        run,
                        &w.repo_path,
                        candidate_sha,
                    )
                })
                .transpose()
        });
        if let Err(e) = recorded {
            eprintln!("relais: could not record the landing of {run}: {e}");
        }
    }
    Ok(Ok(Decided {
        run: run.to_string(),
        resolution: Reason::DecisionSalvaged,
        actor: answer.actor,
        from_state: state,
        to_state: State::AcceptedByPerson,
        advice: operational(ledger.decision_advice(run), "decide")?,
        successor_intent: None,
        sentence: format!(
            "recorded {} for {run} by {} (originally ended: {}; candidate contained: {})",
            Reason::DecisionSalvaged.as_str(),
            answer.actor,
            original_reason.as_str(),
            match contained {
                Some(true) => "yes",
                Some(false) => "no",
                None => "unknown",
            }
        ),
    }))
}

/// Whether `run`'s latest verified candidate patch reverse-applies on
/// `candidate_sha`'s tree. `None`, said on stderr, when the repository or
/// the patch is unavailable.
fn salvage_containment(
    ledger: &Ledger,
    run: &RunId,
    candidate_sha: &str,
) -> Result<Option<bool>, CliError> {
    let workspace = operational(ledger.run_workspace(run), "decide")?;
    use relais::learn::salvage::Containment;
    let containment = match workspace {
        Some(workspace) => operational(
            relais::learn::salvage::containment_of_run(
                ledger,
                run,
                &workspace.repo_path,
                candidate_sha,
            ),
            "decide",
        )?,
        None => Containment::Unreachable("the run has no recorded workspace".to_string()),
    };
    if let Containment::Unreachable(reason) = &containment {
        eprintln!(
            "relais decide: could not tell whether run {run}'s verified candidate landed intact \
             in {candidate_sha} ({reason}); recording it as unknown"
        );
    }
    Ok(containment.answer())
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

    /// A ledger holding one pending successor intent, and its id.
    fn ledger_with_intent(tag: &str) -> (Ledger, String, relais::test_support::TempDir) {
        use relais::ledger::{Actor, DecisionAnswer, NewIntent, RecordedVia};
        use relais::lifecycle::{Reason, State};
        let dir = relais::test_support::temp_dir(tag);
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        let decided = RunId::from_stored("run-a");
        ledger
            .insert_run(&decided, "/r", None, &TaskId::from_stored("task-a"), "rk")
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: decided.clone(),
                attempt_id: None,
                from_state: None,
                to_state: State::NeedsDecision,
                reason: "scope_exceeded".into(),
                detail: None,
                at: ledger.now(),
            })
            .expect("waiting");
        let intent = ledger
            .resolve_decision_with_intent(
                &decided,
                &DecisionAnswer {
                    resolution: Reason::DecisionRevised,
                    actor: Actor::Orchestrator,
                    person_quote: None,
                    recorded_via: RecordedVia::Cli,
                    note: None,
                    successor_run: None,
                    from_state: State::NeedsDecision,
                    to_state: State::Failed,
                },
                &NewIntent {
                    predecessor: decided.clone(),
                    contract_digest: "digest".into(),
                    contract_json: "{}".into(),
                    scope_delta: Vec::new(),
                    candidate: None,
                },
            )
            .expect("written")
            .expect("open decision");
        (ledger, intent.intent_id, dir)
    }

    #[test]
    fn a_failure_before_the_run_row_exists_leaves_the_intent_pending() {
        use relais::ledger::{Claim, IntentState};
        let (ledger, id, _dir) = ledger_with_intent("intent-release");
        let run_id = RunId::from_stored("run-new");
        assert!(matches!(
            ledger
                .claim_intent(&id, &run_id, &successor_process)
                .expect("claim"),
            Claim::Claimed(_)
        ));
        release_intent_without_run(&ledger, RunSource::Intent(&id), &run_id, "run failed");
        let after = ledger.successor_intent(&id).expect("read").expect("row");
        assert_eq!(after.state, IntentState::Pending);
        assert_eq!(after.run_id, None);
        assert_eq!(after.refusal.as_deref(), Some("run failed"));
    }

    #[test]
    fn a_failure_after_the_run_row_exists_keeps_the_claim() {
        use relais::ledger::IntentState;
        let (ledger, id, _dir) = ledger_with_intent("intent-keep");
        let run_id = RunId::from_stored("run-new");
        ledger
            .claim_intent(&id, &run_id, &successor_process)
            .expect("claim");
        ledger
            .insert_run(&run_id, "/r", None, &TaskId::from_stored("task-a"), "rk")
            .expect("the run row");
        release_intent_without_run(&ledger, RunSource::Intent(&id), &run_id, "run failed");
        let after = ledger.successor_intent(&id).expect("read").expect("row");
        assert_eq!(after.state, IntentState::Claimed);
        assert_eq!(after.run_id, Some(run_id));
    }

    /// The start and end lines carry the type, the task, the outcome, the
    /// dispatched model, the cost, the wall time and the load at start.
    #[test]
    fn a_replay_logs_one_start_and_one_end_line() {
        let task = TaskId::from_stored("task-abc");
        let log = ReplayLog {
            task_type: Some(relais::contract::TaskType::Feature),
            task: &task,
            arm_model: "haiku",
            load: Some(3.5),
            started: std::time::Instant::now(),
        };
        assert_eq!(
            log.start_line(),
            "relais dataset replay: start type=feature task=task-abc model=haiku load=3.50"
        );
        let cost = relais::ledger::TrialCost::new(
            Some(relais::money::MicroUsd::from_micros(300_000)),
            relais::money::CostCompleteness::Actual,
        )
        .expect("a coherent cost");
        let end = ReplayEnd {
            outcome: "invalid",
            dispatched_model: Some("sonnet"),
            cost: Some(cost),
        };
        assert_eq!(
            log.end_line_after(&end, std::time::Duration::from_millis(12_340)),
            "relais dataset replay: end type=feature task=task-abc outcome=invalid \
             model=sonnet cost=$0.3 wall=12.3s load=3.50"
        );
        let untyped = ReplayLog {
            task_type: None,
            load: None,
            ..log
        };
        assert_eq!(
            untyped.end_line_after(
                &ReplayEnd::unrecorded("interrupted"),
                std::time::Duration::ZERO
            ),
            "relais dataset replay: end type=none task=task-abc outcome=interrupted \
             model=none cost=unknown wall=0.0s load=unknown"
        );
    }

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

    #[test]
    fn resume_records_the_resuming_process_as_the_driver() {
        let (ledger, dir) = temp_ledger("resume-driver");
        let run = RunId::from_stored("run-resume");
        let task = relais::ids::TaskId::from_stored("task-resume");
        ledger
            .insert_run(&run, "/repo", None, &task, "rk")
            .expect("run");
        record_resume_interruption(&ledger, &run, State::Prepared, "reconciled")
            .unwrap_or_else(|_| panic!("resume records"));
        let (pid, started, since) = ledger.run_driver(&run).expect("driver columns");
        assert_eq!(pid, Some(i64::from(std::process::id())));
        // A token on Unix, `None` on Windows (no start-time reader there).
        assert_eq!(
            started,
            relais::procs::start_time(std::process::id()),
            "driver_started"
        );
        assert!(since.is_some(), "driver_since");
        drop(dir);
    }

    #[test]
    fn a_run_is_refused_without_a_recent_hello_or_on_a_claude_code_outside_the_range() {
        use relais::adapter::claude::ProbeFailure;
        let gate = relais::admission::LocalGate::new(Default::default());
        let supported = Some("2.1.291 (Claude Code)");
        let silent = run_refusal("relais run", &gate, "s1", supported, None)
            .expect("answers")
            .expect("no hello is refused");
        assert!(silent.contains("hello"), "{silent}");
        gate.native_hello("s1").expect("hello");
        assert_eq!(
            run_refusal("relais run", &gate, "s1", supported, None).expect("answers"),
            None
        );
        assert!(run_refusal("relais run", &gate, "other", supported, None)
            .expect("answers")
            .is_some());
        for outside in ["2.0.0", "2.2.0", "not a version", "2.1.0 (Claude Code)"] {
            let refused = run_refusal("relais run", &gate, "s1", Some(outside), None)
                .expect("answers")
                .unwrap_or_else(|| panic!("{outside} is refused"));
            assert!(
                refused.contains(relais::native::SUPPORTED_CLAUDE_CODE),
                "{refused}"
            );
        }
        let empty = run_refusal("relais run", &gate, "s1", Some(""), None).expect("answers");
        assert!(empty.is_some(), "an empty answer is refused");
        let failed = run_refusal(
            "relais run",
            &gate,
            "s1",
            None,
            Some(&ProbeFailure::Refused {
                args: "--version".into(),
                ended: "exit 1".into(),
            }),
        )
        .expect("answers")
        .expect("a non-zero exit is refused");
        assert!(failed.contains("exit 1"), "{failed}");
        assert!(!failed.contains("printed nothing"), "{failed}");
    }

    #[test]
    fn a_timed_out_probe_continues_on_a_fresh_hello_and_is_refused_on_a_stale_one() {
        let gate = relais::admission::LocalGate::new(Default::default());
        let late = relais::adapter::claude::ProbeFailure::TimedOut {
            args: "--version".into(),
            after: std::time::Duration::from_secs(30),
        };
        let stale = run_refusal("relais run", &gate, "s1", None, Some(&late))
            .expect("answers")
            .expect("no hello is refused");
        assert!(stale.contains("hello"), "{stale}");
        gate.native_hello("s1").expect("hello");
        assert_eq!(
            run_refusal("relais run", &gate, "s1", None, Some(&late)).expect("answers"),
            None,
            "a late probe warns and continues"
        );
    }

    /// A session transcript: one assistant message with this id.
    fn assistant_line(message_id: &str) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"2026-10-06T10:00:00Z","message":{{"id":"{message_id}","model":"claude-opus-5","usage":{{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":0}}}}}}"#
        )
    }

    /// The projects directory of one session: its main transcript, and
    /// the transcripts of a relais agent (`a-relais`) and of the person's
    /// own subagent (`a-own`).
    fn session_with_two_subagents(dir: &Path) -> PathBuf {
        let projects = dir.join("projects");
        let slug = projects.join("-slug");
        let subagents = slug.join("sess-r").join("subagents");
        std::fs::create_dir_all(&subagents).expect("subagents dir");
        std::fs::write(slug.join("sess-r.jsonl"), assistant_line("msg-main")).expect("main");
        std::fs::write(
            subagents.join("agent-a-relais.jsonl"),
            assistant_line("msg-relais"),
        )
        .expect("relais agent");
        std::fs::write(
            subagents.join("agent-a-own.jsonl"),
            assistant_line("msg-own"),
        )
        .expect("own agent");
        projects
    }

    /// `usage import` books the session's own messages and the person's
    /// subagents, and nothing from the transcript of an agent the ledger
    /// records as a relais dispatch — even though none of that agent's
    /// message ids is in `native_usage_messages` (`rollback_ids_missing`).
    #[test]
    fn usage_import_skips_the_agent_file_of_a_relais_dispatch() {
        let (ledger, dir) = temp_ledger("import-agent-file");
        let projects = session_with_two_subagents(&dir);
        let run = relais::ids::RunId::from_stored("run-r");
        ledger
            .insert_run(
                &run,
                "/repo",
                Some("sess-r"),
                &relais::ids::TaskId::from_stored("task-r"),
                "rk",
            )
            .expect("run");
        let imported = |ledger: &Ledger| match import_session(
            ledger,
            &projects,
            &PriceTable::empty(),
            "sess-r",
        )
        .expect("imports")
        {
            SessionImportOutcome::Imported(summary) => summary,
            SessionImportOutcome::Unattributable | SessionImportOutcome::TranscriptMissing => {
                panic!("the session has a transcript")
            }
        };

        // No dispatch on record: all three files are the session's.
        let (fresh, fresh_dir) = temp_ledger("import-agent-file-fresh");
        assert_eq!(imported(&fresh).messages, 3);
        drop(fresh_dir);

        let dispatch = relais::ids::DispatchId::from_stored("d-r");
        ledger
            .record_dispatch_intent(
                &dispatch,
                &run,
                None,
                &serde_json::json!({}),
                0,
                relais::route::RoutedBy::ConservativeBaseline,
            )
            .expect("dispatch");
        ledger
            .record_native_dispatch(&dispatch, Some("a-relais"))
            .expect("agent bound");
        assert!(!ledger.is_native_booked("msg-relais").expect("lookup"));

        let summary = imported(&ledger);
        assert_eq!(
            summary.messages, 2,
            "the main transcript and the person's agent"
        );
        assert_eq!(summary.skipped_native, 1, "the relais agent's file");
        // Idempotent: a second import inserts nothing either.
        assert_eq!(imported(&ledger).messages, 2);
    }

    #[test]
    fn the_native_flags_are_gone_and_the_native_callbacks_parse() {
        let parse_run = |args: &[&str]| {
            Cli::try_parse_from(["relais", "run", "--task", "t.json"].iter().chain(args))
        };
        assert!(parse_run(&["--protocol"]).is_ok());
        assert!(parse_run(&[]).is_ok());
        assert!(parse_run(&["--native"]).is_err());
        assert!(parse_run(&["--native-spawn-wait", "30"]).is_err());
        let parse = |args: &[&str]| Cli::try_parse_from(["relais", "native"].iter().chain(args));
        assert!(parse(&["hello", "--session", "s1"]).is_ok());
        assert!(parse(&["bound", "--dispatch", "d1", "--agent", "a1"]).is_ok());
        assert!(parse(&["bound", "--dispatch", "d1"]).is_err());
        assert!(parse(&["stopped", "--dispatch", "d1"]).is_ok());
        assert!(parse(&["status"]).is_ok());
        assert!(parse(&["status", "--run", "run-1"]).is_ok());
    }

    /// relais keeps no OS sandbox, so there is nothing for the flag to
    /// verify: it no longer parses.
    #[test]
    fn doctor_verify_sandbox_is_a_usage_error() {
        let parse = |args: &[&str]| Cli::try_parse_from(["relais", "doctor"].iter().chain(args));
        assert!(parse(&[]).is_ok());
        assert!(parse(&["--json"]).is_ok());
        assert!(parse(&["--verify-sandbox"]).is_err());
        assert!(parse(&["--json", "--verify-sandbox"]).is_err());
    }

    #[test]
    fn a_stopped_report_is_read_from_the_plugins_payload() {
        let report = read_stopped_report(
            r#"{"agent":"a1","status":"killed","usage":{"input_tokens":3,"model":"m"},"answer":"x"}"#,
        )
        .expect("parses");
        assert_eq!(report.agent_id, "a1");
        assert_eq!(report.status, relais::admission::AgentStatus::Killed);
        assert_eq!(report.usage.and_then(|usage| usage.input_tokens), Some(3));
        assert!(read_stopped_report(r#"{"agent":"a1","status":"exploded"}"#).is_err());
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

    /// Run first: the native attempt booked the worker's messages from
    /// its subagent transcript, so importing the session inserts the
    /// session's own messages only and reports what it left to the run.
    #[test]
    fn import_leaves_a_native_workers_messages_to_the_run_that_booked_them() {
        use relais::ids::{RunId, TaskId};
        use relais::ledger::UsageEvent;
        use relais::money::CostKind;
        let (ledger, dir) = temp_ledger("import-native");
        let run = RunId::from_stored("run-n");
        ledger
            .insert_run(
                &run,
                "/repo",
                Some("sess-n"),
                &TaskId::from_stored("task-n"),
                "rk",
            )
            .expect("run");
        let line = |id: &str| {
            format!(
                r#"{{"type":"assistant","timestamp":"2026-10-06T10:00:00Z","message":{{"id":"{id}","model":"claude-opus-5","usage":{{"input_tokens":1000000,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation":{{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":0}}}}}}}}"#
            )
        };
        let slug = dir.join("projects").join("-slug");
        let subagents = slug.join("sess-n").join("subagents");
        std::fs::create_dir_all(&subagents).expect("subagents dir");
        std::fs::write(slug.join("sess-n.jsonl"), line("msg-session")).expect("main");
        std::fs::write(subagents.join("agent-a1.jsonl"), line("msg-worker")).expect("agent");
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
        let booked = UsageEvent {
            event_id: "d1".into(),
            run_id: run.clone(),
            attempt_id: None,
            parent_event_id: None,
            model: Some("claude-opus-5".into()),
            input_tokens: Some(1_000_000),
            output_tokens: Some(0),
            cache_read_tokens: None,
            cache_write_tokens: None,
            cost: Some(MicroUsd::from_micros(5_000_000)),
            cost_kind: CostKind::EstimatedApiEquivalent,
            completeness: CostCompleteness::Estimated,
            inclusive: false,
            at: ledger.now(),
            phase: None,
            duration_ms: None,
            requested_model: None,
            requested_effort: None,
            harness: None,
        };
        ledger
            .record_native_usage(&booked, &["msg-worker".to_string()])
            .expect("native booking");
        let SessionImportOutcome::Imported(summary) =
            import_session(&ledger, &dir.join("projects"), &table, "sess-n").expect("imports")
        else {
            panic!("the session has a transcript");
        };
        assert_eq!(summary.messages, 1);
        assert_eq!(summary.skipped_native, 1);
        assert!(skipped_note(summary.skipped_native).contains("1 record(s) skipped"));
        let (session_cost, _) = ledger
            .orchestration_spend_since("2026-01-01T00:00:00Z")
            .expect("spend");
        assert_eq!(
            session_cost,
            MicroUsd::from_micros(5_000_000),
            "the session's own only"
        );
        assert_eq!(
            ledger.run_cost(&run).expect("run cost"),
            MicroUsd::from_micros(5_000_000)
        );
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
        CliOutcome::StaleGrantKey,
        CliOutcome::NothingDetected,
    ];

    #[test]
    fn the_exit_code_table_is_total_and_injective() {
        let mut seen: Vec<i32> = ALL.iter().map(exit_code).collect();
        assert_eq!(seen.len(), 20, "every documented outcome is listed");
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
            task_type: Vec::new(),
            requires_design: false,
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
            task_type: base_task_type,
            requires_design: base_requires_design,
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
            (
                "task_type",
                RecipeSpec {
                    task_type: vec![relais::contract::TaskType::Feature],
                    ..base.clone()
                },
            ),
            (
                "requires_design",
                RecipeSpec {
                    requires_design: true,
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
        assert!(base_task_type.is_empty());
        assert!(!base_requires_design);

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
                    actor: relais::ledger::Actor::Person,
                    person_quote: None,
                    recorded_via: relais::ledger::RecordedVia::Cli,
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
            .record_salvage(&salvaged, "deadbeef", relais::ledger::Actor::Person, None)
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
            candidate: None,
            predecessor: None,
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
