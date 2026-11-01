//! Cleanup the state directory accumulates otherwise (programme "one
//! verdict per tree", ticket t00): a finished run's empty
//! `verify/<run>/` directory, and old `runs/<run>/` artifact directories.
//!
//! Both are judged by facts, never by a guess:
//! - a verify directory goes only when it holds no file at all (a release
//!   that failed leaves a worktree behind, and that stays visible);
//! - a run directory goes only for a run the ledger records as ended,
//!   not waiting on a person, with its receipt stored in the ledger, and
//!   untouched for longer than the asked age. Pruning previews by default.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Remove `dir` when it holds nothing but (nested) empty directories.
/// Returns whether `dir` is gone afterwards; a directory that holds any
/// file, symlink or other entry is left exactly as it was, and so are its
/// empty subdirectories (nothing is removed unless the whole tree is).
pub fn remove_empty_tree(dir: &Path) -> io::Result<bool> {
    match only_empty_dirs(dir) {
        Ok(true) => {}
        Ok(false) => return Ok(false),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(e),
    }
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(e),
    }
}

/// Whether `dir` is a real directory whose whole tree holds only
/// directories. A symlink is an entry, not a directory to descend into.
fn only_empty_dirs(dir: &Path) -> io::Result<bool> {
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Ok(false);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if !kind.is_dir() || !only_empty_dirs(&entry.path())? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// What the ledger says about the run a directory is named after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunRecord {
    /// The run has ended.
    Ended,
    /// The run is still going, or waits on something.
    Live,
    /// The ledger has no such run.
    Unknown,
}

/// Remove every empty `verify/<run>/` directory under `root` whose run has
/// ended, or which no run claims and which nobody touched for `min_age`
/// (an unknown name that is fresh may belong to a run being created).
/// Returns the directories removed; a failure is said on stderr and the
/// directory kept.
pub fn sweep_empty_verify_dirs(
    root: &Path,
    record: &dyn Fn(&str) -> Result<RunRecord, String>,
    now: SystemTime,
    min_age: Duration,
) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return removed,
        Err(e) => {
            eprintln!(
                "relais: the verify directory {} could not be read, nothing is swept: {e}",
                root.display()
            );
            return removed;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                eprintln!(
                    "relais: an entry of {} could not be read, it is kept: {e}",
                    root.display()
                );
                continue;
            }
        };
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let eligible = match record(&name) {
            Ok(RunRecord::Ended) => true,
            Ok(RunRecord::Live) => false,
            Ok(RunRecord::Unknown) => age(&path, now).is_some_and(|a| a >= min_age),
            Err(e) => {
                eprintln!("relais: the run {name} could not be looked up, its verify directory is kept: {e}");
                false
            }
        };
        if !eligible {
            continue;
        }
        match remove_empty_tree(&path) {
            Ok(true) => removed.push(path),
            Ok(false) => {}
            Err(e) => eprintln!(
                "relais: the verify directory {} could not be removed: {e}",
                path.display()
            ),
        }
    }
    removed
}

/// [`sweep_empty_verify_dirs`] judged by the ledger: a terminal run has
/// ended, a non-terminal one is live, a name with no row is unknown (and
/// goes only after a day untouched).
pub fn sweep_ended_verify_dirs(root: &Path, ledger: &crate::ledger::Ledger) -> Vec<PathBuf> {
    sweep_empty_verify_dirs(
        root,
        &|name| {
            let run = crate::ids::RunId::from_stored(name);
            match ledger.run_status(&run) {
                Ok(Some(state)) if state.is_terminal() => Ok(RunRecord::Ended),
                Ok(Some(_)) => Ok(RunRecord::Live),
                Ok(None) => Ok(RunRecord::Unknown),
                Err(e) => Err(e.to_string()),
            }
        },
        SystemTime::now(),
        Duration::from_secs(86_400),
    )
}

