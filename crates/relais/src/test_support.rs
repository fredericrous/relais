//! Helpers the in-file test modules share. `#[cfg(test)]`: nothing here
//! ships.
//!
//! It exists for one rule that was written out seventeen times and got
//! wrong in six of them. A test's scratch directory must be unique per
//! PROCESS and per CALL — the pid alone is shared by every test thread
//! in one binary, and a thread id is reused within it — and it must be
//! pre-cleaned, because a run of this suite that was killed leaves its
//! directories behind and the next run to draw that pid inherits them
//! (`tests.first-properties`). Written once, it is right once.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// The part of a fixture name that makes it this call's and no other's:
/// this process, and a monotonic counter within it.
pub(crate) fn unique_suffix() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// An empty directory named for `tag`, pre-cleaned and created.
pub(crate) fn temp_dir(tag: &str) -> PathBuf {
    make(std::env::temp_dir(), tag)
}

/// The same, under the shortest root the platform has, as a value that
/// removes the directory when it drops — panic or not, so the lifetime
/// is this type's business rather than a trailing statement every call
/// site may or may not remember.
///
/// A Unix socket path is capped at 104 bytes on macOS and 108 on Linux,
/// and the per-user temp directory alone can spend most of that, so an
/// endpoint goes directly under `/tmp` — still in a directory of this
/// call's own, never at a fixed path two runs would share (A15).
pub(crate) fn short_temp_dir(tag: &str) -> TempDir {
    TempDir(if cfg!(unix) {
        make(PathBuf::from("/tmp"), tag)
    } else {
        temp_dir(tag)
    })
}

/// A directory made by [`short_temp_dir`], removed on drop.
///
/// Cleanup is best effort: a leftover only costs disk, while a failure
/// here would hide the result the test actually produced.
pub(crate) struct TempDir(PathBuf);

impl std::ops::Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn make(root: PathBuf, tag: &str) -> PathBuf {
    let dir = root.join(format!("relais-{tag}-{}", unique_suffix()));
    // Best effort: usually absent, and `create_dir_all` below reports
    // anything that keeps the directory from being made.
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("a temp directory for the test");
    dir
}

/// Poll `ready` on a short interval until it holds or `patience` runs
/// out; `true` if it held.
///
/// The alternative — sleep long enough and hope — is the flake that
/// fails on a loaded CI runner and passes on the workstation, and it
/// pays the full sleep on every green run (`tests.first-properties`).
pub(crate) fn wait_until(patience: std::time::Duration, mut ready: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + patience;
    loop {
        if ready() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL);
    }
}

/// `true` if `holds` was still true at the end of `window`, sampled
/// throughout. For a property whose point is that nothing happens — a
/// process that must NOT die of the signal it was sent.
pub(crate) fn stays(window: std::time::Duration, mut holds: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + window;
    while std::time::Instant::now() < deadline {
        if !holds() {
            return false;
        }
        std::thread::sleep(POLL);
    }
    holds()
}

/// How often the two above look. Short enough that a ready test costs
/// the wait and nothing more.
const POLL: std::time::Duration = std::time::Duration::from_millis(10);

#[cfg(test)]
mod tests {
    use super::*;

    // The case a trailing `remove_dir_all(...).ok()` always missed: the
    // test body never reaches its last line. `Drop` runs during unwind
    // regardless.
    #[test]
    fn a_panicking_test_body_still_loses_its_directory() {
        let dir = short_temp_dir("panic-cleanup");
        let path = dir.to_path_buf();
        assert!(path.exists());
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _dir = dir;
            panic!("the test body never gets here on purpose");
        }));
        assert!(outcome.is_err());
        assert!(!path.exists());
    }
}
