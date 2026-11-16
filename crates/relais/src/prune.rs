//! Cleanup the state directory accumulates otherwise (programme "one
//! verdict per tree", ticket t00): a finished run's empty
//! `verify/<run>/` directory, and old `runs/<run>/` artifact directories.
//!
//! Both are judged by facts, never by a guess:
//! - a verify directory goes only when it holds no file at all (a release
//!   that failed leaves a worktree behind, and that stays visible);
//! - a run directory goes only for a run the ledger records as ended,
//!   not waiting on a person, with its receipt stored in the ledger when it
//!   is `accepted` (the only state that stores one), and
//!   untouched for longer than the asked age. Pruning previews by default.

use crate::lifecycle::State;
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

/// Why a run directory is kept. The `Display` is the per-run line; the
/// [`label`](KeepReason::label) is the fixed category the tally groups by,
/// so a tally never carries a path, a run id or an error text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeepReason {
    /// The run has not ended (`state` names where it is).
    NotEnded { state: State },
    /// The run ended in a state a person still has to answer.
    WaitsOnPerson { state: State },
    /// The ledger has no such run.
    NotInLedger,
    /// An `accepted` run whose receipt is not in the ledger.
    NoReceipt,
    /// A run that has not ended pins this run's candidate.
    Pinned,
    /// A package run beneath this one cannot go. `why` is the package run's
    /// own reason: `NotEnded`, `WaitsOnPerson` or `Pinned`.
    PackageRun { run: String, why: Box<KeepReason> },
    /// A checkout remains beneath the directory (`at` names it, when known).
    CheckoutBeneath { at: Option<PathBuf> },
    /// This many baseline-cache entries cite the directory.
    CitedByCache { entries: usize },
    /// A source of the judgement could not be read.
    Unreadable { what: String, error: String },
}

impl KeepReason {
    /// The fixed short category of this reason.
    pub fn label(&self) -> &'static str {
        match self {
            Self::NotEnded { .. } => "not ended",
            Self::WaitsOnPerson { .. } => "waits on a person",
            Self::NotInLedger => "not in the ledger",
            Self::NoReceipt => "no receipt",
            Self::Pinned => "pinned",
            Self::PackageRun { .. } => "package run",
            Self::CheckoutBeneath { .. } => "worktree",
            Self::CitedByCache { .. } => "cited by the cache",
            Self::Unreadable { .. } => "unreadable",
        }
    }

    fn unreadable(what: &str, error: impl std::fmt::Display) -> Self {
        Self::Unreadable {
            what: what.to_string(),
            error: error.to_string(),
        }
    }
}

impl std::fmt::Display for KeepReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotEnded { state } => write!(f, "{} (not ended)", state.as_str()),
            Self::WaitsOnPerson { state } => {
                write!(f, "{} (waits on a person)", state.as_str())
            }
            Self::NotInLedger => write!(f, "not in the ledger"),
            Self::NoReceipt => write!(f, "no receipt in the ledger"),
            Self::Pinned => write!(f, "pinned by a run that has not ended"),
            Self::PackageRun { run, why } => write!(f, "package run {run} is {why}"),
            Self::CheckoutBeneath { at: None } => {
                write!(f, "worktree (a checkout remains beneath it)")
            }
            Self::CheckoutBeneath { at: Some(at) } => write!(
                f,
                "worktree (a checkout remains beneath it: {})",
                at.display()
            ),
            Self::CitedByCache { entries } => {
                write!(f, "cited by {entries} baseline-cache entries")
            }
            Self::Unreadable { what, error } => write!(f, "{what} unreadable: {error}"),
        }
    }
}

/// Why a run directory may or may not be pruned, as the ledger has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunVerdict {
    /// Ended, not waiting on a person, receipt in the ledger when accepted: prunable,
    /// leaving these evidence rows (of any run) pointing into it, as
    /// `(kind, path)`.
    Prunable { dangling: Vec<(String, String)> },
    /// Kept, and why.
    Kept(KeepReason),
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

