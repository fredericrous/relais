//! The binary's module tree. Not part of the library.

pub(crate) mod coordinator;
pub(crate) mod ctx;
pub(crate) mod decide;
pub(crate) mod doctor;
pub(crate) mod evidence;
pub(crate) mod feedback;
pub(crate) mod install;
pub(crate) mod learn;
pub(crate) mod native;
pub(crate) mod outcome;
pub(crate) mod plan;
pub(crate) mod programme;
pub(crate) mod protocol;
pub(crate) mod recipe;
pub(crate) mod replay;
pub(crate) mod report;
pub(crate) mod resume;
pub(crate) mod run;
pub(crate) mod salvage;
pub(crate) mod status;
pub(crate) mod trust;

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use self::coordinator::{coordinator_command, quiet_command};
use self::decide::{decide_command, DecideRequest};
use self::doctor::{
    doctor_command, doctor_effort_template_command, hook_command, hook_respond_command,
    init_command, init_detect_command,
};
use self::evidence::evidence_attach_command;
use self::feedback::{feedback_command, FeedbackRequest};
use self::install::{install_command, uninstall_command, Targets};
use self::learn::{
    dataset_build_command, evaluate_command, export_classifier_command, export_tasks_command,
    import_task_types_command, judge_trial_command, learn_cycle_command, learn_last_command,
    promote_command, rollback_command, seed_command, train_command, SeedOutput,
};
use self::native::native_command;
use self::outcome::{exit_code, CliError, CliOutcome};
use self::plan::{plan_command, PlanFormat};
use self::programme::{
    check_command as programme_check_command, frontier_command as programme_frontier_command,
    list_command as programme_list_command, show_command as programme_show_command, Format,
    Listing,
};
use self::protocol::{announce_unstarted, open_protocol};
use self::recipe::{
    recipe_diff_command, recipe_evaluate_command, recipe_list_command, recipe_promote_command,
    recipe_rollback_command, recipe_show_command, AmendMode,
};
use self::replay::replay_command;
use self::report::{report_command, usage_import_command};
use self::resume::{resume_command, resume_logged, retire_all_command, Retire};
use self::run::{run_command, RunSource};
use self::status::{explain_command, status_command};
use self::trust::trust_command;

use relais::runner::{Reason, State};

