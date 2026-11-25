//! Plumbing every command shares: where the process is, what it reads,
//! and the handles it opens.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use relais::backend::Harness;
use relais::contract::TaskContract;
use relais::ids::{IdSource, RunId, TaskId};
use relais::ledger::Ledger;
use relais::orchestration::PriceTable;
use relais::policy::{MachineSettings, RepoPolicy};
use relais::shared_state::node::NodeError;
use relais::shared_state::{SharedState, SharedStateError};
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

/// The machine's settings, through the shared-state implementation
/// `machine.toml` selects (`LocalState`: the file, read as it always was).
pub(crate) fn load_machine() -> Result<MachineSettings, CliError> {
    machine_of(&*shared_state()?)
}

/// The machine's settings through a state the caller already built: a
/// command that builds one (`relais run`) reads through it, never a second.
pub(crate) fn machine_of(state: &dyn SharedState) -> Result<MachineSettings, CliError> {
    state
        .machine_settings()
        .map_err(|e| shared_state_error("the machine settings", e))
}

/// Write the machine's settings through the shared-state implementation the
/// machine selects (an-t20), as the person at this terminal: the CLI is the
/// operator principal's own hand (an-t12). The outer error is the state
/// that could not be built (`external` with no client, exit 3); the inner
/// is the write's own, which each caller reports as it always did.
pub(crate) fn write_settings(
    edit: &relais::shared_state::SettingsEdit,
) -> Result<Result<relais::shared_state::SettingsWrite, SharedStateError>, CliError> {
    let who = std::env::var("USER")
        .ok()
        .filter(|user| !user.is_empty())
        .unwrap_or_else(|| "operator".to_string());
    Ok(shared_state()?.write_settings(&relais::shared_state::Principal::operator(who), edit))
}

/// The closed shared-state client this build links. Public relais links
/// none; a private build (an-t13) replaces this one line with its factory.
pub(crate) const EXTERNAL_CLIENT: Option<relais::shared_state::ExternalFactory> = None;

/// The shared-state implementation the machine selects, built here at the
/// composition root and nowhere else; refused when it names an external
/// layer this build does not have.
pub(crate) fn shared_state() -> Result<Arc<dyn SharedState>, CliError> {
    relais::shared_state::current_with(EXTERNAL_CLIENT)
        .map_err(|e| shared_state_error("shared state", e))
}

/// The shared state the hook's claim passes go through. A hook never fails
/// a session on bookkeeping: when the machine names an external layer this
/// build does not have, the passes run on the claim files alone
/// (`LocalState`) and each claim event says so in one stderr line.
pub(crate) fn shared_state_for_hook() -> relais::hook::respond::SharedClaims {
    match relais::shared_state::current_with(EXTERNAL_CLIENT) {
        Ok(state) => relais::hook::respond::SharedClaims {
            state,
            warning: None,
        },
        Err(e) => relais::hook::respond::SharedClaims {
            state: Arc::new(relais::shared_state::LocalState::new()),
            // One line, whatever the error's own text holds.
            warning: Some(format!(
                "relais: shared claims skipped, the local claims were updated: {}",
                e.to_string()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            )),
        },
    }
}

/// A shared-state failure as the CLI reports it. The local errors keep the
/// variants they had before the trait existed, so the lines and exit codes
/// a single node prints do not change.
pub(crate) fn shared_state_error(what: &'static str, e: SharedStateError) -> CliError {
    match e {
        SharedStateError::ExternalAbsent { machine_toml } => {
            CliError::SharedStateAbsent { machine_toml }
        }
        SharedStateError::Home(e) => CliError::Home(e),
        SharedStateError::Read { path, cause } => CliError::Read { what, path, cause },
        SharedStateError::Invalid { path, cause } => CliError::Invalid { what, path, cause },
        other => CliError::Operational {
            operation: what,
            cause: Box::new(other),
        },
    }
}

/// A node-half failure as the CLI reports it: the shared half's own error
/// goes through [`shared_state_error`] unchanged, the node's ledger, claim,
/// registry and lock errors are operational failures of `what`.
pub(crate) fn node_error(what: &'static str, e: NodeError) -> CliError {
    match e {
        NodeError::Shared(e) => shared_state_error(what, e),
        e @ (NodeError::Lock { .. }
        | NodeError::Ledger(_)
        | NodeError::Claim(_)
        | NodeError::Registry(_)
        | NodeError::Compensation { .. }) => CliError::Operational {
            operation: what,
            cause: Box::new(e),
        },
    }
}

