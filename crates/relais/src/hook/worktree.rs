//! The default tree for an isolated spawn relais did not mark (SPEC §23).
//!
//! Once relais answers `WorktreeCreate`, Claude Code refuses an agent
//! whose hook prints no path (plan S0, E8), and `WorktreeRemove` never
//! fires for a tree a hook made (E11). So for every spawn that is not a
//! relais dispatch this module reproduces what Claude Code does by
//! default (E9) — `<repo>/.claude/worktrees/<name>` on a new branch
//! `worktree-<name>` from HEAD — keeps a record of what it made, and on
//! the agent's `SubagentStop` removes the tree again when nothing changed
//! in it, branch included.
//!
//! Impure: it runs git and writes files. Every failure on the cleanup
//! side is best effort and reported as a [`Cleanup`], never raised: a
//! hook cannot fail on tidying up.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::workspace::git_command;

const MAX_NAME_CHARS: usize = 128;

/// What `create_default` made, kept so the stop knows what is its own to
/// remove, and whether anything changed since.
#[derive(Debug, Serialize, Deserialize)]
struct Record {
    path: PathBuf,
    root: PathBuf,
    branch: String,
    base_sha: String,
}

/// What `cleanup_after_stop` did, for the journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cleanup {
    /// No record for this agent: its tree, if any, is not ours to touch.
    NoRecord,
    /// The record named a tree that was already gone.
    Gone,
    /// The tree was unchanged and is removed, branch included.
    Removed,
    /// The tree changed (or could not be inspected) and is kept.
    Kept { reason: String },
    /// Something failed; whatever was not undone stays.
    Failed { reason: String },
}

impl Cleanup {
    /// The journal's one-line account of the outcome.
    pub fn label(&self) -> String {
        match self {
            Cleanup::NoRecord => "no_record".to_string(),
            Cleanup::Gone => "gone".to_string(),
            Cleanup::Removed => "removed".to_string(),
            Cleanup::Kept { reason } => format!("kept: {reason}"),
            Cleanup::Failed { reason } => format!("failed: {reason}"),
        }
    }
}

/// Create the default tree for `name` under the repository `cwd` is in,
/// and record it under `records`. Returns the tree's path.
pub fn create_default(
    cwd: &Path,
    name: &str,
    session_id: &str,
    records: &Path,
) -> Result<PathBuf, String> {
    if !is_safe_name(name) {
        return Err(format!("refusing the worktree name {name:?}"));
    }
    if !is_safe_component(session_id) {
        return Err(format!("refusing the session id {session_id:?}"));
    }
    let root = PathBuf::from(git_output(cwd, &["rev-parse", "--show-toplevel"])?);
    let path = root.join(".claude").join("worktrees").join(name);
    if path.symlink_metadata().is_ok() {
        return Err(format!("{} already exists", path.display()));
    }
    let branch = format!("worktree-{name}");
    let path_arg = path.to_string_lossy().into_owned();
    git_output(
        &root,
        &["worktree", "add", "-b", &branch, &path_arg, "HEAD"],
    )?;
    let record = match git_output(&path, &["rev-parse", "HEAD"]) {
        Ok(base_sha) => Record {
            path: path.clone(),
            root: root.clone(),
            branch,
            base_sha,
        },
        Err(reason) => return Err(with_undo(reason, undo(&root, &path, &branch))),
    };
    let record_path = records.join(session_id).join(format!("{name}.json"));
    if let Err(e) = write_record(&record_path, &record) {
        let reason = format!("could not record {}: {e}", path.display());
        return Err(with_undo(reason, undo(&root, &path, &record.branch)));
    }
    Ok(path)
}

