//! The shared per-user coordinator (SPEC §23).
//!
//! One coordinator per OS user, started lazily by the CLI, elected
//! atomically over a permission-restricted Unix socket. It serves the
//! admission state machine in `crate::admission`: registrations,
//! admission, resource leases and aggregate budget reservations. A CLI
//! process exiting never cancels an ongoing run; the coordinator holds no
//! database transaction across anything — callers write the ledger in
//! short transactions of their own.
//!
//! Coordinator outage never turns a strict managed launch into an
//! unmanaged one: `RemoteGate` reports the request as unadmitted and the
//! runner blocks, preserving the request (SPEC §23).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::admission::{
    AdmissionState, Decision, DispatchRequest, Gate, GateError, HeartbeatStatus, ResourceClass,
    RunRegistration, StatusSnapshot,
};
use crate::ledger::Ledger;
use crate::policy::ConcurrencyLimits;

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
    Ok { known: bool },
    Error { detail: String },
    Pong { pid: u32, version: String },
    Decision { decision: Decision },
    Heartbeat { status: HeartbeatStatus },
    Cancelled { dispatches: Vec<String> },
    Status { snapshot: StatusSnapshot },
}

/// The elected coordinator's runtime state.
pub struct Coordinator {
    pub state: Arc<Mutex<AdmissionState>>,
    socket_path: PathBuf,
    lock_path: PathBuf,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
}

/// Atomic election (SPEC §23): an exclusive lock file carrying the
/// winner's PID. A stale lock (dead PID) is taken over; simultaneous
/// startups never create independent schedulers. Returns the listener
/// and the lock path the winner must remove on exit.
pub fn elect(socket_path: &Path) -> Result<(UnixListener, PathBuf), CoordinatorError> {
    let state_dir = socket_path.parent().expect("socket has a parent");
    std::fs::create_dir_all(state_dir).map_err(|e| CoordinatorError(e.to_string()))?;
    let lock_path = state_dir.join("coordinator.lock");
    let pid = std::process::id().to_string();
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
    {
        Ok(mut file) => {
            file.write_all(pid.as_bytes())
                .map_err(|e| CoordinatorError(e.to_string()))?;
            finish_election(socket_path).map(|listener| (listener, lock_path))
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let held = std::fs::read_to_string(&lock_path)
                .ok()
                .and_then(|text| text.trim().parse::<u32>().ok());
            match held {
                Some(held_pid) if held_pid != std::process::id() && !process_alive(held_pid) => {
                    std::fs::write(&lock_path, &pid)
                        .map_err(|e| CoordinatorError(e.to_string()))?;
                    finish_election(socket_path).map(|listener| (listener, lock_path))
                }
                Some(_) => Err(CoordinatorError(
                    "another coordinator is already serving this user".into(),
                )),
                None => Err(CoordinatorError(
                    "lock file exists but is unreadable; refusing to double-elect".into(),
                )),
            }
        }
        Err(e) => Err(CoordinatorError(e.to_string())),
    }
}

fn finish_election(socket_path: &Path) -> Result<UnixListener, CoordinatorError> {
    if socket_path.exists() {
        // A leftover socket from a crashed coordinator: prove it is dead
        // by attempting a connect before removing it.
        if UnixStream::connect(socket_path).is_ok() {
            return Err(CoordinatorError(
                "a live coordinator answered on the socket; not taking over".into(),
            ));
        }
        std::fs::remove_file(socket_path).map_err(|e| CoordinatorError(e.to_string()))?;
    }
    let listener = UnixListener::bind(socket_path).map_err(|e| CoordinatorError(e.to_string()))?;
    // Permission-restricted local socket (SPEC §23): the owner only.
    let mut permissions = std::fs::metadata(socket_path)
        .map_err(|e| CoordinatorError(e.to_string()))?
        .permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(socket_path, permissions)
        .map_err(|e| CoordinatorError(e.to_string()))?;
    Ok(listener)
}

/// `kill(pid, 0)`: liveness without a signal that could terminate
/// anything. Lease expiry never proves a worker died (SPEC §23).
pub fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: signal 0 performs error checking only; no signal is sent.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn terminate(pid: u32) {
    if pid == 0 {
        return;
    }
    // SAFETY: SIGTERM to a process this user owns; a cancelled dispatch
    // bound its own PID to the reservation.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
}

impl Coordinator {
    /// Elect, adopt whatever the ledger still records as live (SPEC §23:
    /// restart reconciles without duplicate live workers), and return
    /// the listener to serve.
    pub fn start(
        socket_path: &Path,
        limits: ConcurrencyLimits,
        ledger: Option<&Ledger>,
    ) -> Result<(Self, UnixListener), CoordinatorError> {
        let (listener, lock_path) = elect(socket_path)?;
        let mut state = AdmissionState::new(limits);
        if let Some(ledger) = ledger {
            let now = Instant::now();
            for (dispatch_id, run_id, pid) in ledger.live_dispatches().unwrap_or_default() {
                let pid = pid.and_then(|pid| u32::try_from(pid).ok());
                // A dead recorded process is not adopted: its ledger row
                // is the runner's to reconcile, not a live seat.
                if pid.is_some_and(|pid| !process_alive(pid)) {
                    continue;
                }
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
                shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
            listener,
        ))
    }

