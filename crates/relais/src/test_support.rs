//! Helpers the in-file test modules share, and — because an integration
//! suite under `tests/` builds against this crate as an ordinary
//! dependency and cannot see `pub(crate)` — the three integration
//! suites too. Nothing here runs on a production path; it is `pub`
//! rather than `#[cfg(test)]` only because that is the one visibility
//! an external test crate can reach.
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

/// The one prefix every scratch directory a test can strand carries.
/// `short_temp_dir`, the three integration suites' own world types, and
/// `doctor`'s stray scan all read this constant rather than each naming
/// the prefix itself — the drift between `short_temp_dir`'s `relais-`
/// and the suites' hand-rolled `rl-hook-`/`rl-pt-`/`rl-it-` is exactly
/// what let 1057 stray directories accumulate while the scan, counting
/// only the former, reported none. Kept short: these directories hold
/// Unix sockets, and a socket path is capped near 104 bytes on macOS.
pub const SCRATCH_PREFIX: &str = "rl-";

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

/// An empty directory named for `tag`, pre-cleaned and created, as a
/// value that removes it on drop — the same lifetime rule as
/// [`short_temp_dir`], for the same reason: a trailing `remove_dir_all`
/// does not run when a test panics, which is exactly when the artefacts
/// are least wanted and most likely.
///
/// Under `std::env::temp_dir()` rather than `/tmp`, so it is NOT where a
/// socket may live — on macOS that root is `/var/folders/...`, long
/// enough to spend most of a Unix socket path's 104-byte cap. It is also
/// a root `doctor`'s stray scan has to know about, since anything
/// stranded here is invisible under `/tmp`.
/// `pub` for the same reason `short_temp_dir` is: this module is test
/// scaffolding a test crate has to be able to see, and nothing in the
/// library build calls it, so `pub(crate)` alone reads as dead code to
/// `clippy -D warnings`. It cannot be `#[cfg(test)]` instead —
/// `scripts/check-module-cycles.py` allows one top-level `#[cfg(test)]`
/// item per file, the trailing `mod tests`, so that it knows where test
/// code starts.
pub fn temp_dir(tag: &str) -> TempDir {
    TempDir(make(std::env::temp_dir(), tag))
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
pub fn short_temp_dir(tag: &str) -> TempDir {
    // Both arms go through `make`, so neither can end up outside
    // `SCRATCH_PREFIX` and therefore outside what doctor's scan counts.
    TempDir(if cfg!(unix) {
        make(PathBuf::from("/tmp"), tag)
    } else {
        make(std::env::temp_dir(), tag)
    })
}

/// A directory made by [`short_temp_dir`], removed on drop.
///
/// Cleanup is best effort: a leftover only costs disk, while a failure
/// here would hide the result the test actually produced.
pub struct TempDir(PathBuf);

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
    let dir = root.join(format!("{SCRATCH_PREFIX}{tag}-{}", unique_suffix()));
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
pub fn wait_until(patience: std::time::Duration, mut ready: impl FnMut() -> bool) -> bool {
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
pub fn stays(window: std::time::Duration, mut holds: impl FnMut() -> bool) -> bool {
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

    /// The assertion that would have caught the present hole: a
    /// directory this helper creates carries `SCRATCH_PREFIX`, so
    /// `doctor::count_strays` — reading the same constant — counts it.
    /// Proved against an isolated root rather than the real `/tmp`,
    /// which already carries whatever else this machine's test runs
    /// have stranded.
    #[test]
    fn a_directory_short_temp_dir_makes_is_countable_by_doctors_scan() {
        let isolated_root = temp_dir("scan-isolation");
        // `to_path_buf`, not `clone`: a guard has no `Clone` on purpose —
        // two owners would each remove the tree on drop.
        let world = make(isolated_root.to_path_buf(), "scan-proof");
        let (count, _) = crate::doctor::count_strays(&isolated_root).expect("scan");
        assert_eq!(
            count, 1,
            "the directory `make` just created must be counted"
        );
        // Best effort: this test asserted the count it came for already,
        // and a failed cleanup here only costs disk.
        std::fs::remove_dir_all(&world).ok();
        std::fs::remove_dir_all(&isolated_root).ok();
    }
}
