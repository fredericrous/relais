//! Verification and acceptance (SPEC §10, §18).
//!
//! Verification runs after all candidate-writing descendants have stopped
//! or relinquished their write leases, against an immutable copy of the
//! candidate — a verification worktree checked out at the candidate SHA,
//! so no concurrent worker can modify what is being verified. Record
//! identity, base SHA, contract hash, policy hash, commands, exit
//! statuses, timeouts and log hashes. Preflight captures baseline
//! failures; existing failures are never silently waived. A skipped,
//! inert, unavailable or untrusted required check is a gap, not a pass.
//! An accepted receipt is bound to one candidate and does not authorize
//! merge or survive edits.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::ids::{canonical_json_hash, sha256_hex};
use crate::money::{CostCompleteness, MicroUsd};
use crate::policy::{CommandSpec, DependencyMode, Integrations, VerificationProfile};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckOutcome {
    pub label: String,
    pub argv: Vec<String>,
    pub exit: Option<i32>,
    pub timed_out: bool,
    pub log_path: String,
    pub log_sha256: String,
}

impl CheckOutcome {
    pub fn failed(&self) -> bool {
        self.timed_out || self.exit != Some(0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerificationReport {
    pub candidate_sha: String,
    pub base_sha: String,
    pub contract_hash: String,
    pub policy_hash: String,
    pub checks: Vec<CheckOutcome>,
    /// Required checks that were skipped, inert, unavailable or
    /// untrusted: gaps, never passes (SPEC §10).
    pub gaps: Vec<String>,
    /// Failures that also failed at the base: visible, not waived
    /// automatically (SPEC §10).
    pub baseline_failures: Vec<String>,
    pub amont_bypasses: Vec<String>,
    pub amont_downgrades: Vec<String>,
}
impl VerificationReport {
    pub fn accepted(&self) -> bool {
        self.gaps.is_empty() && self.checks.iter().all(|check| !check.failed())
    }

    pub fn new_failures(&self) -> Vec<String> {
        self.checks
            .iter()
            .filter(|check| check.failed())
            .filter(|check| !self.baseline_failures.contains(&check.label))
            .map(|check| check.label.clone())
            .collect()
    }
}

/// Run one command against a directory, capturing the merged log to a file
/// under `logs_dir`, hashing it, and enforcing a wall timeout. The log is
/// evidence: it exists whether the check passed or failed.
pub fn run_command(
    dir: &Path,
    spec: &CommandSpec,
    logs_dir: &Path,
    label: &str,
) -> std::io::Result<CheckOutcome> {
    std::fs::create_dir_all(logs_dir)?;
    let log_path = logs_dir.join(format!("{label}.log"));
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&log_path)?;
    let mut command = Command::new(&spec.argv[0]);
    command
        .args(&spec.argv[1..])
        .current_dir(dir)
        .stdout(log_file.try_clone()?)
        .stderr(log_file);
    let started = Instant::now();
    let mut child = command.spawn()?;
    let timeout = Duration::from_secs(spec.timeout_seconds.max(1));
    let timed_out = loop {
        if child.try_wait()?.is_some() {
            break false;
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            break true;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let status = child.wait()?;
    let log_bytes = std::fs::read(&log_path)?;
    Ok(CheckOutcome {
        label: label.to_string(),
        argv: spec.argv.clone(),
        exit: status.code(),
        timed_out,
        log_path: log_path.to_string_lossy().into_owned(),
        log_sha256: sha256_hex(&log_bytes),
    })
}

/// Run a full profile against a directory (used for both the candidate
/// worktree and the baseline preflight at the base SHA).
pub fn run_profile(
    dir: &Path,
    profile: &VerificationProfile,
    logs_dir: &Path,
    prefix: &str,
) -> std::io::Result<Vec<CheckOutcome>> {
    let mut outcomes = Vec::new();
    for (index, command) in profile.commands.iter().enumerate() {
        let label = format!("{prefix}-cmd{index}");
        outcomes.push(run_command(dir, command, logs_dir, &label)?);
    }
    Ok(outcomes)
}

/// The effective check inventory from `amont list --json`
/// (envelope `amont-list-v1`). Parsed defensively: the envelope's `format`
/// field is asserted before reading, and unknown check statuses are
/// treated as gaps rather than passes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AmontInventory {
    pub checks: Vec<AmontCheck>,
    pub bypasses: Vec<String>,
    pub downgrades: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AmontCheck {
    pub id: String,
    pub stage: Option<String>,
    pub source: Option<String>,
    pub effective_severity: Option<String>,
    pub status: Option<String>,
    pub reason: Option<String>,
}

/// Statuses that mean the check is genuinely in force. Docs say
/// `ready|inert|skipped|unavailable`; code has emitted `runs` — anything
/// not in the active set is a gap, which is the conservative direction
/// (SPEC §10, and the amont docs/code status mismatch).
const ACTIVE_STATUSES: &[&str] = &["ready", "runs", "active"];

pub fn parse_amont_list(stdout: &str) -> Option<AmontInventory> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    match value.get("format").and_then(|f| f.as_str()) {
        Some("amont-list-v1") => {}
        other => {
            let _ = other;
            return None;
        }
    }
    let mut checks = Vec::new();
    for check in value.get("checks")?.as_array()? {
        let id = check.get("id").and_then(|id| id.as_str())?.to_string();
        checks.push(AmontCheck {
            id,
            stage: check
                .get("stage")
                .and_then(|s| s.as_str())
                .map(String::from),
            source: check
                .get("source")
                .and_then(|s| s.as_str())
                .map(String::from),
            effective_severity: check
                .get("effective_severity")
                .and_then(|s| s.as_str())
                .map(String::from),
            status: check
                .get("status")
                .and_then(|s| s.as_str())
                .map(String::from),
            reason: check
                .get("reason")
                .and_then(|s| s.as_str())
                .map(String::from),
        });
    }
    let strings = |key: &str| -> Vec<String> {
        value
            .get(key)
            .and_then(|v| v.as_array())
            .map(|entries| entries.iter().map(|entry| entry.to_string()).collect())
            .unwrap_or_default()
    };
    Some(AmontInventory {
        checks,
        bypasses: strings("bypasses"),
        downgrades: strings("downgrades"),
    })
}

pub fn amont_list(repo_dir: &Path, stage: Option<&str>, pushed: bool) -> Option<AmontInventory> {
    let mut command = Command::new("amont");
    command.arg("list").arg("--json").current_dir(repo_dir);
    if let Some(stage) = stage {
        command.arg("--stage").arg(stage);
    }
    if pushed {
        command.arg("--pushed");
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse_amont_list(&String::from_utf8_lossy(&output.stdout))
}

/// Gaps among the checks the run depends on: required amont checks that
/// are inert, skipped, unavailable or of unknown status are gaps, and a
/// check the inventory does not list at all is a gap too (SPEC §10).
pub fn amont_gaps(inventory: Option<&AmontInventory>, required_ids: &[String]) -> Vec<String> {
    let Some(inventory) = inventory else {
        return required_ids
            .iter()
            .map(|id| format!("{id}: inventory unavailable"))
            .collect();
    };
    let mut gaps = Vec::new();
    for required in required_ids {
        match inventory.checks.iter().find(|check| &check.id == required) {
            None => gaps.push(format!("{required}: not in effective inventory")),
            Some(check) => match check.status.as_deref() {
                Some(status) if ACTIVE_STATUSES.contains(&status) => {}
                Some(status) => gaps.push(format!(
                    "{required}: status `{status}` — a skipped, inert, unavailable or untrusted required check is a gap, not a pass"
                )),
                None => gaps.push(format!("{required}: no status reported")),
            },
        }
    }
    gaps
}

/// A runner receipt (SPEC §12). Written by the runner into the ledger and
/// the run's artifact directory; a worker JSON document cannot impersonate
/// one because only `store_receipt` writes this shape and the ledger never
/// reads it back from worker output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    pub run_id: String,
    pub candidate_sha: String,
    pub base_sha: String,
    pub contract_hash: String,
    pub policy_hash: String,
    pub outcome: String,
    pub verification: VerificationReport,
    pub models_used: Vec<String>,
    pub attempts: u32,
    pub cost_completeness: CostCompleteness,
    pub cost: MicroUsd,
}

impl Receipt {
    pub fn hash(&self) -> String {
        canonical_json_hash(&serde_json::to_value(self).expect("receipt serializes"))
    }
}

/// Verification needs a working tree at an exact SHA that no worker can
/// touch: a throwaway worktree checked out at the candidate commit, then
/// removed. If creation fails the outcome is blocked, not a pass.
pub fn verification_worktree<'a>(
    repo_dir: &Path,
    candidate_sha: &str,
    path: &Path,
) -> Result<VerificationWorktree<'a>, std::io::Error> {
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
    let output = Command::new("git")
        .args([
            "worktree",
            "add",
            "--detach",
            &path.to_string_lossy(),
            candidate_sha,
        ])
        .current_dir(repo_dir)
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "git worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(VerificationWorktree {
        repo_dir: repo_dir.to_path_buf(),
        path: path.to_path_buf(),
        _marker: std::marker::PhantomData,
    })
}

