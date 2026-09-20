//! The shared per-user coordinator (SPEC §23).
//!
//! One coordinator per OS user, started lazily by the CLI, elected
//! atomically over a permission-restricted local endpoint (`crate::ipc`:
//! a Unix socket, or its loopback-and-nonce equivalent on Windows). It
//! serves the
//! admission state machine in `crate::admission`: registrations,
//! admission, resource leases and aggregate budget reservations. A CLI
//! process exiting never cancels an ongoing run; the coordinator holds no
//! database transaction across anything — callers write the ledger in
//! short transactions of their own.
//!
//! Coordinator outage never turns a strict managed launch into an
//! unmanaged one: `RemoteGate` reports the request as unadmitted and the
//! runner blocks, preserving the request (SPEC §23).

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::admission::{
    AdmissionState, BindOutcome, Decision, DispatchRequest, Gate, GateError, HeartbeatStatus,
    ResourceClass, ResumeOutcome, RunRegistration, Signal, StatusSnapshot,
};
use crate::ipc::{Listener, Stream};
use crate::ledger::Ledger;
use crate::policy::ConcurrencyLimits;
/// Liveness without a signal that could terminate anything (SPEC §23:
/// lease expiry never proves a worker died). Re-exported for `resume`.
pub use crate::procs::alive as process_alive;
use crate::procs::{kill, terminate, LockFile};

/// Defaults when machine settings name no limit. Illustrative sizing
/// (SPEC §23), not benchmark-derived.
pub const DEFAULT_LIMITS: ConcurrencyLimits = ConcurrencyLimits {
    max_active_agents: Some(6),
    max_active_agents_per_session: Some(3),
    max_heavy_commands: Some(2),
    max_training_jobs: Some(1),
    max_agent_depth: Some(3),
    max_agents_per_run: Some(24),
    training_when_idle: true,
};

/// A daemon with nothing registered for this long exits and removes its
/// socket; the next CLI call starts a fresh one.
pub const IDLE_EXIT: Duration = Duration::from_secs(600);
const RECONCILE_EVERY: Duration = Duration::from_secs(15);
/// Ceiling on one wire request. Every legitimate request is a short JSON
/// object; anything larger is a malformed or hostile client, and is
/// answered with an error rather than buffered.
const MAX_REQUEST_BYTES: u64 = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const START_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatorError(pub String);

impl std::fmt::Display for CoordinatorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "coordinator: {}", self.0)
    }
}

impl std::error::Error for CoordinatorError {}

/// Wire protocol: one JSON request per line, one JSON response per
/// line, one request per connection. Unknown methods are rejected so a
/// version skew between CLI and daemon is loud.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Ping,
    RegisterSession {
        session_id: String,
    },
    RegisterRun {
        registration: RunRegistration,
    },
    RequestAdmission {
        request: DispatchRequest,
    },
    Bind {
        dispatch_id: String,
        agent_id: Option<String>,
        pid: Option<u32>,
    },
    Heartbeat {
        dispatch_id: String,
    },
    /// The worker saw its cancellation on a heartbeat and is stopping:
    /// the seat and the reservation go now rather than at lease grace
    /// (C5). Without this on the wire, `acknowledge_cancel` existed and
    /// nothing could ever call it.
    AcknowledgeCancel {
        dispatch_id: String,
    },
    MarkWaiting {
        dispatch_id: String,
    },
    Resume {
        dispatch_id: String,
    },
    Release {
        dispatch_id: String,
    },
    /// `spent_micros: None` = unknown usage, which stays uncertain
    /// rather than becoming zero (SPEC §23).
    Settle {
        dispatch_id: String,
        spent_micros: Option<i64>,
    },
    Withdraw {
        dispatch_id: String,
    },
    /// The run reached an end state; nothing is cancelled or signalled.
    /// Until a run is finished (or cancelled) it keeps the daemon from
    /// idle-exiting under a run that is merely verifying.
    FinishRun {
        run_id: String,
    },
    CancelDispatch {
        dispatch_id: String,
    },
    CancelRun {
        run_id: String,
    },
    CancelSession {
        session_id: String,
    },
    Status,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    /// `known` = the coordinator had the dispatch or run. `detail` says
    /// why a call that was understood was nevertheless not applied — a
    /// refused bind, a refused resume — so the caller gets a reason and
    /// not just a false.
    Ok {
        known: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Error {
        detail: String,
    },
    Pong {
        pid: u32,
        version: String,
    },
    Decision {
        decision: Decision,
    },
    Heartbeat {
        status: HeartbeatStatus,
    },
    Cancelled {
        dispatches: Vec<String>,
    },
    Status {
        snapshot: StatusSnapshot,
    },
}

/// The elected coordinator's runtime state.
pub struct Coordinator {
    pub state: Arc<Mutex<AdmissionState>>,
    socket_path: PathBuf,
    lock_path: PathBuf,
    /// Held for the daemon's whole life. Dropping it — or dying, in any
    /// way at all — is what lets the next coordinator elect.
    lock: LockFile,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
}

