//! Salvage evidence for labelling (SPEC §17 labels).
//!
//! A run a person salvaged (`relais decide --answer salvaged`) is a
//! positive label only when the runner itself had verified the candidate
//! and stopped for review findings, and that verified candidate survives
//! intact in the commit that landed. Everything else about a salvage says
//! nothing about the worker's reasoning: the person rewrote the change, or
//! the run stopped for a reason unrelated to review (interrupted, budget,
//! blocked).
//!
//! This module holds the pure reading of a decision into closed evidence
//! ([`salvage_evidence_of`]) and the effectful containment test
//! ([`candidate_contained`]: `git apply --cached --check -R` of the
//! candidate patch on the salvaged commit's tree, against a temporary
//! index so the repository's own index is never touched).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ids::RunId;
use crate::ledger::{DecisionRecord, EvidenceKind, Ledger, LedgerError};
use crate::lifecycle::{Reason, State};

/// What the ledger says about a run's salvage, read before labelling.
/// Closed, so every labelling site must decide what each case means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SalvageEvidence {
    /// The run was never salvaged (it may still have a decision row).
    NotSalvaged,
    /// Salvaged, but the run was raised from some other state or reason
    /// than `needs_review` / `review_findings`: the runner never finished
    /// verifying a candidate its review then questioned.
    RaisedElsewhere,
    /// Raised from `needs_review` with `review_findings`, and the verified
    /// candidate patch survives intact in the landed commit.
    ReviewedAndContained,
    /// Raised from `needs_review` with `review_findings`, but the landed
    /// commit rewrote the candidate.
    ReviewedAndRewritten,
    /// Raised from `needs_review` with `review_findings`; containment was
    /// never computed (repository or patch unreachable).
    ReviewedContainmentUnknown,
}

/// Read a run's decision into [`SalvageEvidence`]. Reads the RAISING state
/// and reason the decision row keeps (`record_salvage` preserves them),
/// never the last transition row, which a same-state `worktree_retired`
/// row would have overwritten.
pub fn salvage_evidence_of(decision: Option<&DecisionRecord>) -> SalvageEvidence {
    let Some(decision) = decision else {
        return SalvageEvidence::NotSalvaged;
    };
    if decision.resolution != Some(Reason::DecisionSalvaged) {
        return SalvageEvidence::NotSalvaged;
    }
    let raised_for_review = decision.raised_state == State::NeedsReview
        && decision.raised_reason == Reason::ReviewFindings.as_str();
    if !raised_for_review {
        return SalvageEvidence::RaisedElsewhere;
    }
    match decision.candidate_contained {
        Some(true) => SalvageEvidence::ReviewedAndContained,
        Some(false) => SalvageEvidence::ReviewedAndRewritten,
        None => SalvageEvidence::ReviewedContainmentUnknown,
    }
}

/// The repository to test containment in: the run's recorded path, or,
/// when that is a removed `<name>-wt-<suffix>` worktree, the primary
/// checkout `<name>` beside it. `None` when neither is a directory.
pub fn reachable_repo(repo_path: &Path) -> Option<PathBuf> {
    if repo_path.is_dir() {
        return Some(repo_path.to_path_buf());
    }
    let name = repo_path.file_name()?.to_str()?;
    let (primary, _) = name.split_once("-wt-")?;
    if primary.is_empty() {
        return None;
    }
    let candidate = repo_path.with_file_name(primary);
    candidate.is_dir().then_some(candidate)
}

static NEXT_INDEX: AtomicU64 = AtomicU64::new(0);

/// A scratch directory holding the temporary index; removed on drop.
struct ScratchIndex(PathBuf);

impl ScratchIndex {
    fn create() -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!(
            "relais-salvage-{}-{}",
            std::process::id(),
            NEXT_INDEX.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }

    fn index_file(&self) -> PathBuf {
        self.0.join("index")
    }
}

impl Drop for ScratchIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(repo: &Path, index: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_INDEX_FILE", index)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command
}

/// What one containment test found. Closed: a git or IO failure is
/// `Unreachable` with the text that says why, never folded into an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Containment {
    /// The candidate survives intact in the commit.
    Contained,
    /// The commit rewrote the candidate.
    Rewritten,
    /// It cannot be told; the reason is the error text (git's stderr
    /// included).
    Unreachable(String),
}

impl Containment {
    /// The answer to record, `None` when unreachable.
    pub fn answer(&self) -> Option<bool> {
        match self {
            Self::Contained => Some(true),
            Self::Rewritten => Some(false),
            Self::Unreachable(_) => None,
        }
    }
}

