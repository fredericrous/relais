//! In-place runs (ADR-0008): the worker edits the caller's own linked
//! worktree instead of a fork.
//!
//! What makes that safe is four things this module owns:
//!
//! * a LOCK per worktree, held by the kernel for the run's life, plus a
//!   per-run MARKER that outlives a crash (an interrupted run is a marker
//!   with a free lock);
//! * a START SNAPSHOT `S`, a commit of the working tree (dirty edits
//!   included) built in a PRIVATE index, so the person's real index is
//!   never staged, and kept under `refs/relais/starts/<run>`;
//! * a persisted FINALIZATION: the tree the worktree must end as (the
//!   accepted candidate, or `S`) is written to the marker before the tree
//!   is touched, and applying it is idempotent, so a crash at any point is
//!   finished by `resume --retire`;
//! * an APPLY that never destroys what it did not make: a path-type change
//!   over a directory holding ignored or unknown files is refused before
//!   anything is deleted.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::procs::LockFile;
use crate::workspace::{dirty_paths, git_command, SNAPSHOT_DATE};

fn io_other(error: impl ToString) -> io::Error {
    io::Error::other(error.to_string())
}

/// One git command in `dir`, optionally against a private index. The
/// output is trimmed text; a failure carries git's own message.
fn git_text(dir: &Path, index: Option<&Path>, args: &[&str]) -> io::Result<String> {
    let mut command = git_command(dir);
    command.args(args);
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let output = command.output()?;
    if !output.status.success() {
        return Err(io_other(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The absolute git directory of the worktree at `root`
/// (`.git/worktrees/<name>` for a linked one).
pub fn git_dir(root: &Path) -> io::Result<PathBuf> {
    let dir = git_text(
        root,
        None,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
    )?;
    Ok(PathBuf::from(dir))
}

// ------------------------------------------------------------- lease

/// What the per-run marker says about how far the run got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// The run started; no decision about the tree's end has been made.
    Running,
    /// The end tree is decided and written in [`Marker::target`].
    Finalizing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    /// The accepted candidate: the worktree is left holding the change.
    Accepted,
    /// The start snapshot: the worktree is returned to how it was.
    Start,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub kind: TargetKind,
    /// The tree object the worktree must equal.
    pub tree: String,
}

/// The per-run record under `<git-dir>/relais-inplace/<run>.json`. It is
/// removed last, and only once the target is applied and verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub run: String,
    pub stage: Stage,
    /// `HEAD` as a symbolic ref (`refs/heads/x`), when it was one.
    pub head_symbolic: Option<String>,
    pub head_sha: String,
    /// The start snapshot commit.
    pub start: String,
    /// `git ls-files -s` of the real index at start: what "unchanged"
    /// means at the end.
    pub index_listing: String,
    pub target: Option<Target>,
    /// The ref the unkept work was exported to, once it was.
    pub exported: Option<String>,
}

#[derive(Debug)]
pub enum LeaseRefusal {
    /// Another run holds the worktree.
    Busy {
        holder: Option<String>,
    },
    /// A run was interrupted here and has not been retired.
    Interrupted {
        run: String,
    },
    Io(io::Error),
}

impl From<io::Error> for LeaseRefusal {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// The right to edit one worktree. Dropping it releases the lock; the
/// marker stays until [`Lease::finish`].
pub struct Lease {
    _lock: LockFile,
    dir: PathBuf,
    root: PathBuf,
    run: String,
}

const LOCK_FILE: &str = "relais-inplace.lock";
const HOLDER_FILE: &str = "relais-inplace.holder";
const MARKERS_DIR: &str = "relais-inplace";

fn markers_dir(git_dir: &Path) -> PathBuf {
    git_dir.join(MARKERS_DIR)
}

fn marker_path(git_dir: &Path, run: &str) -> PathBuf {
    markers_dir(git_dir).join(format!("{run}.json"))
}

/// Markers left in `git_dir` by runs that are not running.
pub fn leftover_markers(git_dir: &Path) -> io::Result<Vec<Marker>> {
    let dir = markers_dir(git_dir);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(marker) = serde_json::from_str::<Marker>(&text) {
                    found.push(marker);
                }
            }
        }
    }
    found.sort_by(|a, b| a.run.cmp(&b.run));
    Ok(found)
}