/// A run directory deleted, by its original path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedDir {
    pub path: PathBuf,
    /// Evidence rows its deletion left pointing into it.
    pub dangling: usize,
}

/// What deleting a plan did.
#[derive(Debug, Default)]
pub struct Applied {
    /// The directories gone, each with the evidence rows left pointing into it.
    pub removed: Vec<RemovedDir>,
    /// Prunable when planned, kept by the re-check just before deletion.
    pub kept: Vec<(PathBuf, KeepReason)>,
    /// Deletions that failed.
    pub failed: Vec<FailedDir>,
}

/// A deletion that failed.
#[derive(Debug)]
pub struct FailedDir {
    /// What is left on disk: the original directory when the rename
    /// failed, `.pruning-<run>` after it.
    pub left_on_disk: Left,
    pub error: io::Error,
}

/// What a failed deletion left on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Left {
    /// The path a prune step failed on, as it was: the rename to
    /// `.pruning-<run>` had not happened. The lock, stamp and reading-runs
    /// steps are always `Original`.
    Original(PathBuf),
    /// The path a prune step failed on after it was renamed to
    /// `.pruning-<run>`, which the next prune retries.
    Renamed(PathBuf),
}

impl Left {
    pub fn path(&self) -> &Path {
        match self {
            Self::Original(path) | Self::Renamed(path) => path,
        }
    }

    /// What to tell the person: a renamed directory is retried by the next
    /// prune; one never renamed has no such promise. The failure line
    /// already prints the path.
    pub fn hint(&self) -> &'static str {
        match self {
            Self::Original(_) => "",
            Self::Renamed(_) => " (the next prune retries it; fix the error above first)",
        }
    }
}

/// The prefix of a directory whose deletion has begun.
const PRUNING_PREFIX: &str = ".pruning-";

/// Delete the prunable directories of a plan. `recheck` is asked just
/// before each one and answers why it must be kept after all (`None` =
/// go). The deletion is crash-safe: the directory is renamed to
/// `.pruning-<run>` (the commit point) and then removed, and
/// [`finish_interrupted`] completes a removal a crash cut short.
pub fn apply_run_prune(
    plan: &[RunDir],
    recheck: &dyn Fn(&RunDir) -> Option<KeepReason>,
) -> Applied {
    let mut applied = Applied::default();
    for dir in plan {
        let RunVerdict::Prunable { dangling } = &dir.verdict else {
            continue;
        };
        let removed = || RemovedDir {
            path: dir.path.clone(),
            dangling: dangling.len(),
        };
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
                applied.removed.push(removed());
                continue;
            }
            Err(e) => {
                applied.failed.push(FailedDir {
                    left_on_disk: Left::Original(dir.path.clone()),
                    error: e,
                });
                continue;
            }
        }
        match std::fs::remove_dir_all(&doomed) {
            Ok(()) => applied.removed.push(removed()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => applied.removed.push(removed()),
            // The rename already happened: what is left on disk is `doomed`.
            Err(e) => applied.failed.push(FailedDir {
                left_on_disk: Left::Renamed(doomed),
                error: e,
            }),
        }
    }
    applied
}

impl FailedDir {
    /// See [`Left::hint`].
    pub fn hint(&self) -> &'static str {
        self.left_on_disk.hint()
    }
}

