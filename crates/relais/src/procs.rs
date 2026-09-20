//! Process facts and signals, one vocabulary on every platform.
//!
//! The coordinator asks "is this PID alive" without sending anything
//! that could terminate it, tells a cancelled worker to stop, names the
//! parent process a CLI call belongs to, and the adapter kills a whole
//! worker tree on timeout or cancellation. Unix spells these `kill(2)`,
//! `getppid(2)` and process groups; Windows spells them process handles
//! and a Toolhelp snapshot. Nothing above this module spells them at all.

use std::io;
use std::path::Path;
use std::process::{Child, Command};

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

/// Ask a worker to stop: SIGTERM on Unix, `TerminateProcess` on Windows,
/// which has no gentler cross-process request.
pub fn terminate(pid: u32) {
    if pid == 0 {
        return;
    }
    imp::terminate(pid);
}

/// Stop a worker that ignored `terminate`: SIGKILL on Unix, the same
/// `TerminateProcess` on Windows, which is already unblockable. The
/// coordinator escalates to this exactly once per cancelled dispatch,
/// one grace period after the polite request (SPEC §23 cancellation).
pub fn kill(pid: u32) {
    if pid == 0 {
        return;
    }
    imp::kill(pid);
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
    imp::kill_tree(child)
}

/// An exclusive lock the KERNEL holds on a file for as long as the
/// holder lives, and releases however it dies — SIGKILL, a panic, a
/// power cut. `flock(LOCK_EX|LOCK_NB)` on Unix, `LockFile` on Windows,
/// which locks a byte range exclusively and fails immediately rather
/// than waiting — the same contract, spelled twice.
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

#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};

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

    pub fn terminate(pid: u32) {
        // SAFETY: SIGTERM to a process this user owns; a cancelled dispatch
        // bound its own PID to the reservation.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }

    pub fn kill(pid: u32) {
        // SAFETY: SIGKILL to a process this user owns, after SIGTERM was
        // ignored for a whole grace period.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
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

    pub fn kill_tree(child: &mut Child) -> io::Result<()> {
        // The whole group, which `own_process_group` made the child lead.
        let pgid = child.id() as i32;
        // SAFETY: SIGKILL to a process group this process created.
        let result = unsafe { libc::kill(-pgid, libc::SIGKILL) };
        if result == -1 {
            child.kill()
        } else {
            Ok(())
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command};

    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ACCESS_DENIED, INVALID_HANDLE_VALUE, STILL_ACTIVE,
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

    pub fn terminate(pid: u32) {
        // SAFETY: a terminate handle on a process this user owns; closed
        // before returning.
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if handle.is_null() {
                return;
            }
            TerminateProcess(handle, 1);
            CloseHandle(handle);
        }
    }

    /// Windows has one way to stop another process and it is already
    /// unblockable, so the escalation is the same call.
    pub fn kill(pid: u32) {
        terminate(pid);
    }

    pub fn try_lock_exclusive(file: &std::fs::File) -> io::Result<bool> {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: an exclusive byte-range lock over the whole file on a
        // handle this process owns. `LockFile` never blocks — it fails
        // immediately when any part of the range is already locked, which
        // is `LOCK_NB` — and the lock goes when the handle closes,
        // including on process death.
        let locked = unsafe { LockFile(file.as_raw_handle() as _, 0, 0, u32::MAX, u32::MAX) };
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

    pub fn kill_tree(child: &mut Child) -> io::Result<()> {
        // `taskkill /T` walks the tree by parent PID and forces every
        // member, which is what a process-group SIGKILL does on Unix. A
        // job object would be tighter, but `taskkill` ships with every
        // Windows and needs no handle plumbing through `Command`.
        let status = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match status {
            Ok(status) if status.success() => Ok(()),
            _ => child.kill(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        terminate(pid);
        std::thread::sleep(std::time::Duration::from_millis(200));
        #[cfg(unix)]
        assert!(alive(pid), "the worker ignored the polite request");
        kill(pid);
        let _ = child.wait();
        assert!(!alive(pid));
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
