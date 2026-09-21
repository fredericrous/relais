//! Process facts and signals, one vocabulary on every platform.
//!
//! The coordinator asks "is this PID alive" without sending anything
//! that could terminate it, tells a cancelled worker to stop, names the
//! parent process a CLI call belongs to, and the adapter kills a whole
//! worker tree on timeout or cancellation. Unix spells these `kill(2)`,
//! `getppid(2)` and process groups; Windows spells them process handles
//! and a Toolhelp snapshot. Nothing above this module spells them at all.

use std::io;
use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Is there a live process with this PID? Never a signal that could
/// terminate anything (SPEC §23: lease expiry does not prove death).
///
/// "Alive" means a process with this PID exists, not that it is the
/// process the caller bound. A PID the OS reused between the bind and
/// the question reads as alive, and nothing portable distinguishes it:
/// the process start time that would is `/proc` on Linux, `sysctl` on
/// macOS and a `GetProcessTimes` handle on Windows — three bindings and
/// no shared vocabulary. So the coordinator checks liveness AT BIND
/// (`AdmissionState::bind`), keeps the binding time, and treats reuse
/// within a lease's lifetime as undetected: the documented limit of C6.
pub fn alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    imp::alive(pid)
}

/// Which request was being delivered, for the error that says it was
/// not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKind {
    Terminate,
    Kill,
}

impl SignalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Terminate => "terminate",
            Self::Kill => "kill",
        }
    }
}

/// Why the OS would not deliver a signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalFailure {
    /// No process has that PID any more: it is already over. Nothing
    /// was delivered and nothing needs to be.
    Gone,
    /// A process with that PID exists and does not belong to this user
    /// (`EPERM`, `ERROR_ACCESS_DENIED`). Since relais only ever binds
    /// PIDs of processes it launched, that means the number was
    /// recycled: the worker is gone and a stranger has its PID.
    /// Escalating would mean signalling the stranger.
    NotOurs,
    /// Anything else the OS reported. It may pass.
    Refused,
}

/// A signal the OS would not take, naming what was being sent to whom.
#[derive(Debug)]
pub struct SignalError {
    pub pid: u32,
    pub signal: SignalKind,
    pub cause: SignalFailure,
    source: io::Error,
}

impl SignalError {
    /// Could this delivery succeed if it were tried again? A process
    /// that is gone, or one that is not ours, never will — there is
    /// nothing left to deliver to in either case, and retrying `NotOurs`
    /// means signalling a stranger every reconcile for ever.
    pub fn could_pass(&self) -> bool {
        match self.cause {
            SignalFailure::Gone | SignalFailure::NotOurs => false,
            SignalFailure::Refused => true,
        }
    }
}

impl std::fmt::Display for SignalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self.cause {
            SignalFailure::Gone => "no such process".to_string(),
            SignalFailure::NotOurs => {
                "the process with that pid is not ours, so the pid was recycled".to_string()
            }
            SignalFailure::Refused => self.source.to_string(),
        };
        write!(
            f,
            "{} could not be delivered to pid {}: {what}",
            self.signal.as_str(),
            self.pid
        )
    }
}

impl std::error::Error for SignalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Ask a worker to stop: SIGTERM on Unix, `TerminateProcess` on Windows,
/// which has no gentler cross-process request.
///
/// The result is the delivery, not the worker's answer. Discarding it
/// let the coordinator record a cancellation it had never sent, and read
/// `EPERM` on a recycled PID as a worker that had been asked politely
/// and would be killed next (A7).
pub fn terminate(pid: u32) -> Result<(), SignalError> {
    deliver(pid, SignalKind::Terminate)
}

/// Stop a worker that ignored `terminate`: SIGKILL on Unix, the same
/// `TerminateProcess` on Windows, which is already unblockable. The
/// coordinator escalates to this exactly once per cancelled dispatch,
/// one grace period after the polite request (SPEC §23 cancellation).
pub fn kill(pid: u32) -> Result<(), SignalError> {
    deliver(pid, SignalKind::Kill)
}

fn deliver(pid: u32, signal: SignalKind) -> Result<(), SignalError> {
    if pid == 0 {
        // PID 0 is "this process group" on Unix and no process at all
        // here: a dispatch with nothing bound, never a target.
        return Err(SignalError {
            pid,
            signal,
            cause: SignalFailure::Gone,
            source: io::Error::from(io::ErrorKind::NotFound),
        });
    }
    imp::signal(pid, signal).map_err(|source| SignalError {
        pid,
        signal,
        cause: imp::classify(pid, &source),
        source,
    })
}

/// The uid this process runs as: who the coordinator serves.
#[cfg(unix)]
pub fn uid() -> u32 {
    imp::uid()
}

/// The uid on the other end of a connected Unix socket — `SO_PEERCRED`
/// on Linux, `getpeereid` on macOS and the BSDs. The kernel answers, not
/// the peer, so it is evidence rather than a claim.
#[cfg(unix)]
pub fn peer_uid(fd: std::os::unix::io::RawFd) -> io::Result<u32> {
    imp::peer_uid(fd)
}

/// The parent of this process, when the platform can say.
pub fn parent_pid() -> Option<u32> {
    imp::parent_pid()
}

/// Arrange for `kill_tree` to reach everything the command spawns. Every
/// subprocess relais waits on goes through here.
pub fn own_process_group(command: &mut Command) {
    imp::own_process_group(command);
}