/// Run a git command to completion; `Err` is the reason it did not
/// succeed, with git's stderr.
fn run_git_step(what: &str, command: &mut Command) -> Result<(), String> {
    let output = command
        .output()
        .map_err(|e| format!("{what}: could not run git: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    Err(if stderr.is_empty() {
        format!("{what}: {}", output.status)
    } else {
        format!("{what}: {stderr}")
    })
}

/// Whether `patch` (the candidate the runner verified) reverse-applies on
/// the tree of `sha` in `repo`: [`Containment::Contained`] the candidate
/// survives intact in that commit, [`Containment::Rewritten`] the commit
/// rewrote it, [`Containment::Unreachable`] when it cannot be told
/// (repository, commit or patch unavailable, git or the filesystem
/// failing). Runs `git apply --cached --check -R` against a temporary
/// index, never the repository's own.
pub fn candidate_contained(repo: &Path, patch: &Path, sha: &str) -> Containment {
    patch_check(repo, patch, sha, Direction::Reverse)
}

/// Which way a patch is checked against a tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Direction {
    /// The patch's result is already in the tree (`git apply -R`).
    Reverse,
    /// The patch's change is still to be made (`git apply`).
    Forward,
}

/// `git apply --cached --check` (reversed for [`Direction::Reverse`]) of
/// `patch` on the tree of `sha`, against a temporary index.
/// [`Containment::Contained`] reads "the check passed", [`Containment::Rewritten`]
/// "the patch does not apply that way", whichever the direction.
pub(super) fn patch_check(
    repo: &Path,
    patch: &Path,
    sha: &str,
    direction: Direction,
) -> Containment {
    if !repo.is_dir() {
        return Containment::Unreachable(format!("{} is not a directory", repo.display()));
    }
    if !patch.is_file() {
        return Containment::Unreachable(format!("{} is not a file", patch.display()));
    }
    // Absolute, because git runs from the repository; not canonical: on
    // Windows that is a verbatim `\\?\C:\...` path git cannot open.
    let patch = match std::path::absolute(patch) {
        Ok(patch) => patch,
        Err(e) => {
            return Containment::Unreachable(format!(
                "could not make {} absolute: {e}",
                patch.display()
            ))
        }
    };
    let scratch = match ScratchIndex::create() {
        Ok(scratch) => scratch,
        Err(e) => {
            return Containment::Unreachable(format!("could not create a scratch index: {e}"))
        }
    };
    let index = scratch.index_file();
    let tree = format!("{sha}^{{tree}}");
    if let Err(reason) = run_git_step(
        "git read-tree",
        git(repo, &index).args(["read-tree", &tree]),
    ) {
        return Containment::Unreachable(reason);
    }
    let mut apply = git(repo, &index);
    apply.args(["apply", "--cached", "--check"]);
    if direction == Direction::Reverse {
        apply.arg("-R");
    }
    let applied = match apply.arg(&patch).output() {
        Ok(output) => output,
        Err(e) => return Containment::Unreachable(format!("git apply: could not run git: {e}")),
    };
    if applied.status.success() {
        return Containment::Contained;
    }
    // Exit 1 is "the patch does not apply": the commit rewrote it. Any other
    // status (128: a corrupt patch, a fatal error) says nothing about that.
    if applied.status.code() == Some(1) {
        return Containment::Rewritten;
    }
    let stderr = String::from_utf8_lossy(&applied.stderr);
    let stderr = stderr.trim();
    Containment::Unreachable(if stderr.is_empty() {
        format!("git apply: {}", applied.status)
    } else {
        format!("git apply: {stderr}")
    })
}

/// The run's latest `candidate_patch` evidence path, if one was recorded.
pub fn latest_candidate_patch(
    ledger: &Ledger,
    run: &RunId,
) -> Result<Option<PathBuf>, LedgerError> {
    Ok(ledger
        .evidence(run)?
        .into_iter()
        .rev()
        .find(|row| row.kind == EvidenceKind::CandidatePatch)
        .map(|row| PathBuf::from(row.path)))
}

/// Containment of a run's verified candidate in `sha`, computed from the
/// ledger's evidence and the repository the run ran in.
pub fn containment_of_run(
    ledger: &Ledger,
    run: &RunId,
    repo_path: &Path,
    sha: &str,
) -> Result<Containment, LedgerError> {
    let Some(repo) = reachable_repo(repo_path) else {
        return Ok(Containment::Unreachable(format!(
            "no repository at {}",
            repo_path.display()
        )));
    };
    let Some(patch) = latest_candidate_patch(ledger, run)? else {
        return Ok(Containment::Unreachable(
            "no candidate patch evidence recorded".to_string(),
        ));
    };
    Ok(candidate_contained(&repo, &patch, sha))
}

