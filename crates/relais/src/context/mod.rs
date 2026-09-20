//! Context packages (SPEC §7, §18).
//!
//! The manifest carries contract hash, base SHA, policy hash, tool
//! versions, source paths and fingerprints, architecture evidence and
//! the verification plan. Workers get the objective, acceptance criteria,
//! necessary constraints, a small set of entry points and exact failure
//! evidence — large files and logs are referenced by path and range.
//! Required constraints that exceed the context budget are a sizing
//! problem, never silently truncated. aval verdicts (active, undecided,
//! contradiction, retired, unknown) and aval tool failures stay distinct;
//! only a contradiction blocks affected work, and a missing answer gates
//! only tasks that depend on it.

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::contract::TaskContract;
use crate::ids::sha256_hex;
use crate::policy::RepoPolicy;

/// How much context a worker prompt may carry when `relais.toml` says
/// nothing. Repositories override it with `[context] budget_bytes`, which
/// is part of the hashed authority. Sizing problems are explicit;
/// nothing is silently truncated (SPEC §7).
pub const DEFAULT_CONTEXT_BUDGET_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvalVerdict {
    Active {
        adr: String,
        /// What was decided, in the record's words — the constraint the
        /// worker is handed (SPEC §7: "necessary constraints").
        #[serde(default)]
        choice: Option<String>,
        #[serde(default)]
        reason: Option<String>,
    },
    Undecided,
    Contradiction {
        heads: usize,
    },
    Retired {
        adr: String,
    },
    Unknown,
    /// Exit 1/2/3 from aval: a tool or corpus failure, NOT a verdict.
    /// Kept distinct so a broken corpus can never read as a decision
    /// (SPEC §7).
    ToolFailure {
        exit: i32,
        detail: String,
    },
}

/// Parse `aval resolve <key> --json` output (SEMANTICS.md v1.2.0 is the
/// normative contract): the payload carries `state` and `exit`; an error
/// envelope is `{"ok": false, "exit": N, "error": "..."}`.
pub fn parse_aval_output(exit_code: i32, stdout: &str) -> AvalVerdict {
    let value: serde_json::Value = match serde_json::from_str(stdout.trim()) {
        Ok(value) => value,
        Err(_) => {
            return AvalVerdict::ToolFailure {
                exit: exit_code,
                detail: format!("unparseable aval output: {stdout:?}"),
            }
        }
    };
    if value.get("ok") == Some(&serde_json::Value::Bool(false)) {
        let detail = value
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("aval reported a tool failure")
            .to_string();
        let exit = value
            .get("exit")
            .and_then(|e| e.as_i64())
            .unwrap_or(exit_code as i64) as i32;
        return AvalVerdict::ToolFailure { exit, detail };
    }
    match value.get("state").and_then(|s| s.as_str()) {
        Some("active") => AvalVerdict::Active {
            adr: value
                .get("adr")
                .and_then(|a| a.as_str())
                .unwrap_or("?")
                .to_string(),
            choice: value
                .get("choice")
                .and_then(|c| c.as_str())
                .map(str::to_string),
            reason: value
                .get("reason")
                .and_then(|r| r.as_str())
                .map(str::to_string),
        },
        Some("undecided") => AvalVerdict::Undecided,
        Some("contradiction") => AvalVerdict::Contradiction {
            heads: value
                .get("heads")
                .and_then(|h| h.as_array())
                .map(|heads| heads.len())
                .unwrap_or(0),
        },
        Some("retired") => AvalVerdict::Retired {
            adr: value
                .get("adr")
                .and_then(|a| a.as_str())
                .unwrap_or("?")
                .to_string(),
        },
        Some("unknown") => AvalVerdict::Unknown,
        other => AvalVerdict::ToolFailure {
            exit: exit_code,
            detail: format!("aval emitted an unrecognized state: {other:?}"),
        },
    }
}

