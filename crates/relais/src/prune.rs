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
/// Returns whether `dir` is gone afterwards. Removal is bottom-up with
/// `remove_dir`, which refuses a directory that is not empty, so a file
/// that appears between the look and the removal is never deleted: the
/// directory holding it stays, and so does every directory above it.
pub fn remove_empty_tree(dir: &Path) -> io::Result<bool> {
    let meta = match std::fs::symlink_metadata(dir) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(e),
    };
    if !meta.is_dir() {
        return Ok(false);
    }
    let mut all_gone = true;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            all_gone &= remove_empty_tree(&entry.path())?;
        } else {
            all_gone = false;
        }
    }
    if !all_gone {
        return Ok(false);
    }
    match std::fs::remove_dir(dir) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
        // Not empty after all (something arrived): kept, not an error.
        Err(e) if e.kind() == io::ErrorKind::DirectoryNotEmpty => Ok(false),
        Err(e) => Err(e),
    }
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
/// `None` when the path's own time cannot be read (then nothing is old
/// enough). A listing or an entry that cannot be read counts as modified
/// now: what cannot be seen may be newer than what can, and a directory
/// is never judged old on partial evidence.
fn age(path: &Path, now: SystemTime) -> Option<Duration> {
    let mut newest = std::fs::symlink_metadata(path).ok()?.modified().ok()?;
    match std::fs::read_dir(path) {
        Ok(entries) => {
            for entry in entries {
                match entry.and_then(|e| e.metadata()).and_then(|m| m.modified()) {
                    Ok(modified) => newest = newest.max(modified),
                    Err(_) => return Some(Duration::ZERO), // unreadable entry: fresh
                }
            }
        }
        // A plain file has no listing; any other failure hides entries.
        Err(e) if e.kind() == io::ErrorKind::NotADirectory => {}
        Err(_) => return Some(Duration::ZERO), // unreadable listing: fresh
    }
    Some(now.duration_since(newest).unwrap_or(Duration::ZERO))
}

/// Why a run directory may or may not be pruned, as the ledger has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunVerdict {
    /// Ended, not waiting on a person, receipt in the ledger: prunable,
    /// leaving these evidence rows (of any run) pointing into it, as
    /// `(kind, path)`.
    Prunable { dangling: Vec<(String, String)> },
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
        // `.pruning-<id>` is a deletion in flight: never listed or judged.
        if run.starts_with('.') {
            continue;
        }
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

/// What deleting a plan did.
#[derive(Debug, Default)]
pub struct Applied {
    /// The directories gone (by their original path).
    pub removed: Vec<PathBuf>,
    /// Prunable when planned, kept by the re-check just before deletion.
    pub kept: Vec<(PathBuf, String)>,
    /// Deletions that failed, with the error.
    pub failed: Vec<(PathBuf, io::Error)>,
}

/// The prefix of a directory whose deletion has begun.
const PRUNING_PREFIX: &str = ".pruning-";

/// Delete the prunable directories of a plan. `recheck` is asked just
/// before each one and answers why it must be kept after all (`None` =
/// go). The deletion is crash-safe: the directory is renamed to
/// `.pruning-<run>` (the commit point) and then removed, and
/// [`finish_interrupted`] completes a removal a crash cut short.
pub fn apply_run_prune(plan: &[RunDir], recheck: &dyn Fn(&RunDir) -> Option<String>) -> Applied {
    let mut applied = Applied::default();
    for dir in plan {
        if !matches!(dir.verdict, RunVerdict::Prunable { .. }) {
            continue;
        }
        if let Some(why) = recheck(dir) {
            applied.kept.push((dir.path.clone(), why));
            continue;
        }
        let doomed = dir
            .path
            .with_file_name(format!("{PRUNING_PREFIX}{}", dir.run));
        match std::fs::rename(&dir.path, &doomed) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                applied.removed.push(dir.path.clone());
                continue;
            }
            Err(e) => {
                applied.failed.push((dir.path.clone(), e));
                continue;
            }
        }
        match std::fs::remove_dir_all(&doomed) {
            Ok(()) => applied.removed.push(dir.path.clone()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => applied.removed.push(dir.path.clone()),
            Err(e) => applied.failed.push((dir.path.clone(), e)),
        }
    }
    applied
}

