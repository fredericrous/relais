//! Owned task worktrees (SPEC §8).
//!
//! The base commit is resolved once and an owned worktree is created from
//! that exact SHA; the user's checkout and other worktrees stay untouched.
//! A dirty working tree is refused explicitly — never silently copied.
//! Write scope is checked on the actual diff after each attempt — a scope
//! violation cannot be accepted. This is an acceptance boundary, not
//! filesystem isolation: tools with Bash access are not a sandbox. The
//! runner records an immutable candidate snapshot (a commit object,
//! including added files) outside model control, and a worktree is
//! retired — everything tracked named and exported first — never
//! force-cleaned with unexported changes in it.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::contract::TaskContract;
use crate::ids::sha256_hex;

/// The author and committer date every candidate snapshot is stamped
/// with, and the unix seconds that is.
///
/// Fixed, because `commit-tree` otherwise puts the wall clock into the
/// object and gives the same tree a new identity every second — and
/// candidate identity is exactly what the same-failure recurrence check
/// compares (SPEC §9). Named so the test can assert the stamp instead
/// of sleeping across a clock second to observe it.
pub const SNAPSHOT_DATE: &str = "2000-01-01T00:00:00Z";
pub const SNAPSHOT_DATE_UNIX: i64 = 946_684_800;

#[derive(Debug)]
pub enum WorkspaceError {
    Git(String),
    DirtyBase(Vec<String>),
    ScopeViolation(Vec<String>),
    Io(std::io::Error),
}

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Git(detail) => write!(f, "git failed: {detail}"),
            Self::DirtyBase(paths) => write!(
                f,
                "the working tree is dirty ({}); commit or stash first — relais never copies uncommitted work",
                paths.join(", ")
            ),
            Self::ScopeViolation(paths) => write!(
                f,
                "diff leaves the declared write scope: {}",
                paths.join(", ")
            ),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for WorkspaceError {}

impl From<std::io::Error> for WorkspaceError {
    fn from(e: std::io::Error) -> Self {
        WorkspaceError::Io(e)
    }
}

type Result<T> = std::result::Result<T, WorkspaceError>;

/// Every git process relais spawns, with the ambient repository
/// environment removed. `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`,
/// `GIT_OBJECT_DIRECTORY` and `GIT_ALTERNATE_OBJECT_DIRECTORIES` override
/// `current_dir`: inherited from a hook, a rebase or a parent `git` they
/// silently point the snapshot, the scope check or the worktree at
/// another repository's objects — the run would then verify one tree and
/// record another. Relais always means the directory it names.
pub fn git_command(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(dir);
    for variable in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        command.env_remove(variable);
    }
    command
}

/// Git, as this crate uses it: a subcommand run in a directory, its
/// stdout captured, a non-zero status an error.
///
/// The interface belongs to relais, not to the modules that happen to
/// need a repository fact: `context` fingerprints a read hint with
/// `ls-tree` and `verify` creates a throwaway worktree, and both used to
/// spawn their own `git` — one of them without the `GIT_*` scrubbing
/// `git_command` does, which is how a run could fingerprint one
/// repository and verify another (audit V7). Every `git` process relais
/// starts is now behind this trait, and `SystemGit` is its one
/// production implementation.
pub trait Git {
    /// Run `git <args>` in `dir`. Stdout is returned untrimmed: a
    /// porcelain format encodes meaning in its leading and trailing
    /// bytes.
    fn run(&self, dir: &Path, args: &[&str]) -> Result<String>;
}

/// The git on this machine's PATH.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemGit;

impl Git for SystemGit {
    fn run(&self, dir: &Path, args: &[&str]) -> Result<String> {
        git_raw(dir, args)
    }
}

/// What a scripted git answers to one invocation.
type GitAnswer = dyn Fn(&Path, &[&str]) -> Result<String> + Send + Sync;

/// A scripted git for tests, so a caller's handling of a failing or
/// surprising git can be tested without a repository on disk.
pub struct FakeGit {
    answer: Box<GitAnswer>,
}

impl FakeGit {
    pub fn new(answer: impl Fn(&Path, &[&str]) -> Result<String> + Send + Sync + 'static) -> Self {
        Self {
            answer: Box::new(answer),
        }
    }
}

