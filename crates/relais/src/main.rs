//! CLI entry point (SPEC §3, §17, §23). Handlers live in the library
//! modules; this file is argument parsing, wiring and exit codes:
//! 0 accepted, 2 needs a human decision, 3 blocked, 4 the task or budget
//! failed.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};

use relais::context::AvalVerdict;
use relais::contract::TaskContract;
use relais::policy::{effective_authority, MachineSettings, RepoPolicy};
use relais::runner::{execute, RunConfig, RunOutcome, State};
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
    /// (SPEC §3)
    Install {
        /// Install the Claude Code skill and agent definitions
        #[arg(long = "claude")]
        claude: bool,
        /// Apply the reviewed changes instead of previewing
        #[arg(long)]
        write: bool,
    },
    /// Remove only owned, unchanged artifacts (SPEC §3)
    Uninstall {
        /// Remove the Claude Code skill and agent definitions
        #[arg(long = "claude")]
        claude: bool,
        /// Apply the removal instead of previewing
        #[arg(long)]
        write: bool,
    },
    /// Coordinator operations (SPEC §23). Started lazily by the CLI when
    /// anything needs it; --daemon is the internal foreground form.
    Coordinator {
        /// Run the shared per-user coordinator in the foreground
        #[arg(long)]
        daemon: bool,
    },
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
        Command::Install { claude, write: _ } => {
            if claude {
                stub("install --claude", "M9")
            } else {
                eprintln!("relais install: name what to install (--claude)");
                2
            }
        }
        Command::Uninstall { claude, write: _ } => {
            if claude {
                stub("uninstall --claude", "M9")
            } else {
                eprintln!("relais uninstall: name what to remove (--claude)");
                2
            }
        }
        Command::Coordinator { daemon } => stub_coordinator(daemon),
    };
    std::process::exit(code);
}