/// Finish the removals a crash left behind: every `.pruning-*` directory
/// directly under `runs` goes. Returns the ones that could not.
pub fn finish_interrupted(runs: &Path) -> Vec<(PathBuf, io::Error)> {
    let mut failed = Vec::new();
    let entries = match std::fs::read_dir(runs) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return failed,
        Err(e) => {
            failed.push((runs.to_path_buf(), e));
            return failed;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                failed.push((runs.to_path_buf(), e));
                continue;
            }
        };
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(PRUNING_PREFIX)
        {
            continue;
        }
        match std::fs::remove_dir_all(entry.path()) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => failed.push((entry.path(), e)),
        }
    }
    failed
}

/// The one judgement of whether a run directory may go, shared by
/// `relais resume --prune-runs` and the automatic prune. It reads the
/// ledger's pinned candidates and every baseline-cache file once, when it
/// is loaded; a source it could not read keeps every run.
pub struct RunJudge<'a> {
    ledger: &'a crate::ledger::Ledger,
    pinned: Result<std::collections::BTreeSet<(String, u32)>, String>,
    cache_texts: Result<Vec<String>, String>,
}

impl<'a> RunJudge<'a> {
    pub fn load(ledger: &'a crate::ledger::Ledger, cache_dir: &Path) -> Self {
        Self {
            ledger,
            pinned: ledger.pinned_candidates().map_err(|e| e.to_string()),
            cache_texts: read_json_texts(cache_dir).map_err(|e| e.to_string()),
        }
    }

    /// The verdict on `runs/<run>/` at `path`, checks in order: ended and
    /// not waiting on a person, receipt, pinned candidate, a subtree that
    /// is safe to delete, baseline-cache citation; then the evidence rows
    /// left dangling.
    pub fn judge(&self, run: &str, path: &Path) -> RunVerdict {
        let id = crate::ids::RunId::from_stored(run);
        match self.ledger.run_status(&id) {
            Ok(None) => return RunVerdict::Kept("not in the ledger".to_string()),
            Ok(Some(state)) => {
                if let Some(why) = state_keeps(state) {
                    return RunVerdict::Kept(why);
                }
            }
            Err(e) => return RunVerdict::Kept(format!("ledger: {e}")),
        }
        match self.ledger.receipt(&id) {
            Ok(Some(_)) => {}
            Ok(None) => return RunVerdict::Kept("no receipt in the ledger".to_string()),
            Err(e) => return RunVerdict::Kept(format!("ledger: {e}")),
        }
        let pinned = match &self.pinned {
            Ok(pinned) => pinned,
            Err(e) => return RunVerdict::Kept(format!("pinned candidates unreadable: {e}")),
        };
        if let Some(why) = pin_keeps(pinned, run) {
            return RunVerdict::Kept(why);
        }
        if let Some(why) = self.subtree_keeps(pinned, &id, path) {
            return RunVerdict::Kept(why);
        }
        // A citation may name the directory as it was spelled when it was
        // written or through its real path (a symlinked state directory,
        // `/tmp` against `/private/tmp`): both spellings are looked for.
        let spellings = spellings_of(path);
        let texts = match &self.cache_texts {
            Ok(texts) => texts,
            Err(e) => return RunVerdict::Kept(format!("baseline cache unreadable: {e}")),
        };
        for spelling in &spellings {
            let cited = format!(
                "{}{}",
                spelling.to_string_lossy(),
                std::path::MAIN_SEPARATOR
            );
            let citing = texts.iter().filter(|text| text.contains(&cited)).count();
            if citing > 0 {
                return RunVerdict::Kept(format!("cited by {citing} baseline-cache entries"));
            }
        }
        let mut dangling = Vec::new();
        for spelling in &spellings {
            match self.ledger.evidence_under(spelling) {
                Ok(rows) => {
                    for row in rows {
                        if !dangling.contains(&row) {
                            dangling.push(row);
                        }
                    }
                }
                Err(e) => return RunVerdict::Kept(format!("ledger: {e}")),
            }
        }
        RunVerdict::Prunable { dangling }
    }

