//! File-system helpers with no dependency on the rest of the crate.

use std::io::Write;
use std::path::Path;

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
    Ok(())
}
