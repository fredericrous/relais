//! Detected reverts (SPEC §17 labels): a change relais accepted that landed
//! in git and was undone shortly after is a "no", and nobody has to run
//! `relais feedback` for it.
//!
//! The rule, for an approved run or a contained-salvaged run that ended in
//! the last [`WINDOW_DAYS`] days. The run's verified candidate patch
//! *landed* at the first first-parent commit after the run ended on which
//! it reverse-applies. It was *undone* at the first later first-parent
//! commit touching the patch's paths either (a) whose message says
//! `This reverts commit <landing>`, or (b) on which the patch no longer
//! reverse-applies but applies forward cleanly, none of its paths renamed
//! or copied since the landing (`git diff -M --name-status`) — a move, a
//! refactor, a partial undo or a later retirement is no revert. Either
//! way the undo must fall within [`REVERT_WITHIN_DAYS`] days of the
//! landing. Only `git log --first-parent <end>..<tip> -- <patch paths>`
//! is walked.
//!
//! What is pure (patch paths, log parsing, the message and rename tests) is
//! apart from what runs git ([`inspect`]) and from what writes the ledger
//! ([`detect`]). Every git call is counted against a per-run and a
//! per-build cap; a run that would exceed one is *skipped*, a repository
//! or patch that cannot be read is *unknown*, and neither writes a row.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeDelta, Utc};

use crate::learn::salvage::{self, Containment, Direction};
use crate::ledger::{Ledger, LedgerError, RevertCandidate};
use crate::outcome::{Outcome, OutcomeDetail, OutcomeKind, Strategy};

/// Runs that ended longer ago than this are not looked at.
pub const WINDOW_DAYS: i64 = 60;
/// How soon after the landing an undo still counts.
pub const REVERT_WITHIN_DAYS: i64 = 30;
/// Most git calls spent on one run.
// holds-until: skipped runs stay under a third of those scanned. The first
// pilot (2026-10-12) skipped 24 of 70, right at that line, so the cap is
// measured after release (plan 2026-10-11-orchestrator-decides,
// Verification record) and raised if the busiest histories keep hitting it.
pub const CALLS_PER_RUN: usize = 50;
/// Most git calls spent in one build.
pub const CALLS_PER_BUILD: usize = 2_000;
/// The `source` a row found here carries.
pub const SOURCE: &str = "detected";

/// What one detection pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RevertReport {
    /// Runs looked at.
    pub scanned: usize,
    /// Git calls made.
    pub git_calls: usize,
    /// Runs left unlooked-at because a call cap was reached.
    pub skipped: usize,
    /// Runs whose repository, patch or git could not answer.
    pub unknown: usize,
    /// Why, per distinct reason (first 200 characters), with its count:
    /// printed so an unknown is never unexplained.
    pub unknown_reasons: std::collections::BTreeMap<String, usize>,
    /// Detection did not run: `[learning] revert_detection_off`.
    pub off: bool,
    /// `reverted` rows written.
    pub recorded: usize,
    pub elapsed: Duration,
}

impl RevertReport {
    /// The report of a build in which the person turned detection off.
    pub fn off() -> Self {
        Self {
            off: true,
            ..Self::default()
        }
    }

    fn note_unknown(&mut self, reason: &str) {
        self.unknown += 1;
        let key: String = reason.chars().take(200).collect();
        *self.unknown_reasons.entry(key).or_default() += 1;
    }

    /// One line per distinct unknown reason, for stderr.
    pub fn unknown_lines(&self) -> Vec<String> {
        self.unknown_reasons
            .iter()
            .map(|(reason, n)| format!("  unknown x{n}: {reason}"))
            .collect()
    }

    /// The line a build prints.
    pub fn summary(&self) -> String {
        if self.off {
            return "revert detection: off ([learning] revert_detection_off in machine.toml)"
                .to_string();
        }
        format!(
            "revert detection: {} run(s) scanned, {} git call(s), {} skipped (call cap), \
             {} unknown (repository or patch unreachable), {} recorded, {:.1} s",
            self.scanned,
            self.git_calls,
            self.skipped,
            self.unknown,
            self.recorded,
            self.elapsed.as_secs_f64()
        )
    }
}

/// One first-parent commit of the log walk.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Commit {
    sha: String,
    at: DateTime<Utc>,
    message: String,
}

/// What looking at one run's history found.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Finding {
    /// Landed and undone in time.
    Reverted { landing: Commit, undo: Commit },
    /// Never landed, still standing, or changed in a way that is no undo.
    NoRevert,
    /// Git, the repository or the patch could not tell.
    Unknown(String),
    /// A call cap was reached first.
    Skipped,
}

/// Why a walk stopped before it could say.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Stop {
    /// Git, the repository or the patch could not tell.
    Unknown(String),
    /// A call cap was reached first.
    Skipped,
}