    /// Asked just before a run directory is renamed away: why it must be
    /// kept after all. The pin is read afresh, so one added since this
    /// judge loaded keeps its source, and so is the subtree.
    // holds-until: ~10^4 candidate_inputs rows - the pin set is reloaded in
    // full for each deletion; a per-run indexed query after that
    pub fn recheck(&self, run: &str, path: &Path) -> Option<String> {
        let pinned = match self.ledger.pinned_candidates() {
            Ok(pinned) => pinned,
            Err(e) => return Some(format!("pinned candidates unreadable: {e}")),
        };
        pin_keeps(&pinned, run)
            .or_else(|| self.subtree_keeps(&pinned, &crate::ids::RunId::from_stored(run), path))
    }

    /// Delete a plan, re-checking each run first.
    pub fn apply(&self, plan: &[RunDir]) -> Applied {
        apply_run_prune(plan, &|dir| self.recheck(&dir.run, &dir.path))
    }

    /// The deletion subtree of a run holds more than its own artifacts
    /// when it decomposed: package runs, and the checkouts they left.
    fn subtree_keeps(
        &self,
        pinned: &std::collections::BTreeSet<(String, u32)>,
        root: &crate::ids::RunId,
        path: &Path,
    ) -> Option<String> {
        let mut pending = vec![root.clone()];
        let mut seen = std::collections::BTreeSet::new();
        while let Some(parent) = pending.pop() {
            if !seen.insert(parent.as_str().to_string()) {
                continue;
            }
            let children = match self.ledger.child_runs(&parent) {
                Ok(children) => children,
                Err(e) => return Some(format!("ledger: {e}")),
            };
            for child in children {
                if let Some(why) = state_keeps(child.status) {
                    return Some(format!("package run {} is {why}", child.run));
                }
                if pin_keeps(pinned, child.run.as_str()).is_some() {
                    return Some(format!("package run {} is pinned", child.run));
                }
                pending.push(child.run);
            }
        }
        checkout_beneath(path)
    }
}

/// Why a run in `state` is kept, when it is.
fn state_keeps(state: crate::lifecycle::State) -> Option<String> {
    use crate::lifecycle::State;
    match state {
        State::Prepared
        | State::Running
        | State::Verifying
        | State::Repairing
        | State::Escalating => Some(format!("{} (not ended)", state.as_str())),
        State::NeedsReview | State::NeedsDecision | State::Interrupted => {
            Some(format!("{} (waits on a person)", state.as_str()))
        }
        State::Accepted
        | State::Blocked
        | State::Failed
        | State::BudgetExhausted
        | State::Cancelled
        | State::AcceptedByPerson => None,
    }
}

fn pin_keeps(pinned: &std::collections::BTreeSet<(String, u32)>, run: &str) -> Option<String> {
    pinned
        .range((run.to_string(), 0)..=(run.to_string(), u32::MAX))
        .next()
        .map(|_| "pinned by a run that has not ended".to_string())
}

/// A checkout under `dir` (a directory holding `.git`, or the legacy
/// `worktree`), which is the run's work and never pruned with it. A
/// directory that cannot be read keeps the run.
fn checkout_beneath(dir: &Path) -> Option<String> {
    if dir.join(crate::paths::LEGACY_WORKTREE_DIR).exists() {
        return Some("worktree (a checkout remains beneath it)".to_string());
    }
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(e) => return Some(format!("{} cannot be read: {e}", current.display())),
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => return Some(format!("{} cannot be read: {e}", current.display())),
            };
            let child = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => {
                    if child.join(".git").exists() {
                        return Some(format!(
                            "worktree (a checkout remains beneath it: {})",
                            child.display()
                        ));
                    }
                    pending.push(child);
                }
                Ok(_) => {}
                Err(e) => return Some(format!("{} cannot be read: {e}", child.display())),
            }
        }
    }
    None
}