/// Finish the removals a crash left behind: every `.pruning-*` directory
/// directly under `runs` goes. Returns the ones that could not; each
/// `.pruning-*` entry is `renamed`, the listing of `runs` itself is not.
pub fn finish_interrupted(runs: &Path) -> Vec<FailedDir> {
    let mut failed = Vec::new();
    let entries = match std::fs::read_dir(runs) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return failed,
        Err(e) => {
            failed.push(FailedDir {
                left_on_disk: Left::Original(runs.to_path_buf()),
                error: e,
            });
            return failed;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                failed.push(FailedDir {
                    left_on_disk: Left::Original(runs.to_path_buf()),
                    error: e,
                });
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
            Err(e) => failed.push(FailedDir {
                left_on_disk: Left::Renamed(entry.path()),
                error: e,
            }),
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
    /// not waiting on a person, receipt (accepted runs only), pinned candidate, a subtree that
    /// is safe to delete, baseline-cache citation; then the evidence rows
    /// left dangling.
    pub fn judge(&self, run: &str, path: &Path) -> RunVerdict {
        let id = crate::ids::RunId::from_stored(run);
        let state = match self.ledger.run_status(&id) {
            Ok(None) => return RunVerdict::Kept(KeepReason::NotInLedger),
            Ok(Some(state)) => {
                if let Some(why) = state_keeps(state) {
                    return RunVerdict::Kept(why);
                }
                state
            }
            Err(e) => return RunVerdict::Kept(KeepReason::unreadable("ledger", e)),
        };
        // Only an accepted run has a receipt stored; any other ended run
        // never will, so waiting for one would keep it forever.
        if state == State::Accepted {
            match self.ledger.receipt(&id) {
                Ok(Some(_)) => {}
                Ok(None) => return RunVerdict::Kept(KeepReason::NoReceipt),
                Err(e) => return RunVerdict::Kept(KeepReason::unreadable("ledger", e)),
            }
        }
        let pinned = match &self.pinned {
            Ok(pinned) => pinned,
            Err(e) => return RunVerdict::Kept(KeepReason::unreadable("pinned candidates", e)),
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
            Err(e) => return RunVerdict::Kept(KeepReason::unreadable("baseline cache", e)),
        };
        for spelling in &spellings {
            let cited = format!(
                "{}{}",
                spelling.to_string_lossy(),
                std::path::MAIN_SEPARATOR
            );
            let citing = texts.iter().filter(|text| text.contains(&cited)).count();
            if citing > 0 {
                return RunVerdict::Kept(KeepReason::CitedByCache { entries: citing });
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
                Err(e) => return RunVerdict::Kept(KeepReason::unreadable("ledger", e)),
            }
        }
        RunVerdict::Prunable { dangling }
    }

    /// Asked just before a run directory is renamed away: why it must be
    /// kept after all. The pin is read afresh, so one added since this
    /// judge loaded keeps its source, and so is the subtree.
    // holds-until: ~10^4 candidate_inputs rows - the pin set is reloaded in
    // full for each deletion; a per-run indexed query after that
    pub fn recheck(&self, run: &str, path: &Path) -> Option<KeepReason> {
        let pinned = match self.ledger.pinned_candidates() {
            Ok(pinned) => pinned,
            Err(e) => return Some(KeepReason::unreadable("pinned candidates", e)),
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
    ) -> Option<KeepReason> {
        let mut pending = vec![root.clone()];
        let mut seen = std::collections::BTreeSet::new();
        while let Some(parent) = pending.pop() {
            if !seen.insert(parent.as_str().to_string()) {
                continue;
            }
            let children = match self.ledger.child_runs(&parent) {
                Ok(children) => children,
                Err(e) => return Some(KeepReason::unreadable("ledger", e)),
            };
            for child in children {
                if let Some(why) = state_keeps(child.status) {
                    return Some(KeepReason::PackageRun {
                        run: child.run.to_string(),
                        why: Box::new(why),
                    });
                }
                if pin_keeps(pinned, child.run.as_str()).is_some() {
                    return Some(KeepReason::PackageRun {
                        run: child.run.to_string(),
                        why: Box::new(KeepReason::Pinned),
                    });
                }
                pending.push(child.run);
            }
        }
        checkout_beneath(path)
    }
}

/// Why a run in `state` is kept, when it is.
fn state_keeps(state: State) -> Option<KeepReason> {
    match state {
        State::Prepared
        | State::Running
        | State::Verifying
        | State::Repairing
        | State::Escalating => Some(KeepReason::NotEnded { state }),
        State::NeedsReview | State::NeedsDecision | State::Interrupted => {
            Some(KeepReason::WaitsOnPerson { state })
        }
        State::Accepted
        | State::Blocked
        | State::Failed
        | State::BudgetExhausted
        | State::Cancelled
        | State::AcceptedByPerson => None,
    }
}

fn pin_keeps(pinned: &std::collections::BTreeSet<(String, u32)>, run: &str) -> Option<KeepReason> {
    pinned
        .range((run.to_string(), 0)..=(run.to_string(), u32::MAX))
        .next()
        .map(|_| KeepReason::Pinned)
}

/// A checkout under `dir` (a directory holding `.git`, or the legacy
/// `worktree`), which is the run's work and never pruned with it. A
/// directory that cannot be read keeps the run.
fn checkout_beneath(dir: &Path) -> Option<KeepReason> {
    if dir.join(crate::paths::LEGACY_WORKTREE_DIR).exists() {
        return Some(KeepReason::CheckoutBeneath { at: None });
    }
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(e) => return Some(KeepReason::unreadable(&current.display().to_string(), e)),
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => return Some(KeepReason::unreadable(&current.display().to_string(), e)),
            };
            let child = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => {
                    if child.join(".git").exists() {
                        return Some(KeepReason::CheckoutBeneath { at: Some(child) });
                    }
                    pending.push(child);
                }
                Ok(_) => {}
                Err(e) => return Some(KeepReason::unreadable(&child.display().to_string(), e)),
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

/// The automatic prune's age, as the environment and the default make it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoPruneSetting {
    /// `RELAIS_AUTO_PRUNE_DAYS=0`.
    Off,
    /// Prune runs untouched for this many days.
    Days(u64),
    /// The variable holds something that is not a number of days.
    Invalid(String),
}

/// Where the automatic prune's setting came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingSource {
    Default,
    Environment,
}

