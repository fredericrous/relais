//! File-system helpers with no dependency on the rest of the crate.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Replace a file's contents so that a failed write leaves the previous
/// contents intact: a temporary file IN THE SAME DIRECTORY (rename is
/// only atomic within a filesystem), flushed and fsynced, then renamed
/// over the destination. `rename(2)` replaces atomically on POSIX and
/// `ReplaceFile`/`MoveFileEx` semantics give the same guarantee on
/// Windows, where `std::fs::rename` overwrites an existing file.
///
/// The temporary file is removed on every failure path, so a full disk
/// or a permission error leaves nothing behind but the original.
pub(crate) fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    write_atomic_with_mode(path, contents, None)
}

/// [`write_atomic`], with the staged file given `mode` (Unix permission
/// bits) BEFORE the rename, and the directory fsynced after it. A file
/// that holds grants must not pass through a moment where it is world
/// readable, and a rename that is not on disk can come back as the old
/// file after a crash. `None` leaves the mode to the umask, as
/// `write_atomic` always has. Windows has no mode bits; there it is
/// `write_atomic` plus nothing.
pub fn write_atomic_with_mode(
    path: &Path,
    contents: &str,
    mode: Option<u32>,
) -> std::io::Result<()> {
    // A sibling, hidden, and unique per process and per call: two relais
    // processes installing at once must not share a staging file.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "relais-owned".to_string());
    let parent = path.parent().unwrap_or(Path::new("."));
    let temp = parent.join(format!(
        ".{file_name}.relais-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let staged = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temp)?;
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        }
        #[cfg(not(unix))]
        let _ = mode;
        file.write_all(contents.as_bytes())?;
        file.flush()?;
        // Durability, not just visibility: without this the rename can be
        // ordered before the data on a crash, and the file comes back
        // empty rather than old.
        file.sync_all()?;
        Ok(())
    })();
    if let Err(e) = staged {
        // Best effort: the staging file is already unreachable by name
        // for anything but this call, and the error to report is the
        // write's, not the cleanup's.
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&temp, path) {
        // Same: the error to report is the rename's; a staging file
        // that cannot be removed is named by this call alone.
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    // The rename lives in the directory: fsync it so the new name is
    // durable, not only the new bytes. Unix only — Windows cannot open
    // a directory as a file, and NTFS journals the rename itself.
    #[cfg(unix)]
    if mode.is_some() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// A directory only its owner can enter: created if absent and, when it
/// already exists, forced to 0700 whatever the umask or an earlier
/// version left it. Windows has no mode bits; there it is
/// `create_dir_all`.
pub(crate) fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Append `line` plus a newline to `path` for the owner alone: opened
/// `O_APPEND | O_CREAT` with mode 0600, and written with ONE `write_all`,
/// so two writers append whole lines, never halves of them.
pub(crate) fn append_private_line(path: &Path, line: &str) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let mut bytes = Vec::with_capacity(line.len() + 1);
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\n');
    file.write_all(&bytes)
}

/// An exclusive lock the KERNEL holds on a file for as long as the
/// holder lives, and releases however it dies: SIGKILL, a panic, a
/// power cut. It is `File::try_lock`, which is `flock(LOCK_EX|LOCK_NB)`
/// on Unix and `LockFileEx` with `FAIL_IMMEDIATELY` on Windows, so it
/// never waits. Nothing here names a platform API.
///
/// On Windows that lock is mandatory and covers the whole file, so
/// another process cannot read the PID written by [`LockFile::write_pid`]
/// while the lock is held; the PID is informational, never the election's
/// evidence.
///
/// One caveat, and it is the kernel's: the lock belongs to the open
/// file description, and `fork` copies it. A process spawned while the
/// lock is held owns a copy of the descriptor until it `exec`s, which
/// closes it (Rust opens files close-on-exec). So for the microseconds
/// of somebody else's fork/exec, a released lock can still read as
/// held. Election already answers that by retrying, so the window costs
/// a poll, never a wedge.
///
/// This is what makes coordinator election atomic (SPEC §23: "atomically
/// elect one coordinator"). A PID written into a file is not: after a
/// SIGKILL the file stays and the PID gets recycled by an unrelated
/// process. With the lock, the file's content is informational and the
/// question "is a coordinator serving?" is answered by the kernel.
#[derive(Debug)]
pub struct LockFile {
    file: std::fs::File,
}

