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
use std::time::Duration;

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
    /// Verification inputs the candidate changed (build manifests, tests,
    /// the commands themselves): reviewed explicitly, never silently
    /// accepted (SPEC §10).
    #[serde(default)]
    pub verification_inputs_changed: Vec<String>,
    /// Optional integrations that were not available: reported, never
    /// counted as passed (SPEC §5).
    #[serde(default)]
    pub integration_gaps: Vec<String>,
    /// Whether the baseline results came from the cache (SPEC §18) or a
    /// fresh run at the base.
    #[serde(default)]
    pub baseline_cached: bool,
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
/// evidence: it exists whether the check passed or failed. The LABEL is
/// stable across baseline and candidate runs (it identifies the command);
/// the LOG STEM differs so logs never collide.
pub fn run_command(
    dir: &Path,
    spec: &CommandSpec,
    logs_dir: &Path,
    label: &str,
    log_stem: &str,
) -> std::io::Result<CheckOutcome> {
    std::fs::create_dir_all(logs_dir)?;
    let log_path = logs_dir.join(format!("{log_stem}.log"));
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
    crate::adapter::own_process_group(&mut command);
    let mut child = command.spawn()?;
    let timeout = Duration::from_secs(spec.timeout_seconds.max(1));
    let (status, timed_out, _cancelled) = crate::adapter::wait_for_exit(&mut child, timeout, None)?;
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

/// A check's label: the command's own identity (`make check@1a2b3c4d`),
/// so a baseline failure matches the same command at the candidate even
/// after the profile is reordered or a command is inserted — a position
/// (`cmd0`) would silently re-pair them.
pub fn check_label(spec: &CommandSpec) -> String {
    let identity = sha256_hex(
        serde_json::to_string(&spec.argv)
            .expect("argv serializes")
            .as_bytes(),
    );
    format!(
        "{}@{}",
        spec.argv.first().map(String::as_str).unwrap_or("?"),
        &identity[..8]
    )
}

/// Run a full profile against a directory. The SAME labels identify the
/// same commands at the base and at every candidate, which is what makes
/// baseline-vs-candidate comparison meaningful (SPEC §10).
pub fn run_profile(
    dir: &Path,
    profile: &VerificationProfile,
    logs_dir: &Path,
    prefix: &str,
) -> std::io::Result<Vec<CheckOutcome>> {
    let mut outcomes = Vec::new();
    for (index, command) in profile.commands.iter().enumerate() {
        let label = check_label(command);
        let log_stem = format!("{prefix}-cmd{index}");
        outcomes.push(run_command(dir, command, logs_dir, &label, &log_stem)?);
    }
    Ok(outcomes)
}

/// Files the profile's verdict depends on: build manifests, lockfiles,
/// the test tree, and any repository-relative program the profile runs.
/// A candidate that changes one of these can make verification pass by
/// changing what verification IS (SPEC §10: "changes to required checks,
/// fixtures or acceptance tests receive explicit review and cannot
/// silently weaken the contract"), so the runner requires review for it.
pub const DEFAULT_VERIFICATION_INPUTS: &[&str] = &[
    "Makefile",
    "makefile",
    "GNUmakefile",
    "justfile",
    "Cargo.toml",
    "Cargo.lock",
    "**/Cargo.toml",
    "rust-toolchain.toml",
    "package.json",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "pyproject.toml",
    "requirements*.txt",
    "go.mod",
    "go.sum",
    "**/tests/**",
    "**/test/**",
    "**/__tests__/**",
    "**/*_test.*",
    "**/*.test.*",
    "**/*.spec.*",
    "**/conftest.py",
    "**/pytest.ini",
    "**/fixtures/**",
];

/// The verification-input patterns for a profile: the defaults, the
/// profile's own declarations, and each command's program when it lives
/// in the repository (`./scripts/check.sh`).
pub fn verification_inputs(profile: &VerificationProfile) -> Vec<String> {
    let mut patterns: Vec<String> = DEFAULT_VERIFICATION_INPUTS
        .iter()
        .map(|p| p.to_string())
        .collect();
    patterns.extend(profile.inputs.iter().cloned());
    for command in &profile.commands {
        if let Some(program) = command.argv.first() {
            let relative = program.trim_start_matches("./");
            if program.starts_with("./") || program.contains('/') && !program.starts_with('/') {
                patterns.push(relative.to_string());
            }
        }
    }
    patterns.sort();
    patterns.dedup();
    patterns
}

/// Which of `changed` are verification inputs.
pub fn verification_inputs_touched(
    profile: &VerificationProfile,
    changed: &[String],
) -> Vec<String> {
    let mut builder = globset::GlobSetBuilder::new();
    for pattern in verification_inputs(profile) {
        if let Ok(glob) = globset::GlobBuilder::new(&pattern)
            .literal_separator(true)
            .build()
        {
            builder.add(glob);
        }
    }
    let Ok(set) = builder.build() else {
        return Vec::new();
    };
    changed
        .iter()
        .filter(|path| set.is_match(path))
        .cloned()
        .collect()
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
    pub status: Option<AmontStatus>,
    pub reason: Option<String>,
}

/// What amont's inventory says about a check. Docs say
/// `ready|inert|skipped|unavailable`; code has emitted `runs` — only the
/// in-force spellings count as such, and a spelling this relais does not
/// know is kept verbatim and treated as a gap, the conservative direction
/// (SPEC §10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AmontStatus {
    Ready,
    Runs,
    Active,
    Inert,
    Skipped,
    Unavailable,
    #[serde(untagged)]
    Unknown(String),
}