impl Lease {
    /// Take the worktree for `run`. Refuses when another run holds the lock
    /// (`Busy`, atomic) or when a previous run died here without being
    /// retired (`Interrupted`): its partial edits are in the tree and
    /// nothing may start on top of them unseen.
    pub fn acquire(root: &Path, run: &str) -> Result<Self, LeaseRefusal> {
        let git_dir = git_dir(root)?;
        std::fs::create_dir_all(markers_dir(&git_dir))?;
        let Some(lock) = LockFile::try_acquire(&git_dir.join(LOCK_FILE))? else {
            let holder = std::fs::read_to_string(git_dir.join(HOLDER_FILE))
                .ok()
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty());
            return Err(LeaseRefusal::Busy { holder });
        };
        if let Some(left) = leftover_markers(&git_dir)?.into_iter().next() {
            return Err(LeaseRefusal::Interrupted { run: left.run });
        }
        crate::fsutil::write_atomic(&git_dir.join(HOLDER_FILE), run)?;
        Ok(Self {
            _lock: lock,
            dir: git_dir,
            root: root.to_path_buf(),
            run: run.to_string(),
        })
    }

    /// Take back the worktree of a run that died here, to finish what it
    /// decided. Needs the lock, so a live run is never taken over; needs
    /// the run's marker, so there is something to finish.
    pub fn reclaim(root: &Path, run: &str) -> Result<Self, LeaseRefusal> {
        let git_dir = git_dir(root)?;
        let Some(lock) = LockFile::try_acquire(&git_dir.join(LOCK_FILE))? else {
            let holder = std::fs::read_to_string(git_dir.join(HOLDER_FILE))
                .ok()
                .map(|text| text.trim().to_string());
            return Err(LeaseRefusal::Busy { holder });
        };
        read_marker(&git_dir, run)?;
        Ok(Self {
            _lock: lock,
            dir: git_dir,
            root: root.to_path_buf(),
            run: run.to_string(),
        })
    }

    pub fn git_dir(&self) -> &Path {
        &self.dir
    }

    /// The index relais itself reads and writes: private to this run, so
    /// nothing the worker does through a variable or a flag reaches it.
    pub fn relais_index(&self) -> PathBuf {
        markers_dir(&self.dir).join(format!("{}.relais.index", self.run))
    }

    fn orig_index(&self) -> PathBuf {
        markers_dir(&self.dir).join(format!("{}.index.orig", self.run))
    }

    /// Write the marker (atomically) for the run's current stage.
    pub fn write_marker(&self, marker: &Marker) -> io::Result<()> {
        let text = serde_json::to_string_pretty(marker).map_err(io_other)?;
        crate::fsutil::write_atomic(&marker_path(&self.dir, &self.run), &text)
    }

    pub fn read_marker(&self) -> io::Result<Marker> {
        read_marker(&self.dir, &self.run)
    }

    /// The end of a clean run: the marker and the run's private files go,
    /// last. The original-index copy stays when the real index changed.
    pub fn finish(self, keep_original_index: bool) -> io::Result<()> {
        // The run's private files are scratch: one left behind costs disk
        // and nothing else, so a failure to remove them must not keep the
        // marker alive and make the next run read a finished one as dead.
        // The marker is the record, and its removal is the one that counts.
        for scratch in [self.relais_index(), self.dir.join(HOLDER_FILE)]
            .into_iter()
            .chain((!keep_original_index).then(|| self.orig_index()))
        {
            if let Err(e) = std::fs::remove_file(&scratch) {
                if e.kind() != io::ErrorKind::NotFound {
                    eprintln!("relais: {} was not removed: {e}", scratch.display());
                }
            }
        }
        std::fs::remove_file(marker_path(&self.dir, &self.run))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn run(&self) -> &str {
        &self.run
    }

    /// Keep a copy of the real index, so a worker that bypasses the private
    /// one and stages something leaves evidence the person can restore.
    pub fn keep_original_index(&self) -> io::Result<()> {
        let real = self.dir.join("index");
        if real.is_file() {
            std::fs::copy(real, self.orig_index())?;
        }
        Ok(())
    }
}

pub fn read_marker(git_dir: &Path, run: &str) -> io::Result<Marker> {
    let text = std::fs::read_to_string(marker_path(git_dir, run))?;
    serde_json::from_str(&text).map_err(io_other)
}

// ---------------------------------------------------- start snapshot

/// What the worktree looked like when the run took it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Start {
    /// The commit every diff, scope check and verification base is taken
    /// against: `HEAD` when the tree is clean, else a synthetic commit of
    /// the working tree.
    pub sha: String,
    /// Uncommitted paths the start snapshot includes; empty when clean.
    pub dirty: Vec<String>,
    pub head_symbolic: Option<String>,
    pub head_sha: String,
    pub index_listing: String,
}

/// A tree of the whole working tree (tracked and untracked-not-ignored,
/// `.relais/` scratch excluded) built in the private `index`, which is
/// reset to `HEAD` first so nothing from a previous call leaks in.
pub fn working_tree(root: &Path, index: &Path) -> io::Result<String> {
    // A stale private index is rebuilt below; one that cannot be removed
    // fails the `read-tree` that follows, which is reported.
    std::fs::remove_file(index).or_else(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(e)
        }
    })?;
    git_text(root, Some(index), &["read-tree", "HEAD"])?;
    git_text(
        root,
        Some(index),
        &["add", "-A", "--", ".", ":(exclude).relais"],
    )?;
    git_text(root, Some(index), &["write-tree"])
}