/// Invoke `aval resolve KEY --json` in a repository. The exit-code contract
/// is part of aval's stable interface: 0 active, 4 undecided,
/// 5 contradiction, 6 retired, 7 unknown, 1/2/3 tool or corpus failure.
pub fn aval_resolve(repo_dir: &Path, key: &str, scope: Option<&str>) -> AvalVerdict {
    // aval takes no `--` separator, so a key that looks like a flag would
    // be parsed as one. Such a key is a mapping error, reported as the
    // tool failure it would otherwise become in a less legible form.
    if key.starts_with('-') || key.trim().is_empty() {
        return AvalVerdict::ToolFailure {
            exit: 2,
            detail: format!("decision key `{key}` is not a key aval can be asked for"),
        };
    }
    let mut command = std::process::Command::new("aval");
    command
        .arg("resolve")
        .arg(key)
        .arg("--json")
        .current_dir(repo_dir);
    if let Some(scope) = scope {
        command.arg("--scope").arg(scope);
    }
    match command.output() {
        Ok(output) => parse_aval_output(
            output.status.code().unwrap_or(-1),
            &String::from_utf8_lossy(&output.stdout),
        ),
        Err(e) => AvalVerdict::ToolFailure {
            exit: -1,
            detail: format!("aval could not be launched: {e}"),
        },
    }
}

/// Architecture evidence assembled for a contract: explicit keys from the
/// contract plus repo path-to-key mappings whose paths the declared scope
/// could touch. Unlisted keys never block unrelated work (SPEC §4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchitectureEvidence {
    pub resolved: Vec<(String, AvalVerdict)>,
}

/// Why a context package could not be assembled. Sizing is a distinct
/// outcome from missing evidence (SPEC §7).
#[derive(Debug, Clone, PartialEq)]
pub enum ContextError {
    /// Required constraints exceed the configured budget. Never
    /// truncated, never silently dropped.
    SizingProblem {
        required_bytes: usize,
        budget_bytes: usize,
    },
    /// A contradiction on a key the task depends on blocks affected work.
    ContradictionBlocked {
        key: String,
        heads: usize,
    },
    /// The task depends on a key aval cannot currently answer.
    NeedsDecision {
        key: String,
        verdict: AvalVerdict,
    },
    ToolFailure {
        key: String,
        detail: String,
    },
}

impl std::fmt::Display for ContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SizingProblem {
                required_bytes,
                budget_bytes,
            } => write!(
                f,
                "sizing problem: required context is {required_bytes} bytes against a {budget_bytes}-byte budget; raise the budget or split the task"
            ),
            Self::ContradictionBlocked { key, heads } => write!(
                f,
                "aval reports a contradiction on `{key}` ({heads} heads); affected work is blocked until it is resolved"
            ),
            Self::NeedsDecision { key, verdict } => write!(
                f,
                "the task depends on `{key}` but aval's answer is {verdict:?}; needs_decision"
            ),
            Self::ToolFailure { key, detail } => {
                write!(f, "aval tool failure while resolving `{key}`: {detail}")
            }
        }
    }
}

impl std::error::Error for ContextError {}

/// A source file's identity at the base revision: git's own blob id,
/// which is a content hash the repository already computed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileFingerprint {
    pub path: String,
    pub blob: String,
}

/// How many files a context package fingerprints at most; beyond that
/// the manifest names the hint and says it was truncated rather than
/// growing without bound.
pub const FINGERPRINT_CAP: usize = 400;