impl AmontStatus {
    pub fn parse(text: &str) -> Self {
        serde_json::from_value(serde_json::Value::String(text.to_string()))
            .unwrap_or_else(|_| Self::Unknown(text.to_string()))
    }

    /// Genuinely in force, as opposed to inert, skipped, unavailable or
    /// something this relais cannot vouch for.
    pub fn in_force(&self) -> bool {
        matches!(self, Self::Ready | Self::Runs | Self::Active)
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Ready => "ready",
            Self::Runs => "runs",
            Self::Active => "active",
            Self::Inert => "inert",
            Self::Skipped => "skipped",
            Self::Unavailable => "unavailable",
            Self::Unknown(text) => text,
        }
    }
}

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
                .map(AmontStatus::parse),
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
            Some(check) => match &check.status {
                Some(status) if status.in_force() => {}
                Some(status) => gaps.push(format!(
                    "{required}: status `{}` — a skipped, inert, unavailable or untrusted required check is a gap, not a pass",
                    status.as_str()
                )),
                None => gaps.push(format!("{required}: no status reported")),
            },
        }
    }
    gaps
}

/// What one candidate's verification established: the checks that ran,
/// the required checks that are gaps, and what amont's inventory declares
/// about itself.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Verified {
    pub checks: Vec<CheckOutcome>,
    pub gaps: Vec<String>,
    pub amont_bypasses: Vec<String>,
    pub amont_downgrades: Vec<String>,
}

/// The baseline cache key: everything the base verdict is a function of
/// that relais can observe (SPEC §18: "candidate content, dependency
/// lockfiles, toolchain, command, configuration"). Lockfiles are part of
/// the base tree; the toolchain is the recorded tool versions.
pub fn baseline_key(
    base_sha: &str,
    profile: &VerificationProfile,
    tools: &crate::context::ToolVersions,
) -> String {
    sha256_hex(
        serde_json::json!({
            "base": base_sha,
            "profile": profile,
            "tools": tools,
        })
        .to_string()
        .as_bytes(),
    )
}

/// Cached baseline failures, one JSON file per key, opt-in per profile
/// (SPEC §18: not cacheable by default). A miss reruns the checks; a
/// corrupt entry is a miss.
pub struct BaselineCache {
    dir: PathBuf,
}