/// The bounds a candidate recipe is admitted under: the incumbent's own,
/// with effort catalogs resolved as routing resolves them — from the
/// harness probe and machine.toml. A machine.toml that is absent or invalid
/// states no effort fact, and a harness that does not answer accepts none:
/// both leave the catalogs unknown, which admits only the incumbent's
/// configured efforts, never more.
pub(crate) fn tuning_bounds(incumbent: &RepoPolicy) -> route::TuningBounds {
    let settings = shared_state()
        .ok()
        .and_then(|state| state.machine_settings().ok())
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
    price_table_of(&*shared_state()?)
}

/// [`load_price_table`] through a state the caller already built.
pub(crate) fn price_table_of(state: &dyn SharedState) -> Result<PriceTable, CliError> {
    Ok(machine_pricing_of(state)?.unwrap_or_else(PriceTable::empty))
}

/// The machine's `[pricing]` table; `None` when it has none, so a cost can
/// say that no table is the reason it is unknown.
pub(crate) fn load_machine_pricing() -> Result<Option<PriceTable>, CliError> {
    machine_pricing_of(&*shared_state()?)
}

/// [`load_machine_pricing`] through a state the caller already built.
fn machine_pricing_of(state: &dyn SharedState) -> Result<Option<PriceTable>, CliError> {
    // Absent is the defaults, which carry no `[pricing]` table.
    Ok(machine_of(state)?.pricing)
}

/// The machine's settings for the session router's native commands: a
/// failure is a sentence, which each command says in its own way.
pub(crate) fn machine_for_router() -> Result<MachineSettings, String> {
    shared_state()
        .map_err(|e| e.to_string())?
        .machine_settings()
        .map_err(|e| e.to_string())
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
    hook_admission_settings_from(&shared_state_for_hook().state)
}

