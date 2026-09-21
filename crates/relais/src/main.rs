//! CLI entry point (SPEC §3, §17, §23). Handlers live in the library
//! modules; this file is argument parsing, wiring and exit codes:
//! 0 accepted, 2 needs a human decision, 3 blocked, 4 the task or budget
//! failed.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};

use relais::backend::Backend;
use relais::context::AvalVerdict;
use relais::contract::TaskContract;
use relais::learn::predict::RegistryPredictor;
use relais::policy::{effective_authority, MachineSettings, RepoPolicy};
use relais::runner::{execute, Reason, RunConfig, State, Terminal};
use relais::{doctor, ledger::Ledger, paths, report, route, workspace};

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
    },
    /// Create a relais.toml policy for this repository
    Init,
    /// Preflight a task contract: validate, resolve base, explain the
    /// route without launching a model (SPEC §3)
    Plan {
        /// Path to the task contract
        #[arg(long = "task")]
        task: PathBuf,
    },
    /// Validate the contract and start an execution (SPEC §3)
    Run {
        /// Path to the task contract
        #[arg(long = "task")]
        task: PathBuf,
    },
    /// Show recent runs, or one run's current state
    Status { run_id: Option<String> },
    /// Explain one run: route reasons, transitions, costs, decisions
    Explain { run_id: String },
    /// Reconcile interrupted state; never blindly repeats the last
    /// command (SPEC §3)
    Resume { run_id: String },
    /// Cost and outcome reporting since a date (SPEC §11)
    Report {
        /// Inclusive lower bound, e.g. 2026-09-01
        #[arg(long)]
        since: Option<String>,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
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
        run_id: String,
        /// What actually happened to the accepted change
        #[arg(long = "outcome")]
        outcome: FeedbackOutcome,
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

fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Doctor { json } => doctor_command(json),
        Command::Init => init_command(),
        Command::Plan { task } => plan_command(&task),
        Command::Run { task } => run_command(&task),
        Command::Status { run_id } => status_command(run_id.as_deref()),
        Command::Explain { run_id } => explain_command(&run_id),
        Command::Resume { run_id } => resume_command(&run_id),
        Command::Report { since, json } => report_command(since.as_deref(), json),
        Command::Dataset { cmd } => match cmd {
            DatasetCommand::Build => dataset_build_command(),
        },
        Command::Train => train_command(),
        Command::Evaluate { artifact } => evaluate_command(&artifact),
        Command::Promote { artifact_id } => promote_command(&artifact_id),
        Command::Feedback { run_id, outcome } => feedback_command(&run_id, outcome),
        Command::Install {
            claude,
            write,
            user,
        } => {
            if claude {
                install_command(write, user)
            } else {
                eprintln!("relais install: name what to install (--claude)");
                2
            }
        }
        Command::Uninstall {
            claude,
            write,
            user,
        } => {
            if claude {
                uninstall_command(write, user)
            } else {
                eprintln!("relais uninstall: name what to remove (--claude)");
                2
            }
        }
        Command::Coordinator { cmd } => coordinator_command(cmd),
    };
    std::process::exit(code);
}

fn install_command(write: bool, user: bool) -> i32 {
    let root = if user {
        relais::install::InstallRoot::user()
    } else {
        relais::install::InstallRoot::project(&project_dir())
    };
    let plan = root.plan();
    println!(
        "relais install --claude ({})\n{}",
        if user { "user level" } else { "project level" },
        plan.render()
    );
    if !write {
        println!("preview only: re-run with --write to apply");
        return 0;
    }
    match root.apply(&plan) {
        Ok(applied) => {
            println!("applied {} change(s)", applied.len());
            0
        }
        Err(e) => {
            eprintln!("relais install: {e}");
            1
        }
    }
}

