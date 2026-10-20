//! The native subcommands.

use super::status::{intent_json, listed_intents};
use crate::*;

/// Where a run stands when a cancel arrives.
pub(crate) enum CancelStanding {
    /// The ledger has no such run.
    Unknown,
    /// Nothing is left to cancel: the line to print (already ended, or
    /// reaped by this call because its driver is gone).
    Settled(String),
    /// A live driver; `session` is the session that started the run.
    Live { session: Option<String> },
}

/// How long `native cancel` waits for a live driver to end its run.
pub(crate) const CANCEL_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Judge a run for cancelling: an ended run is reported as it is, one whose
/// driver is gone or silent is reaped as cancelled (any session may clear
/// it), and any other is live.
pub(crate) fn cancel_standing(ledger: &Ledger, run: &RunId) -> Result<CancelStanding, CliError> {
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
pub(crate) fn reap_if_driverless(
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
pub(crate) fn await_cancel(ledger: &Ledger, run: &RunId) -> Result<String, CliError> {
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
pub(crate) fn native_cancel_command(
    run_id: &str,
    session: Option<&str>,
) -> Result<CliOutcome, CliError> {
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

/// `relais native contract`: the model writes a task as JSON, the plugin
/// pipes it here. Refused, by its schema error, unless it is a contract —
/// so a sentence of prose never becomes a run. Saved as
/// `<root>/.relais/tasks/<hash>.json`, under the `.relais/` the dirty-tree
/// check already leaves alone, with a `.gitignore` that keeps it out of
/// `git status`.
pub(crate) fn native_contract_command() -> Result<CliOutcome, CliError> {
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

/// `relais native …`: what the relais plugin tells the coordinator, and
/// the run timeline it reads back (SPEC §23, §29). Exit 0 acknowledges.
pub(crate) fn native_command(cmd: NativeCommand) -> Result<CliOutcome, CliError> {
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
pub(crate) fn router_state_command(session: &str) -> CliOutcome {
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
pub(crate) fn gate_stamp_command(repo: &std::path::Path) -> CliOutcome {
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
pub(crate) fn review_outcome_labels(
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
pub(crate) fn review_outcome_preview(
    ledger: Option<PathBuf>,
    plan_root: Option<PathBuf>,
) -> CliOutcome {
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
pub(crate) fn review_outcome_ingest(
    ledger: Option<PathBuf>,
    plan_root: Option<PathBuf>,
) -> CliOutcome {
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
pub(crate) fn router_observe_command() -> CliOutcome {
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
pub(crate) fn router_envelope_command(
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
pub(crate) fn router_off_command(
    by: &str,
    source: relais::router::Source,
) -> Result<CliOutcome, CliError> {
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
pub(crate) fn native_usage_command(dispatch: &str) -> Result<CliOutcome, CliError> {
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

pub(crate) fn native_gate() -> Result<relais::coordinator::RemoteGate, CliError> {
    let socket = relais::coordinator::socket_path().map_err(CliError::Home)?;
    Ok(relais::coordinator::RemoteGate::new(socket))
}

pub(crate) fn acknowledged(
    what: &str,
    answer: Result<(), GateError>,
) -> Result<CliOutcome, CliError> {
    match answer {
        Ok(()) => Ok(CliOutcome::Accepted),
        Err(e) => {
            eprintln!("relais native {what}: {e}");
            Ok(CliOutcome::OperationalFailure)
        }
    }
}

pub(crate) fn read_stopped_report(payload: &str) -> Result<StoppedReport, serde_json::Error> {
    serde_json::from_str(payload)
}

/// The most runs `relais native status --session` lists.
pub(crate) const SESSION_RUNS_LIMIT: usize = 20;

/// The stall threshold in force: [`resume::STALLED_AFTER`], unless
/// `RELAIS_STALLED_AFTER_SECS` names another (the seam tests inject through).
pub(crate) fn stalled_after() -> std::time::Duration {
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
pub(crate) fn native_status_command(
    run: Option<&str>,
    session: Option<&str>,
) -> Result<CliOutcome, CliError> {
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
pub(crate) fn native_status_of_session(
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
pub(crate) enum StatusAccess {
    /// The ledger is writable: an orphan is reaped.
    Reaping,
    /// The ledger is not: an orphan is only reported (`orphaned: true`).
    ReadOnly,
}

pub(crate) fn open_status_ledger() -> Result<(Ledger, StatusAccess), CurrentUnknown> {
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
pub(crate) enum CurrentUnknown {
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
pub(crate) fn current_of_run(
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
pub(crate) fn reap_for_status(
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