/// The paths a patch touches, from its `---`/`+++` and rename/copy
/// headers.
fn patch_paths(patch: &str) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    for line in patch.lines() {
        let named = line
            .strip_prefix("--- a/")
            .or_else(|| line.strip_prefix("+++ b/"))
            .or_else(|| line.strip_prefix("rename from "))
            .or_else(|| line.strip_prefix("rename to "))
            .or_else(|| line.strip_prefix("copy from "))
            .or_else(|| line.strip_prefix("copy to "));
        if let Some(path) = named {
            let path = path.split('\t').next().unwrap_or(path);
            if !path.is_empty() {
                paths.insert(path.to_string());
            }
        }
    }
    paths
}

const RECORD_START: char = '\u{1}';
const FIELD_SEP: char = '\u{2}';

/// The `git log --format=%x01%H%x02%ct%x02%B` output as commits, in the
/// order git printed them. A record that does not parse is dropped by the
/// caller's count, not guessed at.
fn parse_log(output: &str) -> Option<Vec<Commit>> {
    output
        .split(RECORD_START)
        .filter(|record| !record.trim().is_empty())
        .map(|record| {
            let mut fields = record.splitn(3, FIELD_SEP);
            let sha = fields.next()?.trim().to_string();
            let seconds: i64 = fields.next()?.trim().parse().ok()?;
            let message = fields.next()?.to_string();
            Some(Commit {
                sha,
                at: DateTime::from_timestamp(seconds, 0)?,
                message,
            })
        })
        .collect()
}

/// Whether `message` is git's revert message for `landing`.
fn reverts_commit(message: &str, landing: &str) -> bool {
    let needle = format!("This reverts commit {landing}");
    message.contains(&needle)
}

/// Whether a `--name-status` listing shows any of `paths` renamed or
/// copied (as source or destination).
fn moves_any(name_status: &str, paths: &BTreeSet<String>) -> bool {
    name_status.lines().any(|line| {
        let mut fields = line.split('\t');
        let status = fields.next().unwrap_or("");
        (status.starts_with('R') || status.starts_with('C'))
            && fields.any(|path| paths.contains(path))
    })
}

/// Git calls left for one run, and how many were spent.
struct Meter {
    left: usize,
    spent: usize,
}

impl Meter {
    fn new(cap: usize) -> Self {
        Self {
            left: cap,
            spent: 0,
        }
    }

    /// Take `calls` from the budget; `Err` when there are not that many.
    fn spend(&mut self, calls: usize) -> Result<(), Stop> {
        if self.left < calls {
            return Err(Stop::Skipped);
        }
        self.left -= calls;
        self.spent += calls;
        Ok(())
    }
}