/// The preferred tip among `for-each-ref`'s `<refname> <sha>` lines:
/// origin's HEAD, then `main`, then `master`; `HEAD` when none exists.
pub(crate) fn default_tip(refs: &str) -> String {
    let named: Vec<(&str, &str)> = refs
        .lines()
        .filter_map(|line| line.trim().split_once(' '))
        .collect();
    [
        "refs/remotes/origin/HEAD",
        "refs/heads/main",
        "refs/heads/master",
    ]
    .iter()
    .find_map(|want| {
        named
            .iter()
            .find(|(name, _)| name == want)
            .map(|(_, sha)| sha.to_string())
    })
    .unwrap_or_else(|| "HEAD".to_string())
}

/// Record a contained salvage's candidate commit as the run's landing, with
/// the commit's own time, only when the commit is on the default tip's
/// first-parent chain (the chain revert detection walks). `Ok(false)` when
/// the repository or the commit cannot be read or is off that chain:
/// nothing is recorded and detection finds the landing itself later.
pub fn record_salvage_landing(
    ledger: &Ledger,
    run: &RunId,
    repo_path: &Path,
    sha: &str,
) -> Result<bool, LedgerError> {
    let Some(repo) = reachable_repo(repo_path) else {
        return Ok(false);
    };
    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git")
            .current_dir(&repo)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).to_string())
    };
    let Some(shown) = git(&["show", "-s", "--format=%H%n%cI", sha]) else {
        return Ok(false);
    };
    let mut lines = shown.lines();
    let (Some(full), Some(landed_at)) = (lines.next(), lines.next()) else {
        return Ok(false);
    };
    let Some(refs) = git(&[
        "for-each-ref",
        "--format=%(refname) %(objectname)",
        "refs/remotes/origin/HEAD",
        "refs/heads/main",
        "refs/heads/master",
    ]) else {
        return Ok(false);
    };
    let tip = default_tip(&refs);
    let Some(chain) = git(&["rev-list", "--first-parent", &tip]) else {
        return Ok(false);
    };
    if !chain.lines().any(|l| l == full) {
        return Ok(false);
    }
    let Ok(landed_at) = chrono::DateTime::parse_from_rfc3339(landed_at.trim()) else {
        return Ok(false);
    };
    ledger.record_landing(
        run,
        full,
        &landed_at.with_timezone(&chrono::Utc).to_rfc3339(),
    )?;
    Ok(true)
}

/// What one backfill pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackfillReport {
    /// Salvages whose containment was computed and recorded.
    pub recorded: usize,
    /// Salvages whose repository or patch is gone: they stay NULL and are
    /// excluded from labelling.
    pub unreachable: usize,
    /// How many salvages were unreachable for each reason.
    pub unreachable_reasons: std::collections::BTreeMap<String, usize>,
}