impl Git for FakeGit {
    fn run(&self, dir: &Path, args: &[&str]) -> Result<String> {
        (self.answer)(dir, args)
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    git_raw(dir, args).map(|output| output.trim().to_string())
}

/// Like `git`, but without trimming: `status --porcelain` encodes the
/// status in leading bytes that a whole-output trim would eat.
fn git_raw(dir: &Path, args: &[&str]) -> Result<String> {
    let output = git_command(dir)
        .args(args)
        .output()
        .map_err(|e| WorkspaceError::Git(format!("git could not be launched: {e}")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let failure = output
            .status
            .code()
            .map(|code| format!("exit {code}"))
            .unwrap_or_else(|| "terminated by a signal".to_string());
        Err(WorkspaceError::Git(format!(
            "git {} failed ({failure}): {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

/// Resolve `base_ref` once to a full commit SHA (SPEC §4).
pub fn resolve_base(repo_dir: &Path, base_ref: &str) -> Result<String> {
    git(repo_dir, &["rev-parse", &format!("{base_ref}^{{commit}}")])
}

/// Explicit dirty-tree handling (SPEC §8, §14): every uncommitted path is
/// reported, because relais will not silently ignore or copy them.
///
/// `repo_dir` is the policy root — where `relais.toml` sits, which since
/// the upward search is not necessarily the git root. `git status`
/// prints paths relative to the GIT root whatever directory it runs in,
/// so the scratch directory is recognised through the policy root's own
/// position in the repository (audit V5).
pub fn dirty_paths(repo_dir: &Path) -> Result<Vec<String>> {
    let scratch = ScratchPrefix::resolve(&SystemGit, repo_dir)?;
    let status = git_raw(repo_dir, &["status", "--porcelain"])?;
    Ok(status
        .lines()
        .filter_map(|line| line.get(3..).map(|path| path.trim().to_string()))
        // `.relais/` is where the /relais skill writes the task contract
        // it is about to hand over. It is relais's own scratch, never part
        // of a candidate — the worktree is created from the base SHA, so
        // nothing in it is copied — and a contract that dirtied the tree
        // it describes would refuse every run it was written for.
        .filter(|path| !scratch.covers(path))
        .collect())
}

/// Where relais's scratch directory sits as `git status` prints paths:
/// the policy root's path relative to the git root, plus `.relais/`.
/// With `relais.toml` at the git root this is just `.relais/`; with it in
/// `crates/relais`, an untracked contract prints as
/// `crates/relais/.relais/task.json`, which the root-anchored test read
/// as the user's uncommitted work and refused the run it described.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScratchPrefix {
    /// `""` at the git root, otherwise `"crates/relais/"`.
    prefix: String,
}

impl ScratchPrefix {
    /// Ask git once where the top level is, and where the policy root
    /// sits inside it.
    pub fn resolve(git: &dyn Git, policy_root: &Path) -> Result<Self> {
        let answer = git.run(
            policy_root,
            &["rev-parse", "--show-toplevel", "--show-prefix"],
        )?;
        // `--show-toplevel` first, then `--show-prefix`: the prefix is
        // the policy root's own path under that top level, which is
        // exactly the frame `git status` prints paths in. An empty
        // second line is the top level itself.
        let prefix = answer.lines().nth(1).unwrap_or("").trim();
        Ok(Self::under(prefix))
    }

    /// The prefix as git spells it (`""` or `"crates/relais/"`).
    pub fn under(prefix: &str) -> Self {
        let prefix = prefix.trim().trim_start_matches('/');
        let prefix = match prefix {
            "" => String::new(),
            other if other.ends_with('/') => other.to_string(),
            other => format!("{other}/"),
        };
        Self { prefix }
    }

    /// Is this status path inside relais's scratch directory? Paths with
    /// non-ASCII bytes arrive C-quoted, which the quotes are stripped for.
    pub fn covers(&self, status_path: &str) -> bool {
        let path = status_path.trim_matches('"');
        let Some(rest) = path.strip_prefix(&self.prefix) else {
            return false;
        };
        rest == ".relais" || rest == ".relais/" || rest.starts_with(".relais/")
    }
}

/// Owned worktree at an exact SHA. Detached: the run owns no branch
/// namespace until a candidate snapshot names one.
pub struct TaskWorktree {
    pub path: PathBuf,
    pub base_sha: String,
}

pub fn create_worktree(
    repo_dir: &Path,
    base_sha: &str,
    worktree_path: &Path,
) -> Result<TaskWorktree> {
    // The worktree root lives outside the run's artifact directory
    // (SPEC §8, audit B6), so its parent is ours to create.
    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    git(
        repo_dir,
        &[
            "worktree",
            "add",
            "--detach",
            &worktree_path.to_string_lossy(),
            base_sha,
        ],
    )?;
    Ok(TaskWorktree {
        path: worktree_path.to_path_buf(),
        base_sha: base_sha.to_string(),
    })
}

impl TaskWorktree {
    /// Paths changed vs. the base, untracked files included — what the
    /// scope check actually judges (SPEC §8: on the actual diff).
    pub fn changed_paths(&self) -> Result<Vec<String>> {
        let tracked = git(&self.path, &["diff", "--name-only", &self.base_sha])?;
        let untracked = git(&self.path, &["ls-files", "--others", "--exclude-standard"])?;
        let mut paths: Vec<String> = tracked
            .lines()
            .chain(untracked.lines())
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect();
        paths.sort();
        paths.dedup();
        Ok(paths)
    }

    /// Immutable candidate snapshot, including added files, outside model
    /// control: a commit object created by plumbing, with no commit hooks
    /// and no working-tree mutation. Author, committer, dates and message
    /// are all FIXED, so the SHA is a pure function of (tree, base):
    /// identical content is an identical candidate identity, which the
    /// same-failure recurrence check depends on (SPEC §9). `commit-tree`
    /// would otherwise stamp the wall clock into the object and give the
    /// same tree a new identity every second.
    pub fn snapshot_candidate(&self, _label: &str) -> Result<String> {
        git(&self.path, &["add", "-A"])?;
        let tree = git(&self.path, &["write-tree"])?;
        let commit = git_command(&self.path)
            .args([
                "commit-tree",
                &tree,
                "-p",
                &self.base_sha,
                "-m",
                "relais candidate snapshot",
            ])
            .env("GIT_AUTHOR_NAME", "relais")
            .env("GIT_AUTHOR_EMAIL", "relais@localhost")
            .env("GIT_AUTHOR_DATE", SNAPSHOT_DATE)
            .env("GIT_COMMITTER_NAME", "relais")
            .env("GIT_COMMITTER_EMAIL", "relais@localhost")
            .env("GIT_COMMITTER_DATE", SNAPSHOT_DATE)
            .output()
            .map_err(|e| WorkspaceError::Git(format!("commit-tree: {e}")))?;
        if !commit.status.success() {
            return Err(WorkspaceError::Git(format!(
                "commit-tree failed: {}",
                String::from_utf8_lossy(&commit.stderr)
            )));
        }
        Ok(String::from_utf8_lossy(&commit.stdout).trim().to_string())
    }

    /// Paths the CANDIDATE changes against the base — read from the two
    /// commit objects, never from the working tree, so what the scope
    /// check judges is exactly what verification ran and the receipt
    /// names (SPEC §8, §10). A descendant still writing after the
    /// snapshot cannot move a path out of this list. NUL-separated and
    /// unquoted: git C-quotes non-ASCII names on the line-oriented form,
    /// which would defeat both the glob and the protected-prefix test.
    pub fn changed_paths_in(&self, candidate_sha: &str) -> Result<Vec<String>> {
        let output = git_raw(
            &self.path,
            &[
                "diff",
                "--name-only",
                "-z",
                "--no-renames",
                &self.base_sha,
                candidate_sha,
            ],
        )?;
        let mut paths: Vec<String> = output
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(|path| path.to_string())
            .collect();
        paths.sort();
        paths.dedup();
        Ok(paths)
    }

    /// Does the candidate carry the same tree as the base? Knowable from
    /// the objects before a single verification command is spent.
    pub fn same_tree_as_base(&self, candidate_sha: &str) -> Result<bool> {
        let base = git(
            &self.path,
            &["rev-parse", &format!("{}^{{tree}}", self.base_sha)],
        )?;
        let candidate = git(
            &self.path,
            &["rev-parse", &format!("{candidate_sha}^{{tree}}")],
        )?;
        Ok(base == candidate)
    }

    /// Export the candidate as a patch artifact. Untrimmed: a patch whose
    /// last line lost its newline is one `git apply` calls corrupt.
    pub fn export_patch(&self, candidate_sha: &str, out_path: &Path) -> Result<()> {
        let diff = git_raw(&self.path, &["diff", &self.base_sha, candidate_sha])?;
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(out_path, diff.as_bytes())?;
        Ok(())
    }
}

/// Protected repository configuration (SPEC §8): changes here are rejected
/// unless the contract explicitly includes them in its write scope. The
/// policy file itself, relais's own scratch, the check gate's trust
/// declaration, Claude Code project settings, the aval registry, CI on
/// every forge relais runs against, the project instructions a
/// reviewer's session loads, and the ignore file (an unprotected
/// `.gitignore` lets a worker hide files from the diff, the snapshot and
/// the exported patch) are all positions a worker could use to widen its
/// own authority or narrow what is seen.
///
/// Matched on any path COMPONENT, not as a root-anchored prefix:
/// `src/CLAUDE.md` and `src/.claude/settings.json` are loaded by a Claude
/// Code session exactly as the root ones are, and a root-anchored test
/// left them writable inside an ordinary `src/**` scope (audit V10).
pub const PROTECTED_PATH_PREFIXES: &[&str] = &[
    "relais.toml",
    "amont.conf",
    ".adr.yaml",
    ".claude/",
    ".relais/",
    ".github/workflows/",
    ".forgejo/workflows/",
    ".gitea/workflows/",
    ".gitlab-ci.yml",
    ".gitignore",
    "CLAUDE.md",
    "AGENTS.md",
];

/// A path or pattern's components, ignoring leading and trailing
/// separators.
fn path_components(path: &str) -> Vec<&str> {
    path.split('/').filter(|part| !part.is_empty()).collect()
}

/// Does `components` contain the protected area's components, in order,
/// starting at some component boundary? `src/.claude/settings.json`
/// contains `.claude`; `src/myclaude/x` does not, because a component is
/// matched whole.
fn contains_components(components: &[&str], protected: &str) -> bool {
    let needle = path_components(protected);
    if needle.is_empty() || needle.len() > components.len() {
        return false;
    }
    components
        .windows(needle.len())
        .any(|window| window == needle.as_slice())
}

/// The protected area a path falls under, if any.
pub fn protected_prefix(path: &str) -> Option<&'static str> {
    let components = path_components(path);
    PROTECTED_PATH_PREFIXES
        .iter()
        .copied()
        .find(|protected| contains_components(&components, protected))
}

pub fn is_protected_path(path: &str) -> bool {
    protected_prefix(path).is_some()
}

/// Does a write-scope pattern NAME this protected area, as opposed to
/// merely matching it? `.claude/**`, `src/.claude/**` and `**/CLAUDE.md`
/// all name one; `**` and `src/**` name none, which is the whole point of
/// SPEC §8's "explicitly within an approved contract".
pub fn pattern_names_protected(pattern: &str, protected: &str) -> bool {
    contains_components(&path_components(pattern), protected)
}

/// Check the candidate's diff against the declared scope. A path is in
/// scope when some write_scope pattern matches it (gitignore-style
/// semantics via globset). Protected paths additionally need the contract
/// to name them explicitly, pattern-for-pattern.
pub fn check_scope(
    worktree: &TaskWorktree,
    candidate_sha: &str,
    contract: &TaskContract,
) -> std::result::Result<Vec<String>, WorkspaceError> {
    // The scope arrives compiled: `TaskContract::from_json_str` judged
    // every pattern before the worker ran (P5), so there is nothing to
    // fail here and no glob error to report as a git failure.
    let scope = contract.write_scope();
    let patterns: &[String] = scope.map_or(&[], |scope| scope.patterns());

    let mut violations = Vec::new();
    for path in worktree.changed_paths_in(candidate_sha)? {
        let in_scope = scope.is_some_and(|scope| scope.is_match(&path));
        // "Explicitly within an approved contract" (SPEC §8) means the
        // scope NAMES this protected area — a pattern carrying its
        // components, at the depth the file lives — not a blanket `**`
        // that happens to match it, and not some other protected area.
        let explicitly_allowed = match protected_prefix(&path) {
            None => true,
            Some(protected) => {
                in_scope
                    && patterns
                        .iter()
                        .any(|pattern| pattern_names_protected(pattern, protected))
            }
        };
        if !in_scope || !explicitly_allowed {
            violations.push(if in_scope {
                format!("{path} (protected repository configuration)")
            } else {
                path
            });
        }
    }
    if violations.is_empty() {
        Ok(Vec::new())
    } else {
        Err(WorkspaceError::ScopeViolation(violations))
    }
}

/// The ref name a run's attempt candidate is kept under.
pub fn candidate_ref(run_id: &str, attempt: u32) -> String {
    format!("refs/relais/candidates/{run_id}/{attempt}")
}

/// The namespace every candidate ref of one run lives under, with its
/// trailing slash.
fn candidate_namespace(run_id: &str) -> String {
    format!("refs/relais/candidates/{run_id}/")
}

/// The patch a retired worktree's final tree is exported to, in the
/// run's artifact directory, beside the per-attempt `candidate-N.patch`.
pub const FINAL_PATCH: &str = "candidate-final.patch";

/// What a retired worktree's tree had to be exported as, when it held
/// something no named candidate of the run already did: the ref that
/// keeps the final snapshot reachable and the patch it was written to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exported {
    pub reference: String,
    pub patch_path: PathBuf,
}

/// What retiring a worktree did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retirement {
    /// `None` when the tree was already held by a named candidate of the
    /// run (or was the base), so nothing needed exporting.
    pub exported: Option<Exported>,
    /// The size of the worktree directory that was removed, ignored
    /// build output included.
    pub bytes_reclaimed: u64,
}

/// Retire a run's worktree: keep everything tracked, then remove the
/// directory, ignored build output and all (SPEC §8).
///
/// (a) The tree — tracked and untracked-but-not-ignored, exactly what
/// [`TaskWorktree::snapshot_candidate`] snapshots — is compared, tree
/// object to tree object, with every candidate already named under
/// `refs/relais/candidates/<run>/` and with the base. When none of them
/// holds it, it is snapshotted, named `…/<run>/final` (or the first
/// free `final-N` if a differing `final` already exists, so retiring
/// twice never overwrites what the first retirement kept) and exported
/// to [`FINAL_PATCH`] in `artifacts_dir`. (b) The worktree is removed
/// with `git worktree remove --force`, and the parent directory the
/// runner made for the run's worktrees with it once it is empty.
/// `--force` is what takes ignored output (`target/`, `node_modules/`)
/// and the index the snapshot staged; it is acceptable here precisely
/// because (a) has just guaranteed that nothing tracked is unexported —
/// the guarantee §8 asks for, given by construction rather than by a
/// status check. (c) What was done is returned for the record.
///
/// Idempotent: a second call on the same tree finds it under `final`
/// and exports nothing. A worktree that is gone already is an error,
/// not a silent success, because "retired" must mean it was seen.
pub fn retire(worktree: &TaskWorktree, run_id: &str, artifacts_dir: &Path) -> Result<Retirement> {
    // Every repository-level command below runs INSIDE the worktree: its
    // `.git` file names the repository's common dir, which is where refs
    // and the worktree list live. The path the run was launched from is
    // deliberately not consulted — it is often a task worktree that was
    // removed long before the leftover is swept (the four legacy runs on
    // the first `resume --retire`), and git answers from the link anyway.
    let repo_dir = &worktree.path;
    let snapshot = worktree.snapshot_candidate("retirement")?;
    let tree = tree_of(&worktree.path, &snapshot)?;
    let base_tree = tree_of(&worktree.path, &worktree.base_sha)?;
    let named = named_candidates(repo_dir, run_id)?;
    let already_kept = tree == base_tree || named.iter().any(|(_, kept)| *kept == tree);
    let exported = if already_kept {
        None
    } else {
        let reference = free_final_ref(run_id, &named);
        git(repo_dir, &["update-ref", &reference, &snapshot])?;
        let stem = reference.rsplit('/').next().unwrap_or("final").to_string();
        let patch_path = artifacts_dir.join(format!("candidate-{stem}.patch"));
        worktree.export_patch(&snapshot, &patch_path)?;
        Some(Exported {
            reference,
            patch_path,
        })
    };
    let bytes_reclaimed = dir_size(&worktree.path)?;
    // `worktree remove` cannot run from inside the directory it deletes;
    // the common dir is where the worktree list lives, so ask git for it.
    let common_dir = git(
        repo_dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    git(
        Path::new(common_dir.trim()),
        &[
            "worktree",
            "remove",
            "--force",
            &worktree.path.to_string_lossy(),
        ],
    )?;
    remove_if_empty(worktree.path.parent())?;
    Ok(Retirement {
        exported,
        bytes_reclaimed,
    })
}

/// The tree object a commit carries.
fn tree_of(dir: &Path, commit: &str) -> Result<String> {
    git(dir, &["rev-parse", &format!("{commit}^{{tree}}")])
}

/// Every candidate ref of one run, as `(refname, tree)` pairs.
fn named_candidates(repo_dir: &Path, run_id: &str) -> Result<Vec<(String, String)>> {
    let listed = git(
        repo_dir,
        &[
            "for-each-ref",
            "--format=%(refname) %(tree)",
            &format!("{}*", candidate_namespace(run_id)),
        ],
    )?;
    Ok(listed
        .lines()
        .filter_map(|line| {
            let (name, tree) = line.trim().split_once(' ')?;
            Some((name.to_string(), tree.to_string()))
        })
        .collect())
}

/// `…/<run>/final`, or the first `final-N` not yet taken.
fn free_final_ref(run_id: &str, named: &[(String, String)]) -> String {
    let namespace = candidate_namespace(run_id);
    let taken = |name: &str| named.iter().any(|(existing, _)| existing == name);
    let first = format!("{namespace}final");
    if !taken(&first) {
        return first;
    }
    (2u32..)
        .map(|n| format!("{namespace}final-{n}"))
        .find(|name| !taken(name))
        .unwrap_or(first)
}

/// Remove a directory only when it is empty — the run's worktree parent
/// after its last worktree went. `None` and "not empty" are both
/// nothing to do; anything else is an error.
fn remove_if_empty(dir: Option<&Path>) -> Result<()> {
    let Some(dir) = dir else {
        return Ok(());
    };
    if std::fs::read_dir(dir)?.next().is_none() {
        std::fs::remove_dir(dir)?;
    }
    Ok(())
}

/// The bytes a directory holds: every regular file and symlink under
/// it, without following links, so a worktree's ignored build output
/// counts and nothing outside it does.
pub fn dir_size(path: &Path) -> std::io::Result<u64> {
    let mut total = 0;
    let mut pending = vec![path.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else {
                total += metadata.len();
            }
        }
    }
    Ok(total)
}

/// A run's worktree still on disk under the state directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedWorktree {
    pub run_id: String,
    pub path: PathBuf,
    /// Its size, ignored build output included.
    pub bytes: u64,
}

/// Every run worktree still under `state_dir`, in path order: each
/// `worktrees/<run>/<name>` and each legacy `runs/<run>/worktree`
/// (`paths`) that carries a `.git` — a directory without one is not a
/// worktree, whatever made it. A missing `worktrees/` or `runs/` is
/// simply no worktrees; any other read failure is an error, since a
/// sweep that could not look is not a sweep that found nothing.
pub fn retained_worktrees(state_dir: &Path) -> Result<Vec<RetainedWorktree>> {
    let mut found = Vec::new();
    for run_dir in subdirectories(&state_dir.join(crate::paths::WORKTREES_DIR))? {
        for candidate in subdirectories(&run_dir)? {
            push_if_worktree(&mut found, &run_dir, candidate)?;
        }
    }
    for run_dir in subdirectories(&state_dir.join(crate::paths::RUNS_DIR))? {
        let legacy = run_dir.join(crate::paths::LEGACY_WORKTREE_DIR);
        if legacy.is_dir() {
            push_if_worktree(&mut found, &run_dir, legacy)?;
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(found)
}

fn push_if_worktree(
    found: &mut Vec<RetainedWorktree>,
    run_dir: &Path,
    path: PathBuf,
) -> Result<()> {
    if !path.join(".git").exists() {
        return Ok(());
    }
    let run_id = run_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let bytes = dir_size(&path)?;
    found.push(RetainedWorktree {
        run_id,
        path,
        bytes,
    });
    Ok(())
}

/// The subdirectories of `dir`, or none when `dir` does not exist.
fn subdirectories(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(WorkspaceError::Io(e)),
    };
    let mut dirs = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            dirs.push(entry.path());
        }
    }
    dirs.sort();
    Ok(dirs)
}

/// Point a ref at a candidate commit. `commit-tree` produces an object
/// nothing references: it survives only until the next `git gc`, which
/// would take the receipt's `candidate_sha`, the retained worktree's
/// history and the patch's base with it. A ref under
/// `refs/relais/candidates/` is outside `refs/heads`, so it shows up in
/// no branch listing, is not pushed by a default refspec, and keeps the
/// object reachable for exactly as long as relais says it should be.
pub fn name_candidate(repo_dir: &Path, run_id: &str, attempt: u32, sha: &str) -> Result<String> {
    let name = candidate_ref(run_id, attempt);
    git(repo_dir, &["update-ref", &name, sha])?;
    Ok(name)
}

/// Drop every candidate ref of one run, releasing its commits to the
/// next `git gc`. Nothing calls this yet: a candidate outlives its run on
/// purpose (SPEC §8 — the patch and the base revision stay integrable
/// afterwards), and deciding WHEN a run's evidence stops being wanted is
/// a retention policy, not a runner step. It is the one operation a
/// future `relais gc` needs, and it is here so the ref namespace has an
/// owner rather than growing without one.
pub fn forget_run_refs(repo_dir: &Path, run_id: &str) -> Result<Vec<String>> {
    let prefix = format!("refs/relais/candidates/{run_id}/");
    let listed = git(
        repo_dir,
        &["for-each-ref", "--format=%(refname)", &format!("{prefix}*")],
    )?;
    let names: Vec<String> = listed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    for name in &names {
        git(repo_dir, &["update-ref", "-d", name])?;
    }
    Ok(names)
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(sha256_hex(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo() -> (PathBuf, PathBuf) {
        let dir = crate::test_support::temp_dir("ws");
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        // The machine's global hooksPath is amont's; test repos point at
        // an empty dir so fixture commits do not run real gates.
        let no_hooks = dir.join("no-hooks");
        std::fs::create_dir_all(&no_hooks).expect("mkdir");
        git(&repo, &["init", "-q"]).expect("init");
        git(
            &repo,
            &["config", "core.hooksPath", &no_hooks.to_string_lossy()],
        )
        .expect("hooks off");
        git(&repo, &["config", "user.email", "relais@test"]).expect("email");
        git(&repo, &["config", "user.name", "relais test"]).expect("name");
        std::fs::write(repo.join("file.txt"), "base\n").expect("write");
        std::fs::create_dir_all(repo.join("src")).expect("mkdir");
        std::fs::write(repo.join("src/main.rs"), "fn main() {}\n").expect("write");
        git(&repo, &["add", "-A"]).expect("add");
        git(&repo, &["commit", "-q", "-m", "base"]).expect("commit");
        (dir, repo)
    }

    /// Test cleanup: the fixture's worktree, whose content the test
    /// has finished asserting on.
    fn remove_worktree(repo: &Path, wt_path: &Path) {
        git(
            repo,
            &["worktree", "remove", "--force", &wt_path.to_string_lossy()],
        )
        .expect("removed");
    }

    fn contract_with_scope(scope: &[&str]) -> TaskContract {
        TaskContract::from_json_str(
            &serde_json::json!({
                "schema_version": 1,
                "kind": "change",
                "objective": "Do the thing",
                "base_ref": "HEAD",
                "write_scope": scope,
                "acceptance": ["it is done"],
                "verification_profile": "default",
            })
            .to_string(),
        )
        .expect("contract parses")
    }

    #[test]
    fn the_contract_directory_never_counts_as_dirty() {
        let (_dir, repo) = temp_repo();
        std::fs::create_dir_all(repo.join(".relais")).expect("mkdir");
        std::fs::write(repo.join(".relais/task.json"), "{}").expect("contract");
        assert!(
            dirty_paths(&repo).expect("status").is_empty(),
            "the /relais skill's own contract file must not refuse the run it describes"
        );
        std::fs::write(repo.join("file.txt"), "dirty\n").expect("dirty");
        assert_eq!(
            dirty_paths(&repo).expect("status"),
            vec!["file.txt".to_string()],
            "everything else still counts"
        );
        let root = ScratchPrefix::under("");
        assert!(root.covers(".relais/task.json"));
        assert!(root.covers(r#"".relais/t\303\242che.json""#));
        assert!(!root.covers(".relais-notes.md"));
    }

    /// V5: `relais.toml` may sit below the git root, and `git status`
    /// prints paths relative to the GIT root wherever it runs. The
    /// root-anchored test saw `crates/relais/.relais/task.json` as the
    /// user's uncommitted work and refused every run the contract in it
    /// described.
    #[test]
    fn the_contract_directory_is_found_under_a_subdirectory_policy_root() {
        let (_dir, repo) = temp_repo();
        let policy_root = repo.join("crates/relais");
        std::fs::create_dir_all(policy_root.join(".relais")).expect("mkdir");
        std::fs::write(policy_root.join("relais.toml"), "schema_version = 1\n").expect("policy");
        std::fs::write(policy_root.join(".relais/task.json"), "{}").expect("contract");
        git(&repo, &["add", "crates"]).expect("add");
        git(&repo, &["commit", "-q", "-m", "policy below the root"]).expect("commit");

        let prefix = ScratchPrefix::resolve(&SystemGit, &policy_root).expect("prefix");
        assert_eq!(prefix, ScratchPrefix::under("crates/relais/"));
        assert!(prefix.covers("crates/relais/.relais/task.json"));
        assert!(
            !prefix.covers(".relais/task.json"),
            "a `.relais/` at the git root is not this policy root's scratch"
        );

        assert!(
            dirty_paths(&policy_root).expect("status").is_empty(),
            "the contract the run was written for does not refuse the run"
        );
        // And the status still runs from the git root, where the path
        // the porcelain prints is the one the prefix accounts for.
        assert!(dirty_paths(&repo).expect("status").is_empty());
        std::fs::write(policy_root.join("src.rs"), "// work\n").expect("write");
        assert_eq!(
            dirty_paths(&policy_root).expect("status"),
            vec!["crates/relais/src.rs".to_string()],
            "everything else still counts, in git's own frame"
        );
    }

    /// V10: Claude Code loads a nested `CLAUDE.md` and a nested
    /// `.claude/settings.json` exactly as it loads the root ones, so a
    /// candidate that writes them inside an ordinary `src/**` scope is
    /// widening its own authority.
    #[test]
    fn protected_configuration_is_protected_at_any_depth() {
        for path in [
            "src/CLAUDE.md",
            "src/.claude/settings.json",
            "crates/x/AGENTS.md",
            "apps/web/.gitignore",
            ".forgejo/workflows/ci.yaml",
            "sub/.gitea/workflows/ci.yaml",
            ".gitlab-ci.yml",
            "crates/relais/relais.toml",
            "crates/relais/.relais/task.json",
        ] {
            assert!(is_protected_path(path), "{path} is protected");
        }
        for path in ["src/claude.md.rs", "src/myclaude/x", "docs/agents-md.md"] {
            assert!(!is_protected_path(path), "{path} is ordinary work");
        }
        // Naming the area is what unlocks it, at the depth it lives.
        assert!(pattern_names_protected("src/.claude/**", ".claude/"));
        assert!(pattern_names_protected("**/CLAUDE.md", "CLAUDE.md"));
        assert!(!pattern_names_protected("src/**", ".claude/"));
        assert!(!pattern_names_protected("**", "CLAUDE.md"));
    }

    #[test]
    fn nested_protected_paths_are_refused_inside_an_ordinary_scope() {
        let (_dir, repo) = temp_repo();
        let sha = resolve_base(&repo, "HEAD").expect("base");
        let wt_path = repo.parent().unwrap().join("wt-nested-prot");
        let wt = create_worktree(&repo, &sha, &wt_path).expect("worktree");
        std::fs::create_dir_all(wt_path.join("src/.claude")).expect("mkdir");
        std::fs::write(wt_path.join("src/CLAUDE.md"), "# do as I say\n").expect("write");
        std::fs::write(wt_path.join("src/.claude/settings.json"), "{}").expect("write");
        let candidate = wt.snapshot_candidate("attempt-1").expect("snapshot");
        let contract = contract_with_scope(&["src/**"]);
        let err = check_scope(&wt, &candidate, &contract).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("src/CLAUDE.md (protected"), "{message}");
        assert!(
            message.contains("src/.claude/settings.json (protected"),
            "{message}"
        );
        // Named explicitly, at the depth it lives: allowed.
        let explicit = contract_with_scope(&["src/**", "src/CLAUDE.md", "src/.claude/**"]);
        assert!(check_scope(&wt, &candidate, &explicit).is_ok(), "{message}");
        remove_worktree(&repo, &wt_path);
    }

    #[test]
    fn resolves_base_and_reports_dirty_explicitly() {
        let (_dir, repo) = temp_repo();
        let sha = resolve_base(&repo, "HEAD").expect("base resolves");
        assert_eq!(sha.len(), 40);
        assert!(dirty_paths(&repo).expect("clean").is_empty());
        std::fs::write(repo.join("file.txt"), "dirty\n").expect("dirty");
        let dirty = dirty_paths(&repo).expect("dirty listed");
        assert_eq!(dirty, vec!["file.txt".to_string()]);
    }

    #[test]
    fn worktree_is_owned_and_isolated() {
        let (_dir, repo) = temp_repo();
        let sha = resolve_base(&repo, "HEAD").expect("base");
        let wt_path = repo.parent().unwrap().join("wt");
        let wt = create_worktree(&repo, &sha, &wt_path).expect("worktree");
        std::fs::write(wt_path.join("src/main.rs"), "fn main() { changed }\n").expect("edit");
        std::fs::write(wt_path.join("src/added.rs"), "pub fn added() {}\n").expect("add");
        let changed = wt.changed_paths().expect("changed");
        assert_eq!(
            changed,
            vec!["src/added.rs".to_string(), "src/main.rs".to_string()]
        );
        let main_content =
            std::fs::read_to_string(repo.join("src/main.rs")).expect("main untouched");
        assert!(
            main_content.contains("fn main() {}"),
            "original checkout untouched"
        );
        remove_worktree(&repo, &wt_path);
    }

    #[test]
    fn snapshot_captures_untracked_files_immutably() {
        let (_dir, repo) = temp_repo();
        let sha = resolve_base(&repo, "HEAD").expect("base");
        let wt_path = repo.parent().unwrap().join("wt-snap");
        let wt = create_worktree(&repo, &sha, &wt_path).expect("worktree");
        std::fs::write(wt_path.join("src/untracked.txt"), "added by worker\n").expect("add");
        let candidate = wt.snapshot_candidate("attempt-1").expect("snapshot");
        assert_eq!(candidate.len(), 40);
        // The candidate contains the added file even though it was never
        // tracked or committed by the worker.
        let show = git(&repo, &["show", &format!("{candidate}:src/untracked.txt")]).expect("show");
        assert_eq!(show, "added by worker");

        // Export the patch and verify the candidate is fully captured.
        let patch = repo.parent().unwrap().join("candidate.patch");
        wt.export_patch(&candidate, &patch).expect("export");
        let patch_text = std::fs::read_to_string(&patch).expect("patch");
        assert!(patch_text.contains("+added by worker"), "{patch_text}");
        // The artifact is what the user integrates: it must apply as is.
        git(&repo, &["apply", "--check", &patch.to_string_lossy()])
            .expect("the exported patch applies to the base checkout");
        remove_worktree(&repo, &wt_path);
    }

    #[test]
    fn scope_check_accepts_declared_writes() {
        let (_dir, repo) = temp_repo();
        let sha = resolve_base(&repo, "HEAD").expect("base");
        let wt_path = repo.parent().unwrap().join("wt-scope");
        let wt = create_worktree(&repo, &sha, &wt_path).expect("worktree");
        std::fs::write(wt_path.join("src/main.rs"), "// touched\n").expect("edit");
        let contract = contract_with_scope(&["src/**"]);
        let candidate = wt.snapshot_candidate("attempt-1").expect("snapshot");
        assert!(check_scope(&wt, &candidate, &contract)
            .expect("scope")
            .is_empty());
        remove_worktree(&repo, &wt_path);
    }

    #[test]
    fn scope_violation_cannot_be_accepted() {
        let (_dir, repo) = temp_repo();
        let sha = resolve_base(&repo, "HEAD").expect("base");
        let wt_path = repo.parent().unwrap().join("wt-viol");
        let wt = create_worktree(&repo, &sha, &wt_path).expect("worktree");
        std::fs::write(wt_path.join("outside.rs"), "// out of scope\n").expect("edit");
        let contract = contract_with_scope(&["src/**"]);
        let candidate = wt.snapshot_candidate("attempt-1").expect("snapshot");
        let err = check_scope(&wt, &candidate, &contract).unwrap_err();
        assert!(matches!(err, WorkspaceError::ScopeViolation(_)), "{err}");
        remove_worktree(&repo, &wt_path);
    }

    #[test]
    fn protected_paths_need_explicit_scope() {
        let (_dir, repo) = temp_repo();
        let sha = resolve_base(&repo, "HEAD").expect("base");
        let wt_path = repo.parent().unwrap().join("wt-prot");
        let wt = create_worktree(&repo, &sha, &wt_path).expect("worktree");
        std::fs::create_dir_all(wt_path.join(".claude")).expect("mkdir");
        std::fs::write(wt_path.join(".claude/settings.json"), "{}").expect("write");
        let candidate = wt.snapshot_candidate("attempt-1").expect("snapshot");
        // Named in write scope as a directory pattern: still protected.
        let loose = contract_with_scope(&["**"]);
        assert!(
            check_scope(&wt, &candidate, &loose).is_err(),
            "a broad pattern must not unlock .claude/"
        );
        // Explicitly named, pattern-for-pattern: allowed.
        let explicit = contract_with_scope(&["**", ".claude/**"]);
        assert!(
            check_scope(&wt, &candidate, &explicit).is_ok(),
            "explicit naming allows it"
        );
        // Naming ONE protected area unlocks that area only: the policy
        // file is a different prefix and stays protected.
        std::fs::write(wt_path.join("relais.toml"), "schema_version = 1\n").expect("write");
        let candidate = wt.snapshot_candidate("attempt-2").expect("snapshot");
        let err = check_scope(&wt, &candidate, &explicit).unwrap_err();
        assert!(err.to_string().contains("relais.toml (protected"), "{err}");
        remove_worktree(&repo, &wt_path);
    }

    #[test]
    fn candidate_identity_is_a_function_of_content_not_time() {
        let (_dir, repo) = temp_repo();
        let sha = resolve_base(&repo, "HEAD").expect("base");
        let wt_path = repo.parent().unwrap().join("wt-ident");
        let wt = create_worktree(&repo, &sha, &wt_path).expect("worktree");
        std::fs::write(wt_path.join("src/main.rs"), "fn main() { v2 }\n").expect("edit");
        let first = wt.snapshot_candidate("attempt-1").expect("snapshot");
        // What makes identity independent of time is the FIXED stamp,
        // so that is what is asserted — instantly, on every run. The
        // 1.1 s sleep this replaces only ever observed the property
        // indirectly, and only if the suite happened to cross a second.
        let stamps = git(&wt_path, &["show", "-s", "--format=%at %ct", &first]).expect("show");
        assert_eq!(
            stamps.trim(),
            format!("{SNAPSHOT_DATE_UNIX} {SNAPSHOT_DATE_UNIX}"),
            "author and committer dates are the fixed stamp, not the wall clock"
        );
        let second = wt.snapshot_candidate("attempt-2").expect("snapshot");
        assert_eq!(
            first, second,
            "same tree, same identity, whatever the clock says"
        );
        assert!(!wt.same_tree_as_base(&first).expect("tree compare"));
        std::fs::write(wt_path.join("src/main.rs"), "fn main() {}\n").expect("revert");
        let reverted = wt.snapshot_candidate("attempt-3").expect("snapshot");
        assert!(wt.same_tree_as_base(&reverted).expect("tree compare"));
        remove_worktree(&repo, &wt_path);
    }

    #[test]
    fn scope_judges_the_snapshot_not_the_live_tree_and_reads_non_ascii_paths() {
        let (_dir, repo) = temp_repo();
        let sha = resolve_base(&repo, "HEAD").expect("base");
        let wt_path = repo.parent().unwrap().join("wt-toctou");
        let wt = create_worktree(&repo, &sha, &wt_path).expect("worktree");
        std::fs::create_dir_all(wt_path.join(".claude")).expect("mkdir");
        std::fs::write(wt_path.join(".claude/réglages.json"), "{}").expect("write");
        let candidate = wt.snapshot_candidate("attempt-1").expect("snapshot");
        // The worker (or a straggling descendant) deletes the file after
        // the snapshot: the candidate still carries it, and the check
        // still sees it, under its real name.
        std::fs::remove_file(wt_path.join(".claude/réglages.json")).expect("rm");
        let paths = wt.changed_paths_in(&candidate).expect("paths");
        assert_eq!(paths, vec![".claude/réglages.json".to_string()]);
        let loose = contract_with_scope(&["**"]);
        let err = check_scope(&wt, &candidate, &loose).unwrap_err();
        assert!(
            err.to_string().contains("réglages.json (protected"),
            "{err}"
        );
        remove_worktree(&repo, &wt_path);
    }

    #[test]
    fn a_named_candidate_survives_gc_and_forget_releases_it() {
        let (_dir, repo) = temp_repo();
        let sha = resolve_base(&repo, "HEAD").expect("base");
        let wt_path = repo.parent().unwrap().join("wt-ref");
        let wt = create_worktree(&repo, &sha, &wt_path).expect("worktree");
        std::fs::write(wt_path.join("src/main.rs"), "fn main() { kept }\n").expect("edit");
        let candidate = wt.snapshot_candidate("attempt-1").expect("snapshot");
        let name = name_candidate(&repo, "run-42", 1, &candidate).expect("ref");
        assert_eq!(name, "refs/relais/candidates/run-42/1");
        assert_eq!(
            git(&repo, &["rev-parse", &name]).expect("resolves"),
            candidate
        );
        // A named candidate is not a branch: it is invisible to branch
        // listings and to a default push refspec.
        let branches = git(&repo, &["branch", "--list"]).expect("branches");
        assert!(!branches.contains("run-42"), "{branches}");
        // Pruning everything unreachable leaves the candidate alone.
        remove_worktree(&repo, &wt_path);
        git(&repo, &["gc", "--prune=now", "-q"]).expect("gc");
        assert_eq!(
            git(&repo, &["rev-parse", &name]).expect("still there"),
            candidate
        );
        assert_eq!(
            forget_run_refs(&repo, "run-42").expect("forget"),
            vec![name.clone()]
        );
        assert!(
            git(&repo, &["rev-parse", "--verify", &name]).is_err(),
            "the ref is gone"
        );
        assert!(
            forget_run_refs(&repo, "run-42")
                .expect("forget again")
                .is_empty(),
            "forgetting twice is not an error"
        );
    }

    #[test]
    fn an_ambient_git_environment_cannot_redirect_a_git_call() {
        let (_dir, repo) = temp_repo();
        // A hook, a rebase or a parent `git` exports these; they beat
        // `current_dir`, so an unsanitised child would read the wrong
        // repository. `git_command` removes them.
        let command = git_command(&repo);
        let removed: Vec<&std::ffi::OsStr> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key)
            .collect();
        for variable in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        ] {
            assert!(
                removed.contains(&std::ffi::OsStr::new(variable)),
                "{variable} is cleared"
            );
        }
    }

    /// An edit no candidate holds survives retirement as a ref and a
    /// patch; the ignored build output goes with the directory.
    #[test]
    fn retirement_keeps_an_unexported_edit_and_drops_ignored_output() {
        let (dir, repo) = temp_repo();
        std::fs::write(repo.join(".gitignore"), "target/\n").expect("ignore");
        git(&repo, &["add", ".gitignore"]).expect("add");
        git(&repo, &["commit", "-q", "-m", "ignore"]).expect("commit");
        let sha = resolve_base(&repo, "HEAD").expect("base");
        let wt_path = dir.join("worktrees/run-7/task");
        let wt = create_worktree(&repo, &sha, &wt_path).expect("worktree");
        // Attempt 1 was snapshotted and named; the worker then kept
        // writing: one tracked edit and a build directory.
        std::fs::write(wt_path.join("src/main.rs"), "fn main() { v1 }\n").expect("edit");
        let first = wt.snapshot_candidate("attempt-1").expect("snapshot");
        name_candidate(&repo, "run-7", 1, &first).expect("named");
        std::fs::write(wt_path.join("src/main.rs"), "fn main() { unexported }\n").expect("edit");
        std::fs::create_dir_all(wt_path.join("target/debug")).expect("mkdir");
        std::fs::write(wt_path.join("target/debug/bin"), vec![0u8; 4096]).expect("build output");

        let artifacts = dir.join("runs/run-7");
        let retired = retire(&wt, "run-7", &artifacts).expect("retired");
        let exported = retired.exported.expect("the edit was in no patch");
        assert_eq!(exported.reference, "refs/relais/candidates/run-7/final");
        assert_eq!(exported.patch_path, artifacts.join(FINAL_PATCH));
        let patch = std::fs::read_to_string(&exported.patch_path).expect("patch");
        assert!(patch.contains("+fn main() { unexported }"), "{patch}");
        let kept = git(
            &repo,
            &["show", &format!("{}:src/main.rs", exported.reference)],
        )
        .expect("the ref resolves");
        assert_eq!(kept, "fn main() { unexported }");
        assert!(
            retired.bytes_reclaimed >= 4096,
            "the ignored output counts: {}",
            retired.bytes_reclaimed
        );
        assert!(!wt_path.exists(), "the worktree is gone, target/ included");
        assert!(
            !wt_path.parent().unwrap().exists(),
            "and the run's empty worktree parent with it"
        );
        let worktrees = git(&repo, &["worktree", "list"]).expect("list");
        assert!(!worktrees.contains("run-7"), "{worktrees}");
        // The patch applies to the base: it is what the user integrates.
        git(
            &repo,
            &["apply", "--check", &exported.patch_path.to_string_lossy()],
        )
        .expect("the final patch applies");
    }

    /// The path the run was launched from may be gone by the time a
    /// leftover is swept: a task worktree removed after the run, the
    /// case the first `resume --retire` on a real machine hit. The
    /// leftover's own `.git` link still names the repository, and that
    /// is what retirement runs against.
    #[test]
    fn retirement_survives_a_vanished_launch_directory() {
        let (dir, repo) = temp_repo();
        let sha = resolve_base(&repo, "HEAD").expect("base");
        // The run was launched from a linked worktree that no longer exists.
        let launch = dir.join("launch-wt");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                &launch.to_string_lossy(),
                &sha,
            ],
        )
        .expect("launch worktree");
        let wt_path = dir.join("worktrees/run-9/task");
        let wt = create_worktree(&launch, &sha, &wt_path).expect("worktree");
        std::fs::write(wt_path.join("src/main.rs"), "fn main() { orphan }\n").expect("edit");
        git(
            &repo,
            &["worktree", "remove", "--force", &launch.to_string_lossy()],
        )
        .expect("the launch directory vanishes");
        assert!(!launch.exists());

        let artifacts = dir.join("runs/run-9");
        let retired = retire(&wt, "run-9", &artifacts).expect("retired from the leftover alone");
        let exported = retired.exported.expect("the orphan edit was unexported");
        let kept = git(
            &repo,
            &["show", &format!("{}:src/main.rs", exported.reference)],
        )
        .expect("the ref resolves in the repository");
        assert_eq!(kept, "fn main() { orphan }");
        assert!(!wt_path.exists(), "the leftover is gone");
    }