fn commit_of(root: &Path, tree: &str, parent: &str, message: &str) -> io::Result<String> {
    let output = git_command(root)
        .args(["commit-tree", tree, "-p", parent, "-m", message])
        .env("GIT_AUTHOR_NAME", "relais")
        .env("GIT_AUTHOR_EMAIL", "relais@localhost")
        .env("GIT_AUTHOR_DATE", SNAPSHOT_DATE)
        .env("GIT_COMMITTER_NAME", "relais")
        .env("GIT_COMMITTER_EMAIL", "relais@localhost")
        .env("GIT_COMMITTER_DATE", SNAPSHOT_DATE)
        .output()?;
    if !output.status.success() {
        return Err(io_other(format!(
            "commit-tree failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// `HEAD` as `(symbolic ref if any, sha)`.
pub fn head(root: &Path) -> io::Result<(Option<String>, String)> {
    let sha = git_text(root, None, &["rev-parse", "HEAD"])?;
    let symbolic = git_text(root, None, &["symbolic-ref", "-q", "HEAD"]).ok();
    Ok((symbolic, sha))
}

/// Snapshot the worktree for `lease`'s run. The person's real index and
/// working files are read, never changed.
pub fn start_snapshot(lease: &Lease) -> io::Result<Start> {
    let root = lease.root();
    let (head_symbolic, head_sha) = head(root)?;
    let index_listing = git_text(root, None, &["ls-files", "-s"])?;
    let dirty = dirty_paths(root).map_err(io_other)?;
    let sha = if dirty.is_empty() {
        head_sha.clone()
    } else {
        let tree = working_tree(root, &lease.relais_index())?;
        commit_of(root, &tree, &head_sha, "relais start snapshot")?
    };
    git_text(
        root,
        None,
        &[
            "update-ref",
            &format!("refs/relais/starts/{}", lease.run()),
            &sha,
        ],
    )?;
    lease.keep_original_index()?;
    Ok(Start {
        sha,
        dirty,
        head_symbolic,
        head_sha,
        index_listing,
    })
}

// ---------------------------------------------------------- apply

/// A path the apply refuses to touch: something the worktree holds that is
/// neither in the tree being left nor in the tree being restored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collision {
    pub path: String,
    pub why: &'static str,
}

#[derive(Debug)]
pub enum ApplyError {
    /// Nothing was changed.
    Collisions(Vec<Collision>),
    Io(io::Error),
    /// The tree after applying is not the target.
    Mismatch {
        expected: String,
        found: String,
    },
}

impl From<io::Error> for ApplyError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Collisions(found) => {
                let paths: Vec<String> = found
                    .iter()
                    .map(|collision| format!("{} ({})", collision.path, collision.why))
                    .collect();
                write!(
                    f,
                    "restoring would replace a path that holds files relais did not make: {}",
                    paths.join(", ")
                )
            }
            Self::Io(e) => write!(f, "{e}"),
            Self::Mismatch { expected, found } => write!(
                f,
                "the worktree is tree {found} after applying, not {expected}"
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Change {
    Delete(String),
    Write(String),
}

fn changes(root: &Path, index: &Path, from: &str, to: &str) -> io::Result<Vec<Change>> {
    let raw = {
        let output = git_command(root)
            .args(["diff-tree", "-r", "-z", "--no-renames", "--raw", from, to])
            .env("GIT_INDEX_FILE", index)
            .output()?;
        if !output.status.success() {
            return Err(io_other(format!(
                "git diff-tree failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        output.stdout
    };
    // `:<modes> <shas> <status>\0<path>\0`, repeated.
    let mut out = Vec::new();
    let mut parts = raw.split(|byte| *byte == 0).filter(|part| !part.is_empty());
    while let Some(meta) = parts.next() {
        let Some(path) = parts.next() else { break };
        let meta = String::from_utf8_lossy(meta);
        let path = String::from_utf8_lossy(path).to_string();
        match meta.split_whitespace().last() {
            Some("D") => out.push(Change::Delete(path)),
            Some(_) => out.push(Change::Write(path)),
            None => {}
        }
    }
    Ok(out)
}

fn ancestors(path: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut current = Path::new(path).parent();
    while let Some(parent) = current {
        if parent.as_os_str().is_empty() {
            break;
        }
        found.push(parent.to_string_lossy().to_string());
        current = parent.parent();
    }
    found
}

/// Files and symlinks below `dir` (relative to `root`), recursively.
fn leaves_below(root: &Path, dir: &str) -> io::Result<Vec<String>> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_string()];
    while let Some(current) = pending.pop() {
        for entry in std::fs::read_dir(root.join(&current))? {
            let entry = entry?;
            let relative = format!("{current}/{}", entry.file_name().to_string_lossy());
            if entry.file_type()?.is_dir() {
                pending.push(relative);
            } else {
                found.push(relative);
            }
        }
    }
    Ok(found)
}

/// Which writes and deletions would run into something they must not
/// destroy: a directory that must become a file but holds files outside
/// the deletions, or a file (ignored, say) sitting where a directory must
/// be created.
fn collisions(root: &Path, plan: &[Change]) -> io::Result<Vec<Collision>> {
    let deleting: BTreeSet<&str> = plan
        .iter()
        .filter_map(|change| match change {
            Change::Delete(path) => Some(path.as_str()),
            Change::Write(_) => None,
        })
        .collect();
    let mut found = Vec::new();
    for change in plan {
        let Change::Write(path) = change else {
            continue;
        };
        let target = root.join(path);
        if let Ok(meta) = std::fs::symlink_metadata(&target) {
            if meta.is_dir() {
                for leaf in leaves_below(root, path)? {
                    if !deleting.contains(leaf.as_str()) {
                        found.push(Collision {
                            path: leaf,
                            why: "inside a directory that must become a file",
                        });
                    }
                }
            }
        }
        for ancestor in ancestors(path) {
            if let Ok(meta) = std::fs::symlink_metadata(root.join(&ancestor)) {
                if !meta.is_dir() && !deleting.contains(ancestor.as_str()) {
                    found.push(Collision {
                        path: ancestor,
                        why: "a file where a directory must be created",
                    });
                }
            }
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found.dedup();
    Ok(found)
}

/// Make the worktree at `root` equal `target_tree`, touching only what
/// differs and never what it cannot account for.
///
/// The current tree (tracked and untracked-not-ignored files) is read into
/// the private `index`; the difference to `target_tree` becomes deletions
/// (post-order: files first, then directories only once empty) and writes
/// (pre-order, each by `git checkout-index` against an index loaded with
/// the target). Ignored files outside a collision are never touched, and
/// the real index never is. Idempotent: it recomputes from what is there,
/// so a run killed halfway is finished by running it again.
pub fn apply_tree(root: &Path, index: &Path, target_tree: &str) -> Result<(), ApplyError> {
    let current = working_tree(root, index)?;
    if current == target_tree {
        return Ok(());
    }
    let mut plan = changes(root, index, &current, target_tree)?;
    let collided = collisions(root, &plan)?;
    if !collided.is_empty() {
        return Err(ApplyError::Collisions(collided));
    }
    let mut deletions: Vec<&String> = plan
        .iter()
        .filter_map(|change| match change {
            Change::Delete(path) => Some(path),
            Change::Write(_) => None,
        })
        .collect();
    deletions.sort_by_key(|path| std::cmp::Reverse(path.matches('/').count()));
    for path in deletions {
        let full = root.join(path);
        match std::fs::remove_file(&full) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let mut parent = full.parent().map(Path::to_path_buf);
        while let Some(dir) = parent {
            if dir == root || std::fs::remove_dir(&dir).is_err() {
                break;
            }
            parent = dir.parent().map(Path::to_path_buf);
        }
    }
    plan.retain(|change| matches!(change, Change::Write(_)));
    plan.sort_by(|a, b| match (a, b) {
        (Change::Write(a), Change::Write(b)) => a.cmp(b),
        _ => std::cmp::Ordering::Equal,
    });
    std::fs::remove_file(index).or_else(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(e)
        }
    })?;
    git_text(root, Some(index), &["read-tree", target_tree])?;
    for change in &plan {
        let Change::Write(path) = change else {
            continue;
        };
        if let Some(parent) = root.join(path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        git_text(root, Some(index), &["checkout-index", "-f", "--", path])?;
    }
    let after = working_tree(root, index)?;
    if after != target_tree {
        return Err(ApplyError::Mismatch {
            expected: target_tree.to_string(),
            found: after,
        });
    }
    Ok(())
}

// ------------------------------------------------------- finalization

/// What the end of an in-place run decided to do with the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The tree now equals the target, and `index` says what became of the
    /// person's own index.
    Applied { index: IndexOutcome },
    /// `HEAD` moved during the run (a worker commit); tree and branch were
    /// left exactly as they are.
    HeadMoved { was: String, now: String },
}

/// What became of the person's own git index. Only something outside
/// relais's private index can have changed it: a worker that ran `git add`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexOutcome {
    /// Its listing at the end is the one at the start.
    Untouched,
    /// It had changed, and the byte copy taken at the start is back in place.
    Restored,
    /// It had changed and was left so; the copy of the original is kept
    /// beside the markers.
    Changed(Unrestored),
}

/// Why a changed index was left as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unrestored {
    /// The caller does not restore (`IndexPolicy::Keep`).
    NotAsked,
    /// The restore was tried and failed: the lock was held, the rename was
    /// refused, the copy was missing, or the listing still differed.
    Failed(String),
}

/// What `finalize` does about a changed index. The run that did the work
/// restores it; a later `resume --retire` does not, because staging done
/// after the run died is not the worker's to discard (ADR-0009).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexPolicy {
    Restore,
    Keep,
}

/// Put the byte copy of the index taken at the start back.
///
/// Takes git's own `index.lock` first (create-new): a git running at that
/// moment then fails with "index.lock exists" instead of racing the rename,
/// and a lock already there means one is running and the restore is left.
/// The lock is removed on every error path. Success is only claimed when the
/// listing afterwards equals the one at the start.
fn restore_index(lease: &Lease, listing_at_start: &str) -> io::Result<()> {
    let real = lease.dir.join("index");
    let lock = lease.dir.join("index.lock");
    let mut staged = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)?;
    let put_back = (|| {
        let mut original = std::fs::File::open(lease.orig_index())?;
        io::copy(&mut original, &mut staged)?;
        staged.sync_all()?;
        drop(staged);
        std::fs::rename(&lock, &real)
    })();
    if let Err(e) = put_back {
        // The lock is ours and the rename did not consume it.
        let _ = std::fs::remove_file(&lock);
        return Err(e);
    }
    if real_index_changed(&lease.root, listing_at_start)? {
        return Err(io_other(
            "the restored index does not list what it did at the start",
        ));
    }
    Ok(())
}

/// Decide, record, export, apply, and only then remove the marker.
///
/// `export` writes whatever no candidate holds to the run's `final` ref and
/// returns that ref; it is called once, and a resume finds `exported` in
/// the marker and skips it, so a half-restored tree never overwrites it.
pub fn finalize(
    lease: Lease,
    target: Target,
    index: IndexPolicy,
    export: &mut dyn FnMut() -> io::Result<String>,
) -> Result<Outcome, ApplyError> {
    let mut marker = lease.read_marker()?;
    let (_, now) = head(lease.root())?;
    if now != marker.head_sha {
        let was = marker.head_sha.clone();
        if marker.exported.is_none() {
            marker.exported = Some(export()?);
            lease.write_marker(&marker)?;
        }
        lease.finish(true)?;
        return Ok(Outcome::HeadMoved { was, now });
    }
    // A decision already on the marker wins over the one offered now: a
    // run that was killed after deciding is finished the way it decided.
    let target = marker.target.clone().unwrap_or(target);
    marker.stage = Stage::Finalizing;
    marker.target = Some(target.clone());
    lease.write_marker(&marker)?;
    if marker.exported.is_none() {
        marker.exported = Some(export()?);
        lease.write_marker(&marker)?;
    }
    apply_tree(lease.root(), &lease.relais_index(), &target.tree)?;
    let outcome = if !real_index_changed(lease.root(), &marker.index_listing)? {
        IndexOutcome::Untouched
    } else if index == IndexPolicy::Restore {
        match restore_index(&lease, &marker.index_listing) {
            Ok(()) => IndexOutcome::Restored,
            Err(error) => IndexOutcome::Changed(Unrestored::Failed(error.to_string())),
        }
    } else {
        IndexOutcome::Changed(Unrestored::NotAsked)
    };
    lease.finish(matches!(outcome, IndexOutcome::Changed(_)))?;
    Ok(Outcome::Applied { index: outcome })
}

/// Did the person's real index change since the run started?
pub fn real_index_changed(root: &Path, listing_at_start: &str) -> io::Result<bool> {
    Ok(git_text(root, None, &["ls-files", "-s"])? != listing_at_start)
}

/// The run, other than `except`, that holds the worktree at `root`: any
/// `*.json` under its `relais-inplace/` directory counts, parseable or not,
/// because a marker relais cannot read is still a run it cannot vouch for.
/// `Err` when the git directory cannot be read, which callers treat as "do
/// not clone from here": a tree whose state is unknown is not a safe source.
pub fn held_by(root: &Path, except: Option<&str>) -> io::Result<Option<String>> {
    let dir = markers_dir(&git_dir(root)?);
    if !dir.is_dir() {
        return Ok(None);
    }
    let mut held = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            if let Some(run) = path.file_stem().and_then(|stem| stem.to_str()) {
                if Some(run) != except {
                    held.push(run.to_string());
                }
            }
        }
    }
    held.sort();
    Ok(held.into_iter().next())
}

/// A stable name for a marker's digest, for messages.
pub fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{temp_dir, TempDir};

    fn git(dir: &Path, args: &[&str]) -> String {
        git_text(dir, None, args).unwrap_or_else(|e| panic!("{args:?}: {e}"))
    }

    /// A repository and a linked worktree of it, `.gitignore` ignoring
    /// `*.log` and `cache/`, one tracked file.
    fn linked_worktree() -> (TempDir, PathBuf) {
        let base = temp_dir("inplace");
        let live = base.join("live");
        std::fs::create_dir_all(&live).expect("mkdir");
        let no_hooks = base.join("no-hooks");
        std::fs::create_dir_all(&no_hooks).expect("mkdir");
        git(&live, &["init", "-q", "-b", "main"]);
        git(
            &live,
            &["config", "core.hooksPath", &no_hooks.to_string_lossy()],
        );
        git(&live, &["config", "user.email", "t@t"]);
        git(&live, &["config", "user.name", "t"]);
        // Byte-exact checkouts: the tests compare file contents, and git's
        // CRLF conversion on Windows would read back "a\r\n" for "a\n".
        git(&live, &["config", "core.autocrlf", "false"]);
        std::fs::write(live.join(".gitignore"), "*.log\ncache/\n").expect("ignore");
        std::fs::write(live.join("a.txt"), "a\n").expect("a");
        git(&live, &["add", "-A"]);
        git(&live, &["commit", "-q", "-m", "base"]);
        let wt = base.join("wt");
        git(
            &live,
            &["worktree", "add", "-q", "-b", "task", &wt.to_string_lossy()],
        );
        (base, wt)
    }

    fn tree_of(dir: &Path, rev: &str) -> String {
        git(dir, &["rev-parse", &format!("{rev}^{{tree}}")])
    }

    #[test]
    fn one_run_holds_a_worktree_and_a_dead_one_blocks_the_next() {
        let (_keep, wt) = linked_worktree();
        let first = Lease::acquire(&wt, "run-1").expect("free");
        match Lease::acquire(&wt, "run-2") {
            Err(LeaseRefusal::Busy { holder }) => assert_eq!(holder.as_deref(), Some("run-1")),
            other => panic!("a held worktree is busy: {:?}", other.err()),
        }
        let (symbolic, sha) = head(&wt).expect("head");
        first
            .write_marker(&Marker {
                run: "run-1".into(),
                stage: Stage::Running,
                head_symbolic: symbolic,
                head_sha: sha.clone(),
                start: sha,
                index_listing: String::new(),
                target: None,
                exported: None,
            })
            .expect("marker");
        drop(first);
        match Lease::acquire(&wt, "run-2") {
            Err(LeaseRefusal::Interrupted { run }) => assert_eq!(run, "run-1"),
            other => panic!("an unretired marker blocks: {:?}", other.err()),
        }
    }

    #[test]
    fn a_clean_start_is_head_and_a_dirty_one_is_a_commit_of_the_working_tree() {
        let (_keep, wt) = linked_worktree();
        let lease = Lease::acquire(&wt, "run-1").expect("lease");
        let clean = start_snapshot(&lease).expect("clean");
        assert_eq!(clean.sha, clean.head_sha);
        assert!(clean.dirty.is_empty());
        drop(lease);

        std::fs::write(wt.join("a.txt"), "edited\n").expect("edit");
        std::fs::write(wt.join("new.txt"), "new\n").expect("new");
        git(&wt, &["add", "new.txt"]); // staged by the person
        let staged_before = git(&wt, &["ls-files", "-s"]);
        let status_before = git(&wt, &["status", "--porcelain"]);
        let lease = Lease::acquire(&wt, "run-2").expect("lease");
        let dirty = start_snapshot(&lease).expect("dirty");
        assert_ne!(dirty.sha, dirty.head_sha, "a synthetic commit");
        assert_eq!(dirty.dirty.len(), 2, "{:?}", dirty.dirty);
        assert_eq!(
            git(&wt, &["show", &format!("{}:a.txt", dirty.sha)]),
            "edited"
        );
        assert_eq!(
            git(&wt, &["rev-parse", "refs/relais/starts/run-2"]),
            dirty.sha,
            "kept so the uncommitted work is recoverable"
        );
        assert_eq!(
            git(&wt, &["ls-files", "-s"]),
            staged_before,
            "the real index is untouched"
        );
        assert_eq!(git(&wt, &["status", "--porcelain"]), status_before);
    }

    #[test]
    fn applying_the_start_tree_undoes_edits_additions_and_deletions_but_not_ignored_files() {
        let (_keep, wt) = linked_worktree();
        std::fs::write(wt.join("keep.txt"), "keep\n").expect("keep");
        let lease = Lease::acquire(&wt, "run-1").expect("lease");
        let start = start_snapshot(&lease).expect("start");
        let start_tree = tree_of(&wt, &start.sha);

        std::fs::write(wt.join("a.txt"), "worker edit\n").expect("edit");
        std::fs::remove_file(wt.join("keep.txt")).expect("delete");
        std::fs::create_dir_all(wt.join("src/deep")).expect("mkdir");
        std::fs::write(wt.join("src/deep/new.rs"), "x").expect("new");
        std::fs::create_dir_all(wt.join("cache")).expect("mkdir");
        std::fs::write(wt.join("cache/blob"), "ignored").expect("ignored");
        std::fs::write(wt.join("build.log"), "ignored").expect("ignored");

        apply_tree(&wt, &lease.relais_index(), &start_tree).expect("apply");
        assert_eq!(std::fs::read_to_string(wt.join("a.txt")).expect("a"), "a\n");
        assert_eq!(
            std::fs::read_to_string(wt.join("keep.txt")).expect("keep"),
            "keep\n"
        );
        assert!(!wt.join("src").exists(), "emptied directories go too");
        assert!(wt.join("cache/blob").exists() && wt.join("build.log").exists());
        // Idempotent: applying again changes nothing and still succeeds.
        apply_tree(&wt, &lease.relais_index(), &start_tree).expect("again");
    }

    #[test]
    fn a_file_replaced_by_a_directory_holding_ignored_files_blocks_and_deletes_nothing() {
        let (_keep, wt) = linked_worktree();
        std::fs::write(wt.join("item"), "a file\n").expect("item");
        let lease = Lease::acquire(&wt, "run-1").expect("lease");
        let start = start_snapshot(&lease).expect("start");
        let start_tree = tree_of(&wt, &start.sha);

        std::fs::remove_file(wt.join("item")).expect("rm");
        std::fs::create_dir_all(wt.join("item")).expect("mkdir");
        std::fs::write(wt.join("item/code.rs"), "worker\n").expect("code");
        std::fs::write(
            wt.join("item/debug.log"),
            "ignored, not the worker's to lose",
        )
        .expect("log");

        match apply_tree(&wt, &lease.relais_index(), &start_tree) {
            Err(ApplyError::Collisions(found)) => {
                assert!(
                    found.iter().any(|c| c.path == "item/debug.log"),
                    "{found:?}"
                );
            }
            other => panic!(
                "expected a collision: {:?}",
                other.err().map(|e| e.to_string())
            ),
        }
        assert!(wt.join("item/debug.log").exists(), "nothing was deleted");
        assert!(wt.join("item/code.rs").exists());
    }

    #[test]
    fn a_directory_replaced_by_a_file_and_the_reverse_both_restore() {
        let (_keep, wt) = linked_worktree();
        std::fs::create_dir_all(wt.join("dir")).expect("mkdir");
        std::fs::write(wt.join("dir/inner.txt"), "inner\n").expect("inner");
        std::fs::write(wt.join("file"), "file\n").expect("file");
        let lease = Lease::acquire(&wt, "run-1").expect("lease");
        let start = start_snapshot(&lease).expect("start");
        let start_tree = tree_of(&wt, &start.sha);

        // dir -> file, file -> dir
        std::fs::remove_dir_all(wt.join("dir")).expect("rm dir");
        std::fs::write(wt.join("dir"), "now a file\n").expect("dir as file");
        std::fs::remove_file(wt.join("file")).expect("rm file");
        std::fs::create_dir_all(wt.join("file")).expect("file as dir");
        std::fs::write(wt.join("file/x"), "x\n").expect("x");

        apply_tree(&wt, &lease.relais_index(), &start_tree).expect("restores both");
        assert_eq!(
            std::fs::read_to_string(wt.join("dir/inner.txt")).expect("inner"),
            "inner\n"
        );
        assert_eq!(
            std::fs::read_to_string(wt.join("file")).expect("file"),
            "file\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_replaced_by_a_directory_restores_as_a_symlink() {
        let (_keep, wt) = linked_worktree();
        std::os::unix::fs::symlink("a.txt", wt.join("link")).expect("link");
        let lease = Lease::acquire(&wt, "run-1").expect("lease");
        let start = start_snapshot(&lease).expect("start");
        let start_tree = tree_of(&wt, &start.sha);

        std::fs::remove_file(wt.join("link")).expect("rm link");
        std::fs::create_dir_all(wt.join("link")).expect("dir");
        std::fs::write(wt.join("link/f"), "f\n").expect("f");

        apply_tree(&wt, &lease.relais_index(), &start_tree).expect("restore");
        assert_eq!(
            std::fs::read_link(wt.join("link"))
                .expect("still a link")
                .to_string_lossy(),
            "a.txt"
        );
    }

    fn marker_for(lease: &Lease, start: &Start) -> Marker {
        Marker {
            run: lease.run().to_string(),
            stage: Stage::Running,
            head_symbolic: start.head_symbolic.clone(),
            head_sha: start.head_sha.clone(),
            start: start.sha.clone(),
            index_listing: start.index_listing.clone(),
            target: None,
            exported: None,
        }
    }

    #[test]
    fn finalizing_to_the_start_restores_exports_once_and_removes_the_marker_last() {
        let (_keep, wt) = linked_worktree();
        let lease = Lease::acquire(&wt, "run-1").expect("lease");
        let start = start_snapshot(&lease).expect("start");
        lease
            .write_marker(&marker_for(&lease, &start))
            .expect("marker");
        let gd = lease.git_dir().to_path_buf();
        std::fs::write(wt.join("a.txt"), "worker\n").expect("edit");

        let mut exports = 0;
        let outcome = finalize(
            lease,
            Target {
                kind: TargetKind::Start,
                tree: tree_of(&wt, &start.sha),
            },
            IndexPolicy::Restore,
            &mut || {
                exports += 1;
                Ok("refs/relais/candidates/run-1/final".to_string())
            },
        )
        .expect("finalize");
        assert_eq!(
            outcome,
            Outcome::Applied {
                index: IndexOutcome::Untouched
            }
        );
        assert_eq!(exports, 1);
        assert_eq!(std::fs::read_to_string(wt.join("a.txt")).expect("a"), "a\n");
        assert!(
            leftover_markers(&gd).expect("ls").is_empty(),
            "the marker goes last"
        );
    }

    #[test]
    fn a_run_killed_after_deciding_is_finished_the_way_it_decided_and_exports_nothing_again() {
        let (_keep, wt) = linked_worktree();
        let lease = Lease::acquire(&wt, "run-1").expect("lease");
        let start = start_snapshot(&lease).expect("start");
        let accepted_tree = {
            std::fs::write(wt.join("a.txt"), "accepted change\n").expect("edit");
            working_tree(&wt, &lease.relais_index()).expect("tree")
        };
        let mut marker = marker_for(&lease, &start);
        marker.stage = Stage::Finalizing;
        marker.target = Some(Target {
            kind: TargetKind::Accepted,
            tree: accepted_tree.clone(),
        });
        marker.exported = Some("refs/relais/candidates/run-1/final".into());
        lease.write_marker(&marker).expect("marker");
        // The process died here, mid-restore: the tree is half way somewhere else.
        std::fs::write(wt.join("a.txt"), "half applied\n").expect("half");
        drop(lease);

        let reclaimed = Lease::reclaim(&wt, "run-1").expect("reclaim");
        let outcome = finalize(
            reclaimed,
            // Offered a different target: the persisted decision wins.
            Target {
                kind: TargetKind::Start,
                tree: tree_of(&wt, &start.sha),
            },
            IndexPolicy::Restore,
            &mut || panic!("an export already on the marker is not repeated"),
        )
        .expect("finish");
        assert_eq!(
            outcome,
            Outcome::Applied {
                index: IndexOutcome::Untouched
            }
        );
        assert_eq!(
            std::fs::read_to_string(wt.join("a.txt")).expect("a"),
            "accepted change\n"
        );
    }

    /// A marker anywhere under `relais-inplace/` means a run holds the
    /// worktree, readable or not; the run asking is not held against itself;
    /// a directory git cannot read is "cannot tell", never "free".
    #[test]
    fn a_marker_means_the_worktree_is_held_unless_it_is_the_askers_own() {
        let (keep, wt) = linked_worktree();
        assert_eq!(held_by(&wt, None).expect("readable"), None);
        let lease = Lease::acquire(&wt, "run-1").expect("lease");
        let start = start_snapshot(&lease).expect("start");
        lease
            .write_marker(&marker_for(&lease, &start))
            .expect("marker");
        assert_eq!(
            held_by(&wt, None).expect("readable"),
            Some("run-1".to_string())
        );
        assert_eq!(
            held_by(&wt, Some("run-1")).expect("readable"),
            None,
            "its own marker"
        );
        let corrupt = lease.git_dir().join("relais-inplace/run-2.json");
        std::fs::write(&corrupt, "{ not json").expect("corrupt marker");
        assert_eq!(
            held_by(&wt, Some("run-1")).expect("readable"),
            Some("run-2".to_string()),
            "a marker relais cannot parse still holds the worktree"
        );
        let nowhere = keep.join("not-a-repo");
        std::fs::create_dir_all(&nowhere).expect("mkdir");
        assert!(held_by(&nowhere, None).is_err());
    }

    /// A worker's `git add` goes to the person's real index. A run that did
    /// the work gives the index back byte for byte, and keeps no copy.
    #[test]
    fn a_staged_file_in_the_real_index_is_restored_byte_for_byte() {
        let (_keep, wt) = linked_worktree();
        let lease = Lease::acquire(&wt, "run-1").expect("lease");
        let start = start_snapshot(&lease).expect("start");
        lease
            .write_marker(&marker_for(&lease, &start))
            .expect("marker");
        let gd = lease.git_dir().to_path_buf();
        let index = gd.join("index");
        let before = std::fs::read(&index).expect("index at start");

        std::fs::write(wt.join("staged.txt"), "staged by the worker\n").expect("file");
        git(&wt, &["add", "staged.txt"]);
        assert_ne!(
            std::fs::read(&index).expect("index"),
            before,
            "the worker changed it"
        );

        let outcome = finalize(
            lease,
            Target {
                kind: TargetKind::Start,
                tree: tree_of(&wt, &start.sha),
            },
            IndexPolicy::Restore,
            &mut || Ok("refs/relais/candidates/run-1/final".to_string()),
        )
        .expect("finalize");
        assert_eq!(
            outcome,
            Outcome::Applied {
                index: IndexOutcome::Restored
            }
        );
        assert_eq!(
            std::fs::read(&index).expect("index"),
            before,
            "byte for byte"
        );
        assert!(
            !gd.join("relais-inplace/run-1.index.orig").exists(),
            "nothing left to offer"
        );
        assert!(!gd.join("index.lock").exists(), "git's lock is released");
    }

    /// A git running at the end holds `index.lock`; relais does not race it,
    /// does not remove a lock it did not make, and keeps the copy.
    #[test]
    fn a_held_index_lock_leaves_the_index_alone_and_keeps_the_copy() {
        let (_keep, wt) = linked_worktree();
        let lease = Lease::acquire(&wt, "run-1").expect("lease");
        let start = start_snapshot(&lease).expect("start");
        lease
            .write_marker(&marker_for(&lease, &start))
            .expect("marker");
        let gd = lease.git_dir().to_path_buf();
        std::fs::write(wt.join("staged.txt"), "x\n").expect("file");
        git(&wt, &["add", "staged.txt"]);
        let staged = std::fs::read(gd.join("index")).expect("index");
        std::fs::write(gd.join("index.lock"), "someone else's git").expect("their lock");

        let outcome = finalize(
            lease,
            Target {
                kind: TargetKind::Start,
                tree: tree_of(&wt, &start.sha),
            },
            IndexPolicy::Restore,
            &mut || Ok("refs/relais/candidates/run-1/final".to_string()),
        )
        .expect("finalize");
        assert!(
            matches!(
                &outcome,
                Outcome::Applied {
                    index: IndexOutcome::Changed(Unrestored::Failed(why))
                } if !why.is_empty()
            ),
            "a failed restore says why: {outcome:?}"
        );
        assert_eq!(std::fs::read(gd.join("index")).expect("index"), staged);
        assert!(
            gd.join("relais-inplace/run-1.index.orig").is_file(),
            "the copy is kept"
        );
        assert_eq!(
            std::fs::read_to_string(gd.join("index.lock")).expect("their lock"),
            "someone else's git",
            "a lock relais did not make is left as it is"
        );
    }

    /// `resume --retire` of a run that died may come days later: the index is
    /// reported and the copy kept, never written over (ADR-0009).
    #[test]
    fn the_keep_policy_reports_a_changed_index_and_does_not_touch_it() {
        let (_keep, wt) = linked_worktree();
        let lease = Lease::acquire(&wt, "run-1").expect("lease");
        let start = start_snapshot(&lease).expect("start");
        lease
            .write_marker(&marker_for(&lease, &start))
            .expect("marker");
        let gd = lease.git_dir().to_path_buf();
        std::fs::write(wt.join("staged.txt"), "x\n").expect("file");
        git(&wt, &["add", "staged.txt"]);
        let staged = std::fs::read(gd.join("index")).expect("index");

        let outcome = finalize(
            lease,
            Target {
                kind: TargetKind::Start,
                tree: tree_of(&wt, &start.sha),
            },
            IndexPolicy::Keep,
            &mut || Ok("refs/relais/candidates/run-1/final".to_string()),
        )
        .expect("finalize");
        assert_eq!(
            outcome,
            Outcome::Applied {
                index: IndexOutcome::Changed(Unrestored::NotAsked)
            }
        );
        assert_eq!(std::fs::read(gd.join("index")).expect("index"), staged);
        assert!(gd.join("relais-inplace/run-1.index.orig").is_file());
    }

    #[test]
    fn a_worker_commit_blocks_the_restore_and_leaves_tree_and_branch_alone() {
        let (_keep, wt) = linked_worktree();
        let lease = Lease::acquire(&wt, "run-1").expect("lease");
        let start = start_snapshot(&lease).expect("start");
        lease
            .write_marker(&marker_for(&lease, &start))
            .expect("marker");
        std::fs::write(wt.join("a.txt"), "committed by the worker\n").expect("edit");
        git(&wt, &["commit", "-q", "-am", "worker"]);
        let moved_to = git(&wt, &["rev-parse", "HEAD"]);

        let outcome = finalize(
            lease,
            Target {
                kind: TargetKind::Start,
                tree: tree_of(&wt, &start.sha),
            },
            IndexPolicy::Restore,
            &mut || Ok("refs/relais/candidates/run-1/final".to_string()),
        )
        .expect("finalize");
        assert_eq!(
            outcome,
            Outcome::HeadMoved {
                was: start.head_sha,
                now: moved_to.clone()
            }
        );
        assert_eq!(
            git(&wt, &["rev-parse", "HEAD"]),
            moved_to,
            "the branch was not reset"
        );
        assert_eq!(
            std::fs::read_to_string(wt.join("a.txt")).expect("a"),
            "committed by the worker\n"
        );
    }
}