/// Kill the child and everything it spawned: a harness that started its
/// own children must not survive the kill (SPEC §23 cancellation).
pub fn kill_tree(child: &mut Child) -> io::Result<()> {
    match kill_group(child.id()) {
        GroupKill::Killed | GroupKill::NothingLeft => Ok(()),
        // The group refused the signal; the direct child is still ours
        // to stop, and its failure is the caller's to see.
        GroupKill::Refused { detail } => child
            .kill()
            .map_err(|e| io::Error::new(e.kind(), format!("process group: {detail}; child: {e}"))),
    }
}

/// Kill a whole process group by its id — which, for everything relais
/// spawns, is the child's own PID (`own_process_group`). Reported rather
/// than returned as a bare `io::Result` because "there was nothing left
/// to kill" is the ordinary outcome, not an error.
///
/// The leader may already have been reaped when this is called: a group
/// outlives its leader for as long as any member is in it, which is the
/// whole point — a backgrounded grandchild holding the stdout pipe is
/// exactly what this reaches. The reaped PID could in principle be
/// recycled AND made a group leader by an unrelated process between the
/// reap and this call; that window is microseconds wide and is the same
/// documented limit as `alive` (C6).
pub fn kill_group(pgid: u32) -> GroupKill {
    if pgid == 0 {
        return GroupKill::NothingLeft;
    }
    imp::kill_group(pgid)
}

/// What killing the worker's process group found. Recorded on the
/// `ProcessEnd`: a survivor relais could not stop is a fact about the
/// machine the run happened on, not something to discard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupKill {
    /// The group was empty: the child took everything it spawned with it.
    NothingLeft,
    /// Survivors were signalled — a backgrounded grandchild, an MCP
    /// server, a `npm run dev &`.
    Killed,
    /// The kill itself failed. Any survivor is still running.
    Refused { detail: String },
}

impl GroupKill {
    /// One line for a receipt or a log.
    pub fn describe(&self) -> String {
        match self {
            Self::NothingLeft => "process group: nothing left to kill".to_string(),
            Self::Killed => "process group: survivors killed".to_string(),
            Self::Refused { detail } => format!("process group: kill refused ({detail})"),
        }
    }
}

/// Whether everything the child wrote was captured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Captured {
    /// Both pipes reached end of file: this is everything that was written.
    Complete,
    /// A pipe was still open after the process group was killed and the
    /// drain grace elapsed — something inherited it and survived — or a
    /// read failed. What was read is kept; the rest is lost.
    Partial { detail: String },
}

impl Captured {
    pub fn is_complete(&self) -> bool {
        matches!(self, Self::Complete)
    }
}

/// An exclusive lock the KERNEL holds on a file for as long as the
/// holder lives, and releases however it dies — SIGKILL, a panic, a
/// power cut. `flock(LOCK_EX|LOCK_NB)` on Unix, `LockFile` on Windows,
/// which locks a byte range exclusively and fails immediately rather
/// than waiting — the same contract, spelled twice.
///
/// The file's CONTENT is never locked, on either platform: the Windows
/// lock covers one byte far past end-of-file, because its byte-range
/// locks are mandatory and a lock over the content would stop anybody
/// else reading the PID written there.
///
/// One caveat, and it is the kernel's: the lock belongs to the open
/// file description, and `fork` copies it. A process spawned while the
/// lock is held owns a copy of the descriptor until it `exec`s, which
/// closes it (Rust opens files close-on-exec). So for the microseconds
/// of somebody else's fork/exec, a released lock can still read as
/// held. Election already answers that by retrying — `ensure_running`
/// starts a daemon and pings until it answers — so the window costs a
/// poll, never a wedge.
///
/// This is what makes coordinator election atomic (SPEC §23: "atomically
/// elect one coordinator"). A PID written into a file is not: after a
/// SIGKILL the file stays, the PID gets recycled by an unrelated
/// process, and every later `relais run` reads a live PID and refuses to
/// elect until somebody deletes the file by hand. With the lock, the
/// file's content is informational and the question "is a coordinator
/// serving?" is answered by the kernel.
#[derive(Debug)]
pub struct LockFile {
    file: std::fs::File,
}

impl LockFile {
    /// Take the lock without blocking. `Ok(None)` = somebody else holds
    /// it (a live coordinator); `Err` = the file could not be opened or
    /// locked at all. The lock is released when the returned value is
    /// dropped, and by the kernel if this process dies holding it.
    pub fn try_acquire(path: &Path) -> io::Result<Option<Self>> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        if imp::try_lock_exclusive(&file)? {
            Ok(Some(Self { file }))
        } else {
            Ok(None)
        }
    }

    /// Record who holds it. Informational only — `relais coordinator
    /// status` and a human reading the state directory — never the
    /// election's evidence.
    pub fn write_pid(&mut self) -> io::Result<()> {
        use std::io::{Seek, SeekFrom, Write};
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        write!(self.file, "{}", std::process::id())?;
        self.file.flush()
    }
}