    /// A tree a named candidate already holds exports nothing, and the
    /// worktree is still released.
    #[test]
    fn retirement_of_an_already_named_tree_exports_nothing() {
        let (dir, repo) = temp_repo();
        let sha = resolve_base(&repo, "HEAD").expect("base");
        let wt_path = dir.join("worktrees/run-8/task");
        let wt = create_worktree(&repo, &sha, &wt_path).expect("worktree");
        std::fs::write(wt_path.join("src/main.rs"), "fn main() { done }\n").expect("edit");
        let candidate = wt.snapshot_candidate("attempt-1").expect("snapshot");
        name_candidate(&repo, "run-8", 1, &candidate).expect("named");
        let artifacts = dir.join("runs/run-8");
        let retired = retire(&wt, "run-8", &artifacts).expect("retired");
        assert_eq!(retired.exported, None, "attempt 1 already holds this tree");
        assert!(!artifacts.join(FINAL_PATCH).exists());
        assert!(!wt_path.exists());
        assert!(
            git(
                &repo,
                &[
                    "rev-parse",
                    "--verify",
                    "refs/relais/candidates/run-8/final"
                ]
            )
            .is_err(),
            "no final ref was written"
        );
        // A tree identical to the base needs no ref either.
        let wt_path = dir.join("worktrees/run-9/task");
        let wt = create_worktree(&repo, &sha, &wt_path).expect("worktree");
        let retired = retire(&wt, "run-9", &dir.join("runs/run-9")).expect("retired");
        assert_eq!(retired.exported, None, "the base is durable by definition");
        assert!(!wt_path.exists());
    }

