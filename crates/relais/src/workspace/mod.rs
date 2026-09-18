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

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    git_raw(dir, args).map(|output| output.trim().to_string())
}

/// Like `git`, but without trimming: `status --porcelain` encodes the
/// status in leading bytes that a whole-output trim would eat.
fn git_raw(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
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
        .collect())
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
    /// and no working-tree mutation. The message is FIXED so the SHA is a
    /// pure function of (tree, base) — identical content is an identical
    /// candidate identity, which the same-failure recurrence check
    /// depends on.
    pub fn snapshot_candidate(&self, _label: &str) -> Result<String> {
        git(&self.path, &["add", "-A"])?;
        let tree = git(&self.path, &["write-tree"])?;
        let commit = Command::new("git")
            .args([
                "commit-tree",
                &tree,
                "-p",
                &self.base_sha,
                "-m",
                "relais candidate snapshot",
            ])
            .current_dir(&self.path)
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

    /// Export the candidate as a patch artifact.
    pub fn export_patch(&self, candidate_sha: &str, out_path: &Path) -> Result<()> {
        let diff = git(&self.path, &["diff", &self.base_sha, candidate_sha])?;
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
/// project settings and the aval registry are all positions a worker could
/// use to widen its own authority.
pub const PROTECTED_PATH_PREFIXES: &[&str] = &[
    "relais.toml",
    "amont.conf",
    ".adr.yaml",
    ".claude/",
    ".github/workflows/",
];

pub fn is_protected_path(path: &str) -> bool {
    PROTECTED_PATH_PREFIXES
        .iter()
        .any(|prefix| path == prefix.trim_end_matches('/') || path.starts_with(prefix))
}

/// Check the actual diff against the declared scope. A path is in scope
/// when some write_scope pattern matches it (gitignore-style semantics via
/// globset). Protected paths additionally need the contract to name them
/// explicitly, pattern-for-pattern.
pub fn check_scope(
    worktree: &TaskWorktree,
    contract: &TaskContract,
) -> std::result::Result<Vec<String>, WorkspaceError> {
    let patterns: &[String] = match contract.write_scope.as_deref() {
        Some(patterns) if !patterns.is_empty() => patterns,
        _ => &[],
    };
    let mut matcher = globset::GlobSet::builder();
    for pattern in patterns {
        matcher.add(
            globset::GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()
                .map_err(|e| WorkspaceError::Git(format!("bad scope pattern `{pattern}`: {e}")))?,
        );
    }
    let matcher = matcher.build().expect("valid patterns");

    let mut violations = Vec::new();
    for path in worktree.changed_paths()? {
        let in_scope = matcher.is_match(&path);
        let protected = is_protected_path(&path);
        // "Explicitly within an approved contract" (SPEC §8) means the
        // scope names the protected area itself — a pattern that starts
        // with a protected prefix — not a blanket `**` that happens to
        // match it.
        let explicitly_allowed = protected
            && patterns.iter().any(|pattern| {
                PROTECTED_PATH_PREFIXES
                    .iter()
                    .any(|prefix| pattern.starts_with(prefix))
                    && matcher.is_match(&path)
            });
        if !in_scope || (protected && !explicitly_allowed) {
            violations.push(if protected && !explicitly_allowed {
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
/// does (SPEC §8).
pub fn release_worktree(repo_dir: &Path, worktree_path: &Path, exported: bool) -> Result<()> {
    if !exported {
        if let Ok(status) = git(worktree_path, &["status", "--porcelain"]) {
            if !status.is_empty() {
                return Err(WorkspaceError::UnexportedChanges(
                    worktree_path.to_path_buf(),
                ));
            }
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
        assert!(check_scope(&wt, &contract).expect("scope").is_empty());
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
        let err = check_scope(&wt, &contract).unwrap_err();
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
        // Named in write scope as a directory pattern: still protected.
        let loose = contract_with_scope(&["**"]);
        assert!(
            check_scope(&wt, &loose).is_err(),
            "a broad pattern must not unlock .claude/"
        );
        // Explicitly named, pattern-for-pattern: allowed.
        let explicit = contract_with_scope(&["**", ".claude/**"]);
        assert!(
            check_scope(&wt, &explicit).is_ok(),
            "explicit naming allows it"
        );
        release_worktree(&repo, &wt_path, true).expect("released");
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