#[derive(Parser)]
#[command(
    name = "relais",
    version,
    about = "Coding-agent execution companion: routing, context, verification, accounting"
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
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
        by: Option<relais::report::Dimension>,
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
    /// Read-only queries over programme maps (SPEC §31): which decision
    /// tickets are ready, what a programme holds, whether its files agree
    Programme {
        #[command(subcommand)]
        cmd: ProgrammeCommand,
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
pub(crate) enum ProgrammeCommand {
    /// Every programme map under a root, with its destination and counts
    List {
        /// Directory holding the repositories (default: the parent of the
        /// current repository's main checkout, through the git common dir, else
        /// the current directory)
        #[arg(long)]
        root: Option<PathBuf>,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Open tickets whose blockers are closed and that no `prog/<slug>/<id>`
    /// branch works, one per line: id, type/mode, repos, name
    Frontier {
        /// The programme directory, docs/programmes/<slug>
        dir: PathBuf,
        /// Also list the tickets a branch works, labelled `in-flight <branch>`
        #[arg(long)]
        all: bool,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// The destination, the counts, each blocked ticket and its blockers,
    /// each in-flight ticket and its branch
    Show {
        /// The programme directory, docs/programmes/<slug>
        dir: PathBuf,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Report every problem in the map and its tickets, as
    /// `<path>:<line>: <problem>` on stderr; exit 2 when there is one
    Check {
        /// The programme directory, docs/programmes/<slug>
        dir: PathBuf,
        /// Also print `{problems: [...]}` on stdout
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum QuietCommand {
    /// Poll the active-run directory and print one JSON line per event
    /// until stdout closes or the parent process changes. Reads no stdin.
    Watch {
        /// Print the protocol lines (required: there is no other format)
        #[arg(long, required = true)]
        protocol: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum EvidenceCommand {
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
pub(crate) enum CoordinatorCommand {
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
pub(crate) enum RouterCommand {
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
pub(crate) enum LearnCommand {
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
pub(crate) enum DatasetCommand {
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
    /// Write a described set of runs (JSON lines; an optional first line
    /// `{"seed_schema": 1}`) into a NEW ledger, so `relais report` can be
    /// checked against known counts. Refused (exit 2) unless
    /// `RELAIS_STATE_DIR` names a directory other than the default one with
    /// no ledger file in it; a bad file is refused whole, each bad line
    /// printed as `<file>:<line>: <reason>`, and nothing is written. The
    /// ledger appears complete or not at all
    #[command(name = "seed")]
    Seed {
        /// The seed file, or `-` for stdin
        file: PathBuf,
        /// Print `{"seeded": <n>, "ledger": <path>}` on stdout
        #[arg(long)]
        json: bool,
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
pub(crate) enum JudgeVerdict {
    Pass,
    Fail,
}

#[derive(Subcommand)]
pub(crate) enum TrustCommand {
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
pub(crate) enum GrantSource {
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
pub(crate) enum GrantScope {
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
pub(crate) enum NativeCommand {
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
pub(crate) enum RouterSource {
    #[value(name = "plugin-ask")]
    PluginAsk,
}

pub(crate) fn router_source(source: Option<RouterSource>) -> relais::router::Source {
    match source {
        Some(RouterSource::PluginAsk) => relais::router::Source::PluginAsk,
        None => relais::router::Source::Cli,
    }
}

#[derive(Subcommand)]
pub(crate) enum UsageCommand {
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
pub(crate) enum RecipeCommand {
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
pub(crate) enum FeedbackOutcome {
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
pub(crate) enum DecideAnswer {
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
    /// The run stopped for a review it could not have (`review_over_budget`,
    /// `review_unavailable`): review the same candidate in parts, one
    /// reviewer dispatch per part. Records a successor intent, a `kind:
    /// verify` of this run's candidate with `review: parts`; refused, before
    /// any intent, when a single file exceeds the review budget or more than
    /// 8 parts would be needed. On a patch within the budget it is one part,
    /// a retry of the review.
    ReviewParts,
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
            // The answer is a revision of the run: its successor reviews.
            Self::ReviewParts => Reason::DecisionRevised,
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
            Self::Reject | Self::Revise | Self::Decided | Self::Abandon | Self::ReviewParts => {
                State::Cancelled
            }
            Self::Salvaged => State::AcceptedByPerson,
        }
    }
}

/// The argv that asks clap for the help `args` wanted, when an unknown
/// argument stopped the parse and `-h` or `--help` stands before any `--`
/// (cli.help.help-and-version: `relais --bogus --help` is someone asking
/// how to use us). It keeps the subcommand path the words name and drops
/// everything else, so clap prints the same help it prints for
/// `relais <path> --help`. A word that names no subcommand is skipped, which
/// also skips a flag's value.
fn help_argv(args: &[std::ffi::OsString]) -> Option<Vec<std::ffi::OsString>> {
    use clap::CommandFactory;
    let mut command = Cli::command();
    let mut argv = vec![args.first()?.clone()];
    let mut wants_help = false;
    for arg in args.iter().skip(1) {
        let Some(word) = arg.to_str() else { continue };
        match word {
            "--" => break,
            "-h" | "--help" => wants_help = true,
            _ if word.starts_with('-') => {}
            _ => {
                if let Some(sub) = command.find_subcommand(word).cloned() {
                    argv.push(arg.clone());
                    command = sub;
                }
            }
        }
    }
    wants_help.then(|| {
        argv.push("--help".into());
        argv
    })
}

/// `Cli::parse`, except that help wins over an unknown argument.
fn parse_cli() -> Cli {
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(e) if e.kind() == clap::error::ErrorKind::UnknownArgument => {
            match help_argv(&args).map(Cli::try_parse_from) {
                Some(Err(help)) if help.kind() == clap::error::ErrorKind::DisplayHelp => {
                    help.exit()
                }
                _ => e.exit(),
            }
        }
        Err(e) => e.exit(),
    }
}

pub(crate) fn main() {
    let cli = parse_cli();
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

/// Route one parsed invocation to its handler. Every handler returns the
/// same pair, so the exit code is decided in exactly one place.
pub(crate) fn dispatch(command: Command) -> Result<CliOutcome, CliError> {
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
            DatasetCommand::Seed { file, json } => {
                let output = if json {
                    SeedOutput::Json
                } else {
                    SeedOutput::Human
                };
                seed_command(&file, output)
            }
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
        Command::Programme { cmd } => match cmd {
            ProgrammeCommand::List { root, json } => {
                programme_list_command(root.as_deref(), Format::from_json_flag(json))
            }
            ProgrammeCommand::Frontier { dir, all, json } => programme_frontier_command(
                &dir,
                Listing::from_all_flag(all),
                Format::from_json_flag(json),
            ),
            ProgrammeCommand::Show { dir, json } => {
                programme_show_command(&dir, Format::from_json_flag(json))
            }
            ProgrammeCommand::Check { dir, json } => {
                programme_check_command(&dir, Format::from_json_flag(json))
            }
        },
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

#[cfg(test)]
mod tests {
    use super::*;
    use relais::ledger::Ledger;
    pub(crate) fn temp_ledger(label: &str) -> (Ledger, relais::test_support::TempDir) {
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
    fn help_wins_over_an_unknown_argument() {
        let argv = |words: &[&str]| -> Vec<std::ffi::OsString> {
            std::iter::once("relais")
                .chain(words.iter().copied())
                .map(Into::into)
                .collect()
        };
        let kind = |words: &[&str]| {
            help_argv(&argv(words)).map(|a| Cli::try_parse_from(a).err().map(|e| e.kind()))
        };
        let help = Some(Some(clap::error::ErrorKind::DisplayHelp));
        assert_eq!(kind(&["--bogus", "--help"]), help);
        assert_eq!(kind(&["--bogus", "-h"]), help);
        assert_eq!(
            help_argv(&argv(&["coordinator", "status", "--bogus", "--help"])),
            Some(argv(&["coordinator", "status", "--help"]))
        );
        assert_eq!(
            kind(&["run", "--task", "t.json", "--bogus", "--help"]),
            help
        );
        // No help asked, or asked only after `--`: the usage error stands.
        assert_eq!(help_argv(&argv(&["--bogus-flag-for-probe"])), None);
        assert_eq!(help_argv(&argv(&["run", "--", "--help"])), None);
        assert_eq!(
            Cli::try_parse_from(argv(&["--bogus-flag-for-probe"]))
                .err()
                .map(|e| e.exit_code()),
            Some(2)
        );
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
            DecideAnswer::ReviewParts,
        ] {
            let expected = match answer {
                DecideAnswer::Approve => State::Accepted,
                DecideAnswer::Salvaged => State::AcceptedByPerson,
                DecideAnswer::Reject
                | DecideAnswer::Revise
                | DecideAnswer::Decided
                | DecideAnswer::Abandon
                | DecideAnswer::ReviewParts => State::Cancelled,
            };
            assert_eq!(answer.terminal_state(), expected, "{answer:?}");
        }
    }
}