    /// Retiring twice with a change in between never overwrites what
    /// the first retirement kept.
    #[test]
    fn a_second_final_candidate_gets_its_own_name() {
        let (dir, repo) = temp_repo();
        let sha = resolve_base(&repo, "HEAD").expect("base");
        let artifacts = dir.join("runs/run-10");
        let wt_path = dir.join("worktrees/run-10/task");
        let wt = create_worktree(&repo, &sha, &wt_path).expect("worktree");
        std::fs::write(wt_path.join("src/main.rs"), "fn main() { one }\n").expect("edit");
        let first = retire(&wt, "run-10", &artifacts).expect("retired");
        let wt = create_worktree(&repo, &sha, &wt_path).expect("worktree again");
        std::fs::write(wt_path.join("src/main.rs"), "fn main() { two }\n").expect("edit");
        let second = retire(&wt, "run-10", &artifacts).expect("retired");
        let first = first.exported.expect("one");
        let second = second.exported.expect("two");
        assert_eq!(first.reference, "refs/relais/candidates/run-10/final");
        assert_eq!(second.reference, "refs/relais/candidates/run-10/final-2");
        assert_eq!(second.patch_path, artifacts.join("candidate-final-2.patch"));
        assert_eq!(
            git(
                &repo,
                &["show", &format!("{}:src/main.rs", first.reference)]
            )
            .expect("kept"),
            "fn main() { one }"
        );
        assert!(std::fs::read_to_string(&first.patch_path)
            .expect("first patch")
            .contains("{ one }"));
    }