    /// Serve until shutdown or idle exit. A reconcile thread checks
    /// leases against the process table and signals cancelled workers;
    /// the accept loop handles one short request per connection.
    pub fn serve(self, listener: UnixListener) -> Result<(), CoordinatorError> {
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
                    let mut state = reconcile_state.lock().expect("admission lock");
                    (state.reconcile(now, &process_alive), state.is_idle())
                };
                for (_dispatch, pid) in report.to_signal {
                    terminate(pid);
                }
                match (idle, idle_since) {
                    (false, _) => idle_since = None,
                    (true, None) => idle_since = Some(now),
                    (true, Some(since)) if now.saturating_duration_since(since) >= IDLE_EXIT => {
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
        let _ = std::fs::remove_file(&self.lock_path);
        Ok(())
    }
}

fn handle_connection(
    stream: UnixStream,
    state: &Mutex<AdmissionState>,
    shutdown: &std::sync::atomic::AtomicBool,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(REQUEST_TIMEOUT))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(REQUEST_TIMEOUT))
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut writer = stream;
    let mut line = String::new();
    if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
        return Ok(());
    }
    let response = match serde_json::from_str::<Request>(line.trim()) {
        Ok(Request::Shutdown) => {
            shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
            Response::Ok { known: true }
        }
        Ok(request) => handle(request, &mut state.lock().expect("admission lock")),
        Err(e) => Response::Error {
            detail: format!("unparseable request: {e}"),
        },
    };
    let payload = serde_json::to_string(&response).map_err(|e| e.to_string())?;
    writeln!(writer, "{payload}").map_err(|e| e.to_string())?;
    Ok(())
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
            Response::Ok { known: true }
        }
        Request::RegisterRun { registration } => {
            state.register_run(&registration);
            Response::Ok { known: true }
        }
        Request::RequestAdmission { request } => Response::Decision {
            decision: state.request(&request, now),
        },
        Request::Bind {
            dispatch_id,
            agent_id,
            pid,
        } => Response::Ok {
            known: state.bind(&dispatch_id, agent_id.as_deref(), pid),
        },
        Request::Heartbeat { dispatch_id } => Response::Heartbeat {
            status: state.heartbeat(&dispatch_id, now),
        },
        Request::MarkWaiting { dispatch_id } => Response::Ok {
            known: state.mark_waiting(&dispatch_id, now),
        },
        Request::Resume { dispatch_id } => Response::Ok {
            known: state.resume(&dispatch_id, now),
        },
        Request::Release { dispatch_id } => Response::Ok {
            known: state.release(&dispatch_id, now),
        },
        Request::Settle {
            dispatch_id,
            spent_micros,
        } => Response::Ok {
            known: state.settle(&dispatch_id, spent_micros, now),
        },
        Request::Withdraw { dispatch_id } => Response::Ok {
            known: state.withdraw(&dispatch_id, now),
        },
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
        Request::Shutdown => Response::Ok { known: true },
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
            UnixStream::connect(&self.socket).map_err(|e| CoordinatorError(e.to_string()))?;
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
        self.call(Request::Bind {
            dispatch_id: dispatch_id.into(),
            agent_id: agent_id.map(str::to_string),
            pid,
        })
        .map(|_| ())
    }

    fn heartbeat(&self, dispatch_id: &str) -> Result<HeartbeatStatus, GateError> {
        match self.call(Request::Heartbeat {
            dispatch_id: dispatch_id.into(),
        })? {
            Response::Heartbeat { status } => Ok(status),
            other => Err(GateError(format!("unexpected heartbeat reply: {other:?}"))),
        }
    }

    fn mark_waiting(&self, dispatch_id: &str) -> Result<(), GateError> {
        self.call(Request::MarkWaiting {
            dispatch_id: dispatch_id.into(),
        })
        .map(|_| ())
    }

    fn resume(&self, dispatch_id: &str) -> Result<(), GateError> {
        self.call(Request::Resume {
            dispatch_id: dispatch_id.into(),
        })
        .map(|_| ())
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
    // SAFETY: getppid has no failure mode and touches no memory.
    let parent = unsafe { libc::getppid() };
    format!("unattributed-ppid-{parent}")
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
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
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
        // deep per-user temp dir.
        let dir = PathBuf::from("/tmp").join(format!(
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
        let (listener, lock) = elect(&socket).expect("first coordinator wins");
        assert!(
            elect(&socket).is_err(),
            "simultaneous startup does not double-elect"
        );
        let mode = std::fs::metadata(&socket).expect("socket").permissions();
        assert_eq!(mode.mode() & 0o777, 0o600, "permission-restricted");
        drop(listener);
        // A lock naming a dead PID is taken over; a leftover socket that
        // nobody answers is removed on the way.
        std::fs::write(&lock, "999999999").expect("stale lock");
        let (listener, _) = elect(&socket).expect("stale lock is taken over");
        drop(listener);
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
        let mut stream = UnixStream::connect(&socket).expect("connect");
        writeln!(stream, r#"{{"method":"bogus"}}"#).expect("write");
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).expect("read");
        assert!(line.contains("unparseable request"));

        // Shutdown removes the socket and lock; a state snapshot taken
        // through the shared handle still reflects the served run.
        assert!(matches!(
            client.request(&Request::Shutdown).expect("shutdown"),
            Response::Ok { known: true }
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
        assert_eq!(snapshot.active_by_class.get("model_work"), Some(&1));
        assert_eq!(snapshot.runs["run-x"].admitted_total, 1);
        drop(state);
        std::fs::remove_file(&socket).ok();
        std::fs::remove_dir_all(&dir).ok();
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