fn uninstall_command(write: bool, user: bool) -> i32 {
    let root = if user {
        relais::install::InstallRoot::user()
    } else {
        relais::install::InstallRoot::project(&project_dir())
    };
    let plan = root.uninstall_plan();
    println!(
        "relais uninstall --claude ({})\n{}",
        if user { "user level" } else { "project level" },
        plan.render()
    );
    if plan.actions.is_empty() {
        println!("nothing owned to remove");
        return 0;
    }
    if !write {
        println!("preview only: re-run with --write to apply");
        return 0;
    }
    match root.apply_uninstall(&plan) {
        Ok(applied) => {
            println!(
                "removed {} owned artifact(s); conflicts were kept",
                applied.len()
            );
            0
        }
        Err(e) => {
            eprintln!("relais uninstall: {e}");
            1
        }
    }
}

/// The registry, when learned routing is on and the registry opens; a
/// missing or unreadable registry is the conservative baseline, not an
/// error (SPEC §21: missing trained evidence produces conservative
/// execution without disabling the rest of the product).
fn learned_registry(machine: &MachineSettings) -> Option<relais::learn::registry::Registry> {
    if !machine.routing.learned_enabled {
        return None;
    }
    relais::learn::registry::Registry::open(&paths::registry_dir()).ok()
}

/// `<backend> <version>` for `plan`, which must not need the harness to
/// answer: unknown when it is not installed.
fn harness_identity() -> Option<String> {
    let backend = relais::adapter::claude::ClaudeBackend::discover().ok()?;
    let capabilities = backend.probe()?;
    Some(format!(
        "{} {}",
        backend.name(),
        capabilities.version.as_deref().unwrap_or("?")
    ))
}

fn registry() -> relais::learn::registry::Registry {
    relais::learn::registry::Registry::open(&paths::registry_dir()).unwrap_or_else(|e| {
        eprintln!("relais: artifact registry is unavailable: {e}");
        std::process::exit(1);
    })
}

fn datasets_dir() -> PathBuf {
    paths::state_dir().join("datasets")
}

fn latest_dataset() -> Result<relais::learn::dataset::Dataset, i32> {
    let dir = datasets_dir();
    let newest = std::fs::read_dir(&dir).ok().and_then(|entries| {
        entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .max()
    });
    let Some(path) = newest else {
        eprintln!(
            "relais: no dataset under {} — run `relais dataset build` first",
            dir.display()
        );
        return Err(2);
    };
    let text = std::fs::read_to_string(&path).map_err(|e| {
        eprintln!("relais: cannot read {}: {e}", path.display());
        2
    })?;
    serde_json::from_str(&text).map_err(|e| {
        eprintln!("relais: dataset {} is malformed: {e}", path.display());
        2
    })
}

fn dataset_build_command() -> i32 {
    let (_, repo) = load_repo_policy().unwrap_or_else(|code| std::process::exit(code));
    let ledger = open_ledger();
    // A stored contract revision that does not parse is a corrupt ledger
    // row, not a run without a contract: dataset construction must see
    // the difference, or a version skew silently empties the dataset.
    let contract_of = |run_id: &str| -> Result<
        Option<(TaskContract, String, String)>,
        relais::ledger::LedgerError,
    > {
        let Some((contract_json, objective, tier)) = ledger.run_contract_and_tier(run_id)? else {
            return Ok(None);
        };
        let contract = TaskContract::from_json_str(&contract_json).map_err(|e| {
            relais::ledger::LedgerError::Corrupt {
                what: format!("contract revision of run {run_id}"),
                detail: e.to_string(),
            }
        })?;
        Ok(Some((contract, objective, tier)))
    };
    let dataset = match relais::learn::dataset::build(&ledger, &contract_of, &repo) {
        Ok(dataset) => dataset,
        Err(e) => {
            eprintln!("relais dataset build: {e}");
            return 2;
        }
    };
    let distribution = dataset.acceptance_labels();
    let dir = datasets_dir();
    or_exit(std::fs::create_dir_all(&dir), "dataset build");
    let path = dir.join(format!(
        "{}.json",
        chrono::Utc::now().format("%Y%m%dT%H%M%S")
    ));
    or_exit(
        std::fs::write(
            &path,
            or_exit(serde_json::to_string_pretty(&dataset), "dataset build"),
        ),
        "dataset build",
    );
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
    0
}

