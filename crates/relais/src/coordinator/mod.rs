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
    AdmissionState, AgentSettleOutcome, Attribution, BindOutcome, Decision, DispatchRequest,
    DispatchSource, Enforcement, Gate, GateError, HeartbeatStatus, LifecycleOutcome, PendingSignal,
    Provenance, ReleaseWriteOutcome, ResourceClass, ResumeOutcome, RunRegistration, Signal,
    StatusSnapshot, WaitOutcome, WithdrawOutcome, WriteLeaseOutcome,
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
/// `pub(crate)`: the hook's own admission wait derives its handler
/// timeout from this (`install::settings::derived_pretooluse_timeout`),
/// so the number in force lives here once rather than being copied.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const START_TIMEOUT: Duration = Duration::from_secs(5);
/// How many connections are served at once. Every request is short and
/// capped by `REQUEST_TIMEOUT`, so a small pool is enough; what it
/// prevents is one thread per connection, unjoined, for as many
/// connections as anybody cares to open (A9).
const HANDLER_THREADS: usize = 8;
/// Connections accepted and waiting for a handler. Past this the accept
/// loop blocks, which leaves the backlog where the kernel can bound it.
const ACCEPT_BACKLOG: usize = 64;
/// How often the accept loop wakes to check the shutdown flag when
/// nothing is connecting.
const ACCEPT_POLL: Duration = Duration::from_millis(50);
/// How long to wait out a descriptor exhaustion before trying again, and
/// how many times before giving up and letting the next CLI call elect a
/// fresh daemon.
const DESCRIPTOR_BACKOFF: Duration = Duration::from_millis(250);
const DESCRIPTOR_RETRIES: u32 = 20;

/// The wire protocol this build speaks, independent of the crate
/// version: bumped whenever a request or a response changes shape.
///
/// A daemon left running from an older install answers perfectly well
/// and means something different by the same words — v1 said
/// `{"kind":"ok","known":false}` for "I have never heard of that
/// dispatch", which a v1 client read as success. A `Pong` carries this
/// number so the skew is named at the first call instead of becoming a
/// worker with no seat (A2).
///
/// v3 added `bind_agent_lease` and `settle_by_agent`: a hook-admitted
/// agent's seat is bound on its `PostToolUse` and given back on its
/// `SubagentStop`, and a v2 daemon has neither call.
pub const PROTOCOL_VERSION: u32 = 3;

/// Why a coordinator call, election or startup failed. Every variant
/// names the operation and the entity it was about, so a caller can tell
/// an unreachable daemon from one that answered and refused.
#[derive(Debug)]
pub enum CoordinatorError {
    /// The endpoint could not be reached or bound: no daemon, a stale
    /// socket, a path that cannot be created.
    Connect {
        socket: PathBuf,
        cause: std::io::Error,
    },
    /// An I/O failure on an established connection, or while serving.
    Io {
        operation: &'static str,
        cause: std::io::Error,
    },
    /// The bytes on the wire were not something this version speaks.
    Protocol {
        operation: &'static str,
        detail: String,
    },
    /// The daemon on the other end speaks a different wire protocol.
    /// Stopping it (`relais coordinator stop`) is the whole fix.
    VersionSkew {
        socket: PathBuf,
        ours: u32,
        theirs: u32,
        daemon_version: String,
    },
    /// The coordinator answered, and refused.
    Rejected { detail: String },
    /// The coordinator is on its way out and applied nothing. The caller
    /// retries, which starts a fresh daemon (A11).
    ShuttingDown { socket: PathBuf },
    /// Another coordinator holds the election lock, or the lock could
    /// not be taken at all.
    Election { detail: String },
    /// The set of dispatches the ledger records as live could not be
    /// read. A coordinator that cannot see what is already running must
    /// not start granting seats: every seat would be handed out twice
    /// and every reservation lost (A1).
    LiveSetUnreadable { cause: crate::ledger::LedgerError },
}

impl std::fmt::Display for CoordinatorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect { socket, cause } => {
                write!(f, "coordinator: endpoint {}: {cause}", socket.display())
            }
            Self::Io { operation, cause } => write!(f, "coordinator: {operation}: {cause}"),
            Self::Protocol { operation, detail } => {
                write!(f, "coordinator: {operation}: {detail}")
            }
            Self::VersionSkew {
                socket,
                ours,
                theirs,
                daemon_version,
            } => write!(
                f,
                "coordinator: the daemon on {} (relais {daemon_version}) speaks wire protocol \
                 {theirs} and this relais speaks {ours}; stop it with `relais coordinator stop` \
                 and the next command starts one that matches",
                socket.display()
            ),
            Self::Rejected { detail } => write!(f, "coordinator: {detail}"),
            Self::ShuttingDown { socket } => write!(
                f,
                "coordinator: the daemon on {} is shutting down and applied nothing; retry",
                socket.display()
            ),
            Self::Election { detail } => write!(f, "coordinator: election: {detail}"),
            Self::LiveSetUnreadable { cause } => write!(
                f,
                "coordinator: the ledger's live dispatches could not be read ({cause}); a \
                 coordinator that cannot see what is running will not start"
            ),
        }
    }
}

impl std::error::Error for CoordinatorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Connect { cause, .. } | Self::Io { cause, .. } => Some(cause),
            Self::LiveSetUnreadable { cause } => Some(cause),
            Self::Protocol { .. }
            | Self::VersionSkew { .. }
            | Self::Rejected { .. }
            | Self::ShuttingDown { .. }
            | Self::Election { .. } => None,
        }
    }
}

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
    /// Bind a hook-admitted agent to its dispatch on a lease, with no
    /// process id: the hook path reports the agent a spawn launched
    /// (`PostToolUse`'s `tool_response.agentId`) and never a pid.
    BindAgentLease {
        dispatch_id: String,
        agent_id: String,
        provenance: Provenance,
    },
    /// An agent ended (`SubagentStop`): settle and release whatever
    /// dispatch is bound to it. Named by session and agent because that
    /// payload names no tool call to derive a dispatch id from.
    /// `spent_micros: None` = unknown usage, as for `Settle`.
    SettleByAgent {
        session_id: String,
        agent_id: String,
        spent_micros: Option<i64>,
    },
    /// The run reached an end state; nothing is cancelled or signalled.
    /// Until a run is finished (or cancelled) it keeps the daemon from
    /// idle-exiting under a run that is merely verifying.
    FinishRun {
        run_id: String,
    },
    /// Exclusive write access to a worktree for a dispatch about to write
    /// it (SPEC §23). Refused, with the holder named, when another
    /// dispatch holds it.
    AcquireWrite {
        dispatch_id: String,
        worktree: String,
    },
    /// The dispatch stopped writing; only the holder's release counts.
    ReleaseWrite {
        dispatch_id: String,
        worktree: String,
    },
    /// Who is writing a worktree, for verification to wait on.
    WriteLeaseHolder {
        worktree: String,
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

/// What a call names, when the coordinator has never heard of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Entity {
    Dispatch,
    Run,
}

/// A call the coordinator understood and deliberately did not apply.
/// Each variant carries what the caller needs to decide what to do next
/// — retry, stay waiting, or stop — instead of a sentence to match on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "refusal", rename_all = "snake_case")]
pub enum Refused {
    /// C6: the PID came off the wire and is not a live process, so
    /// nothing was bound to the lease.
    PidNotAlive { pid: Option<u32>, detail: String },
    /// C8: waiting is a self-report, and this one has no live bound
    /// process behind it. The seat is not given back.
    UnverifiableWait { detail: String },
    /// C8: resuming would hold more seats beyond the cap than the
    /// ceiling allows. The parent stays waiting and can poll again.
    OverAdmitted { over: u32, max: u32 },
    /// The dispatch has already ended; there is nothing to relinquish.
    DispatchEnded,
    /// SPEC §23: another dispatch is writing that worktree.
    WorktreeHeld { worktree: String, holder: String },
    /// Only a lease's holder can release it.
    NotTheLeaseHolder { worktree: String },
    /// The caller already received `Granted` for this dispatch: its
    /// launch may be in flight, so the request cannot be abandoned.
    AlreadyClaimed,
    /// A lease was offered for a dispatch already bound to a live
    /// process; a lease carries no pid to signal or check, so it is not
    /// applied over one.
    AlreadyBoundToProcess { detail: String },
}

impl Refused {
    fn describe(&self) -> String {
        match self {
            Self::PidNotAlive { detail, .. }
            | Self::UnverifiableWait { detail }
            | Self::AlreadyBoundToProcess { detail } => detail.clone(),
            Self::OverAdmitted { over, max } => format!(
                "resuming would hold {over} seats beyond the class cap, past the configured \
                 maximum of {max}; it stays waiting and can poll again"
            ),
            Self::DispatchEnded => {
                "the dispatch has already ended; there is nothing to relinquish".to_string()
            }
            Self::WorktreeHeld { worktree, holder } => {
                format!("worktree {worktree} is being written by dispatch {holder}")
            }
            Self::NotTheLeaseHolder { worktree } => {
                format!("this dispatch does not hold the write lease on {worktree}")
            }
            Self::AlreadyClaimed => {
                "the dispatch was already granted to a caller; its launch may be in flight"
                    .to_string()
            }
        }
    }
}