/// Why a supervised run could not be carried out. A process that RAN
/// and failed is a `ProcessEnd` with a non-zero status; this is the
/// launch itself going wrong, which is never a worker's fault.
#[derive(Debug)]
pub enum RunError {
    Spawn(io::Error),
    Wait(io::Error),
    /// The thread draining this pipe panicked; the output is lost, so
    /// the run cannot be reported as a completed attempt.
    ReaderPanicked(&'static str),
    /// The prompt could not be handed to the process in full. The worker
    /// ran on something other than what it was asked to do, so the
    /// attempt is interrupted — never completed (SPEC §9).
    PromptWrite(io::Error),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "spawn: {e}"),
            Self::Wait(e) => write!(f, "wait: {e}"),
            Self::ReaderPanicked(pipe) => write!(f, "{pipe} reader panicked"),
            Self::PromptWrite(e) => write!(f, "the prompt was not written in full: {e}"),
        }
    }
}

impl std::error::Error for RunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(e) | Self::Wait(e) | Self::PromptWrite(e) => Some(e),
            Self::ReaderPanicked(_) => None,
        }
    }
}

/// How a supervised process ended — one answer, not three booleans and
/// an `Option` that had to be read together. `exit_code: None,
/// timed_out: false, cancelled: false` used to mean "died on a signal",
/// which every caller had to know and two of them got wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ended {
    /// Ran to completion and reported this status.
    Exited(i32),
    /// The wall clock ran out and the process group was killed.
    TimedOut,
    /// A cancellation request arrived and the process group was killed.
    Cancelled,
    /// Died on a signal without reporting a status.
    Signalled,
}

impl Ended {
    /// Did it run to completion and report success? The only end that
    /// can carry a usable result.
    pub fn succeeded(self) -> bool {
        matches!(self, Self::Exited(0))
    }

    /// How it ended, for a log line or a receipt.
    pub fn describe(self) -> String {
        match self {
            Self::Exited(code) => format!("exit {code}"),
            Self::TimedOut => "timed out".to_string(),
            Self::Cancelled => "cancelled".to_string(),
            Self::Signalled => "no exit status".to_string(),
        }
    }
}

/// How a process run ended, with everything it wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEnd {
    pub ended: Ended,
    pub stdout: String,
    pub stderr: String,
    /// What killing the process group after the child ended found: a
    /// backgrounded grandchild is a fact about this launch, and the
    /// result of stopping it is recorded rather than discarded.
    pub group: GroupKill,
    /// Whether the pipes reached end of file within the drain grace.
    pub captured: Captured,
}

/// How long a pipe may keep being drained after the child ended and its
/// process group was killed. Readers stop at end of file, which normally
/// arrives the instant the last writer goes; a descendant that survived
/// the kill and holds the pipe open must not hold the run open with it.
const PIPE_DRAIN_GRACE: Duration = Duration::from_secs(5);

/// Runs a command with a wall timeout, streaming stdout+stderr into one
/// captured buffer. Reader threads own the pipes (a blocking read in the
/// wait loop would stall the timeout until the child spoke again); the
/// whole process group is killed once the child has ended, however it
/// ended, before the pipes are drained. On timeout or cancellation the
/// launch counts as interrupted, never as a completed attempt.
pub fn run_with_timeout(
    mut command: Command,
    wall_timeout: Duration,
    stdin_bytes: Option<Vec<u8>>,
    cancel: Option<&AtomicBool>,
    pid_slot: Option<&AtomicU32>,
) -> Result<ProcessEnd, RunError> {
    command.stdin(
        stdin_bytes
            .as_ref()
            .map_or(Stdio::null(), |_| Stdio::piped()),
    );
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    own_process_group(&mut command);
    let mut child = command.spawn().map_err(RunError::Spawn)?;
    if let Some(slot) = pid_slot {
        slot.store(child.id(), Ordering::SeqCst);
    }

    let prompt = stdin_bytes.map(|bytes| {
        let mut stdin = child.stdin.take().expect("stdin is piped when bytes exist");
        let (sent, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let written = stdin.write_all(&bytes).and_then(|()| stdin.flush());
            // The receiver is gone only if the run gave up waiting, which
            // it already recorded; there is nobody left to tell.
            let _ = sent.send(written);
        });
        done
    });

    let stdout = drain(child.stdout.take().expect("stdout piped"), "stdout");
    let stderr = drain(child.stderr.take().expect("stderr piped"), "stderr");

    let supervised = wait_for_exit(&mut child, wall_timeout, cancel).map_err(RunError::Wait)?;
    // The group is already dead (`wait_for_exit` kills it before reaping),
    // so a pipe still open is held by a survivor: give it the grace, then
    // take what was read.
    let stdout = stdout.collect()?;
    let stderr = stderr.collect()?;
    let captured = match (stdout.captured, stderr.captured) {
        (Captured::Complete, Captured::Complete) => Captured::Complete,
        (Captured::Partial { detail }, Captured::Complete)
        | (Captured::Complete, Captured::Partial { detail }) => Captured::Partial { detail },
        (Captured::Partial { detail }, Captured::Partial { detail: other }) => Captured::Partial {
            detail: format!("{detail}; {other}"),
        },
    };
    if let Some(done) = prompt {
        prompt_delivered(done, supervised.ended)?;
    }
    Ok(ProcessEnd {
        ended: supervised.ended,
        stdout: String::from_utf8_lossy(&stdout.bytes).into_owned(),
        stderr: String::from_utf8_lossy(&stderr.bytes).into_owned(),
        group: supervised.group,
        captured,
    })
}