/// Atomic election (SPEC §23): an OS lock on `coordinator.lock`, held
/// for the winner's lifetime. Simultaneous startups never create
/// independent schedulers, and a coordinator that dies — cleanly, by
/// panic, or by SIGKILL — releases the lock at the kernel, so the next
/// `relais run` elects without anybody deleting a file.
///
/// The PID inside the file is informational. It used to be the election
/// itself (`create_new` plus a liveness check on the content), and after
/// a SIGKILL that PID got recycled by some unrelated process: every
/// later election read a live PID, concluded a coordinator was serving,
/// and refused — for ever (C7). The kernel does not confuse a recycled
/// PID for a lock holder.
///
/// Returns the listener and the lock, which the winner must keep.
pub fn elect(socket_path: &Path) -> Result<(Listener, LockFile), CoordinatorError> {
    let state_dir = socket_path.parent().expect("socket has a parent");
    std::fs::create_dir_all(state_dir).map_err(|e| CoordinatorError(e.to_string()))?;
    let lock_path = state_dir.join("coordinator.lock");
    let mut lock = match LockFile::try_acquire(&lock_path) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            return Err(CoordinatorError(
                "another coordinator is already serving this user".into(),
            ))
        }
        Err(e) => {
            return Err(CoordinatorError(format!(
                "cannot take the coordinator lock at {}: {e}",
                lock_path.display()
            )))
        }
    };
    // Who holds it, for a human reading the state directory. A failure
    // here is not an election failure: the lock is already ours.
    let _ = lock.write_pid();
    finish_election(socket_path).map(|listener| (listener, lock))
}

fn finish_election(socket_path: &Path) -> Result<Listener, CoordinatorError> {
    if socket_path.exists() {
        // A leftover endpoint from a crashed coordinator: prove it is dead
        // by attempting a connect before removing it.
        if Stream::connect(socket_path).is_ok() {
            return Err(CoordinatorError(
                "a live coordinator answered on the socket; not taking over".into(),
            ));
        }
        std::fs::remove_file(socket_path).map_err(|e| CoordinatorError(e.to_string()))?;
    }
    // Permission-restricted (SPEC §23): the endpoint binds owner-only.
    Listener::bind(socket_path).map_err(|e| CoordinatorError(e.to_string()))
}

impl Coordinator {
    /// Elect, adopt whatever the ledger still records as live (SPEC §23:
    /// restart reconciles without duplicate live workers), and return
    /// the listener to serve.
    pub fn start(
        socket_path: &Path,
        limits: ConcurrencyLimits,
        ledger: Option<&Ledger>,
    ) -> Result<(Self, Listener), CoordinatorError> {
        let (listener, lock) = elect(socket_path)?;
        let lock_path = socket_path
            .parent()
            .expect("socket has a parent")
            .join("coordinator.lock");
        let mut state = AdmissionState::new(limits);
        if let Some(ledger) = ledger {
            let now = Instant::now();
            for (dispatch_id, run_id, pid) in ledger.live_dispatches().unwrap_or_default() {
                let Some(pid) = pid.and_then(|pid| u32::try_from(pid).ok()) else {
                    // A `launched` row with no PID is a dispatch the
                    // runner recorded before the process existed (SPEC
                    // §12). Adopting it took a seat for a worker that
                    // may never have started and that nothing can ever
                    // bind — three killed runs used to take half the
                    // seats at every start (C2). It is `relais resume`'s
                    // to reconcile against the ledger, not a live lease.
                    continue;
                };
                // A dead recorded process is not adopted: its ledger row
                // is the runner's to reconcile, not a live seat.
                if !process_alive(pid) {
                    continue;
                }
                let pid = Some(pid);
                state.adopt(
                    &DispatchRequest {
                        dispatch_id,
                        run_id,
                        session_id: "unknown".into(),
                        parent_dispatch: None,
                        depth: 0,
                        resource: ResourceClass::ModelWork,
                        reserve_micros: 0,
                    },
                    pid,
                    None,
                    now,
                );
            }
        }
        Ok((
            Self {
                state: Arc::new(Mutex::new(state)),
                socket_path: socket_path.to_path_buf(),
                lock_path,
                lock,
                shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
            listener,
        ))
    }

    /// Serve until shutdown or idle exit. A reconcile thread checks
    /// leases against the process table and signals cancelled workers;
    /// the accept loop handles one short request per connection.
    pub fn serve(self, listener: Listener) -> Result<(), CoordinatorError> {
        let reconcile_state = Arc::clone(&self.state);
        let reconcile_shutdown = Arc::clone(&self.shutdown);
        let socket_for_exit = self.socket_path.clone();
        let lock_for_exit = self.lock_path.clone();
        std::thread::spawn(move || {
            let mut idle_since: Option<Instant> = None;
            loop {
                std::thread::sleep(RECONCILE_EVERY);
                if reconcile_shutdown.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                let now = Instant::now();
                let (report, idle) = {
                    let mut state = lock_state(&reconcile_state);
                    (state.reconcile(now, &process_alive), state.is_idle())
                };
                // The state machine decided; sending is this side's job,
                // and each dispatch is signalled at most twice in its
                // life: once politely, once not (C5).
                for (_dispatch, pid, signal) in report.to_signal {
                    match signal {
                        Signal::Terminate => terminate(pid),
                        Signal::Kill => kill(pid),
                    }
                }
                match (idle, idle_since) {
                    (false, _) => idle_since = None,
                    (true, None) => idle_since = Some(now),
                    (true, Some(since)) if now.saturating_duration_since(since) >= IDLE_EXIT => {
                        // The endpoint goes first: a coordinator starting
                        // in this window finds no answer on the socket
                        // and waits for the lock, which exiting releases.
                        let _ = std::fs::remove_file(&socket_for_exit);
                        let _ = std::fs::remove_file(&lock_for_exit);
                        std::process::exit(0);
                    }
                    _ => {}
                }
            }
        });

        listener
            .set_nonblocking(false)
            .map_err(|e| CoordinatorError(e.to_string()))?;
        for stream in listener.incoming() {
            if self.shutdown.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            let Ok(stream) = stream else { continue };
            let state = Arc::clone(&self.state);
            let shutdown = Arc::clone(&self.shutdown);
            std::thread::spawn(move || {
                let _ = handle_connection(stream, &state, &shutdown);
            });
        }
        let _ = std::fs::remove_file(&self.socket_path);
        // Release the lock before unlinking the file it is held on:
        // Windows refuses to remove a file this process still has open,
        // and a released lock with the file gone is what the next
        // election expects on either platform.
        drop(self.lock);
        let _ = std::fs::remove_file(&self.lock_path);
        Ok(())
    }
}

/// One request thread that panicked must not wedge every later
/// connection: a poisoned admission mutex is recovered, not propagated.
/// The state machine mutates one field at a time and every write path is
/// total, so the worst a recovered lock carries is one half-applied
/// lifecycle event — against a daemon that answers nothing at all.
fn lock_state(state: &Mutex<AdmissionState>) -> std::sync::MutexGuard<'_, AdmissionState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn handle_connection(
    stream: Stream,
    state: &Mutex<AdmissionState>,
    shutdown: &std::sync::atomic::AtomicBool,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(REQUEST_TIMEOUT))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(REQUEST_TIMEOUT))
        .map_err(|e| e.to_string())?;
    // Capped read: a client that never sends a newline used to grow this
    // buffer without bound, one thread per connection.
    let mut reader =
        BufReader::new(stream.try_clone().map_err(|e| e.to_string())?).take(MAX_REQUEST_BYTES);
    let mut writer = stream;
    let mut line = String::new();
    let read = reader.read_line(&mut line).map_err(|e| e.to_string())?;
    if read == 0 {
        return Ok(());
    }
    let oversized = read as u64 == MAX_REQUEST_BYTES && !line.ends_with('\n');
    let response = if oversized {
        Response::Error {
            detail: format!("request exceeds {MAX_REQUEST_BYTES} bytes"),
        }
    } else {
        match serde_json::from_str::<Request>(line.trim()) {
            Ok(Request::Shutdown) => {
                shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
                ok(true)
            }
            Ok(request) => handle(request, &mut lock_state(state)),
            Err(e) => Response::Error {
                detail: format!("unparseable request: {e}"),
            },
        }
    };
    let payload = serde_json::to_string(&response).map_err(|e| e.to_string())?;
    writeln!(writer, "{payload}").map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())?;
    if oversized {
        // The answer is written, but the client is still sending the
        // rest of its over-long request. Dropping the stream now closes
        // a socket with unread data in its receive queue, which on
        // Windows is a RESET: the reply this thread just wrote is
        // discarded and the client sees "connection reset" instead of
        // the reason it was refused. So: half-close, read what is left
        // to a bound and throw it away, then let the stream drop. The
        // cap is not buffered — it is read into an 8 KiB scratch and
        // forgotten, which is what C4 is about.
        let _ = writer.shutdown_write();
        drain_and_discard(&writer);
    }
    Ok(())
}