/// Parse the variable alone: the default when it is unset. `--no-prune` is
/// a per-call choice of the command line and never reaches here.
pub fn auto_prune_setting(env_days: Option<&str>) -> (AutoPruneSetting, SettingSource) {
    match env_days {
        None => (
            AutoPruneSetting::Days(DEFAULT_AUTO_PRUNE_DAYS),
            SettingSource::Default,
        ),
        Some(text) => {
            let setting = match text.trim().parse::<u64>() {
                Ok(0) => AutoPruneSetting::Off,
                Ok(days) => AutoPruneSetting::Days(days),
                Err(_) => AutoPruneSetting::Invalid(text.to_string()),
            };
            (setting, SettingSource::Environment)
        }
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

/// The step of a prune that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneStep {
    Lock,
    Stamp,
    ReadingRuns,
    FinishingLeftover,
    DeletingRun,
}

impl PruneStep {
    fn describe(self) -> &'static str {
        match self {
            Self::Lock => "taking the lock",
            Self::Stamp => "writing the stamp",
            Self::ReadingRuns => "reading runs/",
            Self::FinishingLeftover => "finishing a leftover",
            Self::DeletingRun => "deleting a run",
        }
    }
}

/// A prune failure: the step, the path it was working on and the cause.
/// `Display` includes the cause, so callers print it with `{}` and need not
/// walk the error chain.
#[derive(Debug)]
pub struct PruneFailure {
    pub step: PruneStep,
    /// Where the failed step left the path it worked on.
    pub at: Left,
    pub error: io::Error,
}

impl PruneFailure {
    pub fn new(step: PruneStep, path: &Path, error: io::Error) -> Self {
        Self {
            step,
            at: Left::Original(path.to_path_buf()),
            error,
        }
    }

    pub fn with_left(step: PruneStep, left: Left, error: io::Error) -> Self {
        Self {
            step,
            at: left,
            error,
        }
    }

    /// See [`Left::hint`].
    pub fn hint(&self) -> &'static str {
        self.at.hint()
    }
}

impl std::fmt::Display for PruneFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}: {}",
            self.step.describe(),
            self.at.path().display(),
            self.error
        )
    }
}