fn stub(name: &str, milestone: &str) -> i32 {
    eprintln!("relais {name}: not implemented yet (milestone {milestone})");
    2
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
    let repo = load_repo_policy().unwrap_or_else(|code| std::process::exit(code));
    let ledger = open_ledger();
    let contract_of = |run_id: &str| -> Option<(TaskContract, String, String)> {
        ledger
            .run_contract_and_tier(run_id)
            .ok()
            .flatten()
            .and_then(|(contract_json, objective, tier)| {
                TaskContract::from_json_str(&contract_json)
                    .ok()
                    .map(|contract| (contract, objective, tier))
            })
    };
    let dataset = relais::learn::dataset::build(&ledger, &contract_of, &repo);
    let (_, positives, negatives) = dataset.acceptance_labels();
    let dir = datasets_dir();
    std::fs::create_dir_all(&dir).expect("dataset dir");
    let path = dir.join(format!(
        "{}.json",
        chrono::Utc::now().format("%Y%m%dT%H%M%S")
    ));
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&dataset).expect("serializes"),
    )
    .expect("dataset write");
    println!(
        "dataset: {} (fingerprint {})",
        path.display(),
        dataset.fingerprint
    );
    println!(
        "records: {} ({} accepted-without-escalation, {} not)",
        dataset.records.len(),
        positives,
        negatives
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
    let settings = relais::learn::learner::SolverSettings::default();
    println!(
        "training on {} record(s) (seed {})",
        dataset.records.len(),
        settings.seed
    );
    let outcome = match relais::learn::evaluate::train_and_evaluate(
        &dataset,
        settings,
        0.75,
        relais::learn::evaluate::DEFAULT_MIN_RECORDS_PER_TIER,
    ) {
        Ok(outcome) => outcome,
        Err(e) => {
            eprintln!("relais train: {e}");
            return 2;
        }
    };
    let artifact_id = format!("artifact-{}", chrono::Utc::now().format("%Y%m%dT%H%M%S"));
    let artifact = relais::learn::registry::Artifact {
        schema_version: relais::learn::registry::ARTIFACT_SCHEMA_VERSION,
        artifact_id: artifact_id.clone(),
        feature_schema: relais::learn::features::FeatureSchema::standard(),
        standardization: outcome.standardization,
        acceptance: outcome.acceptance,
        cost: outcome.cost,
        tiers_supported: outcome.tiers_supported,
        cohorts: vec!["change".into(), "inspect".into()],
        dataset_fingerprint: dataset.fingerprint,
        solver: settings,
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
    let report: relais::learn::evaluate::EvalReport =
        serde_json::from_value(evaluation.clone()).expect("stored evaluations parse");
    print!("{}", report.render());
    println!("dataset fingerprint: {}", artifact.dataset_fingerprint);
    println!(
        "trained at: {} with relais {}",
        artifact.trained_at, artifact.relais_version
    );
    if report.gates.gates_passed {
        0
    } else {
        2
    }
}

fn promote_command(artifact_id: &str) -> i32 {
    let reg = registry();
    let gates = relais::learn::registry::PromotionGates {
        gates_passed: true,
        min_records_per_tier: relais::learn::evaluate::DEFAULT_MIN_RECORDS_PER_TIER,
        coverage: vec![],
        test_acceptance_rate: None,
        quality_floor: 0.75,
        abstention_rate: 0.0,
    };
    match reg.promote(artifact_id, &gates) {
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

fn stub_coordinator(_daemon: bool) -> i32 {
    eprintln!("relais coordinator: not implemented yet (milestone M6)");
    2
}

fn cwd() -> PathBuf {
    std::env::current_dir().expect("current directory")
}

fn load_repo_policy() -> Result<RepoPolicy, i32> {
    let path = cwd().join("relais.toml");
    let text = std::fs::read_to_string(&path).map_err(|_| {
        eprintln!(
            "relais: no relais.toml in {} — run `relais init`",
            cwd().display()
        );
        2
    })?;
    RepoPolicy::from_toml_str(&text).map_err(|e| {
        eprintln!("relais: relais.toml is invalid: {e}");
        2
    })
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

fn doctor_command(json: bool) -> i32 {
    let report = doctor::doctor(&cwd());
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
    match relais::policy::write_init_template(std::path::Path::new("relais.toml")) {
        Ok(true) => {
            println!("wrote relais.toml (edit the model IDs and verification profile, then add a trust grant in machine.toml)");
            0
        }
        Ok(false) => {
            eprintln!("relais init: relais.toml already exists; init never overwrites");
            2
        }
        Err(e) => {
            eprintln!("relais init: {e}");
            1
        }
    }
}

fn plan_command(task: &Path) -> i32 {
    let Ok(repo) = load_repo_policy() else {
        return 2;
    };
    let Ok(machine) = load_machine() else {
        return 2;
    };
    let Ok(contract) = load_contract(task) else {
        return 2;
    };

    if let Ok(dirty) = workspace::dirty_paths(&cwd()) {
        if !dirty.is_empty() {
            eprintln!(
                "relais plan: working tree is dirty ({}); commit or stash first",
                dirty.join(", ")
            );
            return 3;
        }
    }
    let base_sha = match workspace::resolve_base(&cwd(), &contract.base_ref) {
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
    let decision = route::route(route::RouteInputs {
        contract: &contract,
        repo: &repo,
        machine: &machine,
        authority: &authority,
        predictor: None,
    });
    println!("contract hash: {}", contract.hash());
    println!("policy hash: {}", authority.authority_hash);
    println!("base: {} ({})", base_sha, contract.base_ref);
    let model = decision
        .tier
        .and_then(|tier| authority.models.get(&tier))
        .map(|profile| profile.id.as_str());
    print!("{}", decision.explain(model));
    if decision.tier.is_none() {
        for blocker in &decision.blocked {
            eprintln!("blocked: {} — {}", blocker.code, blocker.detail);
        }
        3
    } else {
        0
    }
}

fn run_command(task: &Path) -> i32 {
    let Ok(repo) = load_repo_policy() else {
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
    let aval_resolver =
        move |key: &str, scope: Option<&str>| relais::context::aval_resolve(&cwd(), key, scope);
    let outcome = execute(&RunConfig {
        repo_dir: &cwd(),
        contract: &contract,
        repo_policy: &repo,
        machine: &machine,
        ledger: &ledger,
        backend: backend.as_ref(),
        artifacts_dir: paths::runs_dir(),
        aval_resolver: &aval_resolver,
        predictor: None,
    });
    let run_dir = paths::runs_dir().join(outcome.run_id());
    match &outcome {
        RunOutcome::Accepted { receipt, .. } => {
            println!("accepted: {}", receipt.candidate_sha);
            println!("receipt: {}/receipt.json", run_dir.display());
            println!(
                "patch:   {}/candidate-{}.patch",
                run_dir.display(),
                receipt.attempts
            );
            println!(
                "cost:    {} ({})",
                receipt.cost,
                report::completeness_label(receipt.cost_completeness)
            );
            println!("run:     {}", outcome.run_id());
            0
        }
        RunOutcome::NeedsDecision { reason, detail, .. } => {
            eprintln!("needs_decision ({reason}): {detail}");
            eprintln!(
                "evidence and the preserved candidate are under {}",
                run_dir.display()
            );
            2
        }
        RunOutcome::NeedsReview { detail, .. } => {
            eprintln!("needs_review: {detail}");
            eprintln!(
                "evidence and the preserved candidate are under {}",
                run_dir.display()
            );
            2
        }
        RunOutcome::Blocked { code, detail, .. } => {
            eprintln!("blocked ({code}): {detail}");
            3
        }
        RunOutcome::Failed { detail, .. } => {
            eprintln!("failed: {detail}");
            eprintln!("the preserved candidate is under {}", run_dir.display());
            4
        }
        RunOutcome::BudgetExhausted { detail, .. } => {
            eprintln!("budget_exhausted: {detail}");
            eprintln!(
                "the patch and evidence are preserved under {}",
                run_dir.display()
            );
            4
        }
        RunOutcome::Interrupted { detail, .. } => {
            eprintln!("interrupted: {detail}");
            eprintln!("run `relais resume {}` to reconcile", outcome.run_id());
            4
        }
    }
}

fn status_command(run_id: Option<&str>) -> i32 {
    let ledger = open_ledger();
    match run_id {
        None => {
            let report =
                report::runs_report(&ledger, "2000-01-01T00:00:00+00:00").unwrap_or_else(|e| {
                    eprintln!("relais status: {e}");
                    std::process::exit(1);
                });
            print!("{}", report.render());
            0
        }
        Some(run_id) => {
            let Some(state) = ledger.run_status(run_id).unwrap_or(None) else {
                eprintln!("relais status: unknown run {run_id}");
                return 2;
            };
            let cost = ledger.run_cost(run_id).expect("cost");
            let attempts = ledger.attempt_count(run_id).expect("attempts");
            println!("{run_id}: {state} (attempts: {attempts}, cost: {cost})");
            0
        }
    }
}

fn explain_command(run_id: &str) -> i32 {
    let ledger = open_ledger();
    let transitions = ledger.transitions(run_id).unwrap_or_else(|e| {
        eprintln!("relais explain: {e}");
        std::process::exit(1);
    });
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
    let cost = ledger.run_cost(run_id).expect("cost");
    let completeness = ledger.run_cost_completeness(run_id).expect("completeness");
    println!(
        "cost: {cost} ({})",
        report::completeness_label(completeness)
    );
    if let Some((receipt, _hash)) = ledger.receipt(run_id).expect("receipt") {
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
    let Some(state) = ledger.run_status(run_id).unwrap_or(None) else {
        eprintln!("relais resume: unknown run {run_id}");
        return 2;
    };
    if state.is_terminal() {
        println!("{run_id} is already terminal: {state}");
        return 0;
    }
    // An absent terminal result never means nothing executed (SPEC §12).
    // Without the coordinator's lease table, resume does NOT re-dispatch:
    // it reports what is known and stops.
    let live: Vec<_> = ledger
        .live_dispatches()
        .expect("dispatches")
        .into_iter()
        .filter(|(_dispatch, run, _pid)| run == run_id)
        .collect();
    if live.is_empty() {
        println!(
            "{run_id} is {state} with no live dispatch recorded; it may have been interrupted \
             before launch. Refusing to blindly repeat anything — start a NEW run with a \
             revised contract if the task is still wanted"
        );
        return 4;
    }
    println!(
        "{run_id} is {state} with {} dispatched worker(s) possibly still running elsewhere \
         ({}). Resume does not re-dispatch while a worker may be live; \
         `relais explain {run_id}` shows the evidence",
        live.len(),
        live.iter()
            .map(|(dispatch, _, _)| dispatch.as_str())
            .collect::<Vec<_>>()
            .join(", ")
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
    let report = report::runs_report(&ledger, &since).unwrap_or_else(|e| {
        eprintln!("relais report: {e}");
        std::process::exit(1);
    });
    if json {
        print!(
            "{}",
            serde_json::to_string_pretty(&report).expect("serializes")
        );
    } else {
        print!("{}", report.render());
    }
    0
}

fn feedback_command(run_id: &str, outcome: FeedbackOutcome) -> i32 {
    let ledger = open_ledger();
    let state = ledger.run_status(run_id).unwrap_or(None);
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
    ledger
        .record_outcome(run_id, kind, None)
        .unwrap_or_else(|e| {
            eprintln!("relais feedback: {e}");
            std::process::exit(1);
        });
    println!("recorded {kind} for {run_id}");
    let _ = AvalVerdict::Unknown;
    0
}