/// Did the whole prompt reach the process? A write that failed or never
/// finished means the worker read something other than the prompt, which
/// interrupts the attempt (SPEC §9) — except when the process was killed,
/// where a broken pipe is the kill and the kill is already the outcome.
fn prompt_delivered(
    done: std::sync::mpsc::Receiver<io::Result<()>>,
    ended: Ended,
) -> Result<(), RunError> {
    let outcome = match done.recv_timeout(PIPE_DRAIN_GRACE) {
        Ok(written) => written,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("the process never read it (waited {PIPE_DRAIN_GRACE:?} after it ended)"),
        )),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            return Err(RunError::ReaderPanicked("stdin"))
        }
    };
    match (outcome, ended) {
        (Ok(()), _) => Ok(()),
        (Err(e), Ended::Exited(_) | Ended::Signalled) => Err(RunError::PromptWrite(e)),
        // Killed by the wall clock or a cancellation: the pipe broke
        // because relais stopped the process, and that is the outcome
        // already being reported.
        (Err(_), Ended::TimedOut | Ended::Cancelled) => Ok(()),
    }
}

/// A pipe being drained by its own thread, with the bytes read so far
/// reachable even if the thread is still blocked on a survivor.
struct Draining {
    name: &'static str,
    bytes: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    done: std::sync::mpsc::Receiver<io::Result<()>>,
}

/// What one pipe yielded.
struct Drained {
    bytes: Vec<u8>,
    captured: Captured,
}

fn drain(pipe: impl std::io::Read + Send + 'static, name: &'static str) -> Draining {
    let bytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let into = std::sync::Arc::clone(&bytes);
    let (sent, done) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let read = read_to_end(pipe, &into);
        // Nobody to tell is nobody to tell: the run already gave this
        // pipe up as partial and recorded it.
        let _ = sent.send(read);
    });
    Draining { name, bytes, done }
}

impl Draining {
    /// Wait out the drain grace, then take whatever was read. A reader
    /// that panicked loses its pipe entirely, which is a runner failure.
    fn collect(self) -> Result<Drained, RunError> {
        let captured = match self.done.recv_timeout(PIPE_DRAIN_GRACE) {
            Ok(Ok(())) => Captured::Complete,
            Ok(Err(e)) => Captured::Partial {
                detail: format!("{} could not be read in full: {e}", self.name),
            },
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Captured::Partial {
                detail: format!(
                    "{} was still open {PIPE_DRAIN_GRACE:?} after the process group was killed; \
                     a surviving descendant holds it",
                    self.name
                ),
            },
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(RunError::ReaderPanicked(self.name))
            }
        };
        // Poisoning means the reader panicked mid-write; the bytes it did
        // append are still the child's output, and the panic is reported
        // by the disconnected channel above, not hidden here.
        let bytes = match self.bytes.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        Ok(Drained { bytes, captured })
    }
}

/// How a supervised child ended, and what became of the group it led.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Supervised {
    pub ended: Ended,
    pub group: GroupKill,
}