/// `path` as given and, when it differs, as its real path.
fn spellings_of(path: &Path) -> Vec<PathBuf> {
    let mut spellings = vec![path.to_path_buf()];
    if let Ok(real) = std::fs::canonicalize(path) {
        if real != path {
            spellings.push(real);
        }
    }
    spellings
}

/// The text of every `*.json` file directly under `dir` (none when it
/// does not exist).
fn read_json_texts(dir: &Path) -> io::Result<Vec<String>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut texts = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            texts.push(std::fs::read_to_string(&path)?);
        }
    }
    Ok(texts)
}

/// The file whose mtime says when the automatic prune last ran.
pub const STAMP_FILE: &str = "runs-prune.stamp";
/// The lock one automatic prune holds while it runs.
pub const LOCK_FILE: &str = "runs-prune.lock";
/// How old the stamp must be before the automatic prune runs again.
pub const THROTTLE: Duration = Duration::from_secs(86_400);
/// The age the automatic prune uses when nothing says otherwise.
pub const DEFAULT_AUTO_PRUNE_DAYS: u64 = 30;
/// The variable that sets the automatic prune's age in days.
pub const AUTO_PRUNE_DAYS_ENV: &str = "RELAIS_AUTO_PRUNE_DAYS";

/// What the person asked of the automatic prune.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoPruneSetting {
    /// `--no-prune`.
    Disabled,
    /// `RELAIS_AUTO_PRUNE_DAYS=0`.
    Off,
    /// Prune runs untouched for this many days.
    Days(u64),
    /// The variable holds something that is not a number of days.
    Invalid(String),
}

/// Precedence, highest first: `--no-prune`, the variable, the default.
pub fn auto_prune_setting(no_prune: bool, env_days: Option<&str>) -> AutoPruneSetting {
    if no_prune {
        return AutoPruneSetting::Disabled;
    }
    match env_days {
        None => AutoPruneSetting::Days(DEFAULT_AUTO_PRUNE_DAYS),
        Some(text) => match text.trim().parse::<u64>() {
            Ok(0) => AutoPruneSetting::Off,
            Ok(days) => AutoPruneSetting::Days(days),
            Err(_) => AutoPruneSetting::Invalid(text.to_string()),
        },
    }
}

/// What one automatic prune did.
#[derive(Debug)]
pub enum AutoPrune {
    /// The stamp is newer than [`THROTTLE`].
    Throttled,
    /// Another prune holds the lock.
    Held,
    Ran(AutoPruneReport),
}

#[derive(Debug)]
pub struct AutoPruneReport {
    /// Run directories older than the age, kept or not.
    pub listed: usize,
    /// The ones deleted.
    pub removed: Vec<RunDir>,
    /// Failures, finishing a leftover or deleting.
    pub failed: Vec<(PathBuf, io::Error)>,
}

fn stamp_is_fresh(stamp: &Path, now: SystemTime) -> bool {
    // A stamp from the future (a clock set back) counts as stale.
    std::fs::metadata(stamp)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| now.duration_since(modified).ok())
        .is_some_and(|since| since < THROTTLE)
}

