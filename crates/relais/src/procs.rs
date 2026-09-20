//! Process facts and signals, one vocabulary on every platform.
//!
//! The coordinator asks "is this PID alive" without sending anything
//! that could terminate it, tells a cancelled worker to stop, names the
//! parent process a CLI call belongs to, and the adapter kills a whole
//! worker tree on timeout or cancellation. Unix spells these `kill(2)`,
//! `getppid(2)` and process groups; Windows spells them process handles
//! and a Toolhelp snapshot. Nothing above this module spells them at all.

use std::io;
use std::process::{Child, Command};

/// Is there a live process with this PID? Never a signal that could
/// terminate anything (SPEC §23: lease expiry does not prove death).
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

#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};

    pub fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 performs error checking only; no signal is sent.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    pub fn terminate(pid: u32) {
        // SAFETY: SIGTERM to a process this user owns; a cancelled dispatch
        // bound its own PID to the reservation.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
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