/// On an agent's stop: remove the tree `create_default` made for it when
/// nothing changed in it, keep it otherwise, and delete the record either
/// way. A tree with no record is never touched.
pub fn cleanup_after_stop(session_id: &str, agent_id: &str, records: &Path) -> Cleanup {
    let name = format!("agent-{agent_id}");
    if !is_safe_name(&name) || !is_safe_component(session_id) {
        return Cleanup::NoRecord;
    }
    let record_path = records.join(session_id).join(format!("{name}.json"));
    let text = match std::fs::read_to_string(&record_path) {
        Ok(text) => text,
        // No record: this agent's tree, if it has one, is not ours.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Cleanup::NoRecord,
        // A record we cannot read is ours and stays, with the reason in
        // the journal, rather than passing for no record at all.
        Err(e) => {
            return Cleanup::Failed {
                reason: format!("cannot read record {}: {e}", record_path.display()),
            }
        }
    };
    let outcome = match serde_json::from_str::<Record>(&text) {
        Ok(record) => settle(&record),
        Err(e) => Cleanup::Failed {
            reason: format!("unreadable record {}: {e}", record_path.display()),
        },
    };
    // Dropped whatever happened: a kept tree is no longer ours to manage,
    // and a record left behind would be acted on by a later stop.
    if let Err(e) = std::fs::remove_file(&record_path) {
        return Cleanup::Failed {
            reason: format!("could not delete {}: {e}", record_path.display()),
        };
    }
    outcome
}

fn settle(record: &Record) -> Cleanup {
    if !record.path.exists() {
        return Cleanup::Gone;
    }
    let status = match git_output(
        &record.path,
        &["status", "--porcelain", "--untracked-files=all"],
    ) {
        Ok(status) => status,
        Err(reason) => return Cleanup::Failed { reason },
    };
    if !status.is_empty() {
        return Cleanup::Kept {
            reason: "the tree has changes".to_string(),
        };
    }
    match git_output(&record.path, &["rev-parse", "HEAD"]) {
        Ok(head) if head == record.base_sha => {}
        Ok(_) => {
            return Cleanup::Kept {
                reason: "the tree has new commits".to_string(),
            }
        }
        Err(reason) => return Cleanup::Failed { reason },
    }
    let path_arg = record.path.to_string_lossy().into_owned();
    if let Err(reason) = git_output(&record.root, &["worktree", "remove", &path_arg]) {
        return Cleanup::Failed { reason };
    }
    match git_output(&record.root, &["branch", "-D", &record.branch]) {
        Ok(_) => Cleanup::Removed,
        Err(reason) => Cleanup::Failed { reason },
    }
}