/// Briefly read and throw away whatever the peer is still sending, so
/// the close that follows is a clean end of stream. Bounded twice —
/// by bytes and by a short timeout — because the peer may be hostile,
/// slow, or gone.
fn drain_and_discard(stream: &Stream) {
    const DRAIN_TIMEOUT: Duration = Duration::from_millis(250);
    const DRAIN_LIMIT: u64 = 4 * MAX_REQUEST_BYTES;
    let Ok(mut stream) = stream.try_clone() else {
        return;
    };
    if stream.set_read_timeout(Some(DRAIN_TIMEOUT)).is_err() {
        return;
    }
    let mut scratch = [0u8; 8 * 1024];
    let mut left = DRAIN_LIMIT;
    while left > 0 {
        match stream.read(&mut scratch) {
            Ok(0) | Err(_) => return,
            Ok(read) => left = left.saturating_sub(read as u64),
        }
    }
}

/// `Ok` with no further explanation: the call applied, or the
/// coordinator does not know the subject.
fn ok(known: bool) -> Response {
    Response::Ok {
        known,
        detail: None,
    }
}

/// `Ok` for a call that was understood and deliberately not applied.
fn ok_with(known: bool, detail: String) -> Response {
    Response::Ok {
        known,
        detail: Some(detail),
    }
}

