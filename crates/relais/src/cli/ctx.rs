//! Plumbing every command shares: where the process is, what it reads,
//! and the handles it opens.

use std::path::{Path, PathBuf};

use relais::backend::Harness;
use relais::contract::TaskContract;
use relais::ids::{IdSource, TaskId};
use relais::ledger::Ledger;
use relais::orchestration::PriceTable;
use relais::policy::{MachineSettings, RepoPolicy};
use relais::{paths, route};

use super::outcome::{operational, CliError, CliOutcome};

/// The process's working directory. An error, never a panic: tearing down
/// a task worktree deletes the directory a long-running relais is sitting
/// in, and a backtrace is the wrong way to say so (C14).
pub(crate) fn cwd() -> Result<PathBuf, CliError> {
    std::env::current_dir().map_err(CliError::Cwd)
}

/// The directory a repository-level command acts on: the root whose
/// `relais.toml` governs the cwd; failing that, the repository root
/// (where `init` belongs); failing that, the cwd itself. Never a
/// locate error: `doctor`, `init` and `install` report what they find
/// there.
pub(crate) fn project_dir() -> Result<PathBuf, CliError> {
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
pub(crate) fn load_repo_policy() -> Result<(PathBuf, RepoPolicy), CliError> {
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

pub(crate) fn load_machine() -> Result<MachineSettings, CliError> {
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
pub(crate) fn tuning_bounds(incumbent: &RepoPolicy) -> route::TuningBounds {
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

pub(crate) fn load_contract(task: &Path) -> Result<TaskContract, CliError> {
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

pub(crate) fn open_ledger() -> Result<Ledger, CliError> {
    let path = paths::ledger_path().map_err(CliError::Home)?;
    operational(Ledger::open(&path), "the ledger is unavailable")
}

/// What this run's task identity will be, decided before any dispatch
/// (SPEC's task spine): `None` for a fresh task, derived once preflight
/// runs; `Some(id)` for a task the contract declares or `--revise`
/// names, already confirmed on record. A disagreement between the two,
/// or an id naming no task at all, is refused here by name rather than
/// silently picked or created.
pub(crate) fn resolve_task_override(
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

/// The identifier source this process mints with: the wall clock and
/// this process's id, read once, here, at the boundary. `ids` itself
/// takes both as parameters (`effects.no-ambient-access`), which is
/// what lets a test assert on a minted identifier.
pub(crate) fn id_source() -> IdSource {
    IdSource::new(std::time::SystemTime::now, std::process::id())
}

pub(crate) fn registry() -> Result<relais::learn::registry::Registry, CliError> {
    let dir = paths::registry_dir().map_err(CliError::Home)?;
    operational(
        relais::learn::registry::Registry::open(&dir),
        "the artifact registry is unavailable",
    )
}

pub(crate) fn datasets_dir() -> Result<PathBuf, CliError> {
    Ok(paths::state_dir().map_err(CliError::Home)?.join("datasets"))
}

/// `~/.config/relais/machine.toml`'s `[pricing]` table, or an empty one
/// when the file is absent — the same "absent settings means the
/// defaults" posture `coordinator daemon` already takes reading this
/// file, not an error just because nobody has priced anything yet.
pub(crate) fn load_price_table() -> Result<PriceTable, CliError> {
    Ok(load_machine_pricing()?.unwrap_or_else(PriceTable::empty))
}

/// The machine's `[pricing]` table; `None` when it has none, so a cost can
/// say that no table is the reason it is unknown.
pub(crate) fn load_machine_pricing() -> Result<Option<PriceTable>, CliError> {
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

pub(crate) fn read_stdin() -> Result<String, CliError> {
    std::io::read_to_string(std::io::stdin()).map_err(|cause| CliError::Operational {
        operation: "reading stdin",
        cause: Box::new(cause),
    })
}

/// Print one JSON document on stdout. Serialization of a document this
/// file built is an invariant, not a runtime condition, but it is still
/// reported rather than unwrapped.
pub(crate) fn print_document<T: serde::Serialize>(document: &T) -> Result<(), CliError> {
    let text = operational(serde_json::to_string_pretty(document), "rendering JSON")?;
    println!("{text}");
    Ok(())
}

/// The repository policy a cycle reads when there is no `relais.toml`.
pub(crate) fn default_repo_policy() -> Result<RepoPolicy, CliError> {
    RepoPolicy::from_toml_str(&format!(
        "schema_version = {}\n",
        relais::policy::POLICY_SCHEMA_VERSION
    ))
    .map_err(|cause| CliError::Operational {
        operation: "learn cycle",
        cause: Box::new(cause),
    })
}

/// The machine's hook-admission settings, or the stated defaults when
/// machine.toml is missing or will not parse. A hook cannot refuse to
/// answer over a settings problem any more than over anything else it
/// might hit — that failure belongs to `relais doctor`, not to a hook
/// answer holding a tool call open.
pub(crate) fn hook_admission_settings() -> relais::policy::HookAdmissionSettings {
    paths::machine_settings_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| MachineSettings::from_toml_str(&text).ok())
        .map(|machine| machine.admission)
        .unwrap_or_default()
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
pub(crate) fn pricing_preflight(
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
pub(crate) fn pricing_refusals(
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

/// A refusal of a decision: the exit outcome and the sentence that says
/// why. Printed to stderr, or as `{error, exit}` under `--json`.
#[derive(Debug)]
pub(crate) struct Refusal {
    pub(crate) outcome: CliOutcome,
    pub(crate) message: String,
}

pub(crate) fn refusal<T>(outcome: CliOutcome, message: impl Into<String>) -> Result<T, Refusal> {
    Err(Refusal {
        outcome,
        message: message.into(),
    })
}