/// Take a half-made tree back out, returning what could not be undone: a
/// tree or branch left behind with no record is one no cleanup will find,
/// so the person is told about it with the failure that led here.
fn undo(root: &Path, path: &Path, branch: &str) -> Vec<String> {
    let path_arg = path.to_string_lossy().into_owned();
    [
        git_output(root, &["worktree", "remove", "--force", &path_arg])
            .err()
            .map(|e| format!("the tree {path_arg} is left behind ({e})")),
        git_output(root, &["branch", "-D", branch])
            .err()
            .map(|e| format!("the branch {branch} is left behind ({e})")),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The failure that led to an undo, and whatever the undo left behind.
fn with_undo(reason: String, left: Vec<String>) -> String {
    if left.is_empty() {
        reason
    } else {
        format!("{reason}; undoing it failed: {}", left.join("; "))
    }
}

/// A name Claude Code chose, checked before it becomes part of a path and
/// a branch.
fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= MAX_NAME_CHARS
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// A session id used as a directory name: the same alphabet, so it cannot
/// climb out of the records directory.
fn is_safe_component(text: &str) -> bool {
    is_safe_name(text)
}

/// Run git in `dir`; trimmed stdout on success, the reason on failure.
fn git_output(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut command: Command = git_command(dir);
    let output = command
        .args(args)
        .output()
        .map_err(|e| format!("git could not be launched: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// The note a rewritten relais spawn leaves for its `WorktreeCreate`:
/// how many of this session's spawns still await relais's tree. It lets a
/// `WorktreeCreate` that cannot reach the coordinator tell a relais spawn
/// (refused: a default tree is one relais would never judge) from anyone
/// else's (the default tree, as always).
fn pending_note(records: &Path, session_id: &str) -> PathBuf {
    records.join(session_id).join("native-pending")
}

fn pending_count(note: &Path) -> u64 {
    // A missing or unreadable note is no pending relais spawn: the
    // `WorktreeCreate` then takes the default tree, as it does for anyone.
    std::fs::read_to_string(note)
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

fn write_pending(note: &Path, count: u64) -> std::io::Result<()> {
    if count == 0 {
        return match std::fs::remove_file(note) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            Ok(()) | Err(_) => Ok(()),
        };
    }
    if let Some(parent) = note.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(note, count.to_string())
}

/// A marked spawn was rewritten: one more of this session's spawns awaits
/// relais's tree.
pub fn note_native_spawn(records: &Path, session_id: &str) -> std::io::Result<()> {
    if !is_safe_component(session_id) {
        return Err(std::io::Error::other(format!(
            "refusing the session id {session_id:?}"
        )));
    }
    let note = pending_note(records, session_id);
    write_pending(&note, pending_count(&note).saturating_add(1))
}

/// A `WorktreeCreate` of this session was answered (or refused) for a
/// relais spawn: take one pending spawn off the note. `true` when there was
/// one to take.
pub fn take_native_spawn(records: &Path, session_id: &str) -> bool {
    if !is_safe_component(session_id) {
        return false;
    }
    let note = pending_note(records, session_id);
    let count = pending_count(&note);
    if count == 0 {
        return false;
    }
    // Best effort: a note left one too high refuses at most one later
    // unreachable `WorktreeCreate` of this session, which says why.
    let _ = write_pending(&note, count - 1);
    true
}

/// Write a record owner-only, like the hook journal: it names a person's
/// directories.
fn write_record(path: &Path, record: &Record) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        // `.mode` is masked by the umask at creation and ignored for an
        // existing file; set it explicitly, as the journal does.
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = file.metadata()?.permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(path, permissions)?;
    }
    let text = serde_json::to_string(record).map_err(std::io::Error::other)?;
    file.write_all(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_dir;

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = git_command(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    struct World {
        _scratch: crate::test_support::TempDir,
        repo: PathBuf,
        records: PathBuf,
    }

    fn world(tag: &str) -> World {
        let scratch = temp_dir(&format!("hook-worktree-{tag}"));
        let repo = scratch.join("repo");
        std::fs::create_dir_all(&repo).expect("repo dir");
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "a\n").expect("file");
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "chore: init"]);
        // The root git reports, which may differ from the scratch path
        // (a symlinked temp dir).
        let repo = PathBuf::from(git(&repo, &["rev-parse", "--show-toplevel"]));
        let records = scratch.join("records");
        World {
            _scratch: scratch,
            repo,
            records,
        }
    }

    fn record_file(world: &World, session: &str, name: &str) -> PathBuf {
        world.records.join(session).join(format!("{name}.json"))
    }

    #[test]
    fn a_default_tree_is_made_on_its_own_branch_at_head() {
        let w = world("create");
        let path = create_default(&w.repo, "agent-a1", "s1", &w.records).expect("created");
        assert_eq!(path, w.repo.join(".claude/worktrees/agent-a1"));
        assert!(path.join("a.txt").exists());
        assert_eq!(
            git(&path, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "worktree-agent-a1"
        );
        assert_eq!(
            git(&path, &["rev-parse", "HEAD"]),
            git(&w.repo, &["rev-parse", "HEAD"])
        );
        let record = record_file(&w, "s1", "agent-a1");
        assert!(record.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&record).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_tree_is_made_from_a_subdirectory_of_the_repo() {
        let w = world("subdir");
        let sub = w.repo.join("deep");
        std::fs::create_dir_all(&sub).unwrap();
        let path = create_default(&sub, "agent-a1", "s1", &w.records).expect("created");
        assert_eq!(path, w.repo.join(".claude/worktrees/agent-a1"));
    }

    #[test]
    fn unsafe_names_and_existing_paths_fail_and_create_nothing() {
        let w = world("refuse");
        let too_long = "a".repeat(129);
        for name in ["", ".hidden", "a/b", "a b", "../x", too_long.as_str()] {
            let err = create_default(&w.repo, name, "s1", &w.records).expect_err(name);
            assert!(err.contains("refusing"), "{name}: {err}");
        }
        assert!(!w.repo.join(".claude").exists());
        assert!(!w.records.exists());

        create_default(&w.repo, "agent-a1", "s1", &w.records).expect("first");
        let err = create_default(&w.repo, "agent-a1", "s1", &w.records).expect_err("exists");
        assert!(err.contains("already exists"), "{err}");
    }

    #[test]
    fn an_unsafe_session_id_creates_nothing() {
        let w = world("session");
        create_default(&w.repo, "agent-a1", "../s", &w.records).expect_err("unsafe session");
        assert!(!w.repo.join(".claude").exists());
    }

    #[test]
    fn a_directory_outside_a_repository_fails() {
        let scratch = temp_dir("hook-worktree-norepo");
        let records = scratch.join("records");
        let err = create_default(&scratch, "agent-a1", "s1", &records).expect_err("no repo");
        assert!(err.contains("git"), "{err}");
    }

    #[test]
    fn an_unchanged_tree_is_removed_with_its_branch_and_its_record() {
        let w = world("unchanged");
        let path = create_default(&w.repo, "agent-a1", "s1", &w.records).expect("created");
        let outcome = cleanup_after_stop("s1", "a1", &w.records);
        assert_eq!(outcome, Cleanup::Removed);
        assert!(!path.exists());
        assert_eq!(git(&w.repo, &["branch", "--list", "worktree-agent-a1"]), "");
        assert!(!record_file(&w, "s1", "agent-a1").exists());
    }

    #[test]
    fn a_tree_with_an_untracked_file_is_kept_and_its_record_goes() {
        let w = world("untracked");
        let path = create_default(&w.repo, "agent-a1", "s1", &w.records).expect("created");
        std::fs::write(path.join("new.txt"), "x").unwrap();
        let outcome = cleanup_after_stop("s1", "a1", &w.records);
        assert!(matches!(outcome, Cleanup::Kept { .. }), "{outcome:?}");
        assert!(path.exists());
        assert!(!git(&w.repo, &["branch", "--list", "worktree-agent-a1"]).is_empty());
        assert!(!record_file(&w, "s1", "agent-a1").exists());
    }

    #[test]
    fn a_tree_with_a_new_commit_is_kept_and_its_record_goes() {
        let w = world("commit");
        let path = create_default(&w.repo, "agent-a1", "s1", &w.records).expect("created");
        std::fs::write(path.join("new.txt"), "x").unwrap();
        git(&path, &["add", "."]);
        git(&path, &["commit", "-q", "-m", "feat: work"]);
        let outcome = cleanup_after_stop("s1", "a1", &w.records);
        assert!(matches!(outcome, Cleanup::Kept { .. }), "{outcome:?}");
        assert!(path.exists());
        assert!(!record_file(&w, "s1", "agent-a1").exists());
    }

    #[test]
    fn a_tree_with_no_record_is_never_touched() {
        let w = world("norecord");
        let path = w.repo.join(".claude/worktrees/agent-a1");
        git(
            &w.repo,
            &[
                "worktree",
                "add",
                "-b",
                "worktree-agent-a1",
                &path.to_string_lossy(),
                "HEAD",
            ],
        );
        assert_eq!(
            cleanup_after_stop("s1", "a1", &w.records),
            Cleanup::NoRecord
        );
        assert!(path.exists());
    }

    #[test]
    fn a_record_for_a_tree_already_gone_is_dropped() {
        let w = world("gone");
        let path = create_default(&w.repo, "agent-a1", "s1", &w.records).expect("created");
        git(
            &w.repo,
            &["worktree", "remove", "--force", &path.to_string_lossy()],
        );
        assert_eq!(cleanup_after_stop("s1", "a1", &w.records), Cleanup::Gone);
        assert!(!record_file(&w, "s1", "agent-a1").exists());
    }

    #[test]
    fn another_sessions_record_is_not_read() {
        let w = world("session-scope");
        let path = create_default(&w.repo, "agent-a1", "s1", &w.records).expect("created");
        assert_eq!(
            cleanup_after_stop("s2", "a1", &w.records),
            Cleanup::NoRecord
        );
        assert!(path.exists());
    }

    /// What an undo leaves behind travels with the failure that led to it.
    #[test]
    fn an_undo_that_leaves_something_behind_says_so() {
        assert_eq!(with_undo("boom".into(), Vec::new()), "boom");
        let said = with_undo(
            "boom".into(),
            vec!["the branch worktree-x is left behind (locked)".into()],
        );
        assert_eq!(
            said,
            "boom; undoing it failed: the branch worktree-x is left behind (locked)"
        );
    }
}