/// Wait for a child under a wall clock and an optional cancel flag, then
/// kill its WHOLE process group — on every path, including a clean exit.
/// The one loop every subprocess in relais waits in.
///
/// The group kill is unconditional because a child that exits leaves its
/// descendants behind: a backgrounded `npm run dev &`, an MCP server, a
/// `cargo test` grandchild. They inherit the pipes, so draining stdout
/// would block until THEY end — past the wall clock, holding a write
/// lease and a worktree (audit V1) — and a `cargo test` left running in a
/// worktree about to be removed is a corrupted verification.
///
/// The kill happens before the child is reaped wherever possible, so the
/// group id is still the pid the OS has reserved for it.
pub fn wait_for_exit(
    child: &mut std::process::Child,
    wall_timeout: Duration,
    cancel: Option<&AtomicBool>,
) -> std::io::Result<Supervised> {
    enum Stopped {
        OnItsOwn,
        Cancelled,
        TimedOut,
    }
    let started = Instant::now();
    let mut reaped = None;
    let stopped = loop {
        if let Some(status) = child.try_wait()? {
            reaped = Some(status);
            break Stopped::OnItsOwn;
        }
        if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            break Stopped::Cancelled;
        }
        if started.elapsed() >= wall_timeout {
            break Stopped::TimedOut;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let group = kill_group(child.id());
    let status = match reaped {
        Some(status) => status,
        None => child.wait()?,
    };
    let ended = match stopped {
        // A killed process has no usable status: interrupted, never a
        // completed attempt. Decided here, not read from the status —
        // Windows reports a terminated process as exit 1, and 1 is a
        // usage error, not a kill.
        Stopped::Cancelled => Ended::Cancelled,
        Stopped::TimedOut => Ended::TimedOut,
        Stopped::OnItsOwn => match status.code() {
            Some(code) => Ended::Exited(code),
            None => Ended::Signalled,
        },
    };
    Ok(Supervised { ended, group })
}

/// Read a pipe to end of file, appending as it goes so a caller that
/// stops waiting still has what arrived.
fn read_to_end(mut pipe: impl std::io::Read, into: &std::sync::Mutex<Vec<u8>>) -> io::Result<()> {
    let mut chunk = [0u8; 8192];
    loop {
        let read = pipe.read(&mut chunk)?;
        if read == 0 {
            return Ok(());
        }
        match into.lock() {
            Ok(mut buffer) => buffer.extend_from_slice(&chunk[..read]),
            Err(poisoned) => poisoned.into_inner().extend_from_slice(&chunk[..read]),
        }
    }
}

#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    use super::GroupKill;

    pub fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 performs error checking only; no signal is sent.
        if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
            return true;
        }
        // EPERM: the process EXISTS and belongs to someone else. Reading
        // that as dead let the coordinator free a lease whose worker was
        // running, and it is the honest reading Windows already gives for
        // ERROR_ACCESS_DENIED. Only ESRCH means gone.
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    pub fn signal(pid: u32, signal: super::SignalKind) -> io::Result<()> {
        let number = match signal {
            // SIGTERM asks; SIGKILL is the escalation a whole grace
            // period later, and nothing follows it.
            super::SignalKind::Terminate => libc::SIGTERM,
            super::SignalKind::Kill => libc::SIGKILL,
        };
        // SAFETY: a signal to a process this user owns; a cancelled
        // dispatch bound its own PID to the reservation.
        if unsafe { libc::kill(pid as libc::pid_t, number) } == 0 {
            return Ok(());
        }
        Err(io::Error::last_os_error())
    }

    /// `kill(2)` says exactly which it was, so the process table is not
    /// consulted: `ESRCH` is gone and `EPERM` is a process that exists
    /// and belongs to somebody else.
    pub fn classify(_pid: u32, error: &io::Error) -> super::SignalFailure {
        match error.raw_os_error() {
            Some(code) if code == libc::ESRCH => super::SignalFailure::Gone,
            Some(code) if code == libc::EPERM => super::SignalFailure::NotOurs,
            _ => super::SignalFailure::Refused,
        }
    }

    pub fn uid() -> u32 {
        // SAFETY: getuid has no failure mode and touches no memory.
        unsafe { libc::getuid() }
    }

    #[cfg(target_os = "linux")]
    pub fn peer_uid(fd: std::os::unix::io::RawFd) -> io::Result<u32> {
        // SAFETY: `getsockopt` fills a `ucred` whose size is passed in
        // and back out; the value is only read when the call succeeds.
        unsafe {
            let mut cred: libc::ucred = std::mem::zeroed();
            let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
            let rc = libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                std::ptr::addr_of_mut!(cred).cast(),
                &mut len,
            );
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(cred.uid)
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn peer_uid(fd: std::os::unix::io::RawFd) -> io::Result<u32> {
        // SAFETY: `getpeereid` writes two uid_t this frame owns, and
        // they are only read when it succeeds.
        unsafe {
            let mut uid: libc::uid_t = 0;
            let mut gid: libc::gid_t = 0;
            if libc::getpeereid(fd, &mut uid, &mut gid) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(uid)
        }
    }

    pub fn try_lock_exclusive(file: &std::fs::File) -> io::Result<bool> {
        use std::os::unix::io::AsRawFd;
        // SAFETY: an advisory lock on a descriptor this process owns; the
        // kernel drops it when the descriptor closes or the process dies.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        // EWOULDBLOCK (== EAGAIN here) is the whole point: somebody holds it.
        match error.raw_os_error() {
            Some(libc::EWOULDBLOCK) => Ok(false),
            _ => Err(error),
        }
    }

    pub fn parent_pid() -> Option<u32> {
        // SAFETY: getppid has no failure mode and touches no memory.
        Some(unsafe { libc::getppid() } as u32)
    }

    pub fn own_process_group(command: &mut Command) {
        command.process_group(0);
    }

    pub fn kill_group(pgid: u32) -> GroupKill {
        // SAFETY: SIGKILL to a process group this process created, named
        // by the negative pid of the leader `own_process_group` made.
        if unsafe { libc::kill(-(pgid as libc::pid_t), libc::SIGKILL) } == 0 {
            return GroupKill::Killed;
        }
        let error = io::Error::last_os_error();
        // ESRCH: no member left — the ordinary outcome for a child that
        // spawned nothing and ended on its own.
        match error.raw_os_error() {
            Some(libc::ESRCH) => GroupKill::NothingLeft,
            _ => GroupKill::Refused {
                detail: error.to_string(),
            },
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    use super::GroupKill;

    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER,
        INVALID_HANDLE_VALUE, STILL_ACTIVE,
    };
    use windows_sys::Win32::Storage::FileSystem::LockFile;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, TerminateProcess, CREATE_NEW_PROCESS_GROUP,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };

    pub fn alive(pid: u32) -> bool {
        // SAFETY: a query-only handle; closed before returning.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                // A process that exists but belongs to someone else refuses
                // the handle: alive, not ours — the honest reading, as EPERM
                // is on Unix.
                return GetLastError() == ERROR_ACCESS_DENIED;
            }
            let mut code: u32 = 0;
            let ok = GetExitCodeProcess(handle, &mut code) != 0;
            CloseHandle(handle);
            ok && code == STILL_ACTIVE as u32
        }
    }

    /// Windows has one way to stop another process and it is already
    /// unblockable, so `Terminate` and `Kill` are the same call; the
    /// ladder is still walked once each, by the state machine.
    pub fn signal(pid: u32, _signal: super::SignalKind) -> io::Result<()> {
        // SAFETY: a terminate handle on a process this user owns; closed
        // on every path that opened one.
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let delivered = TerminateProcess(handle, 1);
            let error = io::Error::last_os_error();
            CloseHandle(handle);
            if delivered == 0 {
                return Err(error);
            }
            Ok(())
        }
    }

    /// Windows reports `ERROR_ACCESS_DENIED` for BOTH a process that
    /// belongs to somebody else and one that has already exited —
    /// `TerminateProcess` on an exited process is an access violation,
    /// not a no-op. The error alone therefore cannot tell the two
    /// apart, so the process table is asked: if nothing is running with
    /// that PID, the signal had nowhere to go.
    pub fn classify(pid: u32, error: &io::Error) -> super::SignalFailure {
        if !alive(pid) {
            return super::SignalFailure::Gone;
        }
        match error.raw_os_error().map(|code| code as u32) {
            // What `OpenProcess` reports for a PID that never existed.
            Some(ERROR_INVALID_PARAMETER) => super::SignalFailure::Gone,
            Some(ERROR_ACCESS_DENIED) => super::SignalFailure::NotOurs,
            _ => super::SignalFailure::Refused,
        }
    }

    pub fn try_lock_exclusive(file: &std::fs::File) -> io::Result<bool> {
        use std::os::windows::io::AsRawHandle;
        // ONE byte, four gigabytes past anything the file will ever
        // hold. Windows byte-range locks are MANDATORY, not advisory
        // like `flock`: a lock over the file's actual content would stop
        // every other process READING the PID inside it, which is the
        // one thing that content is for. A range nothing reads is a
        // pure token, and locking past end-of-file is legal.
        const TOKEN_OFFSET_HIGH: u32 = 1;
        // SAFETY: an exclusive byte-range lock on a handle this process
        // owns. `LockFile` never blocks — it fails immediately when the
        // range is already locked, which is `LOCK_NB` — and the lock
        // goes when the handle closes, including on process death.
        let locked = unsafe { LockFile(file.as_raw_handle() as _, 0, TOKEN_OFFSET_HIGH, 1, 0) };
        if locked != 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        // ERROR_LOCK_VIOLATION (33) is "somebody holds it", the Windows
        // spelling of EWOULDBLOCK; anything else is a real failure.
        match error.raw_os_error() {
            Some(33) => Ok(false),
            _ => Err(error),
        }
    }

    pub fn parent_pid() -> Option<u32> {
        let me = std::process::id();
        // SAFETY: a process snapshot walked with the documented entry
        // size set; the handle is closed on every path.
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut found = None;
            if Process32FirstW(snapshot, &mut entry) != 0 {
                loop {
                    if entry.th32ProcessID == me {
                        found = Some(entry.th32ParentProcessID);
                        break;
                    }
                    if Process32NextW(snapshot, &mut entry) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snapshot);
            found
        }
    }

    pub fn own_process_group(command: &mut Command) {
        // A new process group so a console break aimed at us does not
        // reach the child; the tree kill below does not depend on it.
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }

    pub fn kill_group(pgid: u32) -> GroupKill {
        // `taskkill /T` walks the tree by parent PID and forces every
        // member, which is what a process-group SIGKILL does on Unix. A
        // job object would be tighter, but `taskkill` ships with every
        // Windows and needs no handle plumbing through `Command`.
        //
        // The documented limit: Windows re-parents a process whose parent
        // exits, so a descendant of a worker that has ALREADY ended is no
        // longer in the tree this walks and survives. The drain grace in
        // `run_with_timeout` is what bounds the wait there; the launch
        // reports its output as partial rather than hanging.
        let status = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pgid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match status {
            Ok(status) if status.success() => GroupKill::Killed,
            // 128: "the process is not running" — nothing was left.
            Ok(status) if status.code() == Some(128) => GroupKill::NothingLeft,
            Ok(status) => GroupKill::Refused {
                detail: format!("taskkill: {status}"),
            },
            Err(e) => GroupKill::Refused {
                detail: format!("taskkill could not be launched: {e}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// A child that prints, then outlives any test timeout, in the
    /// platform's own shell: the point is the kill, not the script.
    fn slow_child() -> Command {
        if cfg!(windows) {
            let mut command = Command::new("cmd");
            command.args([
                "/C",
                "echo start && ping -n 60 127.0.0.1 > NUL && echo done",
            ]);
            command
        } else {
            let mut command = Command::new("sh");
            command.args(["-c", "echo start; sleep 30; echo done"]);
            command
        }
    }

    #[test]
    fn run_with_timeout_kills_slow_children() {
        let command = slow_child();
        let started = Instant::now();
        let pid_slot = AtomicU32::new(0);
        let end = run_with_timeout(
            command,
            Duration::from_millis(300),
            None,
            None,
            Some(&pid_slot),
        )
        .expect("runs");
        assert_ne!(pid_slot.load(Ordering::SeqCst), 0, "the PID was published");
        assert_eq!(
            end.ended,
            Ended::TimedOut,
            "must be marked interrupted by the wall clock"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(
            end.stdout.contains("start"),
            "output before the kill is kept: {}",
            end.stdout
        );
    }

    #[test]
    fn cancellation_kills_the_child_and_is_not_a_timeout() {
        let command = slow_child();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            flag.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let end = run_with_timeout(command, Duration::from_secs(30), None, Some(&cancel), None)
            .expect("runs");
        assert_eq!(
            end.ended,
            Ended::Cancelled,
            "a cancelled run has no terminal result"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// V1: a child that backgrounds something and exits leaves a
    /// descendant holding the stdout pipe. Draining it would block until
    /// THAT descendant ended — thirty seconds here, and in production
    /// past the wall clock, with the lease and the worktree still held.
    ///
    /// Unix only, because the guarantee is: a process GROUP outlives its
    /// leader and one signal reaches every member. Windows has no such
    /// thing — see the sibling test for what it can promise.
    #[cfg(unix)]
    #[test]
    fn a_backgrounded_grandchild_does_not_hold_the_launch_open() {
        let mut command = Command::new("sh");
        command.args(["-c", "echo started; sleep 30 & exit 0"]);
        let started = Instant::now();
        let end =
            run_with_timeout(command, Duration::from_secs(120), None, None, None).expect("runs");
        assert_eq!(end.ended, Ended::Exited(0), "the child itself succeeded");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the launch returned promptly: {:?}",
            started.elapsed()
        );
        assert_eq!(
            end.group,
            GroupKill::Killed,
            "the surviving grandchild was killed, and that is recorded"
        );
        assert!(end.captured.is_complete(), "{:?}", end.captured);
        assert!(end.stdout.contains("started"), "{}", end.stdout);
    }

    /// Windows re-parents a process whose parent exits, and `taskkill /T`
    /// walks the tree by parent: a descendant of an EXITED worker is out
    /// of reach, so it can still hold the pipe. What is promised there is
    /// the bound — the drain grace ends the wait and says the output is
    /// partial, instead of the launch hanging on a process nobody can
    /// name.
    #[cfg(windows)]
    #[test]
    fn a_surviving_descendant_cannot_hold_a_windows_launch_open_forever() {
        let mut command = Command::new("cmd");
        command.args(["/C", "echo started & start /b ping -n 60 127.0.0.1 > NUL"]);
        let started = Instant::now();
        let end =
            run_with_timeout(command, Duration::from_secs(120), None, None, None).expect("runs");
        assert_eq!(end.ended, Ended::Exited(0), "the child itself succeeded");
        assert!(
            started.elapsed() < PIPE_DRAIN_GRACE * 4,
            "the drain grace bounds the wait: {:?}",
            started.elapsed()
        );
        assert!(
            started.elapsed() < Duration::from_secs(55),
            "and it does not wait for the descendant: {:?}",
            started.elapsed()
        );
    }

    /// A child that spawns nothing leaves an empty group: recorded as
    /// such, not as a failed kill.
    #[cfg(unix)]
    #[test]
    fn a_child_that_spawned_nothing_leaves_an_empty_group() {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 0"]);
        let end =
            run_with_timeout(command, Duration::from_secs(10), None, None, None).expect("runs");
        assert_eq!(end.ended, Ended::Exited(0));
        assert_eq!(end.group, GroupKill::NothingLeft);
        assert!(end.group.describe().contains("nothing left"));
    }

    /// V12: the prompt is the task. A process that ends before reading it
    /// ran on something else, so the launch fails instead of reporting a
    /// completed attempt over a truncated prompt.
    #[test]
    fn a_prompt_the_process_never_read_fails_the_launch() {
        let mut command = Command::new("sh");
        // Exits at once without reading stdin; the payload is far larger
        // than any pipe buffer, so the write cannot complete.
        command.args(["-c", "exit 0"]);
        let prompt = vec![b'x'; 4 * 1024 * 1024];
        let error = run_with_timeout(command, Duration::from_secs(30), Some(prompt), None, None)
            .expect_err("a prompt that was not delivered is a failed launch");
        assert!(
            matches!(error, RunError::PromptWrite(_)),
            "{error}: {error:?}"
        );
    }

    /// …but a broken pipe caused by relais's own kill is the kill, not a
    /// prompt failure: the cancellation stays the outcome.
    #[test]
    fn a_cancelled_launch_reports_the_cancellation_not_the_broken_prompt() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            flag.store(true, Ordering::SeqCst);
        });
        let end = run_with_timeout(
            command,
            Duration::from_secs(30),
            Some(vec![b'y'; 4 * 1024 * 1024]),
            Some(&cancel),
            None,
        )
        .expect("a cancelled run is reported, not failed");
        assert_eq!(end.ended, Ended::Cancelled);
    }

    #[test]
    fn run_with_timeout_feeds_stdin_and_captures_output() {
        let mut command = Command::new("sh");
        command.args(["-c", "cat; echo processed"]);
        let end = run_with_timeout(
            command,
            Duration::from_secs(10),
            Some(b"prompt-bytes".to_vec()),
            None,
            None,
        )
        .expect("runs");
        assert_eq!(end.ended, Ended::Exited(0));
        assert!(end.stdout.contains("prompt-bytes"));
        assert!(
            end.stdout.contains("processed"),
            "{} {}",
            end.stdout,
            end.stderr
        );
    }

    #[test]
    fn this_process_is_alive_and_a_dead_pid_is_not() {
        assert!(alive(std::process::id()));
        assert!(!alive(0));
        // A PID no live process plausibly holds on either platform.
        assert!(!alive(u32::MAX - 7));
    }

    #[test]
    fn the_parent_is_known() {
        let parent = parent_pid().expect("the platform names a parent");
        assert_ne!(parent, std::process::id());
    }

    // C7: the lock the kernel releases on any death, including SIGKILL.
    #[test]
    fn an_exclusive_lock_is_held_once_and_released_on_drop() {
        let dir = std::env::temp_dir().join(format!("rl-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("coordinator.lock");
        let mut held = LockFile::try_acquire(&path)
            .expect("acquire")
            .expect("nobody holds it");
        held.write_pid().expect("pid");
        assert!(
            LockFile::try_acquire(&path).expect("probe").is_none(),
            "a second holder is refused while the first lives"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("read").trim(),
            std::process::id().to_string(),
            "the PID is written, as information"
        );
        drop(held);
        // With a little patience: a process this suite spawns elsewhere
        // can hold an inherited copy of the descriptor for the moment
        // between fork and exec, and the lock lives as long as any copy
        // does. Close-on-exec ends it, microseconds later — see the
        // note on `LockFile`.
        assert!(
            acquire_within(&path, std::time::Duration::from_secs(5)).is_some(),
            "the lock goes with its holder, leaving the file behind"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Take the lock, giving a fork/exec window time to close.
    fn acquire_within(path: &std::path::Path, patience: std::time::Duration) -> Option<LockFile> {
        let deadline = std::time::Instant::now() + patience;
        loop {
            if let Ok(Some(lock)) = LockFile::try_acquire(path) {
                return Some(lock);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    // C5: the escalation the coordinator reaches for one grace period
    // after a cancelled worker ignored `terminate`.
    #[test]
    fn kill_ends_a_process_that_ignores_terminate() {
        let mut command = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/C", "ping -n 60 127.0.0.1 > NUL"]);
            c
        } else {
            // SIGTERM ignored: only the hard kill ends this one.
            let mut c = Command::new("sh");
            c.args(["-c", "trap '' TERM; sleep 60"]);
            c
        };
        let mut child = command.spawn().expect("spawn");
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(200));
        terminate(pid).expect("the polite request is delivered");
        std::thread::sleep(std::time::Duration::from_millis(200));
        #[cfg(unix)]
        assert!(alive(pid), "the worker ignored the polite request");
        match kill(pid) {
            Ok(()) => {}
            // Windows has one way to stop another process and it is
            // already unblockable: the polite request ended it, so the
            // escalation finds nothing left to deliver to. That is a
            // delivery failure that must NOT be retried, which is the
            // property under test here.
            Err(e) => {
                let unblockable_terminate = cfg!(windows);
                assert!(
                    unblockable_terminate,
                    "the escalation was not delivered: {e}"
                );
                assert_eq!(e.cause, SignalFailure::Gone, "{e}");
                assert!(!e.could_pass(), "{e}");
            }
        }
        let _ = child.wait();
        assert!(!alive(pid));
    }

    // A7: the delivery was discarded, so the coordinator recorded a
    // cancellation it had never sent. What the OS refused, and whether
    // trying again could ever work, is the whole answer.
    #[test]
    fn a_signal_the_os_refuses_says_which_refusal_it_was() {
        // Nothing is bound to the lease: PID 0 is "this process group"
        // on Unix, and never a worker.
        let unbound = terminate(0).expect_err("pid 0 is not a target");
        assert_eq!(unbound.cause, SignalFailure::Gone);
        assert_eq!(unbound.signal, SignalKind::Terminate);
        assert!(!unbound.could_pass(), "there is nothing to deliver to");

        // A PID no live process plausibly holds: gone, and no escalation
        // ladder is worth walking for it.
        let gone = kill(u32::MAX - 7).expect_err("no such process");
        assert_eq!(gone.cause, SignalFailure::Gone);
        assert_eq!(gone.signal, SignalKind::Kill);
        assert!(!gone.could_pass());
        assert!(gone.to_string().contains("no such process"), "{gone}");

        // A live process this user does not own: `alive` says yes (it is
        // conservative), and the signal says the PID was recycled — so
        // the ladder stops rather than signalling a stranger for ever.
        #[cfg(unix)]
        {
            // PID 1 is init/launchd: alive, and not ours.
            assert!(alive(1), "init is alive");
            if uid() != 0 {
                let stranger = terminate(1).expect_err("not ours");
                assert_eq!(stranger.cause, SignalFailure::NotOurs);
                assert!(
                    !stranger.could_pass(),
                    "retrying means signalling somebody else's process"
                );
                assert!(stranger.to_string().contains("recycled"), "{stranger}");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_uid_we_serve_is_the_one_we_run_as() {
        let me = uid();
        let (here, there) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        use std::os::unix::io::AsRawFd;
        assert_eq!(peer_uid(here.as_raw_fd()).expect("peer"), me);
        assert_eq!(peer_uid(there.as_raw_fd()).expect("peer"), me);
    }

    #[test]
    fn kill_tree_ends_a_child_and_its_grandchild() {
        let mut command = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/C", "ping -n 60 127.0.0.1 > NUL"]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", "sleep 60 & sleep 60"]);
            c
        };
        own_process_group(&mut command);
        let mut child = command.spawn().expect("spawn");
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(alive(pid));
        kill_tree(&mut child).expect("kill");
        let status = child.wait().expect("wait");
        assert!(!status.success());
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!alive(pid));
    }
}