/// Compute `candidate_contained` once for every salvage that has none and
/// whose repository and patch are still on disk; record it. Unreachable
/// ones stay NULL and are counted.
pub fn backfill(ledger: &Ledger) -> Result<BackfillReport, LedgerError> {
    let mut report = BackfillReport::default();
    for (run, repo_path) in ledger.salvages_without_containment()? {
        let contained = match ledger.salvaged_candidate(&run)? {
            Some(sha) => containment_of_run(ledger, &run, &repo_path, &sha)?,
            None => Containment::Unreachable("no salvaged candidate commit recorded".to_string()),
        };
        match contained {
            Containment::Unreachable(reason) => {
                report.unreachable += 1;
                *report.unreachable_reasons.entry(reason).or_default() += 1;
            }
            known => {
                if let Some(answer) = known.answer() {
                    ledger.set_candidate_contained(&run, answer)?;
                }
                report.recorded += 1;
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A repository with a base commit, a candidate patch from base to a
    /// worker's change, and two later commits: one landing the worker's
    /// change as is (plus more), one landing a rewrite.
    struct Repo {
        dir: crate::test_support::TempDir,
        patch: PathBuf,
        contained_sha: String,
        rewritten_sha: String,
    }

    fn repo(name: &str) -> Repo {
        let dir = crate::test_support::temp_dir(name);
        let root: PathBuf = dir.to_path_buf();
        run_git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("a.txt"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
        run_git(&root, &["add", "."]);
        run_git(&root, &["commit", "-q", "-m", "base"]);
        let base = run_git(&root, &["rev-parse", "HEAD"]);
        // The worker's change.
        std::fs::write(root.join("a.txt"), "one\ntwo\nTHREE\nfour\nfive\n").unwrap();
        run_git(&root, &["commit", "-q", "-am", "candidate"]);
        let candidate = run_git(&root, &["rev-parse", "HEAD"]);
        let patch = root.join("candidate.patch.out");
        let diff = run_git(&root, &["diff", &base, &candidate]);
        std::fs::write(&patch, format!("{diff}\n")).unwrap();
        // Landed with the candidate intact, and more on top.
        std::fs::write(root.join("b.txt"), "extra\n").unwrap();
        run_git(&root, &["add", "b.txt"]);
        run_git(&root, &["commit", "-q", "-m", "landed"]);
        let contained_sha = run_git(&root, &["rev-parse", "HEAD"]);
        // A rewrite from the base: same line, different change.
        run_git(&root, &["checkout", "-q", "-b", "rewrite", &base]);
        std::fs::write(root.join("a.txt"), "one\ntwo\n3\nfour\nfive\n").unwrap();
        run_git(&root, &["commit", "-q", "-am", "rewrite"]);
        let rewritten_sha = run_git(&root, &["rev-parse", "HEAD"]);
        Repo {
            dir,
            patch,
            contained_sha,
            rewritten_sha,
        }
    }

    #[test]
    fn a_candidate_that_landed_intact_is_contained() {
        let fixture = repo("salvage-contained");
        assert_eq!(
            candidate_contained(&fixture.dir, &fixture.patch, &fixture.contained_sha),
            Containment::Contained
        );
        std::fs::remove_dir_all(&fixture.dir).ok();
    }

    #[test]
    fn a_rewritten_change_is_not_contained_and_the_real_index_is_untouched() {
        let fixture = repo("salvage-rewritten");
        let index = fixture.dir.join(".git/index");
        let before = std::fs::read(&index).unwrap();
        assert_eq!(
            candidate_contained(&fixture.dir, &fixture.patch, &fixture.rewritten_sha),
            Containment::Rewritten
        );
        assert_eq!(
            std::fs::read(&index).unwrap(),
            before,
            "the repository's own index is never used"
        );
        std::fs::remove_dir_all(&fixture.dir).ok();
    }

    #[test]
    fn an_unavailable_repository_patch_or_commit_is_unreachable_with_a_reason() {
        let fixture = repo("salvage-unknown");
        let missing = fixture.dir.join("no-such-dir");
        let reason = |c: Containment| match c {
            Containment::Unreachable(reason) => reason,
            other => panic!("expected unreachable, got {other:?}"),
        };
        assert!(reason(candidate_contained(
            &missing,
            &fixture.patch,
            &fixture.contained_sha
        ))
        .contains("not a directory"));
        assert!(reason(candidate_contained(
            &fixture.dir,
            &missing,
            &fixture.contained_sha
        ))
        .contains("not a file"));
        let bad_commit = reason(candidate_contained(
            &fixture.dir,
            &fixture.patch,
            &"0".repeat(40),
        ));
        assert!(
            bad_commit.starts_with("git read-tree: ") && bad_commit.len() > "git read-tree: ".len(),
            "git's stderr is in the reason: {bad_commit}"
        );
        std::fs::remove_dir_all(&fixture.dir).ok();
    }

    #[test]
    fn a_corrupt_patch_is_unreachable_with_gits_message() {
        let fixture = repo("salvage-corrupt");
        let corrupt = fixture.dir.join("corrupt.patch.out");
        std::fs::write(
            &corrupt,
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,x +1,x @@\n",
        )
        .unwrap();
        match candidate_contained(&fixture.dir, &corrupt, &fixture.contained_sha) {
            // git's own words, not the status fallback (`exit status: 128`).
            Containment::Unreachable(reason) => assert!(
                reason.starts_with("git apply: ") && reason.contains("corrupt patch"),
                "{reason}"
            ),
            other => panic!("expected unreachable, got {other:?}"),
        }
        std::fs::remove_dir_all(&fixture.dir).ok();
    }

    #[test]
    fn a_removed_worktree_resolves_to_its_primary_checkout() {
        let root = crate::test_support::temp_dir("salvage-wt");
        std::fs::create_dir_all(root.join("proj")).unwrap();
        let gone = root.join("proj-wt-task");
        assert_eq!(reachable_repo(&gone), Some(root.join("proj")));
        assert_eq!(reachable_repo(&root.join("other-wt-x")), None);
        assert_eq!(reachable_repo(&root.join("plain")), None);
        std::fs::remove_dir_all(&root).ok();
    }
}