/// Pure dispatch of one request against the state; the unit the socket
/// tests and the in-process tests share.
pub fn handle(request: Request, state: &mut AdmissionState) -> Response {
    let now = Instant::now();
    match request {
        Request::Ping => Response::Pong {
            pid: std::process::id(),
            version: crate::version().to_string(),
        },
        Request::RegisterSession { session_id } => {
            state.register_session(&session_id);
            ok(true)
        }
        Request::RegisterRun { registration } => {
            state.register_run(&registration);
            ok(true)
        }
        Request::RequestAdmission { request } => Response::Decision {
            decision: state.request(&request, now),
        },
        Request::Bind {
            dispatch_id,
            agent_id,
            pid,
        } => match state.bind(&dispatch_id, agent_id.as_deref(), pid, now, &process_alive) {
            BindOutcome::Bound => ok(true),
            BindOutcome::UnknownDispatch => ok(false),
            // C6: the PID comes off the wire. One that is not running
            // cannot be this client's worker, and once the OS reuses the
            // number it would be somebody else's process — which
            // reconcile would later terminate as a cancelled worker.
            BindOutcome::PidNotAlive => ok_with(
                false,
                format!(
                    "pid {} is not a live process; dispatch {dispatch_id} keeps no process binding",
                    pid.unwrap_or(0)
                ),
            ),
        },
        Request::Heartbeat { dispatch_id } => Response::Heartbeat {
            status: state.heartbeat(&dispatch_id, now),
        },
        Request::AcknowledgeCancel { dispatch_id } => {
            ok(state.acknowledge_cancel(&dispatch_id, now))
        }
        Request::MarkWaiting { dispatch_id } => {
            let marked = state.mark_waiting(&dispatch_id, now, &process_alive);
            if marked {
                ok(true)
            } else {
                // C8: waiting is a self-report. The least it has to be is
                // a live, bound process making it.
                ok_with(
                    false,
                    format!(
                        "dispatch {dispatch_id} is unknown, or has no live bound process to be \
                         waiting; the seat is not given back on an unverifiable claim"
                    ),
                )
            }
        }
        Request::Resume { dispatch_id } => match state.resume(&dispatch_id, now) {
            ResumeOutcome::Resumed => ok(true),
            ResumeOutcome::UnknownDispatch => ok(false),
            ResumeOutcome::OverAdmitted { over, max } => ok_with(
                false,
                format!(
                    "resuming {dispatch_id} would hold {over} seats beyond the class cap, past \
                     the configured maximum of {max}; it stays waiting and can poll again"
                ),
            ),
        },
        Request::Release { dispatch_id } => ok(state.release(&dispatch_id, now)),
        Request::Settle {
            dispatch_id,
            spent_micros,
        } => ok(state.settle(&dispatch_id, spent_micros, now)),
        Request::Withdraw { dispatch_id } => ok(state.withdraw(&dispatch_id, now)),
        Request::FinishRun { run_id } => ok(state.finish_run(&run_id)),
        Request::CancelDispatch { dispatch_id } => {
            let signalled = state.cancel_dispatch(&dispatch_id, now);
            for (_, pid) in &signalled {
                terminate(*pid);
            }
            Response::Cancelled {
                dispatches: signalled.into_iter().map(|(id, _)| id).collect(),
            }
        }
        Request::CancelRun { run_id } => {
            let signalled = state.cancel_run(&run_id, now);
            for (_, pid) in &signalled {
                terminate(*pid);
            }
            Response::Cancelled {
                dispatches: signalled.into_iter().map(|(id, _)| id).collect(),
            }
        }
        Request::CancelSession { session_id } => {
            let signalled = state.cancel_session(&session_id, now);
            for (_, pid) in &signalled {
                terminate(*pid);
            }
            Response::Cancelled {
                dispatches: signalled.into_iter().map(|(id, _)| id).collect(),
            }
        }
        Request::Status => Response::Status {
            snapshot: state.status(now),
        },
        Request::Shutdown => ok(true),
    }
}

/// The CLI-side client: connect, send one line, read one line, close.
pub struct Client {
    socket: PathBuf,
}

impl Client {
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            socket: socket_path,
        }
    }

    pub fn request(&self, request: &Request) -> Result<Response, CoordinatorError> {
        let mut stream =
            Stream::connect(&self.socket).map_err(|e| CoordinatorError(e.to_string()))?;
        stream
            .set_read_timeout(Some(REQUEST_TIMEOUT))
            .map_err(|e| CoordinatorError(e.to_string()))?;
        stream
            .set_write_timeout(Some(REQUEST_TIMEOUT))
            .map_err(|e| CoordinatorError(e.to_string()))?;
        let payload =
            serde_json::to_string(request).map_err(|e| CoordinatorError(e.to_string()))?;
        writeln!(stream, "{payload}").map_err(|e| CoordinatorError(e.to_string()))?;
        stream
            .flush()
            .map_err(|e| CoordinatorError(e.to_string()))?;
        let mut line = String::new();
        BufReader::new(stream)
            .read_line(&mut line)
            .map_err(|e| CoordinatorError(e.to_string()))?;
        match serde_json::from_str::<Response>(line.trim()) {
            Ok(Response::Error { detail }) => Err(CoordinatorError(detail)),
            Ok(response) => Ok(response),
            Err(e) => Err(CoordinatorError(format!(
                "unparseable coordinator response: {e}"
            ))),
        }
    }

    pub fn ping(&self) -> Result<u32, CoordinatorError> {
        match self.request(&Request::Ping)? {
            Response::Pong { pid, .. } => Ok(pid),
            other => Err(CoordinatorError(format!(
                "unexpected reply to ping: {other:?}"
            ))),
        }
    }

    pub fn status(&self) -> Result<StatusSnapshot, CoordinatorError> {
        match self.request(&Request::Status)? {
            Response::Status { snapshot } => Ok(snapshot),
            other => Err(CoordinatorError(format!(
                "unexpected reply to status: {other:?}"
            ))),
        }
    }
}

/// The managed gate: every admission call crosses the socket. Failure is
/// `GateError`, which the runner turns into `blocked:admission_unavailable`
/// — never into an unmanaged launch.
pub struct RemoteGate {
    client: Client,
}

impl RemoteGate {
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            client: Client::new(socket_path),
        }
    }

    fn call(&self, request: Request) -> Result<Response, GateError> {
        self.client
            .request(&request)
            .map_err(|e| GateError(e.to_string()))
    }
}

impl Gate for RemoteGate {
    fn register_run(&self, registration: &RunRegistration) -> Result<(), GateError> {
        self.call(Request::RegisterRun {
            registration: registration.clone(),
        })
        .map(|_| ())
    }