impl std::error::Error for PruneFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// A run directory the automatic prune deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedRun {
    pub run: String,
    pub age: Duration,
    pub bytes: u64,
    /// Evidence rows its deletion left pointing into it.
    pub dangling: usize,
}

#[derive(Debug)]
pub struct AutoPruneReport {
    /// Run directories older than the age, kept or not.
    pub listed: usize,
    /// The ones deleted.
    pub removed: Vec<RemovedRun>,
    /// Failures, finishing a leftover or deleting.
    pub failed: Vec<PruneFailure>,
}

fn stamp_is_fresh(stamp: &Path, now: SystemTime) -> bool {
    // A stamp from the future (a clock set back) counts as stale.
    std::fs::metadata(stamp)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| now.duration_since(modified).ok())
        .is_some_and(|since| since < THROTTLE)
}

/// Take `runs-prune.lock` without waiting: `None` when another prune holds it.
pub fn try_lock_prune(state_dir: &Path) -> Result<Option<crate::procs::LockFile>, PruneFailure> {
    let path = state_dir.join(LOCK_FILE);
    crate::procs::LockFile::try_acquire(&path)
        .map_err(|error| PruneFailure::new(PruneStep::Lock, &path, error))
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
) -> Result<AutoPrune, PruneFailure> {
    let stamp = state_dir.join(STAMP_FILE);
    if stamp_is_fresh(&stamp, now) {
        return Ok(AutoPrune::Throttled);
    }
    let Some(_lock) = try_lock_prune(state_dir)? else {
        return Ok(AutoPrune::Held);
    };
    if stamp_is_fresh(&stamp, now) {
        return Ok(AutoPrune::Throttled);
    }
    std::fs::write(&stamp, format!("{}\n", crate::ledger::now_rfc3339()))
        .map_err(|error| PruneFailure::new(PruneStep::Stamp, &stamp, error))?;
    let runs = state_dir.join(crate::paths::RUNS_DIR);
    let mut failed: Vec<PruneFailure> = finish_interrupted(&runs)
        .into_iter()
        .map(|failed| {
            PruneFailure::with_left(
                PruneStep::FinishingLeftover,
                failed.left_on_disk,
                failed.error,
            )
        })
        .collect();
    let judge = RunJudge::load(ledger, &state_dir.join(crate::paths::BASELINE_CACHE_DIR));
    let plan = plan_run_prune(&runs, age, now, &|run, path| judge.judge(run, path))
        .map_err(|error| PruneFailure::new(PruneStep::ReadingRuns, &runs, error))?;
    let applied = apply(&judge, &plan);
    failed.extend(applied.failed.into_iter().map(|failed| {
        PruneFailure::with_left(PruneStep::DeletingRun, failed.left_on_disk, failed.error)
    }));
    let removed = plan
        .iter()
        .filter_map(|dir| {
            let gone = applied.removed.iter().find(|gone| gone.path == dir.path)?;
            Some(RemovedRun {
                run: dir.run.clone(),
                age: dir.age,
                bytes: dir.bytes,
                dangling: gone.dangling,
            })
        })
        .collect();
    Ok(AutoPrune::Ran(AutoPruneReport {
        listed: plan.len(),
        removed,
        failed,
    }))
}