/// [`hook_admission_settings`] read through `state`: the hook reads them
/// through the state it already built for its claim passes. A client's
/// answer is waited for at most `SHARED_CLAIM_DEADLINE`, on a thread of
/// its own, then the defaults apply: a hook holds no tool call open on a
/// primary that does not answer.
pub(crate) fn hook_admission_settings_from(
    state: &Arc<dyn SharedState>,
) -> relais::policy::HookAdmissionSettings {
    relais::shared_state::node::within(
        state,
        relais::hook::respond::SHARED_CLAIM_DEADLINE,
        |state| {
            state
                .machine_settings()
                .map(|machine| machine.admission)
                .unwrap_or_default()
        },
    )
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

/// The lines for the watched home files that changed while `run` was
/// active (SPEC §8), for `status` and `explain` (stdout); empty when none
/// did. A record that cannot be read is named, not skipped.
pub(crate) fn home_drift_lines(ledger: &Ledger, run: &RunId) -> Vec<String> {
    home_drift_report(ledger, run, relais::homeprint::STATUS_HEADING)
}

/// The same lines for the end of `relais run` (stderr), under a heading
/// that names the command and the run.
pub(crate) fn home_drift_warning_lines(ledger: &Ledger, run: &RunId) -> Vec<String> {
    let (drifts, problems) = relais::runner::drifts_of_run(ledger, run);
    let home = relais::homeprint::print_home();
    home_drift_warning_report(&drifts, problems, run, home.as_deref())
}

/// What `relais run` prints on stderr: every line, a record it could not
/// read included, starts with `relais run: warning: `.
fn home_drift_warning_report(
    drifts: &[relais::homeprint::Drift],
    problems: Vec<String>,
    run: &RunId,
    home: Option<&std::path::Path>,
) -> Vec<String> {
    const PREFIX: &str = "relais run: warning: ";
    let mut lines: Vec<String> = problems
        .into_iter()
        .map(|problem| format!("{PREFIX}{problem}"))
        .collect();
    lines.extend(relais::homeprint::report(
        drifts,
        &format!(
            "{PREFIX}home files changed while {run} was active:",
            run = run.as_str()
        ),
        home,
    ));
    lines
}

fn home_drift_report(ledger: &Ledger, run: &RunId, heading: &str) -> Vec<String> {
    let (drifts, mut lines) = relais::runner::drifts_of_run(ledger, run);
    let home = relais::homeprint::print_home();
    lines.extend(relais::homeprint::report(&drifts, heading, home.as_deref()));
    lines
}

#[cfg(test)]
mod home_drift_warning_tests {
    use super::*;

    #[test]
    fn an_unreadable_record_line_carries_the_run_prefix() {
        let run = RunId::from_stored("run_x");
        let lines = home_drift_warning_report(
            &[],
            vec!["home drift record /a/b unreadable: gone".to_string()],
            &run,
            None,
        );
        assert_eq!(
            lines,
            vec!["relais run: warning: home drift record /a/b unreadable: gone".to_string()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_stored_absolute_path_under_home_prints_with_a_tilde_at_run_end() {
        let run = RunId::from_stored("run_x");
        let drifts = [relais::homeprint::Drift {
            path: "/home/u/.gitconfig".to_string(),
            change: relais::homeprint::Change::Modified,
        }];
        let lines = home_drift_warning_report(
            &drifts,
            Vec::new(),
            &run,
            Some(std::path::Path::new("/home/u")),
        );
        assert!(
            lines.iter().any(|line| line.contains("~/.gitconfig")),
            "{lines:?}"
        );
        assert!(
            !lines.iter().any(|line| line.contains("/home/u/")),
            "{lines:?}"
        );
    }

    /// `node_error` keeps every line and exit class the CLI printed when
    /// these errors were one `SharedStateError`: the shared half's own go
    /// through `shared_state_error`, the node's are `Operational` under the
    /// caller's own `what`.
    #[test]
    fn a_node_error_prints_and_exits_as_the_shared_state_error_did() {
        use relais::shared_state::SharedStateError as S;
        let io = || std::io::Error::other("disk");
        let cases: Vec<(NodeError, String, CliOutcome)> = vec![
            (
                NodeError::Shared(S::ExternalAbsent {
                    machine_toml: "/m.toml".into(),
                }),
                "machine.toml /m.toml sets shared_state = \"external\", but this relais build \
                 has no shared-state client; set shared_state = \"local\" or install the \
                 build that has one"
                    .to_string(),
                CliOutcome::Blocked,
            ),
            (
                NodeError::Shared(S::Home(relais::paths::HomeUnset)),
                relais::paths::HomeUnset.to_string(),
                CliOutcome::Blocked,
            ),
            (
                NodeError::Shared(S::Read {
                    path: "/m.toml".into(),
                    cause: io(),
                }),
                "cannot read claiming /m.toml: disk".to_string(),
                CliOutcome::InvalidInput,
            ),
            (
                NodeError::Shared(S::Invalid {
                    path: "/m.toml".into(),
                    cause: "bad key".into(),
                }),
                "claiming /m.toml is invalid: bad key".to_string(),
                CliOutcome::InvalidInput,
            ),
            (
                NodeError::Claim(relais::programme::claim::ClaimError::LockTimeout {
                    path: "/p/.lock".into(),
                }),
                format!(
                    "claiming: {}",
                    relais::programme::claim::ClaimError::LockTimeout {
                        path: "/p/.lock".into()
                    }
                ),
                CliOutcome::OperationalFailure,
            ),
            (
                NodeError::Registry(relais::learn::registry::ArtifactError::Malformed(
                    "no json".into(),
                )),
                format!(
                    "claiming: {}",
                    relais::learn::registry::ArtifactError::Malformed("no json".into())
                ),
                CliOutcome::OperationalFailure,
            ),
            (
                NodeError::Shared(S::CasConflict {
                    expected: None,
                    found: Some("a".into()),
                }),
                "claiming: cas_conflict: expected nothing, found a".to_string(),
                CliOutcome::OperationalFailure,
            ),
            (
                NodeError::Lock {
                    path: "/p/.pointer.lock".into(),
                    cause: io(),
                },
                "claiming: cannot lock /p/.pointer.lock: disk".to_string(),
                CliOutcome::OperationalFailure,
            ),
            (
                NodeError::Ledger(relais::ledger::LedgerError::Io(io())),
                format!(
                    "claiming: {}",
                    relais::ledger::LedgerError::Io(io())
                ),
                CliOutcome::OperationalFailure,
            ),
            (
                NodeError::Compensation {
                    local: Box::new(NodeError::Lock {
                        path: "/p/.pointer.lock".into(),
                        cause: io(),
                    }),
                    release: Box::new(NodeError::Shared(S::Refused {
                        reason: "gone".into(),
                    })),
                },
                "claiming: cannot lock /p/.pointer.lock: disk (and the shared hold could not be given back: refused: gone)".to_string(),
                CliOutcome::OperationalFailure,
            ),
        ];
        for (error, line, outcome) in cases {
            let cli = node_error("claiming", error);
            assert_eq!(cli.to_string(), line);
            assert_eq!(cli.outcome(), outcome, "{cli}");
        }
        // The shared half's own errors keep their own `CliError` variants.
        assert!(matches!(
            node_error(
                "claiming",
                NodeError::Shared(S::Read {
                    path: "/m.toml".into(),
                    cause: io()
                })
            ),
            CliError::Read { .. }
        ));
        assert!(matches!(
            node_error(
                "claiming",
                NodeError::Shared(S::Invalid {
                    path: "/m.toml".into(),
                    cause: "bad key".into()
                })
            ),
            CliError::Invalid { .. }
        ));
        assert!(matches!(
            node_error(
                "claiming",
                NodeError::Shared(S::Home(relais::paths::HomeUnset))
            ),
            CliError::Home(_)
        ));
    }
}
