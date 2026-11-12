//! `relais native gate-stamp --cwd <repo>`: what amont's pre-push gate
//! stamped on the repository's current TREE (docs/router-protocol.md,
//! "The amont gate stamp"; amont `docs/gate-evidence.md`). Read-only. The
//! parser is pure; `read` is the only function that runs git.

use std::path::Path;
use std::process::Command;

use serde::Serialize;

/// One name the gate's stamps are written under: the notes ref, and the
/// version header a note under that ref must start with. A note is read
/// whole under the name of the ref it sits in (ADR-0068).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoteName {
    pub notes_ref: &'static str,
    pub version: &'static str,
}

/// greengate's name, read first.
pub const GREEN: NoteName = NoteName {
    notes_ref: "refs/notes/greengate-gate",
    version: "greengate-gate-v1",
};

/// amont's name, read only when [`GREEN`] has no note for the tree.
/// LEGACY: removed at the census after the 30-day window (ADR-0068,
/// decisions plan 2026-11-12-greengate-signs-notes-under-green-names,
/// Phase 5), with its place in [`READ_ORDER`].
pub const LEGACY: NoteName = NoteName {
    notes_ref: "refs/notes/amont-gate",
    version: "amont-gate-v1",
};

/// The refs [`read`] tries, in order; the first with a note decides.
pub const READ_ORDER: [NoteName; 2] = [GREEN, LEGACY];

/// One gate run: when (seconds since the epoch) and which gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GateRun {
    pub gate: String,
    pub epoch: u64,
}

/// What `gate-stamp` prints. No stamp is `{"stamped":false}` and nothing
/// else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GateStamp {
    pub stamped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passed: Option<Vec<GateRun>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<Vec<GateRun>>,
}

impl GateStamp {
    pub fn unstamped() -> Self {
        GateStamp {
            stamped: false,
            tree: None,
            passed: None,
            failed: None,
        }
    }
}

/// A `run <epoch> <gate> pass|fail` line; anything else is skipped.
fn parse_run(line: &str) -> Option<(GateRun, bool)> {
    let mut words = line.split_whitespace();
    if words.next()? != "run" {
        return None;
    }
    let epoch: u64 = words.next()?.parse().ok()?;
    let mut rest: Vec<&str> = words.collect();
    let verdict = rest.pop()?;
    let passed = match verdict {
        "pass" => true,
        "fail" => false,
        _ => return None,
    };
    if rest.is_empty() {
        return None;
    }
    Some((
        GateRun {
            gate: rest.join(" "),
            epoch,
        },
        passed,
    ))
}

/// A note's text, read from `name`'s ref, as the stamp of `tree`. A first
/// line that does not start with `name`'s version is no evidence at all,
/// even when it is the other name's version.
pub fn parse_note(name: NoteName, tree: &str, text: &str) -> GateStamp {
    let mut lines = text.lines();
    if !lines.next().is_some_and(|l| l.starts_with(name.version)) {
        return GateStamp::unstamped();
    }
    let (mut passed, mut failed) = (Vec::new(), Vec::new());
    for (run, is_pass) in lines.filter_map(parse_run) {
        if is_pass {
            passed.push(run);
        } else {
            failed.push(run);
        }
    }
    GateStamp {
        stamped: true,
        tree: Some(tree.to_string()),
        passed: Some(passed),
        failed: Some(failed),
    }
}

/// Why the stamp could not be read: git failed, or the directory is not
/// one. Never "there is no stamp".
#[derive(Debug)]
pub struct GateError(pub String);

impl std::fmt::Display for GateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for GateError {}

fn git(repo: &Path, args: &[&str]) -> Result<std::process::Output, GateError> {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .map_err(|e| GateError(format!("git could not run: {e}")))
}