    /// The sweep sees both layouts and only directories that are
    /// worktrees.
    #[test]
    fn retained_worktrees_are_found_in_both_layouts() {
        let (dir, repo) = temp_repo();
        let state = dir.join("state");
        assert!(
            retained_worktrees(&state).expect("no state yet").is_empty(),
            "a state directory that does not exist holds nothing"
        );
        let sha = resolve_base(&repo, "HEAD").expect("base");
        let task = state.join("worktrees/run-a/task");
        create_worktree(&repo, &sha, &task).expect("worktree");
        let legacy = state.join("runs/run-b/worktree");
        create_worktree(&repo, &sha, &legacy).expect("legacy worktree");
        // Leftovers that are not worktrees: an emptied run directory,
        // and a run's artifacts.
        std::fs::create_dir_all(state.join("worktrees/run-c")).expect("mkdir");
        std::fs::write(state.join("runs/run-b/receipt.json"), "{}").expect("artifact");
        let found = retained_worktrees(&state).expect("scan");
        assert_eq!(
            found.iter().map(|w| w.run_id.as_str()).collect::<Vec<_>>(),
            vec!["run-b", "run-a"],
            "path order: runs/ before worktrees/: {found:?}"
        );
        assert_eq!(found[0].path, legacy);
        assert_eq!(found[1].path, task);
        assert!(found.iter().all(|w| w.bytes > 0), "{found:?}");
    }
}
