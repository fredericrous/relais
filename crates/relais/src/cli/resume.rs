//! `relais resume` and the retirement of runs.

use super::ctx::open_ledger;
use super::ctx::project_dir;
use super::outcome::operational;
use super::outcome::CliError;
use super::outcome::CliOutcome;
use super::status::print_pending_intents;
use relais::admission::Gate;
use relais::ids::RunId;
use relais::ledger::Ledger;
use relais::paths;
use relais::resume;
use relais::runner::Reason;
use relais::runner::State;
use relais::workspace;
use std::path::Path;
use std::path::PathBuf;

/// Run this same `relais resume` again with its output appended to `log`
/// (a timestamp line first), and report whether it succeeded. A detached
/// sweep has no terminal to write to.
pub(crate) fn resume_logged(log: &Path) -> Result<CliOutcome, CliError> {
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

/// Whether `resume` also retires the worktree once the run is terminal
/// and its dispatches provably dead. An enum, so the handler reads as
/// what was asked rather than as a bare flag (the clap edge is where the
/// `bool` lives).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Retire {
    Keep,
    Retire,
}

impl Retire {
    pub(crate) fn from_flag(retire: bool) -> Self {
        if retire {
            Self::Retire
        } else {
            Self::Keep
        }
    }
}

pub(crate) fn resume_command(run_id: &str, retire: Retire) -> Result<CliOutcome, CliError> {
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
pub(crate) fn record_resume_interruption(
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
pub(crate) fn orphan_of_run(
    ledger: &Ledger,
    run: &RunId,
) -> Result<Option<resume::Orphan>, CliError> {
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
pub(crate) enum OrphanOutcome {
    /// The run is no longer running; this is the state it is in now (the
    /// interrupted state this call wrote, or the one a racing process did).
    Ended(State),
    /// A dispatch of the run is provably alive: nothing was changed.
    KeptAlive(String),
}

/// Move an orphaned run to `interrupted` through the library's reap (a
/// compare-and-set that closes its dispatches and ends its events only when
/// it wins), and print what it returns.
pub(crate) fn interrupt_orphan(
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
pub(crate) fn run_live_dispatches(
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
pub(crate) fn reconcile_run(
    ledger: &Ledger,
    run: &RunId,
) -> Result<resume::Reconciliation, CliError> {
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
pub(crate) fn retire_all_command() -> Result<CliOutcome, CliError> {
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
pub(crate) fn settle_state(
    ledger: &Ledger,
    run: &RunId,
    path: &Path,
) -> Result<Option<State>, CliError> {
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
pub(crate) struct InPlaceScan {
    runs: Vec<(RunId, PathBuf)>,
    unreadable: usize,
}

pub(crate) fn in_place_leftovers() -> InPlaceScan {
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
pub(crate) enum CheckoutKind {
    Task,
    Verification,
}

/// Every retained task worktree and verification checkout, in that order.
pub(crate) fn retained_with_verification(
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

pub(crate) fn retire_any(
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
pub(crate) fn retire_verification_checkout(
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
pub(crate) fn retire_in_place(
    ledger: &Ledger,
    run: &RunId,
    state: State,
    reconciliation: &resume::Reconciliation,
) -> Result<Option<CliOutcome>, CliError> {
    let runs_dir = paths::runs_dir().map_err(CliError::Home)?;
    retire_in_place_saying(ledger, run, state, reconciliation, &runs_dir, &mut |line| {
        println!("{line}");
    })
}

/// [`retire_in_place`] with its report lines handed to `say`, so `relais
/// decide` can fold them into its own answer instead of printing them.
pub(crate) fn retire_in_place_saying(
    ledger: &Ledger,
    run: &RunId,
    state: State,
    reconciliation: &resume::Reconciliation,
    runs_dir: &Path,
    say: &mut dyn FnMut(String),
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
        say(format!(
            "{run}: kept, a dispatch may still write {} ({})",
            root.display(),
            reconciliation.detail()
        ));
        return Ok(Some(CliOutcome::OperationalFailure));
    }
    let lease = match Lease::reclaim(&root, run.as_str()) {
        Ok(lease) => lease,
        Err(LeaseRefusal::Busy { holder }) => {
            say(format!(
                "{run}: kept, run {} holds {}",
                holder.as_deref().unwrap_or("?"),
                root.display()
            ));
            return Ok(Some(CliOutcome::OperationalFailure));
        }
        Err(LeaseRefusal::Interrupted { .. } | LeaseRefusal::UnreadableMarker { .. }) => {
            return Ok(None)
        }
        Err(LeaseRefusal::Io(e)) => {
            say(format!(
                "{run}: kept, {} could not be taken: {e}",
                root.display()
            ));
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
    let artifacts = runs_dir.join(run.as_str());
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
                say(format!(
                    "{run}: restored to {} in {} (directory kept)",
                    inplace::short(&marker.start),
                    root.display()
                ));
                if matches!(index, inplace::IndexOutcome::Changed(_)) {
                    say(format!(
                    "{run}: your git index changed during the run; the original is kept at {}/relais-inplace/{run}.index.orig",
                    git_dir.display()
                ));
                }
                CliOutcome::Accepted
            }
            Ok(Outcome::HeadMoved { was, now }) => {
                say(format!(
                "{run}: kept, HEAD moved from {} to {} during the run; the tree and the branch were not touched, review them yourself",
                inplace::short(&was),
                inplace::short(&now)
            ));
                CliOutcome::OperationalFailure
            }
            Err(e) => {
                say(format!("{run}: kept, {e}"));
                CliOutcome::OperationalFailure
            }
        },
    ))
}

/// Retire every worktree one terminal run still holds.
pub(crate) fn retire_run(
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
pub(crate) enum Retired {
    Retired,
    /// Not removed: a dispatch may still write it, the ledger lacks what
    /// the retirement needs, or the retirement itself failed (recorded
    /// as `worktree_not_released`).
    Kept,
}

/// One worktree of a terminal run: retired once its dispatches are
/// provably dead, with the transition the runner would have recorded,
/// and one line printed either way.
pub(crate) fn retire_worktree(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::tests::temp_ledger;

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
}