impl LockFile {
    /// Take the lock without blocking. `Ok(None)` = somebody else holds
    /// it (a live coordinator); `Err` = the file could not be opened or
    /// locked at all. The lock is released when the returned value is
    /// dropped, and by the kernel if this process dies holding it.
    pub fn try_acquire(path: &Path) -> std::io::Result<Option<Self>> {
        Self::try_acquire_with(path, std::fs::OpenOptions::new())
    }

    /// [`LockFile::try_acquire`], with the file created 0600 on Unix.
    fn try_acquire_private(path: &Path) -> std::io::Result<Option<Self>> {
        #[cfg(unix)]
        let options = {
            use std::os::unix::fs::OpenOptionsExt;
            let mut options = std::fs::OpenOptions::new();
            options.mode(0o600);
            options
        };
        #[cfg(not(unix))]
        let options = std::fs::OpenOptions::new();
        Self::try_acquire_with(path, options)
    }

    fn try_acquire_with(
        path: &Path,
        mut options: std::fs::OpenOptions,
    ) -> std::io::Result<Option<Self>> {
        let file = options
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }

    /// Record who holds it. Informational only: `relais coordinator
    /// status` and a human reading the state directory, never the
    /// election's evidence.
    pub fn write_pid(&mut self) -> std::io::Result<()> {
        use std::io::{Seek, SeekFrom};
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        write!(self.file, "{}", std::process::id())?;
        self.file.flush()
    }
}

/// The sibling `<path><suffix>`: the name with `suffix` appended whole.
fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Rename `path` to `<path>.1` when it is larger than `max` bytes,
/// replacing the previous generation: one generation is kept.
///
/// The rename happens under a try-lock on `<path>.lock` (created 0600),
/// so two processes that both saw the file over the cap do not rotate
/// twice and destroy the generation the first one made. A held lock
/// skips the rotation: whoever holds it is rotating. The size is read
/// again under the lock, and the file is renamed only if it is still
/// over `max`. Nothing is truncated, so a writer that still holds the
/// old descriptor lands its whole line in `.1`.
///
/// Every error is returned; a caller that treats rotation as best effort
/// ignores it and the next call retries.
pub fn rotate_if_over(path: &Path, max: u64) -> std::io::Result<()> {
    let Some(_lock) = LockFile::try_acquire_private(&sibling_with_suffix(path, ".lock"))? else {
        return Ok(());
    };
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.len() > max => {
            std::fs::rename(path, sibling_with_suffix(path, ".1"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_renames_only_a_file_over_the_cap() {
        let dir = crate::test_support::temp_dir("rotate");
        let path = dir.join("log");
        std::fs::write(&path, b"12345").expect("write");
        rotate_if_over(&path, 5).expect("at the cap");
        assert!(path.exists(), "a file at the cap stays");
        rotate_if_over(&path, 4).expect("over the cap");
        assert!(!path.exists());
        assert_eq!(std::fs::read(dir.join("log.1")).expect("read"), b"12345");
        rotate_if_over(&path, 0).expect("absent file");
        std::fs::remove_dir_all(&dir).ok(); // best-effort scratch cleanup
    }

    #[cfg(unix)]
    #[test]
    fn a_held_rotation_lock_skips_the_rotation() {
        let dir = crate::test_support::temp_dir("rotate-held");
        let path = dir.join("log");
        std::fs::write(&path, b"12345").expect("write");
        let held = LockFile::try_acquire_private(&dir.join("log.lock"))
            .expect("acquire")
            .expect("free");
        rotate_if_over(&path, 1).expect("skipped");
        assert!(path.exists(), "the holder is rotating, not this call");
        drop(held);
        rotate_if_over(&path, 1).expect("rotated");
        assert!(!path.exists());
        std::fs::remove_dir_all(&dir).ok(); // best-effort scratch cleanup
    }
}