/// [`tree_bytes`] of `path` without the subtree at `skip`. A lower bound:
/// an entry or listing that cannot be read counts as 0.
/// holds-until: the doctor reports a state-directory size that is read as
/// too low
pub fn tree_bytes_except(path: &Path, skip: &Path) -> u64 {
    if path == skip {
        return 0;
    }
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| tree_bytes_except(&e.path(), skip))
                .sum()
        })
        .unwrap_or(0)
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
            "old-live" => RunVerdict::Kept(KeepReason::NotEnded {
                state: State::Running,
            }),
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
    fn the_setting_follows_environment_then_default() {
        let days = |text| auto_prune_setting(Some(text));
        assert_eq!(days("0").0, AutoPruneSetting::Off);
        assert_eq!(
            days("7"),
            (AutoPruneSetting::Days(7), SettingSource::Environment)
        );
        assert_eq!(
            auto_prune_setting(None),
            (AutoPruneSetting::Days(30), SettingSource::Default)
        );
        assert_eq!(
            days("soon").0,
            AutoPruneSetting::Invalid("soon".to_string())
        );
    }

    #[test]
    fn every_keep_reason_has_a_pathless_label() {
        let reasons = [
            KeepReason::NotEnded {
                state: State::Running,
            },
            KeepReason::WaitsOnPerson {
                state: State::Interrupted,
            },
            KeepReason::NotInLedger,
            KeepReason::NoReceipt,
            KeepReason::Pinned,
            KeepReason::PackageRun {
                run: "r-1".into(),
                why: Box::new(KeepReason::Pinned),
            },
            KeepReason::CheckoutBeneath {
                at: Some(PathBuf::from("/s/runs/r/c")),
            },
            KeepReason::CitedByCache { entries: 2 },
            KeepReason::Unreadable {
                what: "/s/runs".into(),
                error: "denied".into(),
            },
        ];
        for reason in reasons {
            let label = reason.label();
            assert!(!label.contains('/') && !label.contains("r-1"), "{label}");
        }
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
        let _held = crate::procs::LockFile::try_acquire(&state.join(LOCK_FILE))
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
            (dir.run == "run-a").then_some(KeepReason::Pinned)
        });
        assert!(root.join("run-a").exists());
        assert!(!root.join("run-b").exists());
        assert_eq!(applied.kept.len(), 1);
        std::fs::remove_dir_all(&root).ok(); // best-effort scratch cleanup
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_delete_after_the_rename_names_the_pruning_directory() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("failed-delete");
        let locked = root.join("run-a/locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(locked.join("f"), "x").unwrap();
        let judge = |_: &str, _: &Path| RunVerdict::Prunable {
            dangling: Vec::new(),
        };
        let later = SystemTime::now() + 31 * DAY;
        let plan = plan_run_prune(&root, 30 * DAY, later, &judge).unwrap();
        // A directory that cannot be written keeps its file from unlinking.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        let applied = apply_run_prune(&plan, &|_| None);
        let doomed = root.join(".pruning-run-a");
        if applied.failed.is_empty() {
            // Running as root nothing is refused, so there is no failure to name.
            assert!(!doomed.exists());
            std::fs::remove_dir_all(&root).expect("remove scratch");
            eprintln!("skipped: Running as root, nothing is refused");
            return;
        }
        // The rename moved the locked directory: restore it where it is now.
        let locked_now = if doomed.exists() {
            doomed.join("locked")
        } else {
            locked.clone()
        };
        if locked_now.exists() {
            std::fs::set_permissions(&locked_now, std::fs::Permissions::from_mode(0o755))
                .expect("restore write permission where the directory is now");
        }
        assert_eq!(applied.failed.len(), 1, "{:?}", applied.failed);
        assert_eq!(
            applied.failed[0].left_on_disk,
            Left::Renamed(doomed.clone())
        );
        assert!(doomed.exists() && !root.join("run-a").exists());
        assert!(applied.removed.is_empty());
        std::fs::remove_dir_all(&root).expect("no scratch directory is left behind");
        assert!(!root.exists());
    }

    #[test]
    fn a_removed_run_carries_its_dangling_count() {
        let root = scratch("dangling");
        std::fs::create_dir_all(root.join("run-a")).unwrap();
        let rows = vec![("log".to_string(), "/x".to_string())];
        let judge = |_: &str, _: &Path| RunVerdict::Prunable {
            dangling: rows.clone(),
        };
        let later = SystemTime::now() + 31 * DAY;
        let plan = plan_run_prune(&root, 30 * DAY, later, &judge).unwrap();
        let applied = apply_run_prune(&plan, &|_| None);
        assert_eq!(
            applied.removed,
            vec![RemovedDir {
                path: root.join("run-a"),
                dangling: 1
            }]
        );
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