    fn admit(&self, request: &DispatchRequest) -> Result<Decision, GateError> {
        match self.call(Request::RequestAdmission {
            request: request.clone(),
        })? {
            Response::Decision { decision } => Ok(decision),
            other => Err(GateError(format!("unexpected admission reply: {other:?}"))),
        }
    }

    fn bind(
        &self,
        dispatch_id: &str,
        agent_id: Option<&str>,
        pid: Option<u32>,
    ) -> Result<(), GateError> {
        match self.call(Request::Bind {
            dispatch_id: dispatch_id.into(),
            agent_id: agent_id.map(str::to_string),
            pid,
        })? {
            // A bind the coordinator understood and refused (a PID it
            // cannot see running) is an error to the caller, not silence.
            Response::Ok {
                detail: Some(detail),
                ..
            } => Err(GateError(detail)),
            _ => Ok(()),
        }
    }

    fn heartbeat(&self, dispatch_id: &str) -> Result<HeartbeatStatus, GateError> {
        match self.call(Request::Heartbeat {
            dispatch_id: dispatch_id.into(),
        })? {
            Response::Heartbeat { status } => Ok(status),
            other => Err(GateError(format!("unexpected heartbeat reply: {other:?}"))),
        }
    }

    fn acknowledge_cancel(&self, dispatch_id: &str) -> Result<(), GateError> {
        self.call(Request::AcknowledgeCancel {
            dispatch_id: dispatch_id.into(),
        })
        .map(|_| ())
    }

    fn mark_waiting(&self, dispatch_id: &str) -> Result<(), GateError> {
        match self.call(Request::MarkWaiting {
            dispatch_id: dispatch_id.into(),
        })? {
            Response::Ok {
                detail: Some(detail),
                ..
            } => Err(GateError(detail)),
            _ => Ok(()),
        }
    }

    fn resume(&self, dispatch_id: &str) -> Result<(), GateError> {
        match self.call(Request::Resume {
            dispatch_id: dispatch_id.into(),
        })? {
            // Over the over-admission ceiling: the parent stays waiting
            // and the caller polls again (C8).
            Response::Ok {
                detail: Some(detail),
                ..
            } => Err(GateError(detail)),
            _ => Ok(()),
        }
    }

    fn release(&self, dispatch_id: &str) -> Result<(), GateError> {
        self.call(Request::Release {
            dispatch_id: dispatch_id.into(),
        })
        .map(|_| ())
    }

    fn settle(&self, dispatch_id: &str, spent_micros: Option<i64>) -> Result<(), GateError> {
        self.call(Request::Settle {
            dispatch_id: dispatch_id.into(),
            spent_micros,
        })
        .map(|_| ())
    }

    fn withdraw(&self, dispatch_id: &str) -> Result<(), GateError> {
        self.call(Request::Withdraw {
            dispatch_id: dispatch_id.into(),
        })
        .map(|_| ())
    }

    fn finish_run(&self, run_id: &str) -> Result<(), GateError> {
        self.call(Request::FinishRun {
            run_id: run_id.into(),
        })
        .map(|_| ())
    }

    fn enforcement(&self) -> &'static str {
        "managed (coordinator)"
    }
}

pub fn socket_path() -> PathBuf {
    crate::paths::state_dir().join("relais.sock")
}

/// The interactive session this CLI call belongs to. `RELAIS_SESSION_ID`
/// wins (the /relais skill sets it), then Claude Code's own session
/// variable when present; otherwise the parent PID names the tab and
/// the attribution is labelled as such rather than guessed.
pub fn session_id() -> String {
    if let Some(id) = std::env::var_os("RELAIS_SESSION_ID") {
        let id = id.to_string_lossy().trim().to_string();
        if !id.is_empty() {
            return id;
        }
    }
    if let Some(id) = std::env::var_os("CLAUDE_SESSION_ID") {
        let id = id.to_string_lossy().trim().to_string();
        if !id.is_empty() {
            return id;
        }
    }
    match crate::procs::parent_pid() {
        Some(parent) => format!("unattributed-ppid-{parent}"),
        None => format!("unattributed-pid-{}", std::process::id()),
    }
}