impl BaselineCache {
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
        }
    }

    pub fn get(&self, key: &str) -> Option<Vec<String>> {
        let text = std::fs::read_to_string(self.dir.join(format!("{key}.json"))).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn put(&self, key: &str, failures: &[String]) {
        if std::fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let _ = std::fs::write(
            self.dir.join(format!("{key}.json")),
            serde_json::to_string(failures).expect("serializes"),
        );
    }
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
/// optional gaps are visible and never passes (SPEC §5). `available` is
/// asked about the BINARY the declaration names (`bin = …` overrides the
/// default), not the integration's config name.
pub fn integration_gaps(
    integrations: &Integrations,
    available: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut gaps = Vec::new();
    for (name, default_bin, dependency) in [
        ("aval", "aval", integrations.aval.as_ref()),
        ("amont", "amont", integrations.amont.as_ref()),
        (
            "amont-agent",
            "amont-agent",
            integrations.amont_agent.as_ref(),
        ),
    ] {
        let Some(dependency) = dependency else {
            continue;
        };
        let bin = dependency.bin().unwrap_or(default_bin);
        match dependency.mode() {
            DependencyMode::Required if !available(bin) => {
                gaps.push(format!("{name}: required integration unavailable"))
            }
            DependencyMode::Optional if !available(bin) => gaps.push(format!(
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
        let pass = run_command(
            &dir,
            &command(&["sh", "-c", "echo ok"], 10),
            &logs,
            "cmd0",
            "x-cmd0",
        )
        .expect("run");
        assert!(!pass.failed());
        assert_eq!(pass.exit, Some(0));
        assert_eq!(pass.log_sha256.len(), 64);
        let fail = run_command(
            &dir,
            &command(&["sh", "-c", "echo bad >&2; exit 1"], 10),
            &logs,
            "cmd1",
            "x-cmd1",
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
        let hung = run_command(
            &dir,
            &command(&["sh", "-c", "sleep 5"], 1),
            &logs,
            "cmd0",
            "x-cmd0",
        )
        .expect("run");
        assert!(hung.timed_out);
        assert!(hung.failed());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn labels_are_stable_across_baseline_and_candidate_runs() {
        let dir = std::env::temp_dir().join(format!("relais-verify-l-{}", std::process::id()));
        let logs = dir.join("logs");
        let profile = VerificationProfile {
            commands: vec![command(&["sh", "-c", "true"], 10)],
            amont_checks: Vec::new(),
            inputs: Vec::new(),
            cache_baseline: false,
        };
        let base = run_profile(&dir, &profile, &logs, "base").expect("base");
        let candidate = run_profile(&dir, &profile, &logs, "attempt1").expect("candidate");
        assert_eq!(base[0].label, check_label(&profile.commands[0]));
        assert!(base[0].label.starts_with("sh@"), "{}", base[0].label);
        assert_eq!(
            candidate[0].label, base[0].label,
            "the same command keeps its label"
        );
        assert_ne!(
            base[0].log_path, candidate[0].log_path,
            "logs never collide"
        );
        // A reordered profile does not re-pair a baseline failure with a
        // different command: the label is the command's, not its slot's.
        let reordered = VerificationProfile {
            commands: vec![
                command(&["sh", "-c", "false"], 10),
                profile.commands[0].clone(),
            ],
            ..profile.clone()
        };
        let again = run_profile(&dir, &reordered, &logs, "attempt2").expect("reordered");
        assert_eq!(again[1].label, base[0].label);
        assert_ne!(again[0].label, base[0].label);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn verification_inputs_include_defaults_declared_globs_and_repo_programs() {
        let profile = VerificationProfile {
            commands: vec![
                command(&["./scripts/check.sh"], 10),
                command(&["make", "check"], 10),
            ],
            amont_checks: Vec::new(),
            inputs: vec!["ci/**".into()],
            cache_baseline: false,
        };
        let touched = verification_inputs_touched(
            &profile,
            &[
                "src/lib.rs".into(),
                "Makefile".into(),
                "crates/x/Cargo.toml".into(),
                "tests/smoke.rs".into(),
                "src/foo_test.go".into(),
                "scripts/check.sh".into(),
                "ci/lint.yml".into(),
                "docs/README.md".into(),
            ],
        );
        assert_eq!(
            touched,
            vec![
                "Makefile",
                "crates/x/Cargo.toml",
                "tests/smoke.rs",
                "src/foo_test.go",
                "scripts/check.sh",
                "ci/lint.yml",
            ]
        );
    }

    #[test]
    fn baseline_cache_is_keyed_on_base_profile_and_tools() {
        let dir = std::env::temp_dir().join(format!("relais-bcache-{}", std::process::id()));
        let cache = BaselineCache::new(&dir);
        let profile = VerificationProfile {
            commands: vec![command(&["make", "check"], 10)],
            amont_checks: Vec::new(),
            inputs: Vec::new(),
            cache_baseline: true,
        };
        let tools = crate::context::ToolVersions {
            relais: "0.1.0".into(),
            aval: None,
            amont: Some("amont 1.36.1".into()),
            claude_code: None,
        };
        let key = baseline_key("abc", &profile, &tools);
        assert_eq!(cache.get(&key), None, "a miss reruns the checks");
        cache.put(&key, &["make@deadbeef".to_string()]);
        assert_eq!(cache.get(&key), Some(vec!["make@deadbeef".to_string()]));
        let other_tools = crate::context::ToolVersions {
            amont: Some("amont 1.37.0".into()),
            ..tools.clone()
        };
        assert_ne!(
            key,
            baseline_key("abc", &profile, &other_tools),
            "toolchain is in the key"
        );
        assert_ne!(
            key,
            baseline_key("abd", &profile, &tools),
            "base is in the key"
        );
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
            verification_inputs_changed: Vec::new(),
            integration_gaps: Vec::new(),
            baseline_cached: false,
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
            verification_inputs_changed: Vec::new(),
            integration_gaps: Vec::new(),
            baseline_cached: false,
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
                verification_inputs_changed: Vec::new(),
                integration_gaps: Vec::new(),
                baseline_cached: false,
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
