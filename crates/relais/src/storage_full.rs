//! relais's own storage running out (SPEC §10; plan B4).
//!
//! When relais cannot write its own record (a log, a snapshot, a ref, a
//! patch, the ledger) the run cannot be trusted to keep going, and the
//! little that can still be saved has to be saved in a fixed order. relais
//! therefore keeps a 64 MiB ballast file in the state directory, made at
//! start when space allows. On a full disk it deletes the ballast, checks
//! that the volume really has more room afterwards (an APFS local snapshot
//! can keep a deleted file's blocks), and only then persists: the candidate
//! ref, the `blocked (relais_storage_full)` transition, the follow-up note.
//! Everything here is a calculation on an error or a small file action; the
//! runner decides what the outcome is.
//!
//! Failure injection is a test-only hook ([`at`]): a failpoint a
//! test arms for one thread, naming the write [`Site`] that fails with
//! `StorageFull`. A release build compiles the hook to nothing, and no test
//! fills a real disk.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::disk::{DiskProbe, StatvfsProbe};

/// The ballast's size: 64 MiB.
pub const BALLAST_BYTES: u64 = 64 * 1024 * 1024;

/// The ballast's file name in the state directory.
pub const BALLAST_FILE: &str = "ballast";

/// The write relais makes that can find the disk full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site {
    /// A check's log.
    Log,
    /// Recording a candidate snapshot.
    Snapshot,
    /// Updating the candidate's ref.
    Ref,
    /// Writing a patch file.
    Patch,
    /// A ledger write.
    Ledger,
}

/// Is this the disk being full: `StorageFull`, or the raw `ENOSPC`.
pub fn is_full(error: &io::Error) -> bool {
    const ENOSPC: i32 = 28;
    error.kind() == io::ErrorKind::StorageFull || error.raw_os_error() == Some(ENOSPC)
}

/// Is this SQLite's `SQLITE_FULL`.
pub fn sqlite_full(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::DiskFull
    )
}

/// What [`ensure_ballast`] found or did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BallastState {
    /// Already there at full size.
    Present,
    /// Made now.
    Created,
    /// Not made: the volume has less than twice the ballast free (or its
    /// space cannot be read), so taking 64 MiB would itself risk the disk.
    NoRoom,
    /// Not made: the write failed.
    Failed(String),
}

/// The ballast's path under the state directory.
pub fn ballast_path(state_dir: &Path) -> PathBuf {
    state_dir.join(BALLAST_FILE)
}

/// Make the ballast of `size` bytes at `path` when it is not there and the
/// volume has at least twice `size` available. The bytes are written, not
/// reserved sparsely, so deleting the file really frees blocks.
pub fn ensure_ballast(path: &Path, size: u64, probe: &dyn DiskProbe) -> BallastState {
    if std::fs::metadata(path).is_ok_and(|m| m.len() >= size) {
        return BallastState::Present;
    }
    let roomy = probe
        .volume_of(path)
        .is_some_and(|volume| volume.available >= size.saturating_mul(2));
    if !roomy {
        return BallastState::NoRoom;
    }
    let write = || -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let partial = path.with_extension("partial");
        let chunk = vec![0u8; 1024 * 1024];
        let mut remaining = size;
        {
            use io::Write as _;
            let mut file = std::fs::File::create(&partial)?;
            while remaining > 0 {
                let take = remaining.min(chunk.len() as u64) as usize;
                file.write_all(&chunk[..take])?;
                remaining -= take as u64;
            }
            file.sync_all()?;
        }
        std::fs::rename(&partial, path)
    };
    match write() {
        Ok(()) => BallastState::Created,
        Err(e) => {
            let _ = std::fs::remove_file(path.with_extension("partial"));
            BallastState::Failed(e.to_string())
        }
    }
}

/// What deleting the ballast did to the space available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freed {
    /// The volume has more room now: persisting can go on.
    Rose,
    /// The file went (or could not) and the space did not rise, as when a
    /// local snapshot keeps its blocks: nothing can be persisted safely.
    NoRise,
    /// There was no ballast to delete.
    Missing,
}

/// Delete the ballast and confirm the space rose.
pub fn free_ballast(path: &Path, probe: &dyn DiskProbe) -> Freed {
    if !path.is_file() {
        return Freed::Missing;
    }
    let available = || probe.volume_of(path).map(|volume| volume.available);
    let before = available();
    if std::fs::remove_file(path).is_err() {
        return Freed::NoRise;
    }
    match (before, available()) {
        (Some(before), Some(after)) if after > before => Freed::Rose,
        _ => Freed::NoRise,
    }
}

/// The probe the runner reads space through: `statvfs`, or a test's stub.
pub fn probe() -> Arc<dyn DiskProbe> {
    #[cfg(test)]
    if let Some(stub) = failpoint::stub_probe() {
        return stub;
    }
    Arc::new(StatvfsProbe)
}

/// The failure-injection hook at a write site: in a test build it fails
/// with `StorageFull` when the test armed this site on this thread; in a
/// release build it is nothing.
#[inline]
pub fn at(site: Site) -> io::Result<()> {
    #[cfg(test)]
    return failpoint::hit(site);
    #[cfg(not(test))]
    {
        let _ = site;
        Ok(())
    }
}

pub mod failpoint;

#[cfg(test)]
mod tests {
    use super::failpoint::BallastProbe;
    use super::*;

    #[test]
    fn storage_full_is_recognised_in_io_and_sqlite_errors() {
        assert!(is_full(&io::Error::new(io::ErrorKind::StorageFull, "full")));
        assert!(is_full(&io::Error::from_raw_os_error(28)));
        assert!(!is_full(&io::Error::other("other")));
        let sqlite = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
            None,
        );
        assert!(sqlite_full(&sqlite));
    }

    #[test]
    fn the_ballast_is_made_only_when_space_allows_and_remade_on_the_next_start() {
        let dir = crate::test_support::temp_dir("ballast");
        let path = ballast_path(&dir);
        let size = 4096;
        let tight = BallastProbe {
            ballast: path.clone(),
            low: size,
            high: size,
            keeps_blocks: true,
        };
        // Too little room: nothing is written.
        assert_eq!(ensure_ballast(&path, size, &tight), BallastState::NoRoom);
        assert!(!path.exists());
        let roomy = BallastProbe {
            ballast: path.clone(),
            low: size * 10,
            high: size * 100,
            keeps_blocks: false,
        };
        assert_eq!(ensure_ballast(&path, size, &roomy), BallastState::Created);
        assert_eq!(std::fs::metadata(&path).expect("made").len(), size);
        assert_eq!(ensure_ballast(&path, size, &roomy), BallastState::Present);

        // A full disk frees it, and the next start with room makes it again.
        assert_eq!(free_ballast(&path, &roomy), Freed::Rose);
        assert!(!path.exists());
        assert_eq!(free_ballast(&path, &roomy), Freed::Missing);
        assert_eq!(ensure_ballast(&path, size, &roomy), BallastState::Created);
        assert!(path.exists());
    }

    #[test]
    fn a_ballast_whose_blocks_a_snapshot_keeps_does_not_count_as_freed() {
        let dir = crate::test_support::temp_dir("ballast-snapshot");
        let path = ballast_path(&dir);
        std::fs::write(&path, vec![0u8; 4096]).expect("write");
        let probe = BallastProbe {
            ballast: path.clone(),
            low: 1000,
            high: 1_000_000,
            keeps_blocks: true,
        };
        assert_eq!(free_ballast(&path, &probe), Freed::NoRise);
    }
}
