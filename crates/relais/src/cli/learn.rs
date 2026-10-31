//! The learning commands: the registry, datasets, training, promotion,
//! rollback, export and import, and the learning envelope.

use super::ctx::datasets_dir;
use super::ctx::default_repo_policy;
use super::ctx::load_machine;
use super::ctx::load_repo_policy;
use super::ctx::open_ledger;
use super::ctx::print_document;
use super::ctx::registry;
use super::outcome::operational;
use super::outcome::CliError;
use super::outcome::CliOutcome;
use super::JudgeVerdict;
use relais::backend::Harness;
use relais::contract::TaskContract;
use relais::ids::RunId;
use relais::ledger::Ledger;
use relais::paths;
use relais::policy::MachineSettings;
use std::path::Path;
/// The registry, when learned routing is on and the registry opens; a
/// missing or unreadable registry is the conservative baseline, not an
/// error (SPEC §21: missing trained evidence produces conservative
/// execution without disabling the rest of the product).
///
/// "Unreadable" is not "absent", though, and the difference is the
/// operator's to act on: a registry that will not open is said so on
/// stderr before routing falls back.
pub(crate) fn learned_registry(
    machine: &MachineSettings,
) -> Option<relais::learn::registry::Registry> {
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
pub(crate) fn harness_identity() -> HarnessProbe {
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
pub(crate) struct HarnessProbe {
    pub(crate) identity: Option<String>,
    pub(crate) efforts: Option<relais::catalog::EffortSet>,
}

pub(crate) fn latest_dataset() -> Result<relais::learn::dataset::Dataset, CliError> {
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

pub(crate) fn dataset_build_command() -> Result<CliOutcome, CliError> {
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

pub(crate) fn train_command() -> Result<CliOutcome, CliError> {
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

pub(crate) fn evaluate_command(artifact_id: &str) -> Result<CliOutcome, CliError> {
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

pub(crate) fn promote_command(artifact_id: &str) -> Result<CliOutcome, CliError> {
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
pub(crate) struct CycleOut {
    log: Option<relais::learn::cycle::CycleLog>,
    quiet: bool,
}

impl CycleOut {
    pub(crate) fn open(log: Option<&Path>, quiet: bool) -> Result<Self, CliError> {
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
    pub(crate) fn say(&mut self, line: &str) {
        match &mut self.log {
            Some(log) => log.line(line),
            None => println!("{line}"),
        }
    }

    /// A diagnostic: stderr.
    pub(crate) fn complain(&mut self, line: &str) {
        match &mut self.log {
            Some(log) => log.line(line),
            None => eprintln!("{line}"),
        }
    }

    /// One stage's progress line: stderr, unless `-q`.
    pub(crate) fn progress(&mut self, line: &str) {
        if !self.quiet {
            self.complain(line);
        }
    }
}

/// The Training lease, released when this drops. The coordinator also frees
/// it when this process is gone (pid liveness), so a crash leaves nothing.
pub(crate) struct TrainingGuard {
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
pub(crate) fn learn_cycle_command(
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
pub(crate) fn learn_last_command(json: bool) -> Result<CliOutcome, CliError> {
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
pub(crate) fn export_classifier_command(
    out: &Path,
    since: Option<&str>,
) -> Result<CliOutcome, CliError> {
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
pub(crate) fn export_tasks_command(out: &Path) -> Result<CliOutcome, CliError> {
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
pub(crate) fn import_task_types_command(file: &Path) -> Result<CliOutcome, CliError> {
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

/// How `relais dataset seed` tells its result: chosen once from the flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeedOutput {
    Human,
    Json,
}

const SEED_NEXT_COMMAND: &str = "`RELAIS_STATE_DIR=$(mktemp -d) relais dataset seed <file>`";

/// The text of a guard refusal (reason and next command) and its exit.
fn guard_failure(error: &relais::seed::GuardError) -> (String, CliOutcome) {
    use relais::seed::GuardError;
    match error {
        GuardError::StateDirNotSet => (
            format!(
                "relais dataset seed: refused: {error}; seed writes only into an explicit, \
                 empty state; run {SEED_NEXT_COMMAND}"
            ),
            CliOutcome::InvalidInput,
        ),
        GuardError::DefaultStateDir { .. } => (
            format!(
                "relais dataset seed: refused: {error}, which holds a person's ledger; run \
                 {SEED_NEXT_COMMAND}"
            ),
            CliOutcome::InvalidInput,
        ),
        GuardError::LedgerExists { .. } => (
            format!(
                "relais dataset seed: refused: {error}; seed writes only a ledger that does not \
                 exist yet; use a fresh directory: {SEED_NEXT_COMMAND}"
            ),
            CliOutcome::InvalidInput,
        ),
        GuardError::Io { .. } => (
            format!("relais dataset seed: {error}"),
            CliOutcome::OperationalFailure,
        ),
    }
}

/// `relais dataset seed <file|-> [--json]`. The guard and the whole-file
/// validation run before any ledger is opened or written.
pub(crate) fn seed_command(file: &Path, output: SeedOutput) -> Result<CliOutcome, CliError> {
    use std::io::Read as _;
    let override_dir = std::env::var_os(paths::STATE_DIR_ENV);
    let default_dir = paths::default_state_dir().ok();
    let state_dir =
        match relais::seed::guard_destination(override_dir.as_deref(), default_dir.as_deref()) {
            Ok(dir) => dir,
            Err(error) => {
                let (text, outcome) = guard_failure(&error);
                eprintln!("{text}");
                return Ok(outcome);
            }
        };
    let from_stdin = file == Path::new("-");
    let label = if from_stdin {
        "<stdin>".to_string()
    } else {
        file.display().to_string()
    };
    let mut text = String::new();
    let read = if from_stdin {
        std::io::stdin().read_to_string(&mut text).map(|_| ())
    } else {
        std::fs::read_to_string(file).map(|read| text = read)
    };
    if let Err(error) = read {
        eprintln!("relais dataset seed: {label}: {error}");
        return Ok(CliOutcome::OperationalFailure);
    }
    let now = chrono::Utc::now().to_rfc3339();
    let runs = match relais::seed::parse_seed(&text, &now) {
        Ok(runs) => runs,
        Err(problems) => {
            for problem in &problems {
                eprintln!("{label}:{}: {}", problem.line, problem.reason);
            }
            eprintln!(
                "relais dataset seed: {} bad line(s); nothing was written",
                problems.len()
            );
            return Ok(CliOutcome::InvalidInput);
        }
    };
    let written = relais::seed::write_seed(&state_dir, &runs);
    Ok(report_seed_result(
        written,
        runs.len(),
        output,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    ))
}

/// How a seed write is told to the caller and exited: the one mapping from
/// the writer's outcome to streams and exit status.
fn report_seed_result(
    written: Result<relais::seed::SeedOutcome, relais::seed::SeedError>,
    seeded: usize,
    output: SeedOutput,
    stdout: &mut dyn std::io::Write,
    stderr: &mut dyn std::io::Write,
) -> CliOutcome {
    use relais::seed::SeedOutcome;
    // A closed stderr or stdout cannot be reported anywhere else.
    let say = |sink: &mut dyn std::io::Write, line: String| {
        let _ = writeln!(sink, "{line}"); // nowhere left to report a failed write
    };
    match written {
        Ok(SeedOutcome::Published {
            path,
            cleanup_warning,
        }) => {
            if let Some(warning) = cleanup_warning {
                say(stderr, format!("relais dataset seed: warning: {warning}"));
            }
            match output {
                SeedOutput::Json => say(
                    stdout,
                    serde_json::json!({
                        "seeded": seeded,
                        "ledger": path.display().to_string(),
                    })
                    .to_string(),
                ),
                SeedOutput::Human => say(
                    stderr,
                    format!(
                        "relais dataset seed: seeded {seeded} runs into {}",
                        path.display()
                    ),
                ),
            }
            CliOutcome::Accepted
        }
        Ok(SeedOutcome::DestinationExists { path }) => {
            say(
                stderr,
                format!(
                    "relais dataset seed: refused: {} appeared while seeding; nothing of this \
                     seed was kept; use a fresh directory: {SEED_NEXT_COMMAND}",
                    path.display()
                ),
            );
            CliOutcome::InvalidInput
        }
        Err(error) => {
            say(stderr, format!("relais dataset seed: {error}"));
            CliOutcome::OperationalFailure
        }
    }
}

/// `relais dataset judge-trial <TRIAL_ID> --verdict pass|fail --note <TEXT>`.
pub(crate) fn judge_trial_command(
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
pub(crate) fn rollback_command(json: bool) -> Result<CliOutcome, CliError> {
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
pub(crate) fn learning_envelope_command(
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
pub(crate) fn learning_off_command(
    by: &str,
    source: relais::router::Source,
) -> Result<CliOutcome, CliError> {
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

#[cfg(test)]
mod seed_report_tests {
    use super::*;
    use relais::seed::{SeedError, SeedOutcome};

    fn report(
        written: Result<SeedOutcome, SeedError>,
        output: SeedOutput,
    ) -> (CliOutcome, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let outcome = report_seed_result(written, 3, output, &mut out, &mut err);
        (
            outcome,
            String::from_utf8(out).expect("utf8"),
            String::from_utf8(err).expect("utf8"),
        )
    }

    #[test]
    fn destination_exists_exits_2() {
        let (outcome, out, err) = report(
            Ok(SeedOutcome::DestinationExists {
                path: "/s/ledger.sqlite".into(),
            }),
            SeedOutput::Json,
        );
        assert_eq!(outcome, CliOutcome::InvalidInput);
        assert_eq!(super::super::outcome::exit_code(&outcome), 2);
        assert_eq!(out, "");
        assert!(err.contains("RELAIS_STATE_DIR=$(mktemp -d)"), "{err}");
    }

    #[test]
    fn published_with_a_cleanup_warning_exits_0_and_warns() {
        let (outcome, out, err) = report(
            Ok(SeedOutcome::Published {
                path: "/s/ledger.sqlite".into(),
                cleanup_warning: Some("could not remove x".into()),
            }),
            SeedOutput::Human,
        );
        assert_eq!(super::super::outcome::exit_code(&outcome), 0);
        assert_eq!(out, "");
        assert!(err.contains("warning: could not remove x"), "{err}");
        assert!(err.contains("seeded 3 runs into /s/ledger.sqlite"), "{err}");
    }

    #[test]
    fn json_prints_only_the_object_on_stdout() {
        let (outcome, out, _) = report(
            Ok(SeedOutcome::Published {
                path: "/s/ledger.sqlite".into(),
                cleanup_warning: None,
            }),
            SeedOutput::Json,
        );
        assert_eq!(outcome, CliOutcome::Accepted);
        let document: serde_json::Value = serde_json::from_str(&out).expect("json");
        assert_eq!(document["seeded"], 3);
        assert_eq!(document["ledger"], "/s/ledger.sqlite");
    }

    #[test]
    fn an_io_error_exits_1() {
        let error = SeedError::Io {
            operation: "publish ledger",
            path: "/s/ledger.sqlite".into(),
            source: std::io::Error::other("disk"),
        };
        let (outcome, out, err) = report(Err(error), SeedOutput::Json);
        assert_eq!(super::super::outcome::exit_code(&outcome), 1);
        assert_eq!(out, "");
        assert!(err.contains("publish ledger"), "{err}");
    }

    #[test]
    fn guard_refusals_exit_2_and_io_failures_exit_1() {
        use relais::seed::GuardError;
        let refusals = [
            GuardError::StateDirNotSet,
            GuardError::DefaultStateDir { dir: "/d".into() },
            GuardError::LedgerExists {
                path: "/d/l".into(),
            },
        ];
        for refusal in refusals {
            let (text, outcome) = guard_failure(&refusal);
            assert_eq!(super::super::outcome::exit_code(&outcome), 2, "{text}");
            assert!(text.contains("mktemp -d"), "{text}");
        }
        let io = GuardError::Io {
            operation: "inspect",
            path: "/d/l".into(),
            source: std::io::Error::other("denied"),
        };
        let (text, outcome) = guard_failure(&io);
        assert_eq!(super::super::outcome::exit_code(&outcome), 1, "{text}");
    }
}
