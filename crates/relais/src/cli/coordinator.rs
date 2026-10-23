//! `relais coordinator` and `relais quiet`.

use super::ctx::open_ledger;
use super::ctx::print_document;
use super::native::await_cancel;
use super::native::cancel_standing;
use super::native::CancelStanding;
use super::outcome::CliError;
use super::outcome::CliOutcome;
use super::CoordinatorCommand;
use super::QuietCommand;
use relais::ids::RunId;
use relais::ledger::Ledger;
use relais::paths;
use relais::policy::HookAdmissionSettings;
use relais::policy::MachineSettings;
use std::path::Path;
use std::time::Duration;
/// `coordinator status --json`, in every state. A typed document, never a
/// human sentence on stdout: a caller parsing this had to tell "no daemon"
/// from a parse failure, and the client's error was thrown away
/// altogether (C3).
#[derive(Debug, serde::Serialize)]
#[serde(tag = "coordinator", rename_all = "snake_case")]
pub(crate) enum CoordinatorStatusDocument {
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

pub(crate) fn coordinator_status_command(
    socket: &Path,
    json: bool,
) -> Result<CliOutcome, CliError> {
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

/// `relais quiet watch --protocol`: the watcher ends when its reader goes
/// away, which is how it is meant to stop, so that is success.
pub(crate) fn quiet_command(cmd: QuietCommand) -> Result<CliOutcome, CliError> {
    let QuietCommand::Watch { .. } = cmd;
    let Some(dir) = relais::quiet::active_dir(|name| std::env::var_os(name)) else {
        return Err(CliError::Usage {
            detail: "quiet watch: set QUIET_ACTIVE_DIR (no home or state directory found)".into(),
        });
    };
    let _ = relais::quiet::watch(&dir, &mut std::io::stdout());
    Ok(CliOutcome::Accepted)
}

pub(crate) fn coordinator_command(cmd: CoordinatorCommand) -> Result<CliOutcome, CliError> {
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
                        coordinator::log_line(&format!(
                            "{} is invalid: {e}; refusing to serve with \
                             default limits in place of the ones it states",
                            path.display()
                        ));
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
                    coordinator::log_line(&format!(
                        "the ledger at {} is unreadable ({e}); serving \
                         without it means no dispatch is adopted at election",
                        path.display()
                    ));
                    None
                }
                Err(e) => {
                    coordinator::log_line(&format!(
                        "the ledger path is unavailable ({e}); serving \
                         without it means no dispatch is adopted at election"
                    ));
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
                    coordinator::log_line(&e.to_string());
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
pub(crate) fn finish_workerless_cancel(run_id: &str) -> Result<CliOutcome, CliError> {
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

pub(crate) fn opt(value: Option<u32>) -> String {
    value.map_or_else(|| "unlimited".into(), |value| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