/// Fingerprint the files under each read hint at `base_sha`, from the
/// tree itself (`git ls-tree`), so what the worker was pointed at is
/// recorded exactly as it was — whatever the working tree does later.
pub fn fingerprint_hints(
    repo_dir: &Path,
    base_sha: &str,
    hints: &[String],
) -> Vec<FileFingerprint> {
    let mut out = Vec::new();
    for hint in hints {
        let output = std::process::Command::new("git")
            .args(["ls-tree", "-r", base_sha, "--", hint.trim_end_matches('/')])
            .current_dir(repo_dir)
            .output();
        let Ok(output) = output else { continue };
        if !output.status.success() {
            continue;
        }
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            // `<mode> <type> <object>\t<path>`
            let Some((meta, path)) = line.split_once('\t') else {
                continue;
            };
            let mut fields = meta.split_whitespace();
            let (_mode, kind, object) = (fields.next(), fields.next(), fields.next());
            if kind != Some("blob") {
                continue;
            }
            if let Some(object) = object {
                out.push(FileFingerprint {
                    path: path.to_string(),
                    blob: object.to_string(),
                });
            }
            if out.len() >= FINGERPRINT_CAP {
                out.push(FileFingerprint {
                    path: format!("{hint}: truncated at {FINGERPRINT_CAP} files"),
                    blob: String::new(),
                });
                return out;
            }
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolVersions {
    pub relais: String,
    pub aval: Option<String>,
    pub amont: Option<String>,
    #[serde(default)]
    pub claude_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextManifest {
    pub contract_hash: String,
    pub base_sha: String,
    pub policy_hash: String,
    pub tool_versions: ToolVersions,
    pub fingerprints: Vec<FileFingerprint>,
    pub architecture: ArchitectureEvidence,
    pub verification_profile: String,
    /// Constraints included verbatim; the manifest reports their size so
    /// the sizing problem is observable before dispatch.
    pub constraints: Vec<String>,
    pub budget_bytes: usize,
    /// What the worker actually receives, measured: objective,
    /// acceptance criteria, constraints and entry points together. The
    /// budget is checked against this, not against the constraints
    /// alone, so a sizing problem is raised on the real prompt.
    #[serde(default)]
    pub package_bytes: usize,
    /// Whether a turn ceiling was enforceable on this run's harness
    /// ("harness" or "unavailable"). SPEC §11 lists turns among the
    /// ceilings; Claude Code 2.1.x takes no turn flag, so the receipt
    /// records which it was instead of implying one was applied.
    #[serde(default)]
    pub turn_ceiling: String,
}

pub struct ContextInputs<'a> {
    pub contract: &'a TaskContract,
    pub repo: &'a RepoPolicy,
    pub contract_hash: &'a str,
    pub base_sha: &'a str,
    pub policy_hash: &'a str,
    pub fingerprints: Vec<FileFingerprint>,
    pub tool_versions: ToolVersions,
    /// Whether the harness this run will dispatch on can take a turn
    /// ceiling (`crate::adapter::TurnCeiling`).
    pub turn_ceiling: crate::adapter::TurnCeiling,
    /// Resolve function, so tests can supply verdicts without invoking
    /// the real binary.
    pub resolver: &'a dyn Fn(&str, Option<&str>) -> AvalVerdict,
}

/// Assemble the context manifest. Fails on contradiction (blocked), on
/// an unanswered depended-upon key (needs_decision), on tool failure,
/// and on sizing problems — in that order of precedence, since a blocked
/// task should not pay for context it cannot use.
pub fn assemble(inputs: ContextInputs<'_>) -> Result<ContextManifest, ContextError> {
    let ContextInputs {
        contract,
        repo,
        contract_hash,
        base_sha,
        policy_hash,
        fingerprints,
        tool_versions,
        turn_ceiling,
        resolver,
    } = inputs;

    let mut keys: Vec<String> = contract.architecture.keys.clone();
    for mapping in &repo.architecture.mapping {
        let touched = contract.write_scope.as_deref().is_some_and(|scopes| {
            scopes
                .iter()
                .any(|scope| scope_could_touch(scope, &mapping.paths))
        });
        if touched {
            keys.extend(mapping.keys.iter().cloned());
        }
    }
    keys.sort();
    keys.dedup();

    let mut resolved = Vec::new();
    for key in &keys {
        let verdict = resolver(key, contract.architecture.scope.as_deref());
        match &verdict {
            AvalVerdict::Contradiction { heads } => {
                return Err(ContextError::ContradictionBlocked {
                    key: key.clone(),
                    heads: *heads,
                })
            }
            AvalVerdict::Undecided | AvalVerdict::Unknown | AvalVerdict::Retired { .. } => {
                return Err(ContextError::NeedsDecision {
                    key: key.clone(),
                    verdict: verdict.clone(),
                })
            }
            AvalVerdict::ToolFailure { detail, .. } => {
                return Err(ContextError::ToolFailure {
                    key: key.clone(),
                    detail: detail.clone(),
                })
            }
            AvalVerdict::Active { .. } => {}
        }
        resolved.push((key.clone(), verdict));
    }

    // The constraint is the decision in the record's own words — what
    // `aval resolve --json` carries as `choice` and `reason`; the full
    // body stays behind `aval show ADR` for the worker to fetch on demand.
    let constraints: Vec<String> = resolved
        .iter()
        .filter_map(|(key, verdict)| match verdict {
            AvalVerdict::Active {
                adr,
                choice,
                reason,
            } => Some(match (choice, reason) {
                (Some(choice), Some(reason)) => {
                    format!("`{key}` ({adr}): {choice} — because {reason}")
                }
                (Some(choice), None) => format!("`{key}` ({adr}): {choice}"),
                _ => format!("`{key}` is decided by {adr} (run `aval show {adr}` for the text)"),
            }),
            _ => None,
        })
        .collect();

    // The budget bounds the package the worker receives (SPEC §7:
    // "the objective, acceptance criteria, necessary constraints, a
    // small set of entry points"), not the constraints alone — an
    // objective or an acceptance list that alone overruns the budget is
    // just as much a sizing problem, and counting a subset raised it on
    // the wrong number. Large files and logs are referenced by path, so
    // they are not part of this sum by design.
    let package_bytes = contract.objective.len()
        + contract
            .acceptance
            .iter()
            .map(|criterion| criterion.len())
            .sum::<usize>()
        + constraints
            .iter()
            .map(|constraint| constraint.len())
            .sum::<usize>()
        + contract
            .read_hints
            .iter()
            .map(|hint| hint.len())
            .sum::<usize>();

    let manifest = ContextManifest {
        contract_hash: contract_hash.to_string(),
        base_sha: base_sha.to_string(),
        policy_hash: policy_hash.to_string(),
        tool_versions,
        fingerprints,
        architecture: ArchitectureEvidence { resolved },
        verification_profile: contract.verification_profile.clone(),
        constraints: constraints.clone(),
        budget_bytes: repo.context.budget_bytes,
        package_bytes,
        turn_ceiling: turn_ceiling.as_str().to_string(),
    };

    if manifest.package_bytes > manifest.budget_bytes {
        return Err(ContextError::SizingProblem {
            required_bytes: manifest.package_bytes,
            budget_bytes: manifest.budget_bytes,
        });
    }
    Ok(manifest)
}

/// Same conservative overlap logic as the router, applied between the
/// contract's declared scope and a repo architecture mapping's paths.
fn scope_could_touch(scope: &str, mapping_paths: &[String]) -> bool {
    mapping_paths
        .iter()
        .any(|pattern| crate::route::scope_could_touch(scope, pattern))
}

/// Hash a context manifest for the ledger: content-addressed evidence.
pub fn manifest_hash(manifest: &ContextManifest) -> String {
    sha256_hex(&serde_json::to_vec(manifest).expect("manifest serializes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::ArchitectureConfig;

    fn contract(architecture_keys: &[&str]) -> TaskContract {
        let mut c = TaskContract::from_json_str(
            &serde_json::json!({
                "schema_version": 1,
                "kind": "change",
                "objective": "Fix JSON escaping",
                "base_ref": "HEAD",
                "write_scope": ["crates/amont/**"],
                "acceptance": ["output parses"],
                "verification_profile": "rust-change",
            })
            .to_string(),
        )
        .expect("contract parses");
        c.architecture.keys = architecture_keys.iter().map(|k| k.to_string()).collect();
        c
    }

    fn repo() -> RepoPolicy {
        RepoPolicy::from_toml_str(crate::policy::INIT_TEMPLATE).expect("policy parses")
    }

    fn inputs<'a>(
        contract: &'a TaskContract,
        repo: &'a RepoPolicy,
        resolver: &'a dyn Fn(&str, Option<&str>) -> AvalVerdict,
    ) -> ContextInputs<'a> {
        ContextInputs {
            contract,
            repo,
            contract_hash: "chash",
            base_sha: "deadbeef",
            policy_hash: "phash",
            fingerprints: vec![FileFingerprint {
                path: "crates/amont/src/main.rs".into(),
                blob: "abc".into(),
            }],
            tool_versions: ToolVersions {
                relais: "0.1.0".into(),
                aval: Some("1.3.2".into()),
                amont: Some("1.36.0".into()),
                claude_code: None,
            },
            turn_ceiling: crate::adapter::TurnCeiling::Unavailable,
            resolver,
        }
    }

    fn active(key: &str, _scope: Option<&str>) -> AvalVerdict {
        let _ = key;
        AvalVerdict::Active {
            adr: "ADR-0021".into(),
            choice: None,
            reason: None,
        }
    }

    #[test]
    fn parses_each_verdict_state_from_aval_json() {
        assert_eq!(
            parse_aval_output(
                0,
                r#"{"state":"active","exit":0,"key":"storage.object-store","adr":"ADR-0021",
                    "choice":"RGW behind Ceph","reason":"one storage plane"}"#
            ),
            AvalVerdict::Active {
                adr: "ADR-0021".into(),
                choice: Some("RGW behind Ceph".into()),
                reason: Some("one storage plane".into()),
            }
        );
        assert_eq!(
            parse_aval_output(4, r#"{"state":"undecided","exit":4,"key":"k"}"#),
            AvalVerdict::Undecided
        );
        assert_eq!(
            parse_aval_output(
                5,
                r#"{"state":"contradiction","exit":5,"heads":[{"adr":"ADR-0001"},{"adr":"ADR-0002"}]}"#
            ),
            AvalVerdict::Contradiction { heads: 2 }
        );
        assert_eq!(
            parse_aval_output(6, r#"{"state":"retired","exit":6,"adr":"ADR-0002"}"#),
            AvalVerdict::Retired {
                adr: "ADR-0002".into()
            }
        );
        assert_eq!(
            parse_aval_output(7, r#"{"state":"unknown","exit":7,"key":"k"}"#),
            AvalVerdict::Unknown
        );
    }

    #[test]
    fn tool_failures_stay_distinct_from_verdicts() {
        assert_eq!(
            parse_aval_output(3, r#"{"ok":false,"exit":3,"error":"unreadable corpus"}"#),
            AvalVerdict::ToolFailure {
                exit: 3,
                detail: "unreadable corpus".into()
            }
        );
        assert!(matches!(
            parse_aval_output(1, "not json at all"),
            AvalVerdict::ToolFailure { .. }
        ));
        let verdict = parse_aval_output(0, r#"{"state":"weird"}"#);
        assert!(
            matches!(verdict, AvalVerdict::ToolFailure { .. }),
            "{verdict:?}"
        );
    }

    #[test]
    fn active_decisions_assemble_into_constraints() {
        let c = contract(&["storage.object-store"]);
        let r = repo();
        let manifest = assemble(inputs(&c, &r, &active)).expect("assembles");
        assert_eq!(manifest.architecture.resolved.len(), 1);
        assert_eq!(manifest.constraints.len(), 1);
        assert_eq!(manifest.base_sha, "deadbeef");
        assert_eq!(manifest.fingerprints[0].path, "crates/amont/src/main.rs");
        let hash = manifest_hash(&manifest);
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn contradiction_blocks_affected_work() {
        let c = contract(&["api.gateway"]);
        let r = repo();
        let contradiction = |_: &str, _: Option<&str>| AvalVerdict::Contradiction { heads: 2 };
        let err = assemble(inputs(&c, &r, &contradiction)).unwrap_err();
        assert_eq!(
            err,
            ContextError::ContradictionBlocked {
                key: "api.gateway".into(),
                heads: 2
            }
        );
    }

    #[test]
    fn undecided_gates_only_tasks_that_depend_on_it() {
        let c = contract(&[]);
        let r = repo();
        // No keys: an unrelated undecided key cannot block anything.
        let undecided = |_: &str, _: Option<&str>| AvalVerdict::Undecided;
        assemble(inputs(&c, &r, &undecided)).expect("no keys means no dependency");

        let dependent = contract(&["api.gateway"]);
        let err = assemble(inputs(&dependent, &r, &undecided)).unwrap_err();
        assert!(matches!(err, ContextError::NeedsDecision { .. }), "{err:?}");
    }

    #[test]
    fn tool_failure_is_not_a_verdict() {
        let c = contract(&["k"]);
        let r = repo();
        let broken = |_: &str, _: Option<&str>| AvalVerdict::ToolFailure {
            exit: 3,
            detail: "unreadable".into(),
        };
        let err = assemble(inputs(&c, &r, &broken)).unwrap_err();
        assert!(matches!(err, ContextError::ToolFailure { .. }), "{err:?}");
    }

    #[test]
    fn repo_mapping_keys_are_pulled_in_when_scope_touches() {
        let mut r = repo();
        r.architecture = ArchitectureConfig {
            mapping: vec![crate::policy::ArchitectureMapping {
                paths: vec!["crates/amont/**".into()],
                keys: vec!["output.contract".into()],
                scope: None,
            }],
        };
        let c = contract(&[]);
        let manifest = assemble(inputs(&c, &r, &active)).expect("assembles");
        assert_eq!(
            manifest.architecture.resolved[0].0, "output.contract",
            "mapping keys are resolved when the declared scope could touch their paths"
        );
    }

    #[test]
    fn oversized_constraints_are_a_sizing_problem_never_truncated() {
        let keys: Vec<String> = (0..1500).map(|i| format!("key{i}.decision")).collect();
        let c = contract(&[]);
        let mut r = repo();
        r.architecture = ArchitectureConfig {
            mapping: vec![crate::policy::ArchitectureMapping {
                paths: vec!["crates/amont/**".into()],
                keys,
                scope: None,
            }],
        };
        let big = |_: &str, _: Option<&str>| AvalVerdict::Active {
            adr: "ADR-0001".into(),
            choice: None,
            reason: None,
        };
        let err = assemble(inputs(&c, &r, &big)).unwrap_err();
        assert!(
            matches!(err, ContextError::SizingProblem { required_bytes, budget_bytes }
                if required_bytes > budget_bytes),
            "{err:?}"
        );
    }

    #[test]
    fn the_budget_is_repo_policy_and_bounds_the_whole_package() {
        let mut c = contract(&[]);
        c.objective = "x".repeat(200);
        let mut r = repo();
        // Smaller than the objective alone: the old check counted only
        // constraints, of which this task has none, and passed.
        r.context.budget_bytes = 64;
        let err = assemble(inputs(&c, &r, &active)).unwrap_err();
        assert_eq!(
            err,
            ContextError::SizingProblem {
                required_bytes: c.objective.len()
                    + c.acceptance.iter().map(String::len).sum::<usize>()
                    + c.read_hints.iter().map(String::len).sum::<usize>(),
                budget_bytes: 64,
            },
            "the objective alone overruns the configured budget"
        );

        // The same task fits under a budget the repository raised, and
        // the manifest carries both numbers as evidence.
        r.context.budget_bytes = 4096;
        let manifest = assemble(inputs(&c, &r, &active)).expect("assembles");
        assert_eq!(manifest.budget_bytes, 4096);
        assert!(manifest.package_bytes >= c.objective.len());
        assert_eq!(
            manifest.turn_ceiling, "unavailable",
            "a receipt says which turn ceiling the harness could take"
        );
    }
}