/// How long ago `path`, or any of its direct entries, was last modified.
/// `None` when no time can be read (then nothing is old enough).
fn age(path: &Path, now: SystemTime) -> Option<Duration> {
    let mut newest = std::fs::symlink_metadata(path).ok()?.modified().ok()?;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
                newest = newest.max(modified);
            }
        }
    }
    Some(now.duration_since(newest).unwrap_or(Duration::ZERO))
}

/// Why a run directory may or may not be pruned, as the ledger has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunVerdict {
    /// Ended, not waiting on a person, receipt in the ledger: prunable,
    /// leaving this many evidence rows (of any run) pointing into it.
    Prunable { dangling_evidence: usize },
    /// Kept, and why.
    Kept(String),
}

/// One run directory the preview lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunDir {
    pub run: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub age: Duration,
    pub verdict: RunVerdict,
}

/// List every `runs/<run>/` directory under `root` untouched for at least
/// `older_than`, each with the ledger's verdict. Younger ones are not
/// listed at all. Sorted oldest first.
///
/// A `root` that does not exist is an empty listing; any other read error
/// is returned, and an entry that cannot be read is named on stderr and
/// left out (so it is never deleted).
pub fn plan_run_prune(
    root: &Path,
    older_than: Duration,
    now: SystemTime,
    judge: &dyn Fn(&str, &Path) -> RunVerdict,
) -> io::Result<Vec<RunDir>> {
    let mut dirs = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(dirs),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                eprintln!(
                    "relais: an entry of {} could not be read, it is left alone: {e}",
                    root.display()
                );
                continue;
            }
        };
        let path = entry.path();
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Some(run) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(age) = age(&path, now) else {
            continue;
        };
        if age < older_than {
            continue;
        }
        let verdict = judge(&run, &path);
        dirs.push(RunDir {
            bytes: tree_bytes(&path),
            run,
            path,
            age,
            verdict,
        });
    }
    dirs.sort_by(|a, b| b.age.cmp(&a.age).then_with(|| a.run.cmp(&b.run)));
    Ok(dirs)
}

/// How many `*.json` files directly under `dir` mention `needle`: the
/// baseline cache cites a cached run's retry logs by absolute path under
/// `runs/<run>/`, so a cited run directory must stay for later hits.
pub fn json_files_citing(dir: &Path, needle: &str) -> io::Result<usize> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut citing = 0;
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "json")
            && std::fs::read_to_string(&path)?.contains(needle)
        {
            citing += 1;
        }
    }
    Ok(citing)
}