/// One answer per request, and every outcome a handler can produce has
/// its own variant.
///
/// v1 had a single `Ok { known: bool, detail: Option<String> }`, and
/// every caller matched it with a `_ =>` that read "I have never heard
/// of that dispatch" as success. A worker could bind after a
/// re-election, get `known: false`, and run with no seat, no reservation
/// and no PID on record (A2). The union below has no such arm: a caller
/// that ignores an outcome fails to compile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    /// The call applied.
    Ack,
    /// The coordinator does not know the dispatch or run named. Never
    /// success: after a re-election this is what a live worker's own
    /// lease looks like.
    Unknown {
        entity: Entity,
        id: String,
    },
    /// Understood, and deliberately not applied.
    Refused {
        refusal: Refused,
    },
    /// The request itself could not be served — unparseable, too long,
    /// a method this daemon does not have.
    Error {
        detail: String,
    },
    /// The daemon is on its way out and applied nothing (A11). The
    /// caller retries and starts a fresh one.
    ShuttingDown,
    Pong {
        pid: u32,
        version: String,
        /// The wire protocol this daemon speaks (`PROTOCOL_VERSION`).
        /// Absent from a v1 daemon, which is exactly what makes it
        /// detectable.
        #[serde(default)]
        protocol: u32,
    },
    Decision {
        decision: Decision,
    },
    /// What ending an agent did — including that nothing was bound to
    /// it, which is an ordinary answer and not `Unknown`.
    AgentSettled {
        outcome: AgentSettleOutcome,
    },
    Heartbeat {
        status: HeartbeatStatus,
    },
    WriteLease {
        holder: Option<String>,
    },
    Cancelled {
        dispatches: Vec<String>,
    },
    Status {
        snapshot: Box<StatusSnapshot>,
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
    // `--socket /` is the only path with no directory to hold it: a
    // refusal, not a panic, because the path is operator input.
    let state_dir = socket_path
        .parent()
        .ok_or_else(|| CoordinatorError::Connect {
            socket: socket_path.to_path_buf(),
            cause: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the socket path names no directory to hold the coordinator's state",
            ),
        })?;
    std::fs::create_dir_all(state_dir).map_err(|cause| CoordinatorError::Connect {
        socket: socket_path.to_path_buf(),
        cause,
    })?;
    // The endpoint's directory is the second half of the permission
    // restriction SPEC §23 asks for: a 0600 socket inside a
    // world-readable directory is still a name everybody can see and a
    // path somebody else could replace. Owner-only, verified.
    crate::ipc::restrict_directory(state_dir).map_err(|cause| CoordinatorError::Io {
        operation: "restricting the coordinator state directory",
        cause,
    })?;
    let lock_path = state_dir.join("coordinator.lock");
    let mut lock = match LockFile::try_acquire(&lock_path) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            return Err(CoordinatorError::Election {
                detail: "another coordinator is already serving this user".into(),
            })
        }
        Err(e) => {
            return Err(CoordinatorError::Election {
                detail: format!("cannot take the lock at {}: {e}", lock_path.display()),
            })
        }
    };
    // Who holds it, for a human reading the state directory. A failure
    // here is not an election failure: the lock is already ours, and the
    // file's content was never the election's evidence (C7).
    let _ = lock.write_pid();
    finish_election(socket_path).map(|listener| (listener, lock))
}

fn finish_election(socket_path: &Path) -> Result<Listener, CoordinatorError> {
    if socket_path.exists() {
        // A leftover endpoint from a crashed coordinator: prove it is dead
        // by attempting a connect before removing it.
        if Stream::connect(socket_path).is_ok() {
            return Err(CoordinatorError::Election {
                detail: "a live coordinator answered on the socket; not taking over".into(),
            });
        }
        std::fs::remove_file(socket_path).map_err(|cause| CoordinatorError::Connect {
            socket: socket_path.to_path_buf(),
            cause,
        })?;
    }
    // Permission-restricted (SPEC §23): the endpoint binds owner-only.
    Listener::bind(socket_path).map_err(|cause| CoordinatorError::Connect {
        socket: socket_path.to_path_buf(),
        cause,
    })
}

