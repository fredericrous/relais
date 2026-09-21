//! Owned task worktrees (SPEC §8).
//!
//! The base commit is resolved once and an owned worktree is created from
//! that exact SHA; the user's checkout and other worktrees stay untouched.
//! A dirty working tree is refused explicitly — never silently copied.
//! Write scope is checked on the actual diff after each attempt — a scope
//! violation cannot be accepted. This is an acceptance boundary, not
//! filesystem isolation: tools with Bash access are not a sandbox. The
//! runner records an immutable candidate snapshot (a commit object,
//! including added files) outside model control, and a worktree with
//! unexported changes is never force-cleaned.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::contract::TaskContract;
use crate::ids::sha256_hex;

#[derive(Debug)]
pub enum WorkspaceError {
    Git(String),
    DirtyBase(Vec<String>),
    ScopeViolation(Vec<String>),
    UnexportedChanges(PathBuf),
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
            Self::UnexportedChanges(path) => write!(
                f,
                "worktree {} has unexported changes; refusing to clean it",
                path.display()
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
pub fn dirty_paths(repo_dir: &Path) -> Result<Vec<String>> {
    let status = git_raw(repo_dir, &["status", "--porcelain"])?;
    Ok(status
        .lines()
        .filter_map(|line| line.get(3..).map(|path| path.trim().to_string()))
        // `.relais/` is where the /relais skill writes the task contract
        // it is about to hand over. It is relais's own scratch, never part
        // of a candidate — the worktree is created from the base SHA, so
        // nothing in it is copied — and a contract that dirtied the tree
        // it describes would refuse every run it was written for.
        .filter(|path| !is_relais_scratch(path))
        .collect())
}

/// The one untracked path relais never counts as the user's uncommitted
/// work: its own contract directory.
pub fn is_relais_scratch(path: &str) -> bool {
    let path = path.trim_matches('"');
    path == ".relais" || path == ".relais/" || path.starts_with(".relais/")
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
            .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
            .env("GIT_COMMITTER_NAME", "relais")
            .env("GIT_COMMITTER_EMAIL", "relais@localhost")
            .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
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
/// policy file itself, the check gate's trust declaration, Claude Code
/// project settings, the aval registry, CI, the project instructions a
/// reviewer's session loads, and the ignore file (an unprotected
/// `.gitignore` lets a worker hide files from the diff, the snapshot and
/// the exported patch) are all positions a worker could use to widen its
/// own authority or narrow what is seen.
pub const PROTECTED_PATH_PREFIXES: &[&str] = &[
    "relais.toml",
    "amont.conf",
    ".adr.yaml",
    ".claude/",
    ".github/workflows/",
    ".gitignore",
    "CLAUDE.md",
    "AGENTS.md",
];

/// The protected prefix a path falls under, if any.
pub fn protected_prefix(path: &str) -> Option<&'static str> {
    PROTECTED_PATH_PREFIXES
        .iter()
        .copied()
        .find(|prefix| path == prefix.trim_end_matches('/') || path.starts_with(prefix))
}

pub fn is_protected_path(path: &str) -> bool {
    protected_prefix(path).is_some()
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
        // scope names THIS protected area — a pattern that starts with
        // the prefix the path falls under — not a blanket `**` that
        // happens to match it, and not some other protected prefix.
        let explicitly_allowed = match protected_prefix(&path) {
            None => true,
            Some(prefix) => in_scope && patterns.iter().any(|pattern| pattern.starts_with(prefix)),
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

/// Remove an owned worktree — unless it holds unexported changes. A
/// snapshot taken and exported makes the changes exported; nothing else
/// does (SPEC §8). A status check that cannot run is a reason to keep
/// the worktree, never to force-remove it.
pub fn release_worktree(repo_dir: &Path, worktree_path: &Path, exported: bool) -> Result<()> {
    if !exported {
        let status = git(worktree_path, &["status", "--porcelain"])?;
        if !status.is_empty() {
            return Err(WorkspaceError::UnexportedChanges(
                worktree_path.to_path_buf(),
            ));
        }
    }
    git(
        repo_dir,
        &[
            "worktree",
            "remove",
            "--force",
            &worktree_path.to_string_lossy(),
        ],
    )?;
    Ok(())
}

/// The ref name a run's attempt candidate is kept under.
pub fn candidate_ref(run_id: &str, attempt: u32) -> String {
    format!("refs/relais/candidates/{run_id}/{attempt}")
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
        // Unique per test: pid alone is shared across parallel test
        // threads, and clock nanos can collide under load, which once made
        // two tests stomp the same fixture repo. The counter is monotonic.
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "relais-ws-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
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
        assert!(is_relais_scratch(".relais/task.json"));
        assert!(is_relais_scratch(r#"".relais/t\303\242che.json""#));
        assert!(!is_relais_scratch(".relais-notes.md"));
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
        release_worktree(&repo, &wt_path, true).expect("released");
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
        release_worktree(&repo, &wt_path, true).expect("released");
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
        release_worktree(&repo, &wt_path, true).expect("released");
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
        release_worktree(&repo, &wt_path, true).expect("released");
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
        release_worktree(&repo, &wt_path, true).expect("released");
    }

    #[test]
    fn candidate_identity_is_a_function_of_content_not_time() {
        let (_dir, repo) = temp_repo();
        let sha = resolve_base(&repo, "HEAD").expect("base");
        let wt_path = repo.parent().unwrap().join("wt-ident");
        let wt = create_worktree(&repo, &sha, &wt_path).expect("worktree");
        std::fs::write(wt_path.join("src/main.rs"), "fn main() { v2 }\n").expect("edit");
        let first = wt.snapshot_candidate("attempt-1").expect("snapshot");
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let second = wt.snapshot_candidate("attempt-2").expect("snapshot");
        assert_eq!(
            first, second,
            "same tree, same identity, whatever the clock says"
        );
        assert!(!wt.same_tree_as_base(&first).expect("tree compare"));
        std::fs::write(wt_path.join("src/main.rs"), "fn main() {}\n").expect("revert");
        let reverted = wt.snapshot_candidate("attempt-3").expect("snapshot");
        assert!(wt.same_tree_as_base(&reverted).expect("tree compare"));
        release_worktree(&repo, &wt_path, true).expect("released");
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
        release_worktree(&repo, &wt_path, true).expect("released");
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
        release_worktree(&repo, &wt_path, true).expect("released");
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

    #[test]
    fn unexported_changes_are_never_force_cleaned() {
        let (_dir, repo) = temp_repo();
        let sha = resolve_base(&repo, "HEAD").expect("base");
        let wt_path = repo.parent().unwrap().join("wt-keep");
        let wt = create_worktree(&repo, &sha, &wt_path).expect("worktree");
        std::fs::write(wt_path.join("src/main.rs"), "// unexported work\n").expect("edit");
        let err = release_worktree(&repo, &wt_path, false).unwrap_err();
        assert!(matches!(err, WorkspaceError::UnexportedChanges(_)), "{err}");
        // After exporting a snapshot, the same cleanup succeeds.
        let candidate = wt.snapshot_candidate("attempt-1").expect("snapshot");
        let patch = repo.parent().unwrap().join("keep.patch");
        wt.export_patch(&candidate, &patch).expect("export");
        release_worktree(&repo, &wt_path, true).expect("released after export");
    }
}