/// Delete the prunable directories of a plan. Returns the ones removed
/// and the ones that failed, with the error.
pub fn apply_run_prune(plan: &[RunDir]) -> (Vec<PathBuf>, Vec<(PathBuf, io::Error)>) {
    let mut removed = Vec::new();
    let mut failed = Vec::new();
    for dir in plan {
        if !matches!(dir.verdict, RunVerdict::Prunable { .. }) {
            continue;
        }
        match std::fs::remove_dir_all(&dir.path) {
            Ok(()) => removed.push(dir.path.clone()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => removed.push(dir.path.clone()),
            Err(e) => failed.push((dir.path.clone(), e)),
        }
    }
    (removed, failed)
}

/// The bytes a tree holds (symlinks counted as themselves, not followed).
fn tree_bytes(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path)
        .map(|entries| entries.flatten().map(|e| tree_bytes(&e.path())).sum())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("relais-prune-{tag}-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const DAY: Duration = Duration::from_secs(86_400);

    #[test]
    fn an_empty_tree_goes_and_a_tree_with_a_file_stays_whole() {
        let root = scratch("empty");
        let empty = root.join("run-a");
        std::fs::create_dir_all(empty.join("verify-1/target")).unwrap();
        assert!(remove_empty_tree(&empty).unwrap());
        assert!(!empty.exists());

        let full = root.join("run-b");
        std::fs::create_dir_all(full.join("verify-1")).unwrap();
        std::fs::create_dir_all(full.join("verify-base")).unwrap();
        std::fs::write(full.join("verify-1/left.txt"), "x").unwrap();
        assert!(!remove_empty_tree(&full).unwrap());
        assert!(full.join("verify-1/left.txt").exists());
        assert!(
            full.join("verify-base").exists(),
            "nothing partial is removed"
        );

        assert!(remove_empty_tree(&root.join("never-made")).unwrap());
        std::fs::remove_dir_all(&root).ok(); // best-effort scratch cleanup
    }

    #[test]
    fn the_sweep_removes_ended_and_old_unknown_empties_only() {
        let root = scratch("sweep");
        for name in ["ended", "live", "unknown", "ended-full", "broken"] {
            std::fs::create_dir_all(root.join(name).join("verify-1")).unwrap();
        }
        std::fs::write(root.join("ended-full/verify-1/f"), "x").unwrap();
        let record = |name: &str| match name {
            "ended" | "ended-full" => Ok(RunRecord::Ended),
            "live" => Ok(RunRecord::Live),
            "broken" => Err("ledger busy".to_string()),
            _ => Ok(RunRecord::Unknown),
        };
        // Fresh: the unknown one is not old enough yet.
        let removed = sweep_empty_verify_dirs(&root, &record, SystemTime::now(), DAY);
        assert_eq!(removed, vec![root.join("ended")]);
        assert!(root.join("unknown").exists());
        // A week later the unknown empty one goes; live, full and
        // unreadable ones never do.
        let later = SystemTime::now() + 7 * DAY;
        let removed = sweep_empty_verify_dirs(&root, &record, later, DAY);
        assert_eq!(removed, vec![root.join("unknown")]);
        for kept in ["live", "ended-full", "broken"] {
            assert!(root.join(kept).exists(), "{kept} must stay");
        }
        std::fs::remove_dir_all(&root).ok(); // best-effort scratch cleanup
    }

    #[test]
    fn a_preview_lists_old_runs_and_deletes_nothing_until_applied() {
        let root = scratch("runs");
        for name in ["old-done", "old-live", "young-done"] {
            std::fs::create_dir_all(root.join(name).join("artifacts/logs")).unwrap();
            std::fs::write(root.join(name).join("artifacts/logs/a.log"), "1234").unwrap();
        }
        let judge = |run: &str, _: &Path| match run {
            "old-live" => RunVerdict::Kept("running".to_string()),
            _ => RunVerdict::Prunable {
                dangling_evidence: 2,
            },
        };
        // Everything is fresh: nothing is old enough to list.
        assert!(plan_run_prune(&root, 30 * DAY, SystemTime::now(), &judge)
            .unwrap()
            .is_empty());
        // 31 days on, all three are listed; the preview deletes nothing.
        let later = SystemTime::now() + 31 * DAY;
        let plan = plan_run_prune(&root, 30 * DAY, later, &judge).unwrap();
        assert_eq!(plan.len(), 3);
        assert!(plan.iter().all(|d| d.bytes == 4));
        for name in ["old-done", "old-live", "young-done"] {
            assert!(root.join(name).exists());
        }
        // Applying removes only the prunable ones; a live run stays.
        let (removed, failed) = apply_run_prune(&plan);
        assert!(failed.is_empty());
        assert_eq!(removed.len(), 2);
        assert!(root.join("old-live/artifacts/logs/a.log").exists());
        assert!(!root.join("old-done").exists());
        std::fs::remove_dir_all(&root).ok(); // best-effort scratch cleanup
    }

    #[test]
    fn a_cache_entry_citing_a_run_is_counted() {
        let cache = scratch("cache");
        std::fs::write(cache.join("a.json"), r#"{"p":"/s/runs/run-1/logs/x.log"}"#).unwrap();
        std::fs::write(cache.join("b.json"), r#"{"p":"/s/runs/run-2/logs/x.log"}"#).unwrap();
        std::fs::write(cache.join("c.blob"), "/s/runs/run-1/").unwrap();
        assert_eq!(json_files_citing(&cache, "/s/runs/run-1/").unwrap(), 1);
        assert_eq!(json_files_citing(&cache.join("none"), "x").unwrap(), 0);
        std::fs::remove_dir_all(&cache).ok(); // best-effort scratch cleanup
    }
}