fn train_command() -> i32 {
    let dataset = match latest_dataset() {
        Ok(dataset) => dataset,
        Err(code) => return code,
    };
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
            // build cares whether there is nothing yet (3), not enough of
            // one tier (4), or a solver that ran away (5).
            return match e {
                relais::learn::evaluate::TrainError::NoTrainingRecords { .. } => 3,
                relais::learn::evaluate::TrainError::NoTierCoverage { .. } => 4,
                relais::learn::evaluate::TrainError::SolverDiverged { .. } => 5,
            };
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
        dataset_fingerprint: dataset.fingerprint,
        solver: settings.solver,
        trained_at: relais::ledger::now_rfc3339(),
        relais_version: relais::version().into(),
        evaluation: Some(serde_json::to_value(&outcome.report).expect("serializes")),
    };
    let reg = registry();
    reg.store(&artifact).unwrap_or_else(|e| {
        eprintln!("relais train: {e}");
        std::process::exit(1);
    });
    print!("{}", outcome.report.render());
    println!("candidate artifact: {artifact_id}");
    if outcome.report.gates.gates_passed {
        println!("`relais promote {artifact_id}` activates it (rollback is immediate)");
    } else {
        println!("gates failed; promote will refuse this artifact");
    }
    0
}

fn evaluate_command(artifact_id: &str) -> i32 {
    let reg = registry();
    let artifact = match reg.load(artifact_id) {
        Ok(artifact) => artifact,
        Err(e) => {
            eprintln!("relais evaluate: {e}");
            return 2;
        }
    };
    let Some(evaluation) = &artifact.evaluation else {
        eprintln!("relais evaluate: artifact {artifact_id} carries no evaluation report");
        return 2;
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
                return 2;
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
        0
    } else {
        // The verdict is recomputed here too: a stored `gates_passed` is
        // a claim, and `promote` will check it the same way.
        for failure in failures {
            eprintln!("relais evaluate: gate: {failure}");
        }
        2
    }
}

fn promote_command(artifact_id: &str) -> i32 {
    let reg = registry();
    // The registry checks the evidence and hands back a type that says so;
    // `promote` cannot be called with anything else.
    let evaluated = match reg.evaluated(artifact_id) {
        Ok(evaluated) => evaluated,
        Err(e) => {
            eprintln!("relais promote: {e}");
            return 2;
        }
    };
    match reg.promote(&evaluated) {
        Ok(()) => {
            println!(
                "promoted {artifact_id}; the previous artifact stays for `relais promote` rollback"
            );
            0
        }
        Err(e) => {
            eprintln!("relais promote: {e}");
            2
        }
    }
}

fn coordinator_command(cmd: CoordinatorCommand) -> i32 {
    use relais::coordinator::{self, Request, Response};
    let socket = coordinator::socket_path();
    match cmd {
        CoordinatorCommand::Daemon => {
            // Absent machine settings are not an error for the daemon:
            // the defaults apply and the grant check stays with `run`.
            let limits = std::fs::read_to_string(paths::machine_settings_path())
                .ok()
                .and_then(|text| MachineSettings::from_toml_str(&text).ok())
                .map(|machine| coordinator::effective_limits(&machine.concurrency))
                .unwrap_or_else(|| coordinator::effective_limits(&Default::default()));
            let ledger = Ledger::open(&paths::ledger_path()).ok();
            match coordinator::run_daemon(&socket, limits, ledger.as_ref()) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("relais coordinator: {e}");
                    1
                }
            }
        }
        CoordinatorCommand::Status { json } => {
            let client = coordinator::Client::new(socket.clone());
            let snapshot = match client.status() {
                Ok(snapshot) => snapshot,
                Err(_) => {
                    println!(
                        "no coordinator is serving this user ({}); one starts on the first managed dispatch",
                        socket.display()
                    );
                    return 0;
                }
            };
            if json {
                print!(
                    "{}",
                    serde_json::to_string_pretty(&snapshot).expect("serializes")
                );
                return 0;
            }
            println!("coordinator: {}", socket.display());
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
                "enforcement: managed dispatch only; native subagents are observed, not capped"
            );
            0
        }
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
                    eprintln!("relais coordinator cancel: name exactly one of --run, --dispatch, --session");
                    return 2;
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
                    0
                }
                Ok(other) => {
                    eprintln!("relais coordinator cancel: unexpected reply {other:?}");
                    1
                }
                Err(e) => {
                    eprintln!("relais coordinator cancel: {e}");
                    3
                }
            }
        }
        CoordinatorCommand::Stop => {
            let client = coordinator::Client::new(socket);
            match client.request(&Request::Shutdown) {
                Ok(_) => {
                    // The accept loop observes the flag on its next
                    // connection.
                    let _ = client.ping();
                    println!("coordinator asked to stop");
                    0
                }
                Err(e) => {
                    eprintln!("relais coordinator stop: {e}");
                    3
                }
            }
        }
    }
}