/// Connect to the user's coordinator, starting one lazily when none
/// answers. Starting spawns `relais coordinator daemon` detached from
/// this CLI process so its exit cannot take the coordinator down.
pub fn ensure_running(socket_path: &Path) -> Result<Client, CoordinatorError> {
    let client = Client::new(socket_path.to_path_buf());
    if client.ping().is_ok() {
        return Ok(client);
    }
    let exe = std::env::current_exe().map_err(|e| CoordinatorError(e.to_string()))?;
    let mut command = std::process::Command::new(exe);
    command
        .args(["coordinator", "daemon"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Detached from this CLI's group (Unix) or console group (Windows),
    // so the CLI exiting cannot take the coordinator down.
    crate::procs::own_process_group(&mut command);
    command
        .spawn()
        .map_err(|e| CoordinatorError(format!("cannot start the coordinator: {e}")))?;
    let started = Instant::now();
    while started.elapsed() < START_TIMEOUT {
        if client.ping().is_ok() {
            return Ok(client);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(CoordinatorError(
        "the coordinator did not answer within the start timeout".into(),
    ))
}

/// Foreground daemon entry used by `relais coordinator daemon`.
pub fn run_daemon(
    socket_path: &Path,
    limits: ConcurrencyLimits,
    ledger: Option<&Ledger>,
) -> Result<(), CoordinatorError> {
    let (coordinator, listener) = Coordinator::start(socket_path, limits, ledger)?;
    coordinator.serve(listener)
}

/// Machine limits with the defaults filled in where nothing is set.
pub fn effective_limits(configured: &ConcurrencyLimits) -> ConcurrencyLimits {
    ConcurrencyLimits {
        max_active_agents: configured
            .max_active_agents
            .or(DEFAULT_LIMITS.max_active_agents),
        max_active_agents_per_session: configured
            .max_active_agents_per_session
            .or(DEFAULT_LIMITS.max_active_agents_per_session),
        max_heavy_commands: configured
            .max_heavy_commands
            .or(DEFAULT_LIMITS.max_heavy_commands),
        max_training_jobs: configured
            .max_training_jobs
            .or(DEFAULT_LIMITS.max_training_jobs),
        max_agent_depth: configured
            .max_agent_depth
            .or(DEFAULT_LIMITS.max_agent_depth),
        max_agents_per_run: configured
            .max_agents_per_run
            .or(DEFAULT_LIMITS.max_agents_per_run),
        training_when_idle: configured.training_when_idle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_dir(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // Unix socket paths are short (104 bytes on macOS): /tmp, not the
        // deep per-user temp dir. Windows has no /tmp and no such limit.
        let base = if cfg!(unix) {
            PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        let dir = base.join(format!(
            "rl-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn limits() -> ConcurrencyLimits {
        ConcurrencyLimits {
            max_active_agents: Some(2),
            max_active_agents_per_session: Some(1),
            max_heavy_commands: Some(1),
            max_training_jobs: Some(1),
            max_agent_depth: Some(3),
            max_agents_per_run: Some(24),
            training_when_idle: false,
        }
    }

    /// Elect, allowing for the window in which a process another test
    /// spawned still holds an inherited copy of a released lock
    /// descriptor (see `procs::LockFile`). Production answers the same
    /// window by retrying: `ensure_running` pings until the daemon
    /// answers.
    fn elect_within(
        socket: &Path,
        patience: Duration,
    ) -> Result<(Listener, LockFile), CoordinatorError> {
        let deadline = Instant::now() + patience;
        loop {
            match elect(socket) {
                Ok(won) => return Ok(won),
                Err(e) if Instant::now() >= deadline => return Err(e),
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }

    fn registration(run: &str, session: &str) -> RunRegistration {
        RunRegistration {
            run_id: run.into(),
            session_id: session.into(),
            budget_micros: None,
            max_agents: None,
            max_depth: None,
        }
    }

    fn request(dispatch: &str, run: &str, session: &str) -> DispatchRequest {
        DispatchRequest {
            dispatch_id: dispatch.into(),
            run_id: run.into(),
            session_id: session.into(),
            parent_dispatch: None,
            depth: 0,
            resource: ResourceClass::ModelWork,
            reserve_micros: 0,
        }
    }

    #[test]
    fn election_is_atomic_and_socket_is_owner_only() {
        let dir = temp_dir("elect");
        let socket = dir.join("relais.sock");
        let lock_path = dir.join("coordinator.lock");
        let (listener, lock) = elect(&socket).expect("first coordinator wins");
        assert!(
            elect(&socket).is_err(),
            "simultaneous startup does not double-elect"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&socket).expect("socket").permissions();
            assert_eq!(mode.mode() & 0o777, 0o600, "permission-restricted");
        }
        assert!(
            socket.exists(),
            "the endpoint is at the path on every platform"
        );
        assert_eq!(
            std::fs::read_to_string(&lock_path).expect("lock").trim(),
            std::process::id().to_string(),
            "the holder is named in the file, for a human"
        );
        drop(listener);
        drop(lock);
        let (listener, lock) =
            elect_within(&socket, Duration::from_secs(5)).expect("a released lock is taken");
        drop(listener);
        drop(lock);
        std::fs::remove_dir_all(&dir).ok();
    }

    // C7: the wedge. A lock file left behind by a SIGKILLed coordinator,
    // whose PID content has since been recycled by an unrelated LIVE
    // process, used to refuse every election until somebody deleted the
    // file by hand. The kernel knows nobody holds the lock.
    #[test]
    fn a_lock_file_with_a_live_foreign_pid_and_no_holder_is_taken_over() {
        let dir = temp_dir("wedge");
        let socket = dir.join("relais.sock");
        let lock_path = dir.join("coordinator.lock");
        // This very process: unquestionably alive, unquestionably not a
        // coordinator serving this socket.
        std::fs::write(&lock_path, std::process::id().to_string()).expect("stale lock");
        // And a leftover endpoint nobody answers, from the same death.
        std::fs::write(&socket, "leftover").expect("stale endpoint");
        let (listener, lock) =
            elect_within(&socket, Duration::from_secs(5)).expect("no holder: the lock is free");
        assert!(socket.exists(), "the stale endpoint was replaced, not kept");
        assert_eq!(
            std::fs::read_to_string(&lock_path).expect("lock").trim(),
            std::process::id().to_string()
        );
        assert!(
            elect(&socket).is_err(),
            "and now it really is held: nobody else elects"
        );
        drop(listener);
        drop(lock);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn client_daemon_round_trip_and_shutdown() {
        let dir = temp_dir("rt");
        let socket = dir.join("relais.sock");
        let (coordinator, listener) = Coordinator::start(&socket, limits(), None).expect("start");
        let state = Arc::clone(&coordinator.state);
        let server = std::thread::spawn(move || coordinator.serve(listener));
        let client = Client::new(socket.clone());
        assert_eq!(client.ping().expect("ping"), std::process::id());

        let gate = RemoteGate::new(socket.clone());
        gate.register_run(&registration("run-1", "tab-a"))
            .expect("register");
        assert_eq!(
            gate.admit(&request("d1", "run-1", "tab-a")).expect("admit"),
            Decision::Granted
        );
        assert!(matches!(
            gate.admit(&request("d2", "run-1", "tab-a")).expect("admit"),
            Decision::Queued { .. }
        ));
        gate.bind("d1", Some("agent-1"), Some(std::process::id()))
            .expect("bind");
        assert!(gate.heartbeat("d1").expect("heartbeat").known);
        gate.mark_waiting("d1").expect("waiting");
        assert_eq!(
            gate.admit(&request("d2", "run-1", "tab-a"))
                .expect("drained"),
            Decision::Granted
        );
        gate.resume("d1").expect("resume");
        gate.release("d2").expect("release");
        gate.settle("d2", None).expect("settle");
        let snapshot = client.status().expect("status");
        assert_eq!(snapshot.runs["run-1"].uncertain_settlements, 1);
        assert_eq!(snapshot.sessions, vec!["tab-a".to_string()]);
        assert_eq!(gate.enforcement(), "managed (coordinator)");

        // An unknown method is a loud error, not a silent no-op.
        let mut stream = Stream::connect(&socket).expect("connect");
        writeln!(stream, r#"{{"method":"bogus"}}"#).expect("write");
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).expect("read");
        assert!(line.contains("unparseable request"));

        // C4: a request with no newline in sight is answered with an
        // error at the cap instead of growing the daemon's memory.
        let stream = Stream::connect(&socket).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("timeout");
        let mut writer = stream.try_clone().expect("clone");
        let flood = std::thread::spawn(move || {
            // The server stops reading at the cap: the tail of this
            // write may or may not land, which is the point.
            let _ = writer.write_all(&vec![b'a'; MAX_REQUEST_BYTES as usize + 64]);
        });
        let mut line = String::new();
        // The server answers at the cap, half-closes and drains the rest
        // rather than resetting the connection, so the answer survives
        // the flood on every platform. A read that fails anyway is a
        // transport verdict, not a buffered daemon — assert the thing
        // C4 is about, and say which happened.
        match BufReader::new(stream).read_line(&mut line) {
            Ok(_) => assert!(line.contains("exceeds"), "{line}"),
            // Windows resets a connection whose receive queue still holds
            // bytes at close, and a flooding client can always leave one
            // more packet in flight than the drain waited for. A reset
            // there still proves the daemon refused at the cap instead of
            // buffering; anything else, on any platform, is a failure.
            Err(e) if cfg!(windows) && e.kind() == std::io::ErrorKind::ConnectionReset => {}
            Err(e) => panic!("the refusal was lost to the transport: {e}"),
        }
        let _ = flood.join();

        // Shutdown removes the socket and lock; a state snapshot taken
        // through the shared handle still reflects the served run.
        assert!(matches!(
            client.request(&Request::Shutdown).expect("shutdown"),
            Response::Ok { known: true, .. }
        ));
        // The accept loop needs one more connection to observe the flag.
        let _ = Client::new(socket.clone()).ping();
        server.join().expect("server thread").expect("serve");
        assert!(!socket.exists());
        assert!(!dir.join("coordinator.lock").exists());
        assert_eq!(
            state.lock().expect("lock").status(Instant::now()).runs["run-1"].admitted_total,
            2
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // C9: every other "simultaneous" test is sequential calls on the
    // state machine. This one drives the real socket from twelve
    // threads at once — the shape SPEC §23 actually describes — and
    // asserts the caps hold under a genuine race, not a script.
    #[test]
    fn twelve_threads_racing_the_socket_see_exactly_the_cap_granted() {
        const THREADS: usize = 12;
        let dir = temp_dir("race");
        let socket = dir.join("relais.sock");
        let limits = ConcurrencyLimits {
            max_active_agents: Some(4),
            // Per-session and per-run caps out of the way: the global cap
            // is the one under test.
            max_active_agents_per_session: Some(THREADS as u32),
            max_heavy_commands: Some(1),
            max_training_jobs: Some(1),
            max_agent_depth: Some(3),
            max_agents_per_run: Some(64),
            training_when_idle: false,
        };
        let (coordinator, listener) = Coordinator::start(&socket, limits, None).expect("start");
        let server = std::thread::spawn(move || coordinator.serve(listener));
        let client = Client::new(socket.clone());
        client
            .request(&Request::RegisterRun {
                registration: registration("run-race", "tab-race"),
            })
            .expect("register");

        // Everybody blocks on the same gate, then asks at once.
        let start = Arc::new(std::sync::Barrier::new(THREADS));
        let workers: Vec<_> = (0..THREADS)
            .map(|n| {
                let socket = socket.clone();
                let start = Arc::clone(&start);
                std::thread::spawn(move || {
                    let gate = RemoteGate::new(socket);
                    start.wait();
                    gate.admit(&request(&format!("d-{n}"), "run-race", "tab-race"))
                        .expect("admit")
                })
            })
            .collect();
        let decisions: Vec<Decision> = workers
            .into_iter()
            .map(|worker| worker.join().expect("worker"))
            .collect();
        let granted = decisions
            .iter()
            .filter(|decision| **decision == Decision::Granted)
            .count();
        let queued = decisions
            .iter()
            .filter(|decision| matches!(decision, Decision::Queued { .. }))
            .count();
        assert_eq!(granted, 4, "exactly the cap was granted: {decisions:?}");
        assert_eq!(queued, THREADS - 4, "the rest queued: {decisions:?}");
        let snapshot = client.status().expect("status");
        assert_eq!(snapshot.active_by_class["model_work"], 4);
        assert_eq!(snapshot.queued as usize, THREADS - 4);
        assert_eq!(snapshot.over_admitted, 0);

        // Drain the whole queue by releasing what is admitted, four at a
        // time, and polling with the same IDs. Nothing is granted twice.
        let gate = RemoteGate::new(socket.clone());
        let mut served: std::collections::BTreeSet<String> = (0..THREADS)
            .zip(decisions.iter())
            .filter(|(_, decision)| **decision == Decision::Granted)
            .map(|(n, _)| format!("d-{n}"))
            .collect();
        let mut holding: Vec<String> = served.iter().cloned().collect();
        let mut rounds = 0;
        while served.len() < THREADS {
            rounds += 1;
            assert!(rounds <= THREADS, "the queue drains");
            for id in holding.drain(..) {
                gate.release(&id).expect("release");
                gate.settle(&id, Some(0)).expect("settle");
            }
            for n in 0..THREADS {
                let id = format!("d-{n}");
                if served.contains(&id) {
                    continue;
                }
                if gate
                    .admit(&request(&id, "run-race", "tab-race"))
                    .expect("poll")
                    == Decision::Granted
                {
                    served.insert(id.clone());
                    holding.push(id);
                }
            }
        }
        let snapshot = client.status().expect("status");
        assert_eq!(snapshot.queued, 0, "every request was served");
        assert_eq!(snapshot.runs["run-race"].admitted_total, THREADS as u32);

        let _ = client.request(&Request::Shutdown);
        let _ = Client::new(socket.clone()).ping();
        server.join().expect("server thread").expect("serve");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn restart_adopts_only_live_ledger_dispatches() {
        let dir = temp_dir("adopt");
        let socket = dir.join("relais.sock");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        ledger.insert_run("run-x", "/repo", None).expect("run");
        ledger
            .record_dispatch_intent("live", "run-x", None, &serde_json::json!({}), 0)
            .expect("intent");
        ledger
            .attach_dispatch_process("live", Some(std::process::id()), Some("sess"))
            .expect("attach");
        ledger
            .record_dispatch_intent("dead", "run-x", None, &serde_json::json!({}), 0)
            .expect("intent");
        ledger
            .attach_dispatch_process("dead", Some(999_999_999), None)
            .expect("attach");
        // C2: `launched`, no PID — the row managed_launch writes BEFORE
        // the process exists. Adopting it took a seat nothing could ever
        // bind or free; `relais resume` reconciles it against the ledger.
        ledger
            .record_dispatch_intent("pidless", "run-x", None, &serde_json::json!({}), 0)
            .expect("intent");
        ledger
            .attach_dispatch_process("pidless", None, Some("sess"))
            .expect("attach");
        ledger
            .record_dispatch_intent("finished", "run-x", None, &serde_json::json!({}), 0)
            .expect("intent");
        ledger
            .finish_dispatch("finished", "completed")
            .expect("finish");
        let (coordinator, listener) =
            Coordinator::start(&socket, limits(), Some(&ledger)).expect("start");
        drop(listener);
        let state = coordinator.state.lock().expect("lock");
        let snapshot = state.status(Instant::now());
        assert_eq!(
            snapshot.active_by_class.get("model_work"),
            Some(&1),
            "only the live, bound dispatch is adopted"
        );
        assert_eq!(snapshot.runs["run-x"].admitted_total, 1);
        drop(state);
        std::fs::remove_file(&socket).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    // C3/C4: one request thread panicking must not wedge every later
    // connection — a poisoned admission lock is recovered, not rethrown.
    #[test]
    fn a_poisoned_admission_lock_does_not_wedge_the_daemon() {
        let state = Mutex::new(AdmissionState::new(limits()));
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = state.lock().expect("lock");
            panic!("a request thread died holding the admission lock");
        }));
        std::panic::set_hook(hook);
        assert!(died.is_err());
        assert!(state.is_poisoned());

        let mut recovered = lock_state(&state);
        recovered.register_run(&registration("run-1", "tab-a"));
        assert!(
            !recovered.is_idle(),
            "the state machine keeps serving after a poisoned lock"
        );
    }

    #[test]
    fn remote_gate_reports_an_absent_coordinator_as_unavailable() {
        let gate = RemoteGate::new(PathBuf::from("/tmp/relais-no-such-socket.sock"));
        let err = gate
            .admit(&request("d", "r", "s"))
            .expect_err("no coordinator");
        assert!(err.to_string().starts_with("admission unavailable"));
    }

    #[test]
    fn effective_limits_fill_defaults_only_where_unset() {
        let configured = ConcurrencyLimits {
            max_active_agents: Some(1),
            ..ConcurrencyLimits::default()
        };
        let effective = effective_limits(&configured);
        assert_eq!(effective.max_active_agents, Some(1));
        assert_eq!(effective.max_agents_per_run, Some(24));
        assert!(!effective.training_when_idle);
    }

    #[test]
    fn session_id_is_env_or_labelled_unattributed() {
        let id = session_id();
        assert!(!id.is_empty());
        if std::env::var_os("RELAIS_SESSION_ID").is_none()
            && std::env::var_os("CLAUDE_SESSION_ID").is_none()
        {
            assert!(id.starts_with("unattributed-ppid-"));
        }
    }
}