/// Prune the ended runs untouched for `age`, at most once per
/// [`THROTTLE`]: skip on a fresh stamp, take the lock (skip when held),
/// look at the stamp again, write it, then finish leftovers, judge and
/// hand the plan to `apply`.
pub fn auto_prune(
    ledger: &crate::ledger::Ledger,
    state_dir: &Path,
    age: Duration,
    now: SystemTime,
    apply: &dyn Fn(&RunJudge<'_>, &[RunDir]) -> Applied,
) -> io::Result<AutoPrune> {
    let stamp = state_dir.join(STAMP_FILE);
    if stamp_is_fresh(&stamp, now) {
        return Ok(AutoPrune::Throttled);
    }
    let Some(_lock) = crate::fsutil::LockFile::try_acquire(&state_dir.join(LOCK_FILE))? else {
        return Ok(AutoPrune::Held);
    };
    if stamp_is_fresh(&stamp, now) {
        return Ok(AutoPrune::Throttled);
    }
    std::fs::write(&stamp, format!("{}\n", crate::ledger::now_rfc3339()))?;
    let runs = state_dir.join(crate::paths::RUNS_DIR);
    let mut failed = finish_interrupted(&runs);
    let judge = RunJudge::load(ledger, &state_dir.join("baseline-cache"));
    let plan = plan_run_prune(&runs, age, now, &|run, path| judge.judge(run, path))?;
    let applied = apply(&judge, &plan);
    failed.extend(applied.failed);
    let removed = plan
        .iter()
        .filter(|dir| applied.removed.contains(&dir.path))
        .cloned()
        .collect();
    Ok(AutoPrune::Ran(AutoPruneReport {
        listed: plan.len(),
        removed,
        failed,
    }))
}

/// The bytes a tree holds (symlinks counted as themselves, not followed).
pub fn tree_bytes(path: &Path) -> u64 {
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
    fn an_empty_tree_goes_and_a_file_keeps_its_ancestors() {
        let root = scratch("empty");
        let empty = root.join("run-a");
        std::fs::create_dir_all(empty.join("verify-1/target")).unwrap();
        assert!(remove_empty_tree(&empty).unwrap());
        assert!(!empty.exists());

        // Bottom-up: an empty sibling goes, but a file, its directory and
        // every directory above it stay.
        let full = root.join("run-b");
        std::fs::create_dir_all(full.join("verify-1")).unwrap();
        std::fs::create_dir_all(full.join("verify-base")).unwrap();
        std::fs::write(full.join("verify-1/left.txt"), "x").unwrap();
        assert!(!remove_empty_tree(&full).unwrap());
        assert!(
            full.join("verify-1/left.txt").exists(),
            "a file is never removed"
        );
        assert!(full.is_dir());
        assert!(!full.join("verify-base").exists(), "the empty sibling went");

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
                dangling: Vec::new(),
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
        let applied = apply_run_prune(&plan, &|_| None);
        assert!(applied.failed.is_empty());
        assert_eq!(applied.removed.len(), 2);
        assert!(root.join("old-live/artifacts/logs/a.log").exists());
        assert!(!root.join("old-done").exists());
        std::fs::remove_dir_all(&root).ok(); // best-effort scratch cleanup
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_that_cannot_be_listed_is_never_judged_old() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("unlistable");
        let hidden = root.join("run-x");
        std::fs::create_dir_all(hidden.join("logs")).unwrap();
        std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o000)).unwrap();
        let later = SystemTime::now() + 31 * DAY;
        let judge = |_: &str, _: &Path| RunVerdict::Prunable {
            dangling: Vec::new(),
        };
        let listed = plan_run_prune(&root, 30 * DAY, later, &judge).unwrap();
        // Probed while still locked: true only when this runs as root,
        // which can list anything. Then the directory is no hidden case.
        let still_listable = std::fs::read_dir(&hidden).is_ok();
        std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(still_listable || listed.is_empty(), "{listed:?}");
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

    fn ledger_in(dir: &Path) -> crate::ledger::Ledger {
        crate::ledger::Ledger::open(&dir.join("ledger.sqlite")).expect("open ledger")
    }

    #[test]
    fn the_setting_follows_flag_then_environment_then_default() {
        assert_eq!(
            auto_prune_setting(true, Some("5")),
            AutoPruneSetting::Disabled
        );
        assert_eq!(auto_prune_setting(false, Some("0")), AutoPruneSetting::Off);
        assert_eq!(
            auto_prune_setting(false, Some("7")),
            AutoPruneSetting::Days(7)
        );
        assert_eq!(auto_prune_setting(false, None), AutoPruneSetting::Days(30));
        assert_eq!(
            auto_prune_setting(false, Some("soon")),
            AutoPruneSetting::Invalid("soon".to_string())
        );
    }

    #[test]
    fn the_stamp_is_written_before_deleting_and_throttles_the_next_prune() {
        let state = scratch("auto");
        let ledger = ledger_in(&state);
        let stamp = state.join(STAMP_FILE);
        let seen = std::cell::Cell::new(false);
        let outcome = auto_prune(&ledger, &state, 30 * DAY, SystemTime::now(), &|_, _| {
            seen.set(stamp.exists());
            Applied::default()
        })
        .unwrap();
        assert!(matches!(outcome, AutoPrune::Ran(_)));
        assert!(seen.get(), "the stamp exists when deletion starts");
        let again = auto_prune(&ledger, &state, 30 * DAY, SystemTime::now(), &|_, _| {
            panic!("throttled")
        })
        .unwrap();
        assert!(matches!(again, AutoPrune::Throttled));
        // A day and an hour later it runs again; a stamp from the future is stale.
        let later = SystemTime::now() + DAY + Duration::from_secs(3600);
        let third =
            auto_prune(&ledger, &state, 30 * DAY, later, &|_, _| Applied::default()).unwrap();
        assert!(matches!(third, AutoPrune::Ran(_)));
        let past = SystemTime::now() - 2 * DAY;
        assert!(!stamp_is_fresh(&stamp, past));
        std::fs::remove_dir_all(&state).ok(); // best-effort scratch cleanup
    }

    #[test]
    fn a_held_lock_skips_the_prune() {
        let state = scratch("held");
        let ledger = ledger_in(&state);
        let _held = crate::fsutil::LockFile::try_acquire(&state.join(LOCK_FILE))
            .unwrap()
            .expect("lock");
        let outcome = auto_prune(&ledger, &state, 30 * DAY, SystemTime::now(), &|_, _| {
            panic!("held")
        })
        .unwrap();
        assert!(matches!(outcome, AutoPrune::Held));
        assert!(!state.join(STAMP_FILE).exists());
        std::fs::remove_dir_all(&state).ok(); // best-effort scratch cleanup
    }

    #[test]
    fn a_leftover_pruning_directory_is_finished_and_never_listed() {
        let runs = scratch("leftover");
        std::fs::create_dir_all(runs.join(".pruning-x/logs")).unwrap();
        std::fs::create_dir_all(runs.join("run-a")).unwrap();
        let judge = |_: &str, _: &Path| RunVerdict::Prunable {
            dangling: Vec::new(),
        };
        let later = SystemTime::now() + 31 * DAY;
        let plan = plan_run_prune(&runs, 30 * DAY, later, &judge).unwrap();
        assert_eq!(plan.len(), 1);
        assert!(finish_interrupted(&runs).is_empty());
        assert!(!runs.join(".pruning-x").exists());
        assert!(runs.join("run-a").exists());
        std::fs::remove_dir_all(&runs).ok(); // best-effort scratch cleanup
    }

    #[test]
    fn a_run_the_recheck_keeps_is_not_deleted() {
        let root = scratch("recheck");
        std::fs::create_dir_all(root.join("run-a")).unwrap();
        std::fs::create_dir_all(root.join("run-b")).unwrap();
        let judge = |_: &str, _: &Path| RunVerdict::Prunable {
            dangling: Vec::new(),
        };
        let later = SystemTime::now() + 31 * DAY;
        let plan = plan_run_prune(&root, 30 * DAY, later, &judge).unwrap();
        let applied = apply_run_prune(&plan, &|dir| {
            (dir.run == "run-a").then(|| "pinned".to_string())
        });
        assert!(root.join("run-a").exists());
        assert!(!root.join("run-b").exists());
        assert_eq!(applied.kept.len(), 1);
        std::fs::remove_dir_all(&root).ok(); // best-effort scratch cleanup
    }

    #[test]
    fn a_checkout_beneath_a_run_keeps_it() {
        let root = scratch("checkout");
        let run = root.join("run-a");
        std::fs::create_dir_all(run.join("packages/verify/c/verify-base")).unwrap();
        assert!(checkout_beneath(&run).is_none());
        std::fs::write(run.join("packages/verify/c/verify-base/.git"), "gitdir: x").unwrap();
        assert!(checkout_beneath(&run).is_some());
        std::fs::remove_dir_all(&root).ok(); // best-effort scratch cleanup
    }
}