/// Run git in `repo` and return its stdout.
fn git_text(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(["-c", "core.quotepath=off"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| {
            format!(
                "git {}: could not run git: {e}",
                args.first().unwrap_or(&"")
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "git {}: {}",
            args.first().unwrap_or(&""),
            stderr.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Spend `calls`, then ask git; a git failure is [`Finding::Unknown`].
fn metered_git(meter: &mut Meter, repo: &Path, args: &[&str]) -> Result<String, Stop> {
    meter.spend(1)?;
    git_text(repo, args).map_err(Stop::Unknown)
}

/// A patch check as a result: `Ok(true)` it passes, `Ok(false)` it does
/// not, `Err` the answer is unknown or the budget is gone. Costs two calls.
fn checked(
    meter: &mut Meter,
    repo: &Path,
    patch: &Path,
    sha: &str,
    direction: Direction,
) -> Result<bool, Stop> {
    meter.spend(2)?;
    match salvage::patch_check(repo, patch, sha, direction) {
        Containment::Contained => Ok(true),
        Containment::Rewritten => Ok(false),
        Containment::Unreachable(reason) => Err(Stop::Unknown(reason)),
    }
}

/// Walk one run's history and say whether its candidate landed and was
/// undone in time, starting at a recorded landing if any; also returning the landing
/// the walk itself found (never one it was given).
fn inspect_from(
    repo: &Path,
    patch: &Path,
    ended_at: DateTime<Utc>,
    recorded: Option<&Commit>,
    meter: &mut Meter,
) -> (Finding, Option<Commit>) {
    let walked = walk(repo, patch, ended_at, recorded, meter);
    let finding = match walked.result {
        Ok(finding) => finding,
        Err(Stop::Unknown(reason)) => Finding::Unknown(reason),
        Err(Stop::Skipped) => Finding::Skipped,
    };
    (finding, walked.found)
}

/// What a walk decided, and the landing it found on the way (even when it
/// then stopped), for the caller to record.
struct Walked {
    result: Result<Finding, Stop>,
    found: Option<Commit>,
}

fn walk(
    repo: &Path,
    patch: &Path,
    ended_at: DateTime<Utc>,
    recorded: Option<&Commit>,
    meter: &mut Meter,
) -> Walked {
    let mut found = None;
    let result = scan(repo, patch, ended_at, recorded, meter, &mut found);
    Walked { result, found }
}

fn scan(
    repo: &Path,
    patch: &Path,
    ended_at: DateTime<Utc>,
    recorded: Option<&Commit>,
    meter: &mut Meter,
    found: &mut Option<Commit>,
) -> Result<Finding, Stop> {
    let patch_text = std::fs::read_to_string(patch)
        .map_err(|e| Stop::Unknown(format!("{}: {e}", patch.display())))?;
    let paths = patch_paths(&patch_text);
    // An empty candidate (an inspection, a run that changed nothing) has
    // nothing that could land or be undone: not unknown, just no revert.
    // A non-empty patch with no path it can read (a binary change, a
    // malformed patch) stays unknown.
    if patch_text.trim().is_empty() {
        return Ok(Finding::NoRevert);
    }
    if paths.is_empty() {
        return Err(Stop::Unknown(format!("{} names no path", patch.display())));
    }
    // The default branch's tip: origin's HEAD, else main, else master,
    // else wherever HEAD is. `for-each-ref` prints in refname order, not
    // argument order, so the preference is applied here: a local `main`
    // that lags origin must never stand in for origin's tip.
    let refs = metered_git(
        meter,
        repo,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/remotes/origin/HEAD",
            "refs/heads/main",
            "refs/heads/master",
        ],
    )?;
    let tip = salvage::default_tip(&refs);
    let range = match recorded {
        // The landing is known: only what came after it can undo it.
        Some(landing) => format!("{}..{tip}", landing.sha),
        None => {
            let end = metered_git(
                meter,
                repo,
                &[
                    "rev-list",
                    "-1",
                    "--first-parent",
                    &format!("--before={}", ended_at.to_rfc3339()),
                    &tip,
                ],
            )?;
            match end.trim() {
                "" => tip.clone(),
                end => format!("{end}..{tip}"),
            }
        }
    };
    let mut log_args = vec![
        "log",
        "--first-parent",
        "--reverse",
        "--format=%x01%H%x02%ct%x02%B",
        &range,
        "--",
    ];
    log_args.extend(paths.iter().map(String::as_str));
    let log = metered_git(meter, repo, &log_args)?;
    let commits = parse_log(&log)
        .ok_or_else(|| Stop::Unknown("git log: output could not be read".to_string()))?;

    let mut landing: Option<Commit> = recorded.cloned();
    for commit in commits {
        let Some(landed) = &landing else {
            if checked(meter, repo, patch, &commit.sha, Direction::Reverse)? {
                *found = Some(commit.clone());
                landing = Some(commit);
            }
            continue;
        };
        if commit.at > landed.at + TimeDelta::days(REVERT_WITHIN_DAYS) {
            return Ok(Finding::NoRevert);
        }
        if reverts_commit(&commit.message, &landed.sha) {
            return Ok(Finding::Reverted {
                landing: landed.clone(),
                undo: commit,
            });
        }
        if checked(meter, repo, patch, &commit.sha, Direction::Reverse)? {
            continue;
        }
        // The first commit where it no longer reverse-applies decides.
        if !checked(meter, repo, patch, &commit.sha, Direction::Forward)? {
            return Ok(Finding::NoRevert);
        }
        let diff = metered_git(
            meter,
            repo,
            &["diff", "-M", "--name-status", &landed.sha, &commit.sha],
        )?;
        return Ok(if moves_any(&diff, &paths) {
            Finding::NoRevert
        } else {
            Finding::Reverted {
                landing: landed.clone(),
                undo: commit,
            }
        });
    }
    Ok(Finding::NoRevert)
}

/// The detected outcome for a run (its strategy read off the ledger, as
/// `relais feedback` does), or why it cannot be built.
fn detected_outcome(
    ledger: &Ledger,
    candidate: &RevertCandidate,
) -> Result<Result<Outcome, String>, LedgerError> {
    let Some((_contract, tier)) = ledger.run_contract_and_tier(&candidate.run)? else {
        return Ok(Err(format!(
            "run {} carries no recorded contract",
            candidate.run
        )));
    };
    let detail = OutcomeDetail {
        candidate_sha: None,
        strategy: Strategy {
            tier,
            models: ledger.models_used(&candidate.run)?,
            escalated: ledger.escalation_attempted(&candidate.run)?,
        },
        correction_magnitude: None,
        evidence: vec![],
        actor: "relais".to_string(),
        note: Some("a later commit undid this change".to_string()),
        source: Some(SOURCE.to_string()),
    };
    Ok(Outcome::new(OutcomeKind::Reverted, detail).map_err(|e| e.to_string()))
}

/// Whether revert detection runs: the person's `[learning]
/// revert_detection_off` turns it off (the first step of the undo).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevertDetection {
    On,
    Off,
}

impl RevertDetection {
    /// From the machine settings' `[learning]` table.
    pub fn from_settings(learning: &crate::policy::LearningSettings) -> Self {
        if learning.revert_detection_off {
            Self::Off
        } else {
            Self::On
        }
    }
}

/// Detect reverts, or report that the person turned detection off.
pub fn detect_if(
    detection: RevertDetection,
    ledger: &Ledger,
    now: DateTime<Utc>,
) -> Result<RevertReport, LedgerError> {
    match detection {
        RevertDetection::On => detect(ledger, now),
        RevertDetection::Off => Ok(RevertReport::off()),
    }
}

/// Look for detected reverts among the runs that ended in the last
/// [`WINDOW_DAYS`] days as of `now`, and record each.
pub fn detect(ledger: &Ledger, now: DateTime<Utc>) -> Result<RevertReport, LedgerError> {
    let started = Instant::now();
    let since = (now - TimeDelta::days(WINDOW_DAYS)).to_rfc3339();
    let mut report = RevertReport::default();
    let mut build_left = CALLS_PER_BUILD;
    for candidate in ledger.revert_candidates(&since)? {
        report.scanned += 1;
        let mut meter = Meter::new(CALLS_PER_RUN.min(build_left));
        let (finding, found) = inspect_candidate(ledger, &candidate, &mut meter)?;
        if let Some(landing) = &found {
            ledger.record_landing(&candidate.run, &landing.sha, &landing.at.to_rfc3339())?;
        }
        build_left -= meter.spent;
        report.git_calls += meter.spent;
        match finding {
            Finding::Skipped => report.skipped += 1,
            Finding::Unknown(reason) => report.note_unknown(&reason),
            Finding::NoRevert => {}
            Finding::Reverted { landing, undo } => {
                let outcome = match detected_outcome(ledger, &candidate)? {
                    Ok(outcome) => outcome,
                    Err(reason) => {
                        report.note_unknown(&reason);
                        continue;
                    }
                };
                if ledger.record_detected_revert(
                    &candidate.run,
                    &candidate.task,
                    &outcome,
                    &landing.sha,
                    &undo.sha,
                    undo.at,
                )? {
                    report.recorded += 1;
                }
            }
        }
    }
    report.elapsed = started.elapsed();
    Ok(report)
}

/// [`inspect`] a ledger candidate: its repository and patch come from the
/// ledger and the disk.
fn inspect_candidate(
    ledger: &Ledger,
    candidate: &RevertCandidate,
    meter: &mut Meter,
) -> Result<(Finding, Option<Commit>), LedgerError> {
    let unknown = |reason: String| Ok((Finding::Unknown(reason), None));
    let Some(repo) = salvage::reachable_repo(&candidate.repo_path) else {
        return unknown(format!(
            "no repository at {}",
            candidate.repo_path.display()
        ));
    };
    let Some(patch) = salvage::latest_candidate_patch(ledger, &candidate.run)? else {
        return unknown("no candidate patch evidence recorded".to_string());
    };
    let ended_at = DateTime::parse_from_rfc3339(&candidate.ended_at)
        .map_err(|e| LedgerError::Corrupt {
            what: format!("the end time of run {}", candidate.run),
            detail: format!("`{}` is not an RFC3339 timestamp: {e}", candidate.ended_at),
        })?
        .with_timezone(&Utc);
    // Absolute, because git runs from the repository.
    let patch = std::path::absolute(&patch).unwrap_or(patch);
    let recorded = match ledger.landing_of(&candidate.run)? {
        None => None,
        Some((sha, at)) => {
            let at = DateTime::parse_from_rfc3339(&at)
                .map_err(|e| LedgerError::Corrupt {
                    what: format!("the landing time of run {}", candidate.run),
                    detail: format!("`{at}` is not an RFC3339 timestamp: {e}"),
                })?
                .with_timezone(&Utc);
            Some(Commit {
                sha,
                at,
                message: String::new(),
            })
        }
    };
    Ok(inspect_from(
        &repo,
        &patch,
        ended_at,
        recorded.as_ref(),
        meter,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{RunId, TaskId};
    use crate::ledger::{EvidenceKind, Transition};
    use crate::lifecycle::{Reason, State, UsagePhase};
    use std::path::PathBuf;

    fn inspect(repo: &Path, patch: &Path, ended_at: DateTime<Utc>, meter: &mut Meter) -> Finding {
        inspect_from(repo, patch, ended_at, None, meter).0
    }

    const BASE: &str = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";

    fn run_git_at(dir: &Path, date: &str, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
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

    fn day(n: i64) -> String {
        format!("2026-09-{:02}T12:00:00+00:00", n)
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-10T00:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc)
    }

    /// A repository at base (day 1) with a candidate patch from base to a
    /// change of line three and a new file `new.txt`; nothing landed yet.
    struct World {
        dir: crate::test_support::TempDir,
        ledger: Ledger,
        patch: PathBuf,
        root: PathBuf,
    }

    fn world(name: &str, ended: &str) -> World {
        let dir = crate::test_support::temp_dir(name);
        let root: PathBuf = dir.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        run_git_at(&root, &day(1), &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("a.txt"), BASE).unwrap();
        run_git_at(&root, &day(1), &["add", "."]);
        run_git_at(&root, &day(1), &["commit", "-q", "-m", "base"]);
        // The candidate patch, made on a side branch and thrown away.
        run_git_at(&root, &day(2), &["checkout", "-q", "-b", "side"]);
        std::fs::write(root.join("a.txt"), BASE.replace("three", "THREE")).unwrap();
        std::fs::write(root.join("new.txt"), "fresh\n").unwrap();
        run_git_at(&root, &day(2), &["add", "."]);
        let diff = run_git_at(&root, &day(2), &["diff", "--cached", "--binary"]);
        let patch = dir.join("candidate.patch.out");
        std::fs::write(&patch, format!("{diff}\n")).unwrap();
        run_git_at(&root, &day(2), &["reset", "-q", "--hard"]);
        run_git_at(&root, &day(2), &["checkout", "-q", "main"]);
        run_git_at(&root, &day(2), &["clean", "-fdq"]);
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        let world = World {
            dir,
            ledger,
            patch,
            root,
        };
        world.accept("run-a", "task-a", ended);
        world
    }

    impl World {
        fn accept(&self, run: &str, task: &str, ended: &str) {
            let run_id = RunId::from_stored(run);
            self.ledger
                .insert_run(
                    &run_id,
                    self.root.to_str().unwrap(),
                    None,
                    &TaskId::from_stored(task),
                    "rk",
                )
                .expect("run");
            let contract = crate::contract::TaskContract::from_json_str(
                r#"{"schema_version":1,"kind":"change","objective":"Fix the escaping",
                    "base_ref":"HEAD","write_scope":["crates/**"],"acceptance":["parses"],
                    "verification_profile":"p"}"#,
            )
            .expect("contract");
            let json = serde_json::to_string(&contract.canonical_value()).unwrap();
            let revision = self
                .ledger
                .insert_contract_revision(&run_id, &contract.hash(), &json, "HEAD", None)
                .expect("revision");
            self.ledger
                .insert_attempt(&run_id, revision, 1, "implementation", UsagePhase::Initial)
                .expect("attempt");
            self.ledger
                .record_evidence(
                    &run_id,
                    None,
                    EvidenceKind::CandidatePatch,
                    &self.patch,
                    None,
                )
                .expect("evidence");
            self.ledger
                .record_transition(&Transition {
                    run_id,
                    attempt_id: None,
                    from_state: Some(State::Verifying),
                    to_state: State::Accepted,
                    reason: Reason::DecisionApproved.as_str().into(),
                    detail: None,
                    at: ended.into(),
                })
                .expect("accept");
        }

        fn commit(&self, date: &str, message: &str) -> String {
            run_git_at(&self.root, date, &["add", "-A"]);
            run_git_at(&self.root, date, &["commit", "-q", "-m", message]);
            run_git_at(&self.root, date, &["rev-parse", "HEAD"])
        }

        /// Land the candidate as is, on `date`.
        fn land(&self, date: &str) -> String {
            run_git_at(&self.root, date, &["apply", self.patch.to_str().unwrap()]);
            self.commit(date, "land the candidate")
        }

        fn reverted_rows(&self) -> Vec<crate::ledger::StoredOutcome> {
            self.ledger
                .outcomes_since("2000-01-01T00:00:00+00:00")
                .unwrap()
                .into_iter()
                .filter(|o| o.outcome.kind == OutcomeKind::Reverted)
                .collect()
        }
    }

    #[test]
    fn a_landed_candidate_reverted_within_30_days_is_one_detected_row() {
        let w = world("revert-landed-reverted", &day(3));
        let landing = w.land(&day(5));
        run_git_at(&w.root, &day(10), &["revert", "--no-edit", "HEAD"]);
        let report = detect(&w.ledger, now()).unwrap();
        assert_eq!((report.scanned, report.recorded), (1, 1), "{report:?}");
        let rows = w.reverted_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].outcome.detail.source.as_deref(), Some("detected"));
        let raw: String = rusqlite::Connection::open(w.dir.join("ledger.sqlite"))
            .unwrap()
            .query_row("SELECT detail_json FROM outcomes", [], |r| r.get(0))
            .unwrap();
        assert!(raw.contains(&landing), "{raw}");
        assert!(raw.contains("undo_sha"), "{raw}");
        // A second pass inserts nothing.
        let again = detect(&w.ledger, now()).unwrap();
        assert_eq!(again.recorded, 0);
        assert_eq!(w.reverted_rows().len(), 1);
    }

    #[test]
    fn the_first_pass_records_the_landing_and_the_second_skips_the_search() {
        let w = world("revert-landing-recorded", &day(3));
        let run = RunId::from_stored("run-a");
        assert_eq!(w.ledger.landing_of(&run).unwrap(), None);
        let landing = w.land(&day(5));
        let first = detect(&w.ledger, now()).unwrap();
        let (sha, _at) = w.ledger.landing_of(&run).unwrap().expect("recorded");
        assert_eq!(sha, landing);
        let second = detect(&w.ledger, now()).unwrap();
        assert!(first.git_calls > 2, "{first:?}");
        // for-each-ref and log: no rev-list, no reverse-apply check.
        assert_eq!(second.git_calls, 2, "{second:?}");
        assert_eq!(w.ledger.landing_of(&run).unwrap().unwrap().0, landing);
    }

    #[test]
    fn a_revert_after_a_recorded_landing_is_still_detected() {
        let w = world("revert-after-recorded", &day(3));
        w.land(&day(5));
        let first = detect(&w.ledger, now()).unwrap();
        assert_eq!(first.recorded, 0);
        run_git_at(&w.root, &day(10), &["revert", "--no-edit", "HEAD"]);
        let second = detect(&w.ledger, now()).unwrap();
        assert_eq!(second.recorded, 1, "{second:?}");
        assert_eq!(w.reverted_rows().len(), 1);
    }

    #[test]
    fn a_contained_salvage_landing_is_recorded_with_the_commit_time() {
        let w = world("revert-salvage-landing", &day(3));
        let landing = w.land(&day(5));
        let run = RunId::from_stored("run-a");
        assert!(salvage::record_salvage_landing(&w.ledger, &run, &w.root, &landing).unwrap());
        let (sha, at) = w.ledger.landing_of(&run).unwrap().expect("recorded");
        assert_eq!(sha, landing);
        assert!(at.starts_with("2026-09-05T12:00:00"), "{at}");
    }

    #[test]
    fn a_salvage_candidate_off_the_default_tip_chain_records_no_landing() {
        let w = world("revert-salvage-off-chain", &day(3));
        let run = RunId::from_stored("run-a");
        w.land(&day(5));
        run_git_at(&w.root, &day(6), &["checkout", "-q", "-b", "other"]);
        std::fs::write(w.root.join("b.txt"), "x\n").unwrap();
        let off = w.commit(&day(6), "a commit off main");
        run_git_at(&w.root, &day(6), &["checkout", "-q", "main"]);
        assert!(!salvage::record_salvage_landing(&w.ledger, &run, &w.root, &off).unwrap());
        assert_eq!(w.ledger.landing_of(&run).unwrap(), None);
    }

    #[test]
    fn an_unparsable_recorded_landing_time_is_corrupt() {
        let w = world("revert-bad-landed-at", &day(3));
        let landing = w.land(&day(5));
        w.ledger
            .record_landing(&RunId::from_stored("run-a"), &landing, "not a time")
            .unwrap();
        let err = detect(&w.ledger, now()).unwrap_err();
        assert!(matches!(err, LedgerError::Corrupt { .. }), "{err:?}");
    }

    #[test]
    fn an_undo_by_deleting_the_change_without_a_revert_message_is_detected() {
        let w = world("revert-by-hand", &day(3));
        w.land(&day(5));
        run_git_at(
            &w.root,
            &day(8),
            &["apply", "-R", w.patch.to_str().unwrap()],
        );
        w.commit(&day(8), "take it out again");
        let report = detect(&w.ledger, now()).unwrap();
        assert_eq!(report.recorded, 1, "{report:?}");
    }

    #[test]
    fn a_landed_candidate_edited_around_is_not_a_revert() {
        let w = world("revert-edited-around", &day(3));
        w.land(&day(5));
        std::fs::write(
            w.root.join("a.txt"),
            BASE.replace("three", "THREE").replace("nine", "9"),
        )
        .unwrap();
        w.commit(&day(8), "edit elsewhere in the file");
        let report = detect(&w.ledger, now()).unwrap();
        assert_eq!((report.scanned, report.recorded), (1, 0), "{report:?}");
        assert!(w.reverted_rows().is_empty());
    }

    #[test]
    fn a_candidate_that_never_landed_is_not_a_revert() {
        let w = world("revert-never-landed", &day(3));
        std::fs::write(w.root.join("a.txt"), BASE.replace("three", "3")).unwrap();
        w.commit(&day(6), "a different change to the same file");
        let report = detect(&w.ledger, now()).unwrap();
        assert_eq!((report.scanned, report.recorded, report.unknown), (1, 0, 0));
        assert!(w.reverted_rows().is_empty());
    }

    #[test]
    fn an_added_file_later_renamed_is_not_a_revert() {
        let w = world("revert-renamed", &day(3));
        w.land(&day(5));
        // The commit undoes the edit of a.txt and moves the added file: the
        // patch applies forward again, but its added file was renamed.
        std::fs::write(w.root.join("a.txt"), BASE).unwrap();
        run_git_at(&w.root, &day(8), &["mv", "new.txt", "moved.txt"]);
        w.commit(&day(8), "move the new file, drop the edit");
        let report = detect(&w.ledger, now()).unwrap();
        assert_eq!(report.recorded, 0, "{report:?}");
        assert!(w.reverted_rows().is_empty());
    }

    #[test]
    fn a_removal_40_days_after_landing_is_not_a_revert() {
        let w = world("revert-too-late", &day(3));
        w.land(&day(5));
        run_git_at(
            &w.root,
            "2026-10-15T12:00:00+00:00",
            &["apply", "-R", w.patch.to_str().unwrap()],
        );
        w.commit("2026-10-15T12:00:00+00:00", "retire it");
        let later = DateTime::parse_from_rfc3339("2026-10-20T00:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        let report = detect(&w.ledger, later).unwrap();
        assert_eq!(report.recorded, 0, "{report:?}");
    }

    #[test]
    fn a_revert_message_40_days_after_landing_is_not_a_revert() {
        let w = world("revert-message-too-late", &day(3));
        let landing = w.land(&day(5));
        std::fs::write(w.root.join("a.txt"), BASE).unwrap();
        w.commit(
            "2026-10-15T12:00:00+00:00",
            &format!("Revert it\n\nThis reverts commit {landing}."),
        );
        let later = DateTime::parse_from_rfc3339("2026-10-20T00:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        let report = detect(&w.ledger, later).unwrap();
        assert_eq!(report.recorded, 0, "{report:?}");
    }

    #[test]
    fn an_accepted_unchanged_after_the_undo_stands_one_before_does_not() {
        // After the undo (day 10): an outcome stamped day 12 stands.
        let w = world("revert-vouched-after", &day(3));
        w.land(&day(5));
        run_git_at(&w.root, &day(10), &["revert", "--no-edit", "HEAD"]);
        let task = TaskId::from_stored("task-a");
        record(&w, &task, OutcomeKind::AcceptedUnchanged, &day(12));
        assert_eq!(detect(&w.ledger, now()).unwrap().recorded, 0);
        assert!(w.reverted_rows().is_empty());

        // Before the undo: an outcome stamped day 8 does not block it.
        let w = world("revert-vouched-before", &day(3));
        w.land(&day(5));
        let task = TaskId::from_stored("task-a");
        record(&w, &task, OutcomeKind::AcceptedUnchanged, &day(8));
        run_git_at(&w.root, &day(10), &["revert", "--no-edit", "HEAD"]);
        let report = detect(&w.ledger, now()).unwrap();
        assert_eq!(report.recorded, 1, "{report:?}");
        assert_eq!(w.reverted_rows().len(), 1);
    }

    #[test]
    fn an_orchestrator_revert_after_the_undo_coexists_and_two_builds_add_one_row() {
        let w = world("revert-both-sources", &day(3));
        w.land(&day(5));
        run_git_at(&w.root, &day(10), &["revert", "--no-edit", "HEAD"]);
        let task = TaskId::from_stored("task-a");
        // The orchestrator reported it on day 11, after the undo.
        record(&w, &task, OutcomeKind::Reverted, &day(11));
        rusqlite::Connection::open(w.dir.join("ledger.sqlite"))
            .unwrap()
            .execute(
                "UPDATE outcomes SET detail_json = json_set(detail_json, '$.source', 'orchestrator')",
                [],
            )
            .unwrap();
        assert_eq!(detect(&w.ledger, now()).unwrap().recorded, 1);
        assert_eq!(detect(&w.ledger, now()).unwrap().recorded, 0);
        let mut sources: Vec<Option<String>> = w
            .reverted_rows()
            .into_iter()
            .map(|row| row.outcome.detail.source)
            .collect();
        sources.sort();
        assert_eq!(
            sources,
            vec![
                Some("detected".to_string()),
                Some("orchestrator".to_string())
            ]
        );
    }

    #[test]
    fn the_tip_prefers_origin_head_over_a_lagging_local_main() {
        // `for-each-ref` prints in refname order: heads before remotes.
        let refs = "refs/heads/main aaa\nrefs/heads/master bbb\nrefs/remotes/origin/HEAD ccc\n";
        assert_eq!(salvage::default_tip(refs), "ccc");
        assert_eq!(
            salvage::default_tip("refs/heads/master bbb\nrefs/heads/main aaa\n"),
            "aaa"
        );
        assert_eq!(salvage::default_tip("refs/heads/master bbb\n"), "bbb");
        assert_eq!(salvage::default_tip(""), "HEAD");
    }

    fn record(w: &World, task: &TaskId, kind: OutcomeKind, at: &str) {
        let outcome = Outcome::new(
            kind,
            OutcomeDetail {
                candidate_sha: None,
                strategy: Strategy {
                    tier: crate::policy::Tier::Implementation,
                    models: vec![],
                    escalated: false,
                },
                correction_magnitude: None,
                evidence: vec![],
                actor: "a person".into(),
                note: None,
                source: None,
            },
        )
        .unwrap();
        w.ledger
            .record_outcome(&RunId::from_stored("run-a"), task, &outcome)
            .unwrap();
        rusqlite::Connection::open(w.dir.join("ledger.sqlite"))
            .unwrap()
            .execute("UPDATE outcomes SET at = ?1", [at])
            .unwrap();
    }

    #[test]
    fn an_empty_candidate_is_no_revert_not_unknown() {
        let w = world("revert-empty-patch", &day(3));
        std::fs::write(&w.patch, "").unwrap();
        let report = detect(&w.ledger, now()).unwrap();
        assert_eq!((report.unknown, report.recorded), (0, 0), "{report:?}");
    }

    #[test]
    fn a_non_empty_patch_with_no_readable_path_stays_unknown() {
        let w = world("revert-binary-patch", &day(3));
        std::fs::write(
            &w.patch,
            "diff --git a/logo.png b/logo.png\nindex 1111111..2222222 100644\nBinary files a/logo.png and b/logo.png differ\n",
        )
        .unwrap();
        let report = detect(&w.ledger, now()).unwrap();
        assert_eq!((report.unknown, report.recorded), (1, 0), "{report:?}");
    }

    #[test]
    fn an_unreachable_repository_is_unknown_and_writes_nothing() {
        let w = world("revert-unreachable", &day(3));
        w.land(&day(5));
        run_git_at(&w.root, &day(10), &["revert", "--no-edit", "HEAD"]);
        std::fs::rename(&w.root, w.dir.join("elsewhere")).unwrap();
        let report = detect(&w.ledger, now()).unwrap();
        assert_eq!((report.unknown, report.recorded), (1, 0), "{report:?}");
        assert!(w.reverted_rows().is_empty());
    }

    #[test]
    fn the_per_run_call_cap_skips_the_rest_and_counts_it() {
        let w = world("revert-cap", &day(3));
        w.land(&day(5));
        let ended = DateTime::parse_from_rfc3339(&day(3))
            .unwrap()
            .with_timezone(&Utc);
        // Three calls find the commits; the first patch check needs two more.
        let mut meter = Meter::new(4);
        assert_eq!(
            inspect(&w.root, &w.patch, ended, &mut meter),
            Finding::Skipped
        );
        assert_eq!(meter.spent, 3);
        // A build whose total is spent skips every run.
        let mut meter = Meter::new(0);
        assert_eq!(
            inspect(&w.root, &w.patch, ended, &mut meter),
            Finding::Skipped
        );
        assert_eq!(meter.spent, 0);
    }

    #[test]
    fn pure_readers_of_patches_logs_and_diffs() {
        let patch = "diff --git a/x.txt b/y.txt\nrename from x.txt\nrename to y.txt\n\
                     --- a/x.txt\n+++ b/y.txt\n--- /dev/null\n+++ b/z dir/z.txt\t\n";
        let paths = patch_paths(patch);
        assert!(["x.txt", "y.txt", "z dir/z.txt"]
            .iter()
            .all(|p| paths.contains(*p)));
        assert!(!paths.contains("/dev/null"));
        assert!(reverts_commit(
            "Revert\n\nThis reverts commit abc123.\n",
            "abc123"
        ));
        assert!(!reverts_commit("This reverts commit abc1234", "abc999"));
        assert!(moves_any("R100\tx.txt\ty.txt\nM\tq.txt", &paths));
        assert!(!moves_any("M\tx.txt\nA\ty.txt", &paths));
        assert!(!moves_any("R100\tother\tnamed", &paths));
        let log = "\u{1}aaa\u{2}1700000000\u{2}msg one\n\u{1}bbb\u{2}1700000100\u{2}msg two\n";
        let commits = parse_log(log).unwrap();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[1].sha, "bbb");
        assert!(parse_log("\u{1}aaa\u{2}notanumber\u{2}x").is_none());
    }

    #[test]
    fn the_summary_names_scanned_calls_skipped_and_seconds() {
        let line = RevertReport {
            scanned: 3,
            git_calls: 12,
            skipped: 1,
            ..RevertReport::default()
        }
        .summary();
        assert!(line.contains("3 run(s) scanned") && line.contains("12 git call(s)"));
        assert!(line.contains("1 skipped") && line.ends_with(" s"), "{line}");
    }
}