impl Coordinator {
    /// Elect, adopt whatever the ledger still records as live (SPEC §23:
    /// restart reconciles without duplicate live workers), and return
    /// the listener to serve.
    pub fn start(
        socket_path: &Path,
        limits: ConcurrencyLimits,
        ledger: Option<&Ledger>,
        agent_lease_ttl: Duration,
    ) -> Result<(Self, Listener), CoordinatorError> {
        // Read the live set BEFORE electing: not `unwrap_or_default()`,
        // because an empty live set and an unreadable one look identical
        // and mean the opposite things. On `SQLITE_BUSY` the old code
        // adopted nothing, re-granted every seat that was already taken
        // and lost every reservation with it (A1). A coordinator that
        // cannot see what is running does not start — and failing before
        // the election leaves no endpoint and no lock behind.
        let live = match ledger {
            Some(ledger) => ledger
                .live_dispatches()
                .map_err(|cause| CoordinatorError::LiveSetUnreadable { cause })?,
            None => Vec::new(),
        };
        let (listener, lock) = elect(socket_path)?;
        // `elect` above refused a socket path with no parent, so this
        // one has one; asked again rather than threaded through.
        let lock_path = socket_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("coordinator.lock");
        let mut state = AdmissionState::new(limits);
        // The machine's `binding_lease_secs` governs the lease a
        // hook-admitted agent is held on (SPEC §23): read here, at the
        // one place an `AdmissionState` is constructed for real, rather
        // than left at `DEFAULT_AGENT_LEASE_TTL` for every coordinator
        // regardless of what machine.toml states.
        state.set_agent_lease_ttl(agent_lease_ttl);
        {
            let now = Instant::now();
            for live in live {
                let Some(pid) = live.pid.map(|pid| pid.get()) else {
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
                // `source` is the one column every post-migration row
                // carries and no pre-migration row does (all four were
                // added together): its presence is what tells a fully
                // recorded row from one this coordinator cannot re-derive,
                // so the session, parent and depth below are read off the
                // row rather than guessed either way — a restart widens
                // nothing it does not also report.
                let attribution = if live.source.is_some() {
                    Attribution::Recorded
                } else {
                    Attribution::PreMigration
                };
                state.adopt(
                    &DispatchRequest {
                        dispatch_id: live.dispatch.as_str().to_string(),
                        run_id: live.run.as_str().to_string(),
                        session_id: live.session_id.clone().unwrap_or_else(|| "unknown".into()),
                        parent_dispatch: live.parent_dispatch.clone(),
                        depth: live
                            .depth
                            .and_then(|depth| u32::try_from(depth).ok())
                            .unwrap_or(0),
                        resource: ResourceClass::ModelWork,
                        reserve_micros: live.reserve_micros,
                        // Adopted from the ledger row the runner itself
                        // wrote before this election: a managed dispatch
                        // surviving a restart, not a hook-admitted one —
                        // the hook path never writes a ledger row (SPEC
                        // §23).
                        source: DispatchSource::ManagedRun,
                    },
                    pid,
                    live.agent_id.as_deref(),
                    attribution,
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

    /// Serve until shutdown or idle exit.
    ///
    /// A reconcile thread checks leases against the process table and
    /// signals cancelled workers. The accept loop hands each connection
    /// to a fixed pool of handler threads, which are joined before this
    /// returns: nothing is fire-and-forget, and a flood of connections
    /// costs a bounded queue rather than a thread apiece.
    ///
    /// Shutdown — asked for, or decided by the idle timer — is one path:
    /// the flag goes up, the accept loop sees it BEFORE its next accept,
    /// every connection already in flight is answered `shutting down`
    /// rather than dropped mid-request (A11), and only then is the
    /// endpoint unlinked and the lock released (A5).
    pub fn serve(self, listener: Listener) -> Result<(), CoordinatorError> {
        let reconcile_state = Arc::clone(&self.state);
        let reconcile_shutdown = Arc::clone(&self.shutdown);
        let reconciler = std::thread::spawn(move || {
            reconcile_loop(&reconcile_state, &reconcile_shutdown);
        });

        let (work, queue) = std::sync::mpsc::sync_channel::<Stream>(ACCEPT_BACKLOG);
        let queue = Arc::new(Mutex::new(queue));
        let mut handlers = Vec::with_capacity(HANDLER_THREADS);
        for _ in 0..HANDLER_THREADS {
            let queue = Arc::clone(&queue);
            let state = Arc::clone(&self.state);
            let shutdown = Arc::clone(&self.shutdown);
            handlers.push(std::thread::spawn(move || {
                handler_loop(&queue, &state, &shutdown);
            }));
        }

        let accepting = self.accept_loop(&listener, &work);
        // Dropping the last sender ends every handler's `recv`, so the
        // pool drains what is queued and stops. The listener goes with
        // it: nothing new is accepted while the pool finishes.
        drop(work);
        drop(listener);
        for handler in handlers {
            // A handler that panicked has already lost its connection;
            // the pool is being torn down either way, and a panic here
            // must not skip the unlink below.
            let _ = handler.join();
        }
        self.shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // Same: the reconciler is a background thread with no result
        // to return, and the daemon is going down either way.
        let _ = reconciler.join();

        // Only now that nothing is being served does the endpoint go.
        // Unlinking it while a connection was still in flight was the C1
        // failure this code claims to have fixed (A5).
        // Best-effort removal: a socket that cannot be unlinked is
        // probed and replaced by the next election anyway.
        let _ = std::fs::remove_file(&self.socket_path);
        // Release the lock before unlinking the file it is held on:
        // Windows refuses to remove a file this process still has open,
        // and a released lock with the file gone is what the next
        // election expects on either platform.
        drop(self.lock);
        let _ = std::fs::remove_file(&self.lock_path);
        accepting
    }

    /// Accept connections until the shutdown flag goes up or accepting
    /// itself stops working.
    fn accept_loop(
        &self,
        listener: &Listener,
        work: &std::sync::mpsc::SyncSender<Stream>,
    ) -> Result<(), CoordinatorError> {
        // Non-blocking so the flag is read BEFORE each accept: a
        // blocking accept could only observe shutdown by consuming the
        // connection that woke it, which was then dropped unanswered and
        // read at the client as "EOF while parsing" (A11).
        listener
            .set_nonblocking(true)
            .map_err(|cause| CoordinatorError::Io {
                operation: "setting the listener non-blocking",
                cause,
            })?;
        let mut exhausted = 0u32;
        loop {
            if self.shutdown.load(std::sync::atomic::Ordering::SeqCst) {
                return Ok(());
            }
            match listener.accept() {
                Ok(Some(stream)) => {
                    exhausted = 0;
                    // Blocks once `ACCEPT_BACKLOG` connections are
                    // queued: back-pressure on the kernel's own accept
                    // queue, which is a bounded wait rather than a
                    // thread per connection (A9).
                    if work.send(stream).is_err() {
                        return Ok(());
                    }
                }
                Ok(None) => std::thread::sleep(ACCEPT_POLL),
                Err(e) if crate::ipc::out_of_descriptors(&e) => {
                    // EMFILE/ENFILE: every accept fails the same way
                    // until a descriptor frees, and retrying at full
                    // speed is a hot loop that starves the handlers
                    // trying to close theirs. Back off, then give up:
                    // the next CLI call elects a fresh daemon.
                    exhausted += 1;
                    if exhausted > DESCRIPTOR_RETRIES {
                        return Err(CoordinatorError::Io {
                            operation: "accepting a connection",
                            cause: e,
                        });
                    }
                    std::thread::sleep(DESCRIPTOR_BACKOFF);
                }
                Err(_) => {
                    // A peer that went away between the connection and
                    // the accept (ECONNABORTED), or an interrupted
                    // syscall (EINTR): there is nothing to serve and the
                    // next accept is unaffected.
                    std::thread::sleep(ACCEPT_POLL);
                }
            }
        }
    }
}

/// One tick of lease reconciliation: what the state machine decided is
/// delivered after the guard is gone, and the idle decision is made and
/// acted on under one guard (A5).
fn reconcile_loop(state: &Mutex<AdmissionState>, shutdown: &std::sync::atomic::AtomicBool) {
    let mut idle_since: Option<Instant> = None;
    while wait_for_tick(shutdown) {
        let now = Instant::now();
        let report = {
            let mut state = lock_state(state);
            let report = state.reconcile(now, &process_alive);
            let occupancy = if state.is_idle() {
                Occupancy::Idle
            } else {
                Occupancy::Busy
            };
            match idle_step(occupancy, idle_since, now) {
                IdleStep::Busy => idle_since = None,
                IdleStep::Idle { since } => idle_since = Some(since),
                IdleStep::Exit => {
                    // Decided and acted on under the SAME guard: a
                    // registration served between the decision and the
                    // flag would otherwise be accepted by a daemon
                    // already on its way out, and lost with it (A5).
                    // From here every connection is answered
                    // `shutting down`; `serve` unlinks nothing until
                    // the pool has drained.
                    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
                    return;
                }
            }
            report
        };
        // The state machine decided; sending is this side's job, out
        // from under the admission lock (A6), and each dispatch is
        // signalled at most twice in its life: once politely, once not
        // (C5).
        deliver_signals(state, report.to_signal);
    }
}

/// Wait one reconcile period, in slices, and say whether there is still
/// work to do. `serve` joins this thread before it unlinks anything, so
/// a single fifteen-second sleep would make every `relais coordinator
/// stop` take fifteen seconds to come back.
fn wait_for_tick(shutdown: &std::sync::atomic::AtomicBool) -> bool {
    let deadline = Instant::now() + RECONCILE_EVERY;
    loop {
        if shutdown.load(std::sync::atomic::Ordering::SeqCst) {
            return false;
        }
        if Instant::now() >= deadline {
            return true;
        }
        std::thread::sleep(ACCEPT_POLL);
    }
}

/// What a reconcile tick does about idleness. Pure, so the decision the
/// A5 race is about can be tested without a daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdleStep {
    /// Something is registered, active or queued.
    Busy,
    /// Idle since this moment, and not for long enough yet.
    Idle { since: Instant },
    /// Idle past `IDLE_EXIT`: stop serving.
    Exit,
}

/// Whether the daemon has anything to serve. A named pair rather than a
/// `bool` parameter: at the call site `idle_step(true, ..)` said nothing
/// about which way round it was (`functions.no-flag-arguments`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Occupancy {
    /// Something is registered, active or queued.
    Busy,
    /// Nothing is.
    Idle,
}

fn idle_step(occupancy: Occupancy, idle_since: Option<Instant>, now: Instant) -> IdleStep {
    match (occupancy, idle_since) {
        (Occupancy::Busy, _) => IdleStep::Busy,
        (Occupancy::Idle, None) => IdleStep::Idle { since: now },
        (Occupancy::Idle, Some(since)) => {
            if now.saturating_duration_since(since) >= IDLE_EXIT {
                IdleStep::Exit
            } else {
                IdleStep::Idle { since }
            }
        }
    }
}

/// Deliver the signals the state machine handed over, re-checking
/// liveness first.
///
/// The PID was checked alive when it was bound and the age of that check
/// travels with the signal, but nothing portable proves the number still
/// names the same process (C6, `procs::alive`). Checking again here is
/// the only narrowing of that window there is, and it is what `reconcile`
/// already did — the cancel path did not (A6).
///
/// A delivery the OS would not take for a reason that could pass —
/// anything but "gone" or "not ours" — gives the ladder step back, so
/// the next reconcile offers it again instead of the dispatch silently
/// never being asked to stop (A7).
fn deliver_signals(state: &Mutex<AdmissionState>, signals: Vec<PendingSignal>) {
    for pending in signals {
        if !process_alive(pending.pid) {
            continue;
        }
        let sent = match pending.signal {
            Signal::Terminate => terminate(pending.pid),
            Signal::Kill => kill(pending.pid),
        };
        if let Err(e) = sent {
            if e.could_pass() {
                lock_state(state).signal_undelivered(&pending.dispatch_id, pending.signal);
            }
            eprintln!(
                "relais coordinator: {} for dispatch {} (bound {}s ago): {e}",
                pending.signal.as_str(),
                pending.dispatch_id,
                pending.bound_at.elapsed().as_secs()
            );
        }
    }
}

/// One handler thread: take connections off the queue until the queue's
/// last sender is gone.
fn handler_loop(
    queue: &Mutex<std::sync::mpsc::Receiver<Stream>>,
    state: &Mutex<AdmissionState>,
    shutdown: &std::sync::atomic::AtomicBool,
) {
    loop {
        // The guard is held only to take one connection, never across
        // serving it: the pool is parallel, the queue is not.
        let next = {
            let queue = queue
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            queue.recv()
        };
        let Ok(stream) = next else { return };
        if let Err(e) = handle_connection(stream, state, shutdown) {
            // The connection is gone and there is nobody left to tell;
            // the daemon keeps serving every other one.
            eprintln!("relais coordinator: connection: {e}");
        }
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
) -> Result<(), CoordinatorError> {
    let io = |operation: &'static str| {
        move |cause: std::io::Error| CoordinatorError::Io { operation, cause }
    };
    stream
        .set_read_timeout(Some(REQUEST_TIMEOUT))
        .map_err(io("setting the read timeout"))?;
    stream
        .set_write_timeout(Some(REQUEST_TIMEOUT))
        .map_err(io("setting the write timeout"))?;
    // Capped read: a client that never sends a newline used to grow this
    // buffer without bound, one thread per connection.
    let mut reader = BufReader::new(stream.try_clone().map_err(io("cloning the stream"))?)
        .take(MAX_REQUEST_BYTES);
    let mut writer = stream;
    let mut line = String::new();
    let read = reader
        .read_line(&mut line)
        .map_err(io("reading the request"))?;
    if read == 0 {
        return Ok(());
    }
    let oversized = read as u64 == MAX_REQUEST_BYTES && !line.ends_with('\n');
    let mut signals = Vec::new();
    let response = if oversized {
        Response::Error {
            detail: format!("request exceeds {MAX_REQUEST_BYTES} bytes"),
        }
    } else {
        match serde_json::from_str::<Request>(line.trim()) {
            Ok(Request::Shutdown) => {
                shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
                Response::Ack
            }
            // The flag went up while this connection was in flight. It
            // is answered, not dropped: a daemon that stops mid-request
            // reads at the client as "EOF while parsing" and loses
            // whatever the call was about (A11).
            Ok(_) if shutdown.load(std::sync::atomic::Ordering::SeqCst) => Response::ShuttingDown,
            Ok(request) => {
                let (response, pending) = handle(request, &mut lock_state(state), Instant::now());
                signals = pending;
                response
            }
            Err(e) => Response::Error {
                detail: format!("unparseable request: {e}"),
            },
        }
    };
    // Out from under the admission guard (A6): every other connection
    // would otherwise wait on a signal being delivered to a process that
    // may be stopped.
    deliver_signals(state, signals);
    let payload = serde_json::to_string(&response).map_err(|e| CoordinatorError::Protocol {
        operation: "serializing the response",
        detail: e.to_string(),
    })?;
    writeln!(writer, "{payload}").map_err(io("writing the response"))?;
    writer.flush().map_err(io("flushing the response"))?;
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

/// A lifecycle answer: applied, or the coordinator has no such subject.
fn lifecycle(outcome: LifecycleOutcome, entity: Entity, id: &str) -> Response {
    match outcome {
        LifecycleOutcome::Applied => Response::Ack,
        LifecycleOutcome::Unknown => Response::Unknown {
            entity,
            id: id.to_string(),
        },
    }
}

/// The cancellation answer: which dispatches were cancelled, and the
/// signals their caller must deliver.
fn cancelled(signals: Vec<PendingSignal>) -> (Response, Vec<PendingSignal>) {
    let dispatches = signals
        .iter()
        .map(|pending| pending.dispatch_id.clone())
        .collect();
    (Response::Cancelled { dispatches }, signals)
}

/// Dispatch one request against the state, and say what has to be
/// signalled afterwards.
///
/// It sends nothing itself. Signalling under the admission guard held
/// every other connection for the length of a syscall on a process that
/// may be stopped, and skipped the liveness check `reconcile` does (A6),
/// so the decision comes back as a list and the caller delivers it once
/// the guard is gone. The clock is a parameter; the only ambient fact
/// read here is this process's own PID, which a `Pong` is about.
pub fn handle(
    request: Request,
    state: &mut AdmissionState,
    now: Instant,
) -> (Response, Vec<PendingSignal>) {
    let response = match request {
        Request::Ping => Response::Pong {
            pid: std::process::id(),
            version: crate::version().to_string(),
            protocol: PROTOCOL_VERSION,
        },
        Request::RegisterSession { session_id } => {
            state.register_session(&session_id);
            Response::Ack
        }
        Request::RegisterRun { registration } => {
            state.register_run(&registration, now);
            Response::Ack
        }
        Request::RequestAdmission { request } => Response::Decision {
            decision: state.request(&request, now),
        },
        Request::Bind {
            dispatch_id,
            agent_id,
            pid,
        } => match state.bind(&dispatch_id, agent_id.as_deref(), pid, now, &process_alive) {
            BindOutcome::Bound => Response::Ack,
            BindOutcome::UnknownDispatch => Response::Unknown {
                entity: Entity::Dispatch,
                id: dispatch_id,
            },
            // C6: the PID comes off the wire. One that is not running
            // cannot be this client's worker, and once the OS reuses the
            // number it would be somebody else's process — which
            // reconcile would later terminate as a cancelled worker.
            // Only a lease can hit this, and `bind` is the pid path, so
            // reaching it here would mean a dispatch was bound by pid
            // twice. Refused with the same shape rather than acked: a
            // bind that did not happen must not read as one that did.
            BindOutcome::AlreadyBoundToProcess => Response::Refused {
                refusal: Refused::PidNotAlive {
                    pid,
                    detail: format!(
                        "dispatch {dispatch_id} is already bound to a live process; a second \
                         binding would leave the first unsignalable"
                    ),
                },
            },
            BindOutcome::PidNotAlive => Response::Refused {
                refusal: Refused::PidNotAlive {
                    pid,
                    detail: format!(
                        "pid {} is not a live process; dispatch {dispatch_id} keeps no process \
                         binding",
                        pid.unwrap_or(0)
                    ),
                },
            },
        },
        Request::Heartbeat { dispatch_id } => Response::Heartbeat {
            status: state.heartbeat(&dispatch_id, now),
        },
        Request::AcknowledgeCancel { dispatch_id } => lifecycle(
            state.acknowledge_cancel(&dispatch_id, now),
            Entity::Dispatch,
            &dispatch_id,
        ),
        Request::MarkWaiting { dispatch_id } => {
            match state.mark_waiting(&dispatch_id, now, &process_alive) {
                WaitOutcome::Waiting => Response::Ack,
                WaitOutcome::UnknownDispatch => Response::Unknown {
                    entity: Entity::Dispatch,
                    id: dispatch_id,
                },
                // C8: waiting is a self-report. The least it has to be is
                // a live, bound process making it.
                WaitOutcome::NoLiveProcess => Response::Refused {
                    refusal: Refused::UnverifiableWait {
                        detail: format!(
                            "dispatch {dispatch_id} has no live bound process to be waiting; the \
                             seat is not given back on an unverifiable claim"
                        ),
                    },
                },
                WaitOutcome::Ended => Response::Refused {
                    refusal: Refused::DispatchEnded,
                },
            }
        }
        Request::Resume { dispatch_id } => match state.resume(&dispatch_id, now) {
            ResumeOutcome::Resumed => Response::Ack,
            ResumeOutcome::UnknownDispatch => Response::Unknown {
                entity: Entity::Dispatch,
                id: dispatch_id,
            },
            ResumeOutcome::OverAdmitted { over, max } => Response::Refused {
                refusal: Refused::OverAdmitted { over, max },
            },
        },
        Request::Release { dispatch_id } => lifecycle(
            state.release(&dispatch_id, now),
            Entity::Dispatch,
            &dispatch_id,
        ),
        Request::Settle {
            dispatch_id,
            spent_micros,
        } => lifecycle(
            state.settle(&dispatch_id, spent_micros, now),
            Entity::Dispatch,
            &dispatch_id,
        ),
        Request::Withdraw { dispatch_id } => match state.withdraw(&dispatch_id, now) {
            WithdrawOutcome::Withdrawn => Response::Ack,
            WithdrawOutcome::UnknownDispatch => Response::Unknown {
                entity: Entity::Dispatch,
                id: dispatch_id,
            },
            WithdrawOutcome::Claimed => Response::Refused {
                refusal: Refused::AlreadyClaimed,
            },
        },
        Request::BindAgentLease {
            dispatch_id,
            agent_id,
            provenance,
        } => match state.bind_agent_lease(&dispatch_id, &agent_id, provenance, now) {
            BindOutcome::Bound => Response::Ack,
            BindOutcome::UnknownDispatch => Response::Unknown {
                entity: Entity::Dispatch,
                id: dispatch_id,
            },
            BindOutcome::AlreadyBoundToProcess => Response::Refused {
                refusal: Refused::AlreadyBoundToProcess {
                    detail: format!(
                        "dispatch {dispatch_id} is already bound to a live process; a lease for \
                         agent {agent_id} would leave it unsignalable"
                    ),
                },
            },
            // A lease names no pid, so `bind_agent_lease` never checks
            // one; answered in the pid path's own shape all the same,
            // because a bind that did not happen must not read as one
            // that did.
            BindOutcome::PidNotAlive => Response::Refused {
                refusal: Refused::PidNotAlive {
                    pid: None,
                    detail: format!("dispatch {dispatch_id} keeps no binding"),
                },
            },
        },
        Request::SettleByAgent {
            session_id,
            agent_id,
            spent_micros,
        } => Response::AgentSettled {
            outcome: state.settle_by_agent(&session_id, &agent_id, spent_micros, now),
        },
        Request::FinishRun { run_id } => lifecycle(state.finish_run(&run_id), Entity::Run, &run_id),
        Request::AcquireWrite {
            dispatch_id,
            worktree,
        } => match state.acquire_write(&worktree, &dispatch_id, now) {
            WriteLeaseOutcome::Taken => Response::Ack,
            WriteLeaseOutcome::HeldBy { holder } => Response::Refused {
                refusal: Refused::WorktreeHeld { worktree, holder },
            },
        },
        Request::ReleaseWrite {
            dispatch_id,
            worktree,
        } => match state.release_write(&worktree, &dispatch_id) {
            ReleaseWriteOutcome::Released => Response::Ack,
            ReleaseWriteOutcome::NotTheHolder => Response::Refused {
                refusal: Refused::NotTheLeaseHolder { worktree },
            },
        },
        Request::WriteLeaseHolder { worktree } => Response::WriteLease {
            holder: state
                .write_lease_holder(&worktree)
                .map(|(holder, _)| holder.to_string()),
        },
        Request::CancelDispatch { dispatch_id } => {
            return cancelled(state.cancel_dispatch(&dispatch_id, now))
        }
        Request::CancelRun { run_id } => return cancelled(state.cancel_run(&run_id, now)),
        Request::CancelSession { session_id } => {
            return cancelled(state.cancel_session(&session_id, now))
        }
        Request::Status => Response::Status {
            snapshot: Box::new(state.status(now)),
        },
        Request::Shutdown => Response::Ack,
    };
    (response, Vec::new())
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
        let connect = |cause| CoordinatorError::Connect {
            socket: self.socket.clone(),
            cause,
        };
        let io = |operation: &'static str| {
            move |cause: std::io::Error| CoordinatorError::Io { operation, cause }
        };
        let mut stream = Stream::connect(&self.socket).map_err(connect)?;
        stream
            .set_read_timeout(Some(REQUEST_TIMEOUT))
            .map_err(io("setting the read timeout"))?;
        stream
            .set_write_timeout(Some(REQUEST_TIMEOUT))
            .map_err(io("setting the write timeout"))?;
        let payload = serde_json::to_string(request).map_err(|e| CoordinatorError::Protocol {
            operation: "serializing the request",
            detail: e.to_string(),
        })?;
        writeln!(stream, "{payload}").map_err(io("writing the request"))?;
        stream.flush().map_err(io("flushing the request"))?;
        let mut line = String::new();
        BufReader::new(stream)
            .read_line(&mut line)
            .map_err(io("reading the response"))?;
        match serde_json::from_str::<Response>(line.trim()) {
            Ok(Response::Error { detail }) => Err(CoordinatorError::Rejected { detail }),
            Ok(Response::ShuttingDown) => Err(CoordinatorError::ShuttingDown {
                socket: self.socket.clone(),
            }),
            Ok(response) => Ok(response),
            // A daemon from another install answers in a shape this
            // build cannot read. That is the point of failing here
            // rather than guessing (A2).
            Err(e) => Err(CoordinatorError::Protocol {
                operation: "reading the response",
                detail: format!("unparseable coordinator response: {e}"),
            }),
        }
    }

    /// The daemon's PID, and proof that it speaks this wire protocol.
    /// A version skew is named here — at the first call of every CLI
    /// command — instead of turning into calls that are understood as
    /// something else (A2).
    pub fn ping(&self) -> Result<u32, CoordinatorError> {
        match self.request(&Request::Ping)? {
            Response::Pong {
                pid,
                version,
                protocol,
            } => {
                if protocol == PROTOCOL_VERSION {
                    Ok(pid)
                } else {
                    Err(CoordinatorError::VersionSkew {
                        socket: self.socket.clone(),
                        ours: PROTOCOL_VERSION,
                        theirs: protocol,
                        daemon_version: version,
                    })
                }
            }
            other => Err(CoordinatorError::Protocol {
                operation: "ping",
                detail: format!("unexpected reply: {other:?}"),
            }),
        }
    }

    pub fn status(&self) -> Result<StatusSnapshot, CoordinatorError> {
        match self.request(&Request::Status)? {
            Response::Status { snapshot } => Ok(*snapshot),
            other => Err(CoordinatorError::Protocol {
                operation: "status",
                detail: format!("unexpected reply: {other:?}"),
            }),
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

    fn call(&self, operation: &'static str, request: Request) -> Result<Response, GateError> {
        self.client.request(&request).map_err(|e| match e {
            // The coordinator answered and refused the request itself —
            // an unknown method, an over-long line. That is a protocol
            // disagreement, not an outage.
            CoordinatorError::Rejected { detail } | CoordinatorError::Protocol { detail, .. } => {
                GateError::Protocol { operation, detail }
            }
            other => GateError::Unavailable {
                operation,
                socket: self.client.socket.display().to_string(),
                cause: Box::new(other),
            },
        })
    }

    /// An answer this call cannot act on.
    fn unexpected(operation: &'static str, response: &Response) -> GateError {
        GateError::Protocol {
            operation,
            detail: format!("unexpected reply: {response:?}"),
        }
    }

    /// A refusal, as an error naming what was refused and why.
    fn refused(operation: &'static str, entity: &str, refusal: &Refused) -> GateError {
        GateError::Refused {
            operation,
            entity: entity.to_string(),
            detail: refusal.describe(),
        }
    }
}

/// Every reply is matched exhaustively, and `Unknown` is never one of
/// the arms that mean success: a worker that binds after a re-election
/// gets an outcome its caller has to handle, not a silent `Ok(())` (A2).
impl Gate for RemoteGate {
    fn register_run(&self, registration: &RunRegistration) -> Result<(), GateError> {
        let run_id = registration.run_id.clone();
        match self.call(
            "register_run",
            Request::RegisterRun {
                registration: registration.clone(),
            },
        )? {
            Response::Ack => Ok(()),
            Response::Refused { refusal } => Err(Self::refused(
                "register_run",
                &format!("run {run_id}"),
                &refusal,
            )),
            other => Err(Self::unexpected("register_run", &other)),
        }
    }

    fn admit(&self, request: &DispatchRequest) -> Result<Decision, GateError> {
        match self.call(
            "admit",
            Request::RequestAdmission {
                request: request.clone(),
            },
        )? {
            Response::Decision { decision } => Ok(decision),
            other => Err(Self::unexpected("admit", &other)),
        }
    }

    fn bind(
        &self,
        dispatch_id: &str,
        agent_id: Option<&str>,
        pid: Option<u32>,
    ) -> Result<BindOutcome, GateError> {
        match self.call(
            "bind",
            Request::Bind {
                dispatch_id: dispatch_id.into(),
                agent_id: agent_id.map(str::to_string),
                pid,
            },
        )? {
            Response::Ack => Ok(BindOutcome::Bound),
            // The coordinator has no such dispatch: this worker holds no
            // seat and no reservation. The caller decides what that
            // means; it is never success.
            Response::Unknown { .. } => Ok(BindOutcome::UnknownDispatch),
            Response::Refused {
                refusal: Refused::PidNotAlive { .. },
            } => Ok(BindOutcome::PidNotAlive),
            Response::Refused { refusal } => Err(Self::refused(
                "bind",
                &format!("dispatch {dispatch_id}"),
                &refusal,
            )),
            other => Err(Self::unexpected("bind", &other)),
        }
    }

    fn heartbeat(&self, dispatch_id: &str) -> Result<HeartbeatStatus, GateError> {
        match self.call(
            "heartbeat",
            Request::Heartbeat {
                dispatch_id: dispatch_id.into(),
            },
        )? {
            Response::Heartbeat { status } => Ok(status),
            other => Err(Self::unexpected("heartbeat", &other)),
        }
    }

    fn acknowledge_cancel(&self, dispatch_id: &str) -> Result<LifecycleOutcome, GateError> {
        self.lifecycle_call(
            "acknowledge_cancel",
            dispatch_id,
            Request::AcknowledgeCancel {
                dispatch_id: dispatch_id.into(),
            },
        )
    }

    fn mark_waiting(&self, dispatch_id: &str) -> Result<WaitOutcome, GateError> {
        match self.call(
            "mark_waiting",
            Request::MarkWaiting {
                dispatch_id: dispatch_id.into(),
            },
        )? {
            Response::Ack => Ok(WaitOutcome::Waiting),
            Response::Unknown { .. } => Ok(WaitOutcome::UnknownDispatch),
            Response::Refused {
                refusal: Refused::UnverifiableWait { .. },
            } => Ok(WaitOutcome::NoLiveProcess),
            Response::Refused {
                refusal: Refused::DispatchEnded,
            } => Ok(WaitOutcome::Ended),
            Response::Refused { refusal } => Err(Self::refused(
                "mark_waiting",
                &format!("dispatch {dispatch_id}"),
                &refusal,
            )),
            other => Err(Self::unexpected("mark_waiting", &other)),
        }
    }

    fn resume(&self, dispatch_id: &str) -> Result<ResumeOutcome, GateError> {
        match self.call(
            "resume",
            Request::Resume {
                dispatch_id: dispatch_id.into(),
            },
        )? {
            Response::Ack => Ok(ResumeOutcome::Resumed),
            Response::Unknown { .. } => Ok(ResumeOutcome::UnknownDispatch),
            // Over the over-admission ceiling: the parent stays waiting
            // and the caller polls again (C8). A delay, and the caller
            // can tell it from an outage.
            Response::Refused {
                refusal: Refused::OverAdmitted { over, max },
            } => Ok(ResumeOutcome::OverAdmitted { over, max }),
            Response::Refused { refusal } => Err(Self::refused(
                "resume",
                &format!("dispatch {dispatch_id}"),
                &refusal,
            )),
            other => Err(Self::unexpected("resume", &other)),
        }
    }

    fn release(&self, dispatch_id: &str) -> Result<LifecycleOutcome, GateError> {
        self.lifecycle_call(
            "release",
            dispatch_id,
            Request::Release {
                dispatch_id: dispatch_id.into(),
            },
        )
    }

    fn settle(
        &self,
        dispatch_id: &str,
        spent_micros: Option<i64>,
    ) -> Result<LifecycleOutcome, GateError> {
        self.lifecycle_call(
            "settle",
            dispatch_id,
            Request::Settle {
                dispatch_id: dispatch_id.into(),
                spent_micros,
            },
        )
    }

    fn withdraw(&self, dispatch_id: &str) -> Result<WithdrawOutcome, GateError> {
        match self.call(
            "withdraw",
            Request::Withdraw {
                dispatch_id: dispatch_id.into(),
            },
        )? {
            Response::Ack => Ok(WithdrawOutcome::Withdrawn),
            Response::Unknown { .. } => Ok(WithdrawOutcome::UnknownDispatch),
            Response::Refused {
                refusal: Refused::AlreadyClaimed,
            } => Ok(WithdrawOutcome::Claimed),
            Response::Refused { refusal } => Err(Self::refused(
                "withdraw",
                &format!("dispatch {dispatch_id}"),
                &refusal,
            )),
            other => Err(Self::unexpected("withdraw", &other)),
        }
    }

    fn bind_agent_lease(
        &self,
        dispatch_id: &str,
        agent_id: &str,
        provenance: &Provenance,
    ) -> Result<BindOutcome, GateError> {
        match self.call(
            "bind_agent_lease",
            Request::BindAgentLease {
                dispatch_id: dispatch_id.into(),
                agent_id: agent_id.into(),
                provenance: provenance.clone(),
            },
        )? {
            Response::Ack => Ok(BindOutcome::Bound),
            // No such dispatch: nothing was bound, and never success.
            Response::Unknown { .. } => Ok(BindOutcome::UnknownDispatch),
            Response::Refused {
                refusal: Refused::AlreadyBoundToProcess { .. },
            } => Ok(BindOutcome::AlreadyBoundToProcess),
            Response::Refused {
                refusal: Refused::PidNotAlive { .. },
            } => Ok(BindOutcome::PidNotAlive),
            Response::Refused { refusal } => Err(Self::refused(
                "bind_agent_lease",
                &format!("dispatch {dispatch_id}"),
                &refusal,
            )),
            // Listed rather than caught by a wildcard, so a reply added
            // later has to be placed here on purpose.
            unexpected @ (Response::AgentSettled { .. }
            | Response::Error { .. }
            | Response::ShuttingDown
            | Response::Pong { .. }
            | Response::Decision { .. }
            | Response::Heartbeat { .. }
            | Response::WriteLease { .. }
            | Response::Cancelled { .. }
            | Response::Status { .. }) => Err(Self::unexpected("bind_agent_lease", &unexpected)),
        }
    }

    fn settle_by_agent(
        &self,
        session_id: &str,
        agent_id: &str,
        spent_micros: Option<i64>,
    ) -> Result<AgentSettleOutcome, GateError> {
        match self.call(
            "settle_by_agent",
            Request::SettleByAgent {
                session_id: session_id.into(),
                agent_id: agent_id.into(),
                spent_micros,
            },
        )? {
            Response::AgentSettled { outcome } => Ok(outcome),
            Response::Refused { refusal } => Err(Self::refused(
                "settle_by_agent",
                &format!("agent {agent_id} of session {session_id}"),
                &refusal,
            )),
            // `Unknown` included: nothing bound to an agent is
            // `AgentSettled { NothingBound }`, never `Unknown`, so a daemon
            // answering `Unknown` is not one this build understands.
            unexpected @ (Response::Ack
            | Response::Unknown { .. }
            | Response::Error { .. }
            | Response::ShuttingDown
            | Response::Pong { .. }
            | Response::Decision { .. }
            | Response::Heartbeat { .. }
            | Response::WriteLease { .. }
            | Response::Cancelled { .. }
            | Response::Status { .. }) => Err(Self::unexpected("settle_by_agent", &unexpected)),
        }
    }

    fn finish_run(&self, run_id: &str) -> Result<LifecycleOutcome, GateError> {
        match self.call(
            "finish_run",
            Request::FinishRun {
                run_id: run_id.into(),
            },
        )? {
            Response::Ack => Ok(LifecycleOutcome::Applied),
            Response::Unknown { .. } => Ok(LifecycleOutcome::Unknown),
            Response::Refused { refusal } => Err(Self::refused(
                "finish_run",
                &format!("run {run_id}"),
                &refusal,
            )),
            other => Err(Self::unexpected("finish_run", &other)),
        }
    }

    fn acquire_write(
        &self,
        dispatch_id: &str,
        worktree: &str,
    ) -> Result<WriteLeaseOutcome, GateError> {
        match self.call(
            "acquire_write",
            Request::AcquireWrite {
                dispatch_id: dispatch_id.into(),
                worktree: worktree.into(),
            },
        )? {
            Response::Ack => Ok(WriteLeaseOutcome::Taken),
            Response::Refused {
                refusal: Refused::WorktreeHeld { holder, .. },
            } => Ok(WriteLeaseOutcome::HeldBy { holder }),
            Response::Refused { refusal } => Err(Self::refused(
                "acquire_write",
                &format!("worktree {worktree}"),
                &refusal,
            )),
            other => Err(Self::unexpected("acquire_write", &other)),
        }
    }

    fn release_write(
        &self,
        dispatch_id: &str,
        worktree: &str,
    ) -> Result<ReleaseWriteOutcome, GateError> {
        match self.call(
            "release_write",
            Request::ReleaseWrite {
                dispatch_id: dispatch_id.into(),
                worktree: worktree.into(),
            },
        )? {
            Response::Ack => Ok(ReleaseWriteOutcome::Released),
            Response::Refused {
                refusal: Refused::NotTheLeaseHolder { .. },
            } => Ok(ReleaseWriteOutcome::NotTheHolder),
            Response::Refused { refusal } => Err(Self::refused(
                "release_write",
                &format!("worktree {worktree}"),
                &refusal,
            )),
            other => Err(Self::unexpected("release_write", &other)),
        }
    }

    fn write_lease_holder(&self, worktree: &str) -> Result<Option<String>, GateError> {
        match self.call(
            "write_lease_holder",
            Request::WriteLeaseHolder {
                worktree: worktree.into(),
            },
        )? {
            Response::WriteLease { holder } => Ok(holder),
            other => Err(Self::unexpected("write_lease_holder", &other)),
        }
    }

    fn enforcement(&self) -> Enforcement {
        Enforcement::Coordinator
    }
}

impl RemoteGate {
    /// The three lifecycle calls that answer `Ack` or `Unknown` and
    /// nothing else.
    fn lifecycle_call(
        &self,
        operation: &'static str,
        dispatch_id: &str,
        request: Request,
    ) -> Result<LifecycleOutcome, GateError> {
        match self.call(operation, request)? {
            Response::Ack => Ok(LifecycleOutcome::Applied),
            Response::Unknown { .. } => Ok(LifecycleOutcome::Unknown),
            Response::Refused { refusal } => Err(Self::refused(
                operation,
                &format!("dispatch {dispatch_id}"),
                &refusal,
            )),
            other => Err(Self::unexpected(operation, &other)),
        }
    }
}

/// The endpoint this user's coordinator serves on. Fallible for the same
/// reason the state directory is: without a home directory and without
/// `RELAIS_STATE_DIR` there is nowhere to put it (C10).
pub fn socket_path() -> Result<PathBuf, crate::paths::HomeUnset> {
    Ok(crate::paths::state_dir()?.join("relais.sock"))
}

/// The interactive session this CLI call belongs to. `RELAIS_SESSION_ID`
/// wins (the /relais skill sets it), then Claude Code's own session
/// variable when present; otherwise the parent PID names the tab and
/// the attribution is labelled as such rather than guessed.
pub fn session_id() -> String {
    resolve_session_id(
        |name| std::env::var_os(name),
        crate::procs::parent_pid(),
        std::process::id(),
    )
}

/// The rule behind [`session_id`], with the lookup and the process facts
/// handed in.
///
/// Injected the way `paths::resolve_home` is: read from the ambient
/// environment, the fallback branch could only be asserted when the
/// developer's own shell happened not to export either variable, and the
/// test quietly weakened to `assert!(!id.is_empty())` when it did
/// (`tests.first-properties`).
fn resolve_session_id(
    var: impl Fn(&str) -> Option<std::ffi::OsString>,
    parent: Option<u32>,
    own: u32,
) -> String {
    for name in ["RELAIS_SESSION_ID", "CLAUDE_SESSION_ID"] {
        if let Some(id) = var(name) {
            let id = id.to_string_lossy().trim().to_string();
            if !id.is_empty() {
                return id;
            }
        }
    }
    match parent {
        Some(parent) => format!("unattributed-ppid-{parent}"),
        None => format!("unattributed-pid-{own}"),
    }
}

/// Connect to the user's coordinator, starting one lazily when none
/// answers. Starting spawns `relais coordinator daemon` detached from
/// this CLI process so its exit cannot take the coordinator down.
pub fn ensure_running(socket_path: &Path) -> Result<Client, CoordinatorError> {
    let client = Client::new(socket_path.to_path_buf());
    match client.ping() {
        Ok(_) => return Ok(client),
        // A daemon is serving and speaks another protocol. Starting a
        // second one cannot help — it would lose the election to the
        // first — so say which it is instead of timing out (A2).
        Err(skew @ CoordinatorError::VersionSkew { .. }) => return Err(skew),
        Err(_) => {}
    }
    let exe = std::env::current_exe().map_err(|cause| CoordinatorError::Io {
        operation: "finding the relais executable",
        cause,
    })?;
    let mut command = std::process::Command::new(exe);
    command
        .args(["coordinator", "daemon"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Detached from this CLI's group (Unix) or console group (Windows),
    // so the CLI exiting cannot take the coordinator down.
    crate::procs::own_process_group(&mut command);
    command.spawn().map_err(|cause| CoordinatorError::Io {
        operation: "starting the coordinator",
        cause,
    })?;
    let started = Instant::now();
    while started.elapsed() < START_TIMEOUT {
        match client.ping() {
            Ok(_) => return Ok(client),
            Err(skew @ CoordinatorError::VersionSkew { .. }) => return Err(skew),
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(CoordinatorError::Protocol {
        operation: "starting the coordinator",
        detail: "it did not answer within the start timeout".into(),
    })
}

/// Foreground daemon entry used by `relais coordinator daemon`.
pub fn run_daemon(
    socket_path: &Path,
    limits: ConcurrencyLimits,
    ledger: Option<&Ledger>,
    agent_lease_ttl: Duration,
) -> Result<(), CoordinatorError> {
    let (coordinator, listener) = Coordinator::start(socket_path, limits, ledger, agent_lease_ttl)?;
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
    use crate::ids::{DispatchId, Pid, RunId};

    fn temp_dir(tag: &str) -> crate::test_support::TempDir {
        crate::test_support::short_temp_dir(tag)
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
            source: DispatchSource::ManagedRun,
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
    }

    #[test]
    fn client_daemon_round_trip_and_shutdown() {
        let dir = temp_dir("rt");
        let socket = dir.join("relais.sock");
        let (coordinator, listener) = Coordinator::start(
            &socket,
            limits(),
            None,
            crate::admission::DEFAULT_AGENT_LEASE_TTL,
        )
        .expect("start");
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
        // Write leases over the wire: exclusive, named, released by the
        // holder only, gone with settlement.
        assert!(gate
            .acquire_write("d1", "/wt/one")
            .expect("acquire")
            .taken());
        assert_eq!(
            gate.acquire_write("d2", "/wt/one").expect("acquire"),
            WriteLeaseOutcome::HeldBy {
                holder: "d1".into()
            },
            "a second writer is refused, and told who is writing"
        );
        assert_eq!(
            gate.write_lease_holder("/wt/one")
                .expect("holder")
                .as_deref(),
            Some("d1")
        );
        gate.release_write("d2", "/wt/one").expect("release");
        assert_eq!(
            gate.write_lease_holder("/wt/one")
                .expect("holder")
                .as_deref(),
            Some("d1"),
            "only the holder can release"
        );
        gate.release_write("d1", "/wt/one").expect("release");
        assert_eq!(gate.write_lease_holder("/wt/one").expect("holder"), None);
        gate.release("d2").expect("release");
        gate.settle("d2", None).expect("settle");
        let snapshot = client.status().expect("status");
        assert_eq!(snapshot.runs["run-1"].uncertain_settlements, 1);
        assert_eq!(snapshot.sessions, vec!["tab-a".to_string()]);
        assert_eq!(gate.enforcement(), Enforcement::Coordinator);

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
            Response::Ack
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
        let (coordinator, listener) = Coordinator::start(
            &socket,
            limits,
            None,
            crate::admission::DEFAULT_AGENT_LEASE_TTL,
        )
        .expect("start");
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
    }

    #[test]
    fn restart_adopts_only_live_ledger_dispatches() {
        let dir = temp_dir("adopt");
        let socket = dir.join("relais.sock");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        ledger
            .insert_run(
                &RunId::from_stored("run-x"),
                "/repo",
                None,
                &crate::ids::TaskId::from_stored("task-run-x"),
                "rk",
            )
            .expect("run");
        ledger
            .record_dispatch_intent(
                &DispatchId::from_stored("live"),
                &RunId::from_stored("run-x"),
                None,
                &serde_json::json!({}),
                0,
            )
            .expect("intent");
        ledger
            .attach_dispatch_process(
                &DispatchId::from_stored("live"),
                Some(Pid::new(std::process::id())),
                Some("sess"),
            )
            .expect("attach");
        ledger
            .record_dispatch_intent(
                &DispatchId::from_stored("dead"),
                &RunId::from_stored("run-x"),
                None,
                &serde_json::json!({}),
                0,
            )
            .expect("intent");
        ledger
            .attach_dispatch_process(
                &DispatchId::from_stored("dead"),
                Some(Pid::new(999_999_999)),
                None,
            )
            .expect("attach");
        // C2: `launched`, no PID — the row managed_launch writes BEFORE
        // the process exists. Adopting it took a seat nothing could ever
        // bind or free; `relais resume` reconciles it against the ledger.
        ledger
            .record_dispatch_intent(
                &DispatchId::from_stored("pidless"),
                &RunId::from_stored("run-x"),
                None,
                &serde_json::json!({}),
                0,
            )
            .expect("intent");
        ledger
            .attach_dispatch_process(&DispatchId::from_stored("pidless"), None, Some("sess"))
            .expect("attach");
        ledger
            .record_dispatch_intent(
                &DispatchId::from_stored("finished"),
                &RunId::from_stored("run-x"),
                None,
                &serde_json::json!({}),
                0,
            )
            .expect("intent");
        ledger
            .finish_dispatch(&DispatchId::from_stored("finished"), "completed")
            .expect("finish");
        let (coordinator, listener) = Coordinator::start(
            &socket,
            limits(),
            Some(&ledger),
            crate::admission::DEFAULT_AGENT_LEASE_TTL,
        )
        .expect("start");
        drop(listener);
        let state = coordinator.state.lock().expect("lock");
        let snapshot = state.status(Instant::now());
        assert_eq!(
            snapshot.active_by_class.get("model_work"),
            Some(&1),
            "only the live, bound dispatch is adopted"
        );
        assert_eq!(snapshot.runs["run-x"].admitted_total, 1);
        assert_eq!(
            snapshot.runs["run-x"].session_id, "sess",
            "the ledger row already named the session; adoption reads it \
             rather than placing every restart under \"unknown\""
        );
        assert_eq!(
            snapshot.adopted_pre_migration, 0,
            "this row carries source, parent and depth; nothing about it is unrecorded"
        );
        drop(state);
        std::fs::remove_file(&socket).ok();
    }

    // The objective this migration exists for: a per-session cap that
    // bound before a restart still binds after it, because the adopted
    // dispatch keeps the real session a placeholder "unknown" erased.
    #[test]
    fn a_restarted_coordinator_still_enforces_the_per_session_cap() {
        let dir = temp_dir("adopt-cap");
        let socket = dir.join("relais.sock");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        ledger
            .insert_run(
                &RunId::from_stored("run-cap-1"),
                "/repo",
                None,
                &crate::ids::TaskId::from_stored("task-run-cap-1"),
                "rk",
            )
            .expect("run");
        ledger
            .record_dispatch_intent(
                &DispatchId::from_stored("live"),
                &RunId::from_stored("run-cap-1"),
                None,
                &serde_json::json!({}),
                0,
            )
            .expect("intent");
        ledger
            .attach_dispatch_process(
                &DispatchId::from_stored("live"),
                Some(Pid::new(std::process::id())),
                Some("cap-session"),
            )
            .expect("attach");

        // `limits()` caps `max_active_agents_per_session` at 1: the
        // session above is already at that cap before the coordinator
        // this process runs even elects.
        let (coordinator, listener) = Coordinator::start(
            &socket,
            limits(),
            Some(&ledger),
            crate::admission::DEFAULT_AGENT_LEASE_TTL,
        )
        .expect("start");
        drop(listener);
        let mut state = coordinator.state.lock().expect("lock");
        assert_eq!(
            state.status(Instant::now()).runs["run-cap-1"].session_id,
            "cap-session"
        );
        // A second run in the SAME session — the ordinary shape of a
        // person's tab starting a new run once the first has settled
        // enough to still hold a live seat.
        state.register_run(&registration("run-cap-2", "cap-session"), Instant::now());
        assert_eq!(
            state.request(
                &request("second", "run-cap-2", "cap-session"),
                Instant::now()
            ),
            Decision::Queued { position: 1 },
            "the adopted dispatch's real session still fills the per-session \
             cap; before this fix it was adopted under \"unknown\" and this \
             was Granted"
        );
        drop(state);
        std::fs::remove_file(&socket).ok();
    }

    // C3/C4: one request thread panicking must not wedge every later
    // connection — a poisoned admission lock is recovered, not rethrown.
    //
    // The panic happens in a thread of its own rather than behind a
    // swapped panic hook: the hook is process-global, and taking it away
    // for the length of this test silenced (and briefly un-silenced)
    // every other test running beside it. The stderr line this prints is
    // the expected panic.
    #[test]
    fn a_poisoned_admission_lock_does_not_wedge_the_daemon() {
        let state = Mutex::new(AdmissionState::new(limits()));
        let died = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let _guard = state.lock().expect("lock");
                    panic!("a request thread died holding the admission lock (expected)");
                })
                .join()
        });
        assert!(died.is_err());
        assert!(state.is_poisoned());

        let mut recovered = lock_state(&state);
        recovered.register_run(&registration("run-1", "tab-a"), Instant::now());
        assert!(
            !recovered.is_idle(),
            "the state machine keeps serving after a poisoned lock"
        );
    }

    #[test]
    fn remote_gate_reports_an_absent_coordinator_as_unavailable() {
        // Its own path: a fixed one is shared with every other run of
        // this suite on the machine, including one that might have a
        // daemon on it.
        let dir = temp_dir("absent");
        let gate = RemoteGate::new(dir.join("relais.sock"));
        let err = gate
            .admit(&request("d", "r", "s"))
            .expect_err("no coordinator");
        assert!(err.unavailable(), "{err}");
        assert!(
            err.to_string().starts_with("admission unavailable"),
            "{err}"
        );
    }

    // A2: the hole the union closes. v1 answered "I have never heard of
    // that dispatch" as `Ok { known: false }`, and every `_ =>` arm read
    // it as success — a worker that bound after a re-election ran with
    // no seat, no reservation and no PID on record.
    #[test]
    fn a_reply_that_means_unknown_cannot_be_read_as_success() {
        // The v1 wire shape does not parse at all now, so there is no
        // arm left that could mistake it.
        let legacy = r#"{"kind":"ok","known":false}"#;
        assert!(
            serde_json::from_str::<Response>(legacy).is_err(),
            "the old ambiguous shape is not a response this build accepts"
        );
        // And the shape that replaced it is an outcome the caller has to
        // handle, never `Ok(())`.
        let unknown = Response::Unknown {
            entity: Entity::Dispatch,
            id: "d1".into(),
        };
        let round_tripped: Response =
            serde_json::from_str(&serde_json::to_string(&unknown).expect("serialize"))
                .expect("parse");
        assert_eq!(round_tripped, unknown);
    }

    // The hook's seat lifecycle over the wire: a lease bound to the
    // agent a spawn launched, and the agent's end found by session and
    // agent alone. An agent nothing is bound to is an ordinary answer,
    // not `Unknown`.
    #[test]
    fn an_agent_bound_by_lease_is_settled_by_its_agent_id() {
        let mut state = AdmissionState::new(limits());
        let now = Instant::now();
        let (registered, _) = handle(
            Request::RegisterRun {
                registration: registration("run-h", "sess-h"),
            },
            &mut state,
            now,
        );
        assert_eq!(registered, Response::Ack);
        let (admitted, _) = handle(
            Request::RequestAdmission {
                request: request("d-h", "run-h", "sess-h"),
            },
            &mut state,
            now,
        );
        assert_eq!(
            admitted,
            Response::Decision {
                decision: Decision::Granted
            }
        );
        let (bound, _) = handle(
            Request::BindAgentLease {
                dispatch_id: "d-h".into(),
                agent_id: "agent-01".into(),
                provenance: Provenance::Known,
            },
            &mut state,
            now,
        );
        assert_eq!(bound, Response::Ack);
        let (ended, _) = handle(
            Request::SettleByAgent {
                session_id: "sess-h".into(),
                agent_id: "agent-01".into(),
                spent_micros: None,
            },
            &mut state,
            now,
        );
        assert_eq!(
            ended,
            Response::AgentSettled {
                outcome: AgentSettleOutcome::Settled {
                    dispatch_id: "d-h".into()
                }
            }
        );
        let (again, _) = handle(
            Request::SettleByAgent {
                session_id: "sess-h".into(),
                agent_id: "agent-01".into(),
                spent_micros: None,
            },
            &mut state,
            now,
        );
        assert_eq!(
            again,
            Response::AgentSettled {
                outcome: AgentSettleOutcome::NothingBound
            },
            "a duplicate end finds nothing bound, and says so without failing"
        );
    }

    // A2: a daemon left running from another install answers perfectly
    // well and means something else by the same words. A `Pong` carries
    // the wire protocol so the skew is named at the first call.
    #[test]
    fn a_daemon_on_another_wire_protocol_is_named_not_guessed() {
        let dir = temp_dir("skew");
        let socket = dir.join("relais.sock");
        let listener = Listener::bind(&socket).expect("bind");
        let server = std::thread::spawn(move || {
            let stream = listener.accept().expect("accept").expect("a connection");
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut line = String::new();
            reader.read_line(&mut line).expect("read");
            let mut writer = stream;
            // What a v1 daemon says: a pong with no protocol field.
            writeln!(writer, r#"{{"kind":"pong","pid":1,"version":"0.1.6"}}"#).expect("write");
        });
        let err = Client::new(socket.clone())
            .ping()
            .expect_err("a v1 daemon is not this protocol");
        assert!(
            matches!(
                err,
                CoordinatorError::VersionSkew {
                    ours: PROTOCOL_VERSION,
                    theirs: 0,
                    ..
                }
            ),
            "{err}"
        );
        assert!(err.to_string().contains("coordinator stop"), "{err}");
        server.join().expect("server");
    }

    // A11: a daemon that is shutting down answers the connection it has
    // already accepted. Dropping it read at the client as "EOF while
    // parsing" and lost whatever the call was about.
    #[test]
    fn a_connection_accepted_while_shutting_down_is_answered() {
        let dir = temp_dir("draining");
        let socket = dir.join("relais.sock");
        let listener = Listener::bind(&socket).expect("bind");
        let state = Mutex::new(AdmissionState::new(limits()));
        let shutdown = std::sync::atomic::AtomicBool::new(true);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let stream = listener.accept().expect("accept").expect("a connection");
                handle_connection(stream, &state, &shutdown).expect("answered");
            });
            let err = Client::new(socket.clone())
                .request(&Request::RegisterRun {
                    registration: registration("run-1", "tab-a"),
                })
                .expect_err("nothing was applied");
            assert!(
                matches!(err, CoordinatorError::ShuttingDown { .. }),
                "{err}"
            );
        });
        assert!(
            lock_state(&state).status(Instant::now()).runs.is_empty(),
            "and the registration really was not applied"
        );
    }

    // A5: idle-exit decided under the lock and acted after it, so a
    // registration served in between was accepted by a daemon already
    // unlinking its socket. The decision is a value now, applied while
    // the guard is still held.
    #[test]
    fn idle_exit_is_decided_before_the_guard_is_dropped() {
        // Built by adding, never by subtracting from `now`: an `Instant`
        // is monotonic from an arbitrary epoch, and on Windows that
        // epoch is recent enough that `now - 10min` overflows.
        let t0 = Instant::now();
        let long_after = t0 + IDLE_EXIT * 2;
        assert_eq!(idle_step(Occupancy::Busy, None, t0), IdleStep::Busy);
        assert_eq!(
            idle_step(Occupancy::Busy, Some(t0), long_after),
            IdleStep::Busy,
            "one busy tick forgets how long it was idle before it"
        );
        assert_eq!(
            idle_step(Occupancy::Idle, None, t0),
            IdleStep::Idle { since: t0 }
        );
        let later = t0 + IDLE_EXIT - Duration::from_secs(1);
        assert_eq!(
            idle_step(Occupancy::Idle, Some(t0), later),
            IdleStep::Idle { since: t0 },
            "the clock runs from the first idle tick, not the latest"
        );
        assert_eq!(
            idle_step(Occupancy::Idle, Some(t0), t0 + IDLE_EXIT),
            IdleStep::Exit
        );
    }

    // A1: `SQLITE_BUSY` used to read as "nothing is running", so the
    // coordinator adopted nothing, re-granted every seat already taken
    // and lost every reservation with it. A coordinator that cannot see
    // the live set does not start — and leaves no endpoint behind.
    #[test]
    fn a_coordinator_that_cannot_read_the_live_set_does_not_start() {
        let dir = temp_dir("liveset");
        let socket = dir.join("relais.sock");
        let ledger_path = dir.join("ledger.sqlite");
        let ledger = Ledger::open(&ledger_path).expect("ledger");
        ledger
            .insert_run(
                &RunId::from_stored("run-x"),
                "/repo",
                None,
                &crate::ids::TaskId::from_stored("task-run-x"),
                "rk",
            )
            .expect("run");
        ledger
            .record_dispatch_intent(
                &DispatchId::from_stored("live"),
                &RunId::from_stored("run-x"),
                None,
                &serde_json::json!({}),
                0,
            )
            .expect("intent");
        ledger
            .attach_dispatch_process(
                &DispatchId::from_stored("live"),
                Some(Pid::new(std::process::id())),
                Some("sess"),
            )
            .expect("attach");
        // The table the live set is read from, gone under it. Any read
        // failure is the same failure to see what is running — a busy
        // database is the one that happens in the field, and it is not
        // something a test can produce on demand.
        rusqlite::Connection::open(&ledger_path)
            .expect("second connection")
            .execute("DROP TABLE dispatches", [])
            .expect("drop");
        let error = Coordinator::start(
            &socket,
            limits(),
            Some(&ledger),
            crate::admission::DEFAULT_AGENT_LEASE_TTL,
        )
        .err()
        .expect("start refuses");
        assert!(
            matches!(error, CoordinatorError::LiveSetUnreadable { .. }),
            "{error}"
        );
        assert!(
            !socket.exists(),
            "and no endpoint was left for a client to find"
        );
        assert!(
            !dir.join("coordinator.lock").exists(),
            "nor a lock nobody holds"
        );
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
        let none = |_: &str| None;
        fn set(
            name: &'static str,
            value: &'static str,
        ) -> impl Fn(&str) -> Option<std::ffi::OsString> {
            move |asked: &str| (asked == name).then(|| std::ffi::OsString::from(value))
        }
        // Every branch asserted unconditionally: the developer's own
        // shell cannot weaken this into `assert!(!id.is_empty())`.
        assert_eq!(
            resolve_session_id(set("RELAIS_SESSION_ID", "  sess-1  "), Some(7), 9),
            "sess-1",
            "relais's own variable wins, trimmed"
        );
        assert_eq!(
            resolve_session_id(set("CLAUDE_SESSION_ID", "sess-2"), Some(7), 9),
            "sess-2",
            "the harness's variable is the fallback"
        );
        assert_eq!(
            resolve_session_id(set("RELAIS_SESSION_ID", "   "), Some(7), 9),
            "unattributed-ppid-7",
            "a blank value is no value"
        );
        assert_eq!(
            resolve_session_id(none, Some(7), 9),
            "unattributed-ppid-7",
            "no variable: the parent names the tab, labelled as a guess"
        );
        assert_eq!(
            resolve_session_id(none, None, 9),
            "unattributed-pid-9",
            "no parent either: this process, still labelled"
        );
        assert!(!session_id().is_empty(), "and the ambient call answers");
    }
}