pub struct VerificationWorktree<'a> {
    repo_dir: PathBuf,
    path: PathBuf,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl VerificationWorktree<'_> {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for VerificationWorktree<'_> {
    fn drop(&mut self) {
        // Verification leaves no state behind; failure to clean is
        // reported, not ignored, but never blocks acceptance either way.
        let _ = Command::new("git")
            .args([
                "worktree",
                "remove",
                "--force",
                &self.path.to_string_lossy(),
            ])
            .current_dir(&self.repo_dir)
            .output();
    }
}

/// Integration availability recorded into the report: required gaps block,
/// optional gaps are visible and never passes (SPEC §5).
pub fn integration_gaps(
    integrations: &Integrations,
    available: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut gaps = Vec::new();
    for (name, mode) in [
        ("aval", integrations.aval.as_ref().map(|d| d.mode())),
        ("amont", integrations.amont.as_ref().map(|d| d.mode())),
        (
            "amont-agent",
            integrations.amont_agent.as_ref().map(|d| d.mode()),
        ),
    ] {
        match mode {
            Some(DependencyMode::Required) if !available(name) => {
                gaps.push(format!("{name}: required integration unavailable"))
            }
            Some(DependencyMode::Optional) if !available(name) => gaps.push(format!(
                "{name}: optional integration not installed (reported, not passed)"
            )),
            _ => {}
        }
    }
    gaps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(argv: &[&str], timeout_seconds: u64) -> CommandSpec {
        CommandSpec {
            argv: argv.iter().map(|a| a.to_string()).collect(),
            timeout_seconds,
        }
    }

    #[test]
    fn passing_and_failing_commands_are_recorded_with_log_hashes() {
        let dir = std::env::temp_dir().join(format!("relais-verify-{}", std::process::id()));
        let logs = dir.join("logs");
        let pass =
            run_command(&dir, &command(&["sh", "-c", "echo ok"], 10), &logs, "pass").expect("run");
        assert!(!pass.failed());
        assert_eq!(pass.exit, Some(0));
        assert_eq!(pass.log_sha256.len(), 64);
        let fail = run_command(
            &dir,
            &command(&["sh", "-c", "echo bad >&2; exit 1"], 10),
            &logs,
            "fail",
        )
        .expect("run");
        assert!(fail.failed());
        assert_eq!(fail.exit, Some(1));
        let log = std::fs::read_to_string(&fail.log_path).expect("log exists");
        assert!(log.contains("bad"), "stderr is captured into the log");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn timeouts_kill_the_check_and_count_as_failures() {
        let dir = std::env::temp_dir().join(format!("relais-verify-t-{}", std::process::id()));
        let logs = dir.join("logs");
        let hung =
            run_command(&dir, &command(&["sh", "-c", "sleep 5"], 1), &logs, "hung").expect("run");
        assert!(hung.timed_out);
        assert!(hung.failed());
        std::fs::remove_dir_all(&dir).ok();
    }

    const AMONT_SAMPLE: &str = r#"{
        "format": "amont-list-v1",
        "stage_filter": null,
        "pushed": false,
        "checks": [
            {"id": "pre-commit-lint-shell", "stage": "pre-commit", "source": "builtin", "effective_severity": "block", "status": "runs", "reason": null, "scope_files": [], "command": "sh"},
            {"id": "declared-rubocop", "stage": "pre-commit", "source": "declared", "effective_severity": "block", "status": "inert", "reason": "untrusted declaration", "command": "rubocop"}
        ],
        "bypasses": [],
        "downgrades": [{"id": "pre-push-cargo-test", "severity": "warn"}]
    }"#;

    #[test]
    fn amont_inventory_parses_and_flags_inert_checks_as_gaps() {
        let inventory = parse_amont_list(AMONT_SAMPLE).expect("envelope parses");
        assert_eq!(inventory.checks.len(), 2);
        assert_eq!(inventory.downgrades.len(), 1);
        let gaps = amont_gaps(
            Some(&inventory),
            &[
                "pre-commit-lint-shell".to_string(),
                "declared-rubocop".to_string(),
                "missing-check".to_string(),
            ],
        );
        assert_eq!(
            gaps.len(),
            2,
            "active check passes; inert and missing are gaps"
        );
        assert!(gaps[0].contains("inert"), "{gaps:?}");
        assert!(gaps[1].contains("not in effective inventory"), "{gaps:?}");
    }

    #[test]
    fn wrong_amont_envelope_is_rejected_not_guessed() {
        assert!(parse_amont_list(r#"{"format": "something-else-v9"}"#).is_none());
        assert!(parse_amont_list("not json").is_none());
    }

    #[test]
    fn missing_inventory_means_every_required_check_is_a_gap() {
        let gaps = amont_gaps(None, &["pre-push-cargo-test".to_string()]);
        assert_eq!(gaps, vec!["pre-push-cargo-test: inventory unavailable"]);
    }

    #[test]
    fn accepted_requires_checks_pass_and_no_gaps() {
        let report = VerificationReport {
            candidate_sha: "abc".into(),
            base_sha: "def".into(),
            contract_hash: "ch".into(),
            policy_hash: "ph".into(),
            checks: vec![CheckOutcome {
                label: "cmd0".into(),
                argv: vec!["make".into(), "check".into()],
                exit: Some(0),
                timed_out: false,
                log_path: "x.log".into(),
                log_sha256: "h".into(),
            }],
            gaps: vec![],
            baseline_failures: vec![],
            amont_bypasses: vec![],
            amont_downgrades: vec![],
        };
        assert!(report.accepted());
        assert!(report.new_failures().is_empty());

        let blocked = VerificationReport {
            gaps: vec!["declared-rubocop: status `inert`".into()],
            ..report
        };
        assert!(!blocked.accepted(), "a gap is never a pass");
    }

    #[test]
    fn baseline_failures_are_visible_but_not_waived() {
        let failing_check = CheckOutcome {
            label: "flaky".into(),
            argv: vec![],
            exit: Some(1),
            timed_out: false,
            log_path: "f.log".into(),
            log_sha256: "h".into(),
        };
        let report = VerificationReport {
            candidate_sha: "abc".into(),
            base_sha: "def".into(),
            contract_hash: "ch".into(),
            policy_hash: "ph".into(),
            checks: vec![failing_check],
            gaps: vec![],
            baseline_failures: vec!["flaky".into()],
            amont_bypasses: vec![],
            amont_downgrades: vec![],
        };
        assert!(!report.accepted(), "baseline failures are not auto-waived");
        assert!(
            report.new_failures().is_empty(),
            "the pre-existing failure is visible, not new"
        );
    }

    #[test]
    fn receipt_hash_binds_the_candidate_and_policy() {
        let receipt = Receipt {
            run_id: "run-1".into(),
            candidate_sha: "abc".into(),
            base_sha: "def".into(),
            contract_hash: "ch".into(),
            policy_hash: "ph".into(),
            outcome: "accepted".into(),
            verification: VerificationReport {
                candidate_sha: "abc".into(),
                base_sha: "def".into(),
                contract_hash: "ch".into(),
                policy_hash: "ph".into(),
                checks: vec![],
                gaps: vec![],
                baseline_failures: vec![],
                amont_bypasses: vec![],
                amont_downgrades: vec![],
            },
            models_used: vec!["sonnet".into()],
            attempts: 1,
            cost_completeness: CostCompleteness::Actual,
            cost: MicroUsd::from_micros(12_345),
        };
        let hash = receipt.hash();
        let mut other = receipt.clone();
        other.candidate_sha = "different".into();
        assert_ne!(
            hash,
            other.hash(),
            "a changed candidate has a different identity"
        );
    }

    #[test]
    fn optional_integration_gaps_are_reported_not_passed() {
        let integrations = crate::policy::Integrations {
            aval: Some(crate::policy::Dependency::Mode(DependencyMode::Required)),
            amont: Some(crate::policy::Dependency::Mode(DependencyMode::Optional)),
            amont_agent: None,
        };
        let gaps = integration_gaps(&integrations, &|name| name == "aval");
        assert_eq!(
            gaps,
            vec!["amont: optional integration not installed (reported, not passed)"],
            "required-but-missing is NOT a gap here because aval is available"
        );
        let none = integration_gaps(&integrations, &|_| false);
        assert_eq!(none.len(), 2, "required missing + optional missing");
        assert!(none[0].starts_with("aval: required"));
    }
}