fn opt(value: Option<u32>) -> String {
    value.map_or_else(|| "unlimited".into(), |value| value.to_string())
}

fn cwd() -> PathBuf {
    std::env::current_dir().expect("current directory")
}

/// The directory a repository-level command acts on: the root whose
/// `relais.toml` governs the cwd; failing that, the repository root
/// (where `init` belongs); failing that, the cwd itself. Never an error:
/// `doctor`, `init` and `install` report what they find there.
fn project_dir() -> PathBuf {
    match relais::repo::locate_repo_root(&cwd()) {
        Ok(root) | Err(relais::repo::LocateError::RepoWithoutPolicy(root)) => root,
        Err(relais::repo::LocateError::NotInRepository(start)) => start,
    }
}

/// The repository root and its policy. `relais.toml` is looked up from
/// the cwd upward to the nearest `.git`, so a Claude Code session whose
/// cwd is a subdirectory, or the shell's `cd repo && relais …`, both land
/// on the repository's policy; a directory outside any repository is
/// refused by name rather than guessed at.
fn load_repo_policy() -> Result<(PathBuf, RepoPolicy), i32> {
    let root = relais::repo::locate_repo_root(&cwd()).map_err(|e| {
        eprintln!("relais: {e}");
        2
    })?;
    let path = root.join("relais.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| {
        eprintln!("relais: cannot read {}: {e}", path.display());
        2
    })?;
    let policy = RepoPolicy::from_toml_str(&text).map_err(|e| {
        eprintln!("relais: {} is invalid: {e}", path.display());
        2
    })?;
    Ok((root, policy))
}

fn load_machine() -> Result<MachineSettings, i32> {
    let path = paths::machine_settings_path();
    let text = std::fs::read_to_string(&path).map_err(|_| {
        eprintln!(
            "relais: machine settings {} do not exist; runs stay blocked on the trust grant",
            path.display()
        );
        2
    })?;
    MachineSettings::from_toml_str(&text).map_err(|e| {
        eprintln!("relais: machine.toml is invalid: {e}");
        2
    })
}

fn load_contract(task: &Path) -> Result<TaskContract, i32> {
    let text = std::fs::read_to_string(task).map_err(|e| {
        eprintln!("relais: cannot read {}: {e}", task.display());
        2
    })?;
    TaskContract::from_json_str(&text).map_err(|e| {
        eprintln!("relais: contract {} is invalid: {e}", task.display());
        2
    })
}

fn open_ledger() -> Ledger {
    Ledger::open(&paths::ledger_path()).unwrap_or_else(|e| {
        eprintln!(
            "relais: ledger at {} is unavailable: {e}",
            paths::ledger_path().display()
        );
        std::process::exit(1);
    })
}

/// A ledger or registry read that the CLI cannot continue without. The
/// ledger is external state — locked by another relais, truncated by a
/// crash, or written by a newer version — so a failed read prints one
/// `relais: …` line and exits non-zero; `status`, `explain`, `resume`,
/// `evaluate` and `report` never abort on a panic instead.
fn or_exit<T, E: std::fmt::Display>(result: std::result::Result<T, E>, what: &str) -> T {
    result.unwrap_or_else(|e| {
        eprintln!("relais {what}: {e}");
        std::process::exit(1);
    })
}

fn doctor_command(json: bool) -> i32 {
    let report = doctor::doctor(&project_dir());
    if json {
        print!(
            "{}",
            serde_json::to_string_pretty(&report).expect("serializes")
        );
    } else {
        print!("{}", report.render());
    }
    if report.failed() {
        3
    } else {
        0
    }
}