/// The stamp on `HEAD^{tree}` of the repository at `repo`: the note under
/// the first ref of [`READ_ORDER`] that has one. A note whose header does
/// not match its ref reads as unstamped; the next ref is not tried.
pub fn read(repo: &Path) -> Result<GateStamp, GateError> {
    if !repo.is_dir() {
        return Err(GateError(format!("{} is not a directory", repo.display())));
    }
    let tree = git(repo, &["rev-parse", "--verify", "HEAD^{tree}"])?;
    if !tree.status.success() {
        return Err(GateError(format!(
            "git rev-parse HEAD^{{tree}}: {}",
            String::from_utf8_lossy(&tree.stderr).trim()
        )));
    }
    let tree = String::from_utf8_lossy(&tree.stdout).trim().to_string();
    for name in READ_ORDER {
        let note = git(
            repo,
            &[
                "notes",
                &format!("--ref={}", name.notes_ref),
                "show",
                "HEAD^{tree}",
            ],
        )?;
        if note.status.success() {
            let text = String::from_utf8_lossy(&note.stdout);
            return Ok(parse_note(name, &tree, &text));
        }
        let stderr = String::from_utf8_lossy(&note.stderr);
        if !stderr.contains("no note found") {
            return Err(GateError(format!(
                "git notes --ref={} show: {}",
                name.notes_ref,
                stderr.trim()
            )));
        }
    }
    Ok(GateStamp::unstamped())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(repo: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A one-commit repository with `note` on its tree under amont's ref.
    fn repo_with_note(note: Option<&str>) -> (crate::test_support::TempDir, String) {
        let notes: Vec<(&str, &str)> = note.map(|n| (LEGACY.notes_ref, n)).into_iter().collect();
        repo_with_notes(&notes)
    }

    /// A one-commit repository with each `(ref, note)` on its tree.
    fn repo_with_notes(notes: &[(&str, &str)]) -> (crate::test_support::TempDir, String) {
        let dir = crate::test_support::temp_dir("gate-stamp");
        run_git(&dir, &["init", "-q", "--template="]);
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        run_git(&dir, &["add", "a.txt"]);
        run_git(&dir, &["commit", "-q", "-m", "feat: one"]);
        for (notes_ref, note) in notes {
            run_git(
                &dir,
                &[
                    "notes",
                    &format!("--ref={notes_ref}"),
                    "add",
                    "-m",
                    note,
                    "HEAD^{tree}",
                ],
            );
        }
        let tree = git(&dir, &["rev-parse", "HEAD^{tree}"]).unwrap();
        let tree = String::from_utf8_lossy(&tree.stdout).trim().to_string();
        (dir, tree)
    }

    #[test]
    fn a_stamp_with_passes_and_fails_is_read_from_the_trees_note() {
        let (dir, tree) = repo_with_note(Some(
            "amont-gate-v1\nrun 1760000000 pre-push pass\nrun 1760000100 lint fail\n\
             run 1760000200 pre-push pass\nnoise\nrun x y pass",
        ));
        let stamp = read(&dir).unwrap();
        assert!(stamp.stamped);
        assert_eq!(stamp.tree.as_deref(), Some(tree.as_str()));
        let runs = |v: Option<Vec<GateRun>>| {
            v.unwrap()
                .into_iter()
                .map(|r| (r.gate, r.epoch))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            runs(stamp.passed),
            vec![
                ("pre-push".to_string(), 1_760_000_000),
                ("pre-push".to_string(), 1_760_000_200)
            ]
        );
        assert_eq!(
            runs(stamp.failed),
            vec![("lint".to_string(), 1_760_000_100)]
        );
    }

    #[test]
    fn a_failing_run_is_listed_as_failed() {
        let (dir, _) = repo_with_note(Some("amont-gate-v1\nrun 1760000000 pre-push fail"));
        let stamp = read(&dir).unwrap();
        assert!(stamp.stamped);
        assert!(stamp.passed.unwrap().is_empty());
        assert_eq!(stamp.failed.unwrap()[0].gate, "pre-push");
    }

    #[test]
    fn no_note_is_unstamped_not_an_error() {
        let (dir, _) = repo_with_note(None);
        let stamp = read(&dir).unwrap();
        assert_eq!(stamp, GateStamp::unstamped());
        assert_eq!(
            serde_json::to_string(&stamp).unwrap(),
            r#"{"stamped":false}"#
        );
    }

    #[test]
    fn another_version_is_no_evidence() {
        let (dir, _) = repo_with_note(Some("amont-gate-v2\nrun 1760000000 pre-push pass"));
        assert_eq!(read(&dir).unwrap(), GateStamp::unstamped());
    }

    fn passed_gates(stamp: GateStamp) -> Vec<String> {
        stamp.passed.unwrap().into_iter().map(|r| r.gate).collect()
    }

    #[test]
    fn a_stamp_is_read_from_greengates_ref() {
        let (dir, tree) = repo_with_notes(&[(
            GREEN.notes_ref,
            "greengate-gate-v1\nrun 1760000000 pre-push pass",
        )]);
        let stamp = read(&dir).unwrap();
        assert!(stamp.stamped);
        assert_eq!(stamp.tree.as_deref(), Some(tree.as_str()));
        assert_eq!(passed_gates(stamp), vec!["pre-push"]);
    }

    #[test]
    fn a_stamp_is_read_from_amonts_ref_when_greengates_has_none() {
        let (dir, _) = repo_with_notes(&[(
            LEGACY.notes_ref,
            "amont-gate-v1\nrun 1760000000 legacy pass",
        )]);
        assert_eq!(passed_gates(read(&dir).unwrap()), vec!["legacy"]);
    }

    #[test]
    fn amonts_ref_is_read_when_greengates_ref_exists_without_this_trees_note() {
        let (dir, _) = repo_with_notes(&[(
            LEGACY.notes_ref,
            "amont-gate-v1\nrun 1760000000 legacy pass",
        )]);
        // The green ref exists, with a note on another object only.
        run_git(
            &dir,
            &[
                "notes",
                &format!("--ref={}", GREEN.notes_ref),
                "add",
                "-m",
                "greengate-gate-v1\nrun 1760000100 green pass",
                "HEAD",
            ],
        );
        assert_eq!(passed_gates(read(&dir).unwrap()), vec!["legacy"]);
    }

    #[test]
    fn with_notes_under_both_refs_greengates_note_is_read() {
        let (dir, _) = repo_with_notes(&[
            (
                LEGACY.notes_ref,
                "amont-gate-v1\nrun 1760000000 legacy pass",
            ),
            (
                GREEN.notes_ref,
                "greengate-gate-v1\nrun 1760000100 green pass",
            ),
        ]);
        assert_eq!(passed_gates(read(&dir).unwrap()), vec!["green"]);
    }

    #[test]
    fn a_greengate_header_under_amonts_ref_is_unstamped() {
        let (dir, _) = repo_with_notes(&[(
            LEGACY.notes_ref,
            "greengate-gate-v1\nrun 1760000000 pre-push pass",
        )]);
        assert_eq!(read(&dir).unwrap(), GateStamp::unstamped());
    }

    #[test]
    fn an_amont_header_under_greengates_ref_is_unstamped_not_reread() {
        // The old ref holds a valid note too: a mismatched note under the
        // green ref decides, and the old ref is not tried.
        let (dir, _) = repo_with_notes(&[
            (
                GREEN.notes_ref,
                "amont-gate-v1\nrun 1760000000 pre-push pass",
            ),
            (
                LEGACY.notes_ref,
                "amont-gate-v1\nrun 1760000000 legacy pass",
            ),
        ]);
        assert_eq!(read(&dir).unwrap(), GateStamp::unstamped());
        let (dir, _) = repo_with_notes(&[(
            GREEN.notes_ref,
            "amont-gate-v1\nrun 1760000000 pre-push pass",
        )]);
        assert_eq!(read(&dir).unwrap(), GateStamp::unstamped());
    }

    #[test]
    fn a_directory_that_is_not_a_repository_is_an_error() {
        let dir = crate::test_support::temp_dir("gate-stamp-none");
        assert!(read(&dir).is_err());
        assert!(read(&dir.join("missing")).is_err());
    }

    #[test]
    fn the_document_has_the_contracts_keys() {
        let stamp = parse_note(LEGACY, "abc", "amont-gate-v1\nrun 5 g pass\nrun 6 g fail");
        let value = serde_json::to_value(&stamp).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "stamped": true, "tree": "abc",
                "passed": [{"gate": "g", "epoch": 5}],
                "failed": [{"gate": "g", "epoch": 6}],
            })
        );
    }
}