fn init_command() -> i32 {
    // At the repository root, not the shell's cwd: a policy written in a
    // subdirectory would govern nothing.
    let path = project_dir().join("relais.toml");
    match relais::repo::write_init_template(&path) {
        Ok(true) => {
            println!(
                "wrote {} (edit the model IDs and verification profile, then add a trust grant in machine.toml)",
                path.display()
            );
            0
        }
        Ok(false) => {
            eprintln!(
                "relais init: {} already exists; init never overwrites",
                path.display()
            );
            2
        }
        Err(e) => {
            eprintln!("relais init: {e}");
            1
        }
    }
}

fn plan_command(task: &Path) -> i32 {
    let Ok((root, repo)) = load_repo_policy() else {
        return 2;
    };
    let Ok(machine) = load_machine() else {
        return 2;
    };
    let Ok(contract) = load_contract(task) else {
        return 2;
    };

    if let Ok(dirty) = workspace::dirty_paths(&root) {
        if !dirty.is_empty() {
            eprintln!(
                "relais plan: working tree is dirty ({}); commit or stash first",
                dirty.join(", ")
            );
            return 3;
        }
    }
    let base_sha = match workspace::resolve_base(&root, &contract.base_ref) {
        Ok(sha) => sha,
        Err(e) => {
            eprintln!(
                "relais plan: cannot resolve base_ref {}: {e}",
                contract.base_ref
            );
            return 3;
        }
    };
    let authority = effective_authority(&repo, &machine, &contract);
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
    println!("base: {} ({})", base_sha, contract.base_ref);
    match &decision {
        route::Routed::Route(routed) => {
            let model = authority
                .models
                .get(&routed.tier)
                .map(|profile| profile.id.as_str());
            print!("{}", routed.explain(model));
            0
        }
        route::Routed::Blocked(blocked) => {
            print!("{}", blocked.explain());
            for blocker in blocked.blockers() {
                eprintln!("blocked: {} — {}", blocker.code, blocker.detail);
            }
            3
        }
    }
}

fn run_command(task: &Path) -> i32 {
    let Ok((root, repo)) = load_repo_policy() else {
        return 2;
    };
    let Ok(machine) = load_machine() else {
        return 2;
    };
    let Ok(contract) = load_contract(task) else {
        return 2;
    };
    let ledger = open_ledger();
    let backend = match relais::adapter::claude::ClaudeBackend::discover() {
        Ok(backend) => std::sync::Arc::from(backend),
        Err(e) => {
            eprintln!("relais run: {e}");
            return 3;
        }
    };
    // The three tools a run talks to, each behind the port this crate
    // owns: git, the decision corpus and the check inventory.
    let git = relais::workspace::SystemGit;
    let aval_resolver = relais::context::AvalCli::new(root.clone());
    let hooks = relais::verify::AmontCli::new();
    // What every worker this run dispatches will have in its
    // environment: an allowlist of this process's own, never the whole
    // of it (SPEC §8).
    let worker_env = relais::backend::LaunchEnv::from_process_env();
    // Managed dispatch is the only path `relais run` takes: a missing
    // coordinator blocks the run rather than launching unmanaged
    // (SPEC §23).
    let socket = relais::coordinator::socket_path();
    if let Err(e) = relais::coordinator::ensure_running(&socket) {
        eprintln!("relais run: blocked (admission_unavailable): {e}");
        return 3;
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
    let outcome = execute(&RunConfig {
        repo_dir: &root,
        contract: &contract,
        repo_policy: &repo,
        machine: &machine,
        ledger: &ledger,
        backend: backend.as_ref(),
        git: &git,
        hooks: &hooks,
        worker_env,
        artifacts_dir: paths::runs_dir(),
        aval_resolver: &aval_resolver,
        predictor: predictor
            .as_ref()
            .map(|predictor| predictor as &dyn route::RoutePredictor),
        gate: Some(&gate),
        session_id: relais::coordinator::session_id(),
        heartbeat_every: std::time::Duration::from_secs(30),
    });
    let run_dir = paths::runs_dir().join(outcome.run_id());
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
            0
        }
        Terminal::NeedsDecision { reason, detail } => {
            eprintln!("needs_decision ({reason}): {detail}");
            eprintln!(
                "evidence and the preserved candidate are under {}",
                run_dir.display()
            );
            2
        }
        Terminal::NeedsReview { detail } => {
            eprintln!("needs_review: {detail}");
            eprintln!(
                "evidence and the preserved candidate are under {}",
                run_dir.display()
            );
            2
        }
        Terminal::Blocked { code, detail } => {
            eprintln!("blocked ({code}): {detail}");
            3
        }
        Terminal::Failed { detail } => {
            eprintln!("failed: {detail}");
            eprintln!("the preserved candidate is under {}", run_dir.display());
            4
        }
        Terminal::BudgetExhausted { detail } => {
            eprintln!("budget_exhausted: {detail}");
            eprintln!(
                "the patch and evidence are preserved under {}",
                run_dir.display()
            );
            4
        }
        Terminal::Interrupted { detail } => {
            eprintln!("interrupted: {detail}");
            eprintln!("run `relais resume {}` to reconcile", outcome.run_id());
            4
        }
        Terminal::Cancelled { detail } => {
            eprintln!("cancelled: {detail}");
            eprintln!(
                "the preserved candidate and evidence are under {}",
                run_dir.display()
            );
            4
        }
    }
}

fn status_command(run_id: Option<&str>) -> i32 {
    let ledger = open_ledger();
    match run_id {
        None => {
            let report = or_exit(
                report::runs_report(&ledger, "2000-01-01T00:00:00+00:00"),
                "status",
            );
            print!("{}", report.render());
            0
        }
        Some(run_id) => {
            let Some(state) = or_exit(ledger.run_status(run_id), "status") else {
                eprintln!("relais status: unknown run {run_id}");
                return 2;
            };
            let cost = or_exit(ledger.run_cost(run_id), "status");
            let attempts = or_exit(ledger.attempt_count(run_id), "status");
            println!("{run_id}: {state} (attempts: {attempts}, cost: {cost})");
            0
        }
    }
}

fn explain_command(run_id: &str) -> i32 {
    let ledger = open_ledger();
    let transitions = or_exit(ledger.transitions(run_id), "explain");
    if transitions.is_empty() {
        eprintln!("relais explain: unknown run {run_id}");
        return 2;
    }
    let run_dir = paths::runs_dir().join(run_id);
    let route_file = run_dir.join("route.txt");
    if let Ok(route) = std::fs::read_to_string(&route_file) {
        print!("{route}");
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
    let children = or_exit(ledger.child_runs(run_id), "explain");
    if !children.is_empty() {
        println!("work packages (SPEC §19; each a run of its own, costed into this one):");
        for (child, package, status) in &children {
            println!(
                "  {package}: {child} {status} (attempts: {}, cost: {})",
                or_exit(ledger.attempt_count(child), "explain"),
                or_exit(ledger.run_cost(child), "explain")
            );
        }
    }
    let cost = or_exit(ledger.run_cost(run_id), "explain");
    let completeness = or_exit(ledger.run_cost_completeness(run_id), "explain");
    println!("cost: {}", report::cost_line(cost, completeness));
    if let Some((receipt, _hash)) = or_exit(ledger.receipt(run_id), "explain") {
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
    0
}

fn resume_command(run_id: &str) -> i32 {
    let ledger = open_ledger();
    let Some(state) = or_exit(ledger.run_status(run_id), "resume") else {
        eprintln!("relais resume: unknown run {run_id}");
        return 2;
    };
    if state.is_terminal() {
        println!("{run_id} is already terminal: {state}");
        return 0;
    }
    // An absent terminal result never means nothing executed (SPEC §12).
    // Reconcile liveness first: the process table for bound PIDs, the
    // coordinator for the rest. Resume never re-dispatches; it marks
    // what is provably dead interrupted and preserves everything.
    let live: Vec<_> = or_exit(ledger.live_dispatches(), "resume")
        .into_iter()
        .filter(|(_dispatch, run, _pid)| run == run_id)
        .collect();
    let coordinator_view = relais::coordinator::Client::new(relais::coordinator::socket_path())
        .status()
        .ok()
        .and_then(|snapshot| snapshot.runs.get(run_id).cloned());
    let mut still_live = Vec::new();
    let mut dead = Vec::new();
    let mut uncertain = Vec::new();
    for (dispatch, _run, pid) in &live {
        match pid.and_then(|pid| u32::try_from(pid).ok()) {
            Some(pid) if relais::coordinator::process_alive(pid) => {
                still_live.push(format!("{dispatch} (pid {pid})"));
            }
            Some(pid) => {
                or_exit(
                    ledger.finish_dispatch(dispatch, "reconciled_dead"),
                    "resume",
                );
                dead.push(format!("{dispatch} (pid {pid} is gone)"));
            }
            None => match &coordinator_view {
                Some(run) if run.active > 0 || run.waiting > 0 => {
                    still_live.push(format!("{dispatch} (coordinator holds a lease)"));
                }
                _ => uncertain.push(dispatch.clone()),
            },
        }
    }
    if !still_live.is_empty() {
        println!(
            "{run_id} is {state} with worker(s) still live: {}. Resume does not re-dispatch \
             while a worker may be running; `relais coordinator cancel --run {run_id}` stops \
             it, `relais explain {run_id}` shows the evidence",
            still_live.join(", ")
        );
        return 4;
    }
    let detail = if live.is_empty() {
        "no dispatch was live; the run stopped before or between launches".to_string()
    } else {
        format!(
            "provably dead: [{}]; unknown outcome (uncertain, not retried): [{}]",
            dead.join(", "),
            uncertain.join(", ")
        )
    };
    or_exit(
        ledger.record_transition(&relais::ledger::Transition {
            run_id: run_id.to_string(),
            attempt_id: None,
            from_state: Some(state),
            to_state: State::Interrupted,
            reason: Reason::ReconciledInterrupted.as_str().to_string(),
            detail: Some(serde_json::json!({ "detail": detail })),
            at: relais::ledger::now_rfc3339(),
        }),
        "resume",
    );
    println!(
        "{run_id}: {state} -> interrupted ({detail}). Changes are preserved under {}; nothing \
         was replayed. Start a NEW run with a revised contract if the task is still wanted",
        paths::runs_dir().join(run_id).display()
    );
    4
}

fn report_command(since: Option<&str>, json: bool) -> i32 {
    let since = since.map(|since| since.to_string()).unwrap_or_else(|| {
        chrono::Utc::now()
            .format("%Y-%m-01T00:00:00+00:00")
            .to_string()
    });
    let ledger = open_ledger();
    let report = or_exit(report::runs_report(&ledger, &since), "report");
    if json {
        print!(
            "{}",
            or_exit(serde_json::to_string_pretty(&report), "report")
        );
    } else {
        print!("{}", report.render());
    }
    0
}

fn feedback_command(run_id: &str, outcome: FeedbackOutcome) -> i32 {
    let ledger = open_ledger();
    let state = or_exit(ledger.run_status(run_id), "feedback");
    if state.is_none() {
        eprintln!("relais feedback: unknown run {run_id}");
        return 2;
    }
    // SPEC §20: feedback is attributed to the candidate; absence of
    // feedback is never a positive label. Only accepted runs have a
    // candidate whose later life is worth recording.
    if state != Some(State::Accepted) {
        eprintln!(
            "relais feedback: run {run_id} is {:?}, not accepted — final outcome feedback \
             records what happened to an ACCEPTED change",
            state
        );
        return 2;
    }
    let kind = match outcome {
        FeedbackOutcome::Accepted => "accepted_unchanged",
        FeedbackOutcome::Corrected => "corrected",
        FeedbackOutcome::Reverted => "reverted",
        FeedbackOutcome::Regression => "confirmed_regression",
    };
    or_exit(ledger.record_outcome(run_id, kind, None), "feedback");
    println!("recorded {kind} for {run_id}");
    let _ = AvalVerdict::Unknown;
    0
}
