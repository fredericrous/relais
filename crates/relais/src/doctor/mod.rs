//! Environment diagnosis (SPEC §3: `relais doctor`).
//!
//! Checks the directories in effect, git, the Claude Code binary and its
//! capability report, the aval/amont/amont-agent integrations *at the
//! modes this repository declares*, policy files, the ledger, the
//! artifact registry and the coordinator socket. Findings distinguish
//! hard blockers from warnings; nothing here repairs anything by itself,
//! and a missing optional integration is reported, not counted as fine.
//!
//! Doctor is also where the product admits what it does not do: a turn
//! ceiling the installed harness cannot take, and a `[trials]` envelope
//! no code reads.

use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::backend::Capabilities;
use crate::policy::{Dependency, DependencyMode, MachineSettings, RepoPolicy};
use crate::{ledger::Ledger, paths};

/// How bad one finding is. An enum rather than a string plus a parallel
/// `ok` flag: the two disagreed (an `ok: true` line at `warn`, an
/// `ok: false` one at `ok`), and `render` fell through `_ => "✗"` for
/// anything it did not recognise while `failed()` compared against the
/// literal `"fail"` — so a typo would have been rendered as a blocker and
/// counted as a pass (C5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Checked, and nothing to do.
    Ok,
    /// A gap worth seeing. Never counted as a pass, never a blocker.
    Warn,
    /// Execution is blocked until this is fixed.
    Fail,
}

impl Level {
    /// The mark `render` prints, one per variant.
    pub fn mark(self) -> &'static str {
        match self {
            Level::Ok => "✓",
            Level::Warn => "!",
            Level::Fail => "✗",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub component: &'static str,
    pub level: Level,
    pub detail: String,
}

impl Finding {
    /// Whether this finding is a passed check. Derived from `level`, not
    /// stored beside it: one of the two used to be wrong.
    pub fn ok(&self) -> bool {
        self.level == Level::Ok
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub findings: Vec<Finding>,
}

impl DoctorReport {
    pub fn failed(&self) -> bool {
        self.findings
            .iter()
            .any(|finding| finding.level == Level::Fail)
    }

    pub fn render(&self) -> String {
        let mut out = String::from("relais doctor\n");
        for finding in &self.findings {
            out.push_str(&format!(
                " {} {:<12} {}\n",
                finding.level.mark(),
                finding.component,
                finding.detail
            ));
        }
        if self.failed() {
            out.push_str("\nfix the ✗ findings before running tasks\n");
        }
        out
    }
}

/// One PATH lookup for the whole crate, so `doctor` agrees with the
/// backend's own discovery about what is installed — extension suffixes
/// on Windows included (audit C2).
fn binary_on_path(name: &str) -> Option<std::path::PathBuf> {
    crate::tooling::which(name)
}

fn check_command(name: &str, args: &[&str], findings: &mut Vec<Finding>, component: &'static str) {
    match binary_on_path(name) {
        None => findings.push(Finding {
            component,
            level: Level::Fail,
            detail: format!("`{name}` is not on PATH"),
        }),
        Some(binary) => {
            let output = std::process::Command::new(&binary).args(args).output();
            match output {
                Ok(output) if output.status.success() => findings.push(Finding {
                    component,
                    level: Level::Ok,
                    detail: format!(
                        "`{}` — {}",
                        binary.to_string_lossy(),
                        String::from_utf8_lossy(&output.stdout).trim()
                    ),
                }),
                Ok(_) => findings.push(Finding {
                    component,
                    level: Level::Fail,
                    detail: format!("`{name}` exists but failed to run"),
                }),
                Err(e) => findings.push(Finding {
                    component,
                    level: Level::Fail,
                    detail: format!("`{name}` could not be launched: {e}"),
                }),
            }
        }
    }
}

/// One integration, judged at the mode the repository declared for it
/// (SPEC §5). A missing *required* integration blocks execution, so it
/// is a ✗; a missing *optional* one is a gap that appears explicitly and
/// is never counted as a passed check; `off` is not probed at all, and
/// the line says so rather than leaving the reader to guess.
///
/// `available` and `version` are injected so the rules are testable
/// without a binary on PATH.
pub(crate) fn integration_finding(
    name: &str,
    component: &'static str,
    dependency: Option<&Dependency>,
    available: &dyn Fn(&str) -> bool,
    version: &dyn Fn(&str) -> Option<String>,
) -> Finding {
    let Some(dependency) = dependency else {
        return Finding {
            component,
            level: Level::Ok,
            detail: format!("not declared in [integrations]; `{name}` is not checked"),
        };
    };
    let bin = dependency
        .bin()
        .map(str::to_string)
        .unwrap_or_else(|| default_integration_bin(name));
    let mode = dependency.mode();
    // Installed, at whichever mode asked for it.
    let present = |mode: DependencyMode| Finding {
        component,
        level: Level::Ok,
        detail: match version(&bin) {
            Some(version) => format!("{} — `{bin}` {version}", mode_word(mode)),
            None => format!(
                "{} — `{bin}` is on PATH but would not answer `--version`",
                mode_word(mode)
            ),
        },
    };
    // Exhaustive over the mode: `off` is not probed at all, and the two
    // remaining modes differ only in what a missing binary means.
    match mode {
        DependencyMode::Off => Finding {
            component,
            level: Level::Ok,
            detail: format!("declared `off` in [integrations]; `{bin}` is not checked"),
        },
        DependencyMode::Required if available(&bin) => present(mode),
        DependencyMode::Required => Finding {
            component,
            level: Level::Fail,
            detail: format!(
                "required by [integrations] and `{bin}` is not on PATH; every run is blocked on it"
            ),
        },
        DependencyMode::Optional if available(&bin) => present(mode),
        // Reported as a gap, never as a pass.
        DependencyMode::Optional => Finding {
            component,
            level: Level::Warn,
            detail: format!(
                "optional and `{bin}` is not on PATH; its checks are a reported gap, not a pass"
            ),
        },
    }
}

fn mode_word(mode: DependencyMode) -> &'static str {
    match mode {
        DependencyMode::Required => "required",
        DependencyMode::Optional => "optional",
        DependencyMode::Off => "off",
    }
}

/// The config key is `amont_agent`; the binary is `amont-agent`.
fn default_integration_bin(name: &str) -> String {
    name.replace('_', "-")
}

/// The Claude Code line, including what the harness can and cannot
/// enforce. Split out so the capability text is testable against a fake
/// `--help`.
pub(crate) fn claude_code_finding(caps: &Capabilities) -> Finding {
    let mut detail = format!("version {}", caps.version.as_deref().unwrap_or("?"));
    if !caps.supports_model {
        detail.push_str("; WARNING: --model not advertised in --help");
    }
    if !caps.supports_output_format_json {
        detail.push_str("; WARNING: --output-format json not advertised");
    }
    detail.push_str("; ");
    detail.push_str(caps.turn_ceiling().describe());
    Finding {
        component: "claude-code",
        level: if caps.supports_model {
            Level::Ok
        } else {
            Level::Warn
        },
        detail,
    }
}

/// Whether THIS repository's declaration is granted, and what the grants
/// on this machine are for when it is not.
///
/// A grant is bound to the pair (declaration, repository), so a key that
/// does not match is not a broken grant — it is a grant for something
/// else, and saying which is the difference between "add a grant" and
/// "you already reviewed this, somewhere else" (P2).
pub(crate) fn trust_finding(
    policy: Option<&RepoPolicy>,
    settings: &MachineSettings,
    root: &std::path::Path,
) -> Finding {
    let Some(policy) = policy else {
        return Finding {
            component: "trust",
            level: Level::Warn,
            detail: "no readable relais.toml, so this repository has no declaration to grant"
                .into(),
        };
    };
    let identity = crate::repo::identity(root);
    let key = crate::policy::grant_key(&policy.authority_hash(), &identity);
    match settings.trust.get(&key) {
        Some(grant) => Finding {
            component: "trust",
            level: Level::Ok,
            detail: format!(
                "granted for {identity} by {} on {}",
                grant.reviewed_by, grant.granted_at
            ),
        },
        None => {
            let others: Vec<String> = settings
                .trust
                .values()
                .filter_map(|grant| grant.repo.clone())
                .collect();
            Finding {
                component: "trust",
                level: Level::Warn,
                detail: format!(
                    "no grant for this declaration in {identity} (key {key}); `relais plan` \
                     prints the block to review and paste. {}",
                    if others.is_empty() {
                        format!(
                            "{} grant(s) on this machine are for other declarations or \
                             repositories",
                            settings.trust.len()
                        )
                    } else {
                        format!("the grants on this machine are for: {}", others.join(", "))
                    }
                ),
            }
        }
    }
}

/// `[trials] enabled = true` promises randomized assignment with logged
/// propensities (SPEC §17). This release ships no replay command and no
/// propensity logging, so the flag does nothing at all. Saying so is the
/// whole point: a silent no-op reads as a running experiment.
/// A lockfile at the root and a profile with no setup step: the
/// profile's commands will run in a bare worktree, where the tree's own
/// dependencies MAY be missing — may, because a documentation profile
/// needs none, and relais cannot tell which kind this is. So the line is
/// a warning that names the block to declare, and never a failure, and
/// never something relais runs uninvited (SPEC §7). Nothing is said
/// without a lockfile: a Go or Rust repository has nothing to install,
/// and an "ok" line there would claim knowledge relais does not have.
/// Shared with `relais plan`, so the two say the same thing.
pub fn setup_finding(
    policy: &RepoPolicy,
    lockfiles: &[&crate::repo::Ecosystem],
) -> Option<Finding> {
    if lockfiles.is_empty() {
        return None;
    }
    let present = lockfiles
        .iter()
        .map(|ecosystem| ecosystem.lockfile)
        .collect::<Vec<_>>()
        .join(", ");
    let without: Vec<&String> = policy
        .verification
        .profiles
        .iter()
        .filter(|(_, profile)| profile.setup.is_empty())
        .map(|(name, _)| name)
        .collect();
    if without.is_empty() {
        return Some(Finding {
            component: "setup",
            level: Level::Ok,
            detail: format!("{present} present; every profile declares a setup step"),
        });
    }
    let suggested = lockfiles
        .iter()
        .map(|ecosystem| {
            format!(
                "{} → [[verification.profiles.<name>.setup]] argv = [{}]",
                ecosystem.lockfile,
                ecosystem
                    .setup_argv
                    .iter()
                    .map(|arg| format!("{arg:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let names = without
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(Finding {
        component: "setup",
        level: Level::Warn,
        detail: format!(
            "{present} present, and profile {names} declares no setup step: its commands run \
             in a bare worktree where the tree's dependencies may be unavailable. If they need \
             them, declare the install step (executable authority; changes the policy hash): \
             {suggested}"
        ),
    })
}

pub(crate) fn trials_finding(settings: &MachineSettings) -> Option<Finding> {
    settings.trials.enabled.then(|| Finding {
        component: "trials",
        level: Level::Warn,
        detail: "[trials] enabled = true, but routing trials are NOT implemented in this \
                 release: no replay command, no logged propensity, no randomized assignment. \
                 The flag has no effect — every run is routed by policy and the learner as \
                 if it were absent"
            .into(),
    })
}

/// Where the machine authority and the state actually live for this
/// invocation, and whether the environment moved them. An environment
/// that relocates `machine.toml` relocates the trust grants and the
/// spending ceilings with it, which is worth a `!` even when it is
/// exactly what the caller intended.
pub(crate) fn directory_findings() -> Vec<Finding> {
    let mut findings = Vec::new();
    match paths::home_dir() {
        Ok(_) => {}
        Err(e) => findings.push(Finding {
            component: "home",
            level: if paths::dir_override(paths::CONFIG_DIR_ENV).is_some()
                && paths::dir_override(paths::STATE_DIR_ENV).is_some()
            {
                // Both directories are given explicitly: nothing needs a
                // home directory, so this is informational.
                Level::Warn
            } else {
                Level::Fail
            },
            detail: e.to_string(),
        }),
    }
    // The resolver travels with its label, so the line can never name one
    // directory and print the other's path.
    type Resolve = fn() -> Result<std::path::PathBuf, paths::HomeUnset>;
    for (env, label, carries, resolve) in [
        (
            paths::CONFIG_DIR_ENV,
            "config",
            "the machine authority (trust grants, spending ceilings, permissions)",
            paths::config_dir as Resolve,
        ),
        (
            paths::STATE_DIR_ENV,
            "state",
            "the ledger, the artifact registry and every run's artifacts",
            paths::state_dir as Resolve,
        ),
    ] {
        let (level, detail) = match paths::dir_override(env) {
            Some(dir) => (
                Level::Warn,
                format!(
                    "{label} directory is {} — relocated by {env}, so {carries} come from there, \
                     not from the defaults under $HOME",
                    dir.display()
                ),
            ),
            None => (
                Level::Ok,
                match resolve() {
                    Ok(dir) => format!("{label} directory is {}", dir.display()),
                    // Reported once above as the `home` finding; here it
                    // is only the reason this line has no directory.
                    Err(_) => format!("{label} directory cannot be resolved without HOME"),
                },
            ),
        };
        findings.push(Finding {
            component: "directories",
            level,
            detail,
        });
    }
    findings
}

/// Probe the environment relative to one repository directory (the cwd in
/// practice) and the machine-owned settings.
pub fn doctor(repo_dir: &Path) -> DoctorReport {
    let mut findings = directory_findings();

    check_command("git", &["--version"], &mut findings, "git");

    let mut installed_claude_version: Option<String> = None;
    match crate::adapter::claude::ClaudeBackend::discover() {
        Err(e) => findings.push(Finding {
            component: "claude-code",
            level: Level::Fail,
            detail: e.to_string(),
        }),
        Ok(backend) => match backend.probe_report() {
            // A probe that errored says so: "did not answer" and
            // "answered and refused" are different things to fix.
            Err(failure) => findings.push(Finding {
                component: "claude-code",
                level: Level::Fail,
                detail: failure.to_string(),
            }),
            Ok(caps) => {
                installed_claude_version = caps.version.clone();
                findings.push(claude_code_finding(&caps));
            }
        },
    }
    findings.push(hook_compat_finding(installed_claude_version.as_deref()));

    let policy_path = repo_dir.join("relais.toml");
    let policy = match std::fs::read_to_string(&policy_path) {
        Err(_) => {
            findings.push(Finding {
                component: "relais.toml",
                level: Level::Fail,
                detail: format!(
                    "no relais.toml in {} (searched upward to the repository root) — run `relais init` there",
                    repo_dir.display()
                ),
            });
            None
        }
        Ok(text) => match RepoPolicy::from_toml_str(&text) {
            Ok(policy) => {
                let models = policy
                    .models
                    .keys()
                    .map(|tier| tier.as_str())
                    .collect::<Vec<_>>()
                    .join(",");
                findings.push(Finding {
                    component: "relais.toml",
                    level: Level::Ok,
                    detail: format!(
                        "valid; models configured: {}",
                        if models.is_empty() { "none" } else { &models }
                    ),
                });
                if !policy.verification.profiles.is_empty() {
                    findings.push(Finding {
                        component: "verify",
                        level: Level::Ok,
                        detail: format!(
                            "{} verification profile(s): {}",
                            policy.verification.profiles.len(),
                            policy
                                .verification
                                .profiles
                                .keys()
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    });
                } else {
                    findings.push(Finding {
                        component: "verify",
                        level: Level::Fail,
                        detail: "no verification profiles; acceptance has nothing to run".into(),
                    });
                }
                if let Some(finding) = setup_finding(&policy, &crate::repo::lockfiles(repo_dir)) {
                    findings.push(finding);
                }
                Some(policy)
            }
            Err(e) => {
                findings.push(Finding {
                    component: "relais.toml",
                    level: Level::Fail,
                    detail: e.to_string(),
                });
                None
            }
        },
    };

    // Integrations are judged at the declared modes, not probed blindly:
    // a repository that says `aval = "off"` is not broken for not having
    // aval installed (SPEC §5).
    match &policy {
        Some(policy) => {
            for (name, component, dependency) in [
                ("aval", "aval", policy.integrations.aval.as_ref()),
                ("amont", "amont", policy.integrations.amont.as_ref()),
                (
                    "amont_agent",
                    "amont-agent",
                    policy.integrations.amont_agent.as_ref(),
                ),
            ] {
                findings.push(integration_finding(
                    name,
                    component,
                    dependency,
                    &|bin| crate::tooling::binary_available(bin),
                    &|bin| crate::tooling::integration_version(bin),
                ));
            }
        }
        None => findings.push(Finding {
            component: "integrations",
            level: Level::Warn,
            detail: "no readable relais.toml, so the declared integration modes are unknown \
                     and nothing was probed"
                .into(),
        }),
    }

    // Without a home directory none of the machine-local paths resolve.
    // That is already the `home` finding above; here each dependent check
    // says it could not run rather than inventing a path.
    match paths::machine_settings_path() {
        Err(e) => findings.push(Finding {
            component: "machine.toml",
            level: Level::Fail,
            detail: e.to_string(),
        }),
        Ok(machine_path) => match std::fs::read_to_string(&machine_path) {
            Err(_) => findings.push(Finding {
                component: "machine.toml",
                level: Level::Warn,
                detail: format!(
                    "{} does not exist; every run will be blocked on the missing trust grant",
                    machine_path.display()
                ),
            }),
            // `from_toml_str` validates the ceilings and every grant's
            // reviewer and date (P10), so an invalid grant is named here
            // rather than counted among the valid ones.
            Ok(text) => match MachineSettings::from_toml_str(&text) {
                Ok(settings) => {
                    findings.push(Finding {
                        component: "machine.toml",
                        level: Level::Ok,
                        detail: format!("valid; {} trust grant(s)", settings.trust.len()),
                    });
                    findings.push(trust_finding(policy.as_ref(), &settings, repo_dir));
                    if let Some(trials) = trials_finding(&settings) {
                        findings.push(trials);
                    }
                    findings.push(hook_timeout_finding(repo_dir, &settings.admission));
                }
                Err(e) => findings.push(Finding {
                    component: "machine.toml",
                    level: Level::Fail,
                    detail: e.to_string(),
                }),
            },
        },
    }

    findings.push(ledger_finding());
    findings.push(registry_finding());
    findings.push(worktrees_finding_on_disk());
    findings.push(strays_finding_on_disk());
    findings.push(coordinator_finding());
    findings.push(hook_live_finding(repo_dir));

    DoctorReport { findings }
}

/// The hook compatibility record `relais doctor --probe-hooks` writes,
/// checked against the Claude Code actually on PATH. A record is
/// evidence about one version; trusting it for a different one silently
/// would mean a fixture built on 2.1.x reads as evidence for 2.3.x that
/// installed it (SPEC criteria). Never a blocker: nothing here stops a
/// run, and the record itself is optional.
fn hook_compat_finding(installed_version: Option<&str>) -> Finding {
    let path = match crate::hook::compat_record_path() {
        Ok(path) => path,
        Err(e) => {
            return Finding {
                component: "hook-compat",
                level: Level::Warn,
                detail: e.to_string(),
            }
        }
    };
    match crate::hook::read_compat_record(&path) {
        None => Finding {
            component: "hook-compat",
            level: Level::Warn,
            detail: format!(
                "no hook compatibility record yet — run `relais doctor --probe-hooks` \
                 (costs money, touches the network) to write {}",
                path.display()
            ),
        },
        Some(record) => match installed_version {
            Some(installed) if installed == record.claude_code_version => Finding {
                component: "hook-compat",
                level: Level::Ok,
                detail: format!(
                    "fresh: recorded for Claude Code {} at {}",
                    record.claude_code_version, record.observed_at
                ),
            },
            Some(installed) => Finding {
                component: "hook-compat",
                level: Level::Warn,
                detail: format!(
                    "stale: recorded for Claude Code {}, installed is {installed} — \
                     re-run `relais doctor --probe-hooks`",
                    record.claude_code_version
                ),
            },
            None => Finding {
                component: "hook-compat",
                level: Level::Warn,
                detail: format!(
                    "recorded for Claude Code {}, but the installed version could not be \
                     read — re-run `relais doctor --probe-hooks` once it can",
                    record.claude_code_version
                ),
            },
        },
    }
}

/// What exercising a recorded hook command found. `relais doctor`
/// distinguishes a case above this one — no command recorded at all —
/// before this type ever comes into it; these are what a command that IS
/// recorded can turn out to be, and [`HookHealth::RecordedButDidNotRefuse`]
/// is the dangerous one: a dead guard reads as enforcement to anyone who
/// has not gone looking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HookHealth {
    /// The recorded command was spawned with a fixture spawn payload
    /// against a scratch environment configured to refuse when the
    /// coordinator is unreachable, and it did: exit 0, a deny on stdout.
    Refused,
    /// The recorded command ran, and did not print a deny — wired in and
    /// enforcing nothing. `how` is how it ended, because a hook that
    /// exits non-zero, times out or dies on a signal is a different
    /// defect from one that exits 0 with nothing to say, and reporting
    /// them as one sends a person looking in the wrong place.
    RecordedButDidNotRefuse { how: String },
    /// The exercise never happened: the scratch environment could not be
    /// staged, or the command could not be spawned at all (a recorded
    /// path that no longer exists is the common case). Kept apart from
    /// the case above on purpose — "it ran and enforced nothing" is a
    /// claim about the hook, and asserting it off a run that never
    /// happened is a lie doctor would be telling about the one thing it
    /// was asked to establish.
    CouldNotExercise { why: String },
}

/// The fixture `relais doctor` feeds the recorded hook on stdin: a
/// `PreToolUse` spawn of the Agent tool that names no run the scratch
/// coordinator (there is none reachable) could possibly know about,
/// so a working hook has something concrete to refuse.
fn hook_probe_payload() -> Vec<u8> {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "relais-doctor-hook-probe",
        "tool_name": "Agent",
        "tool_use_id": "relais-doctor-hook-probe-1",
    })
    .to_string()
    .into_bytes()
}

/// Whether a process end amounts to a live refusal: it exited zero (a
/// non-zero exit from a hook corrupts the tool call it was watching, so
/// relais's own hook exits 0 on every path and a broken one often does
/// too — the payload is what actually distinguishes them) and it printed
/// the `permissionDecision` the whole payload shape hinges on.
///
/// Takes the whole end rather than a flag: how it ended is part of the
/// answer, and a bool would have thrown it away exactly where a person
/// reading the finding needs it.
pub(crate) fn hook_health_from_probe(end: &crate::procs::ProcessEnd) -> HookHealth {
    if end.ended.succeeded() && end.stdout.contains("\"permissionDecision\":\"deny\"") {
        return HookHealth::Refused;
    }
    HookHealth::RecordedButDidNotRefuse {
        how: end.ended.describe(),
    }
}

/// Spawn the exact command recorded in settings.json (never anything
/// this crate reconstructs) with the fixture payload on stdin, its
/// config and state directories redirected to a scratch directory of
/// their own so this exercise never touches the person's real hook
/// journal or reserves a seat in their real coordinator, and a
/// machine.toml there that refuses admission when the coordinator
/// cannot be reached — the scratch state directory holds no coordinator
/// socket, so it never can be.
fn probe_recorded_hook(command: &str) -> HookHealth {
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "{}doctor-hook-probe-{}-{}",
        crate::test_support::SCRATCH_PREFIX,
        std::process::id(),
        chrono::Utc::now().format("%Y%m%dT%H%M%S%f")
    )));
    let config_dir = scratch.0.join("config");
    let state_dir = scratch.0.join("state");
    stage_and_run(command, &config_dir, &state_dir)
}

/// The probe's scratch directory, removed when it drops.
///
/// A `Drop` and not a trailing `remove_dir_all`: this is the one place
/// `relais doctor` — production, running for a person — creates a
/// directory of this shape, and a trailing statement does not run if
/// anything above it panics. The tool that reports stranded scratch
/// directories should not be a source of them. It carries
/// `SCRATCH_PREFIX` so that if one does survive a kill, the same scan
/// counts it.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best effort, and deliberately not reported: the probe's answer
        // is about the hook, and a directory that outlives it says
        // nothing about that answer.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Stage the scratch environment and run the command in it. Split out so
/// every way the staging can fail arrives as [`HookHealth::CouldNotExercise`]
/// carrying the reason, rather than being flattened into a verdict about
/// a hook that was never spawned.
fn stage_and_run(command: &str, config_dir: &Path, state_dir: &Path) -> HookHealth {
    for dir in [config_dir, state_dir] {
        if let Err(e) = std::fs::create_dir_all(dir) {
            return HookHealth::CouldNotExercise {
                why: format!("could not create {}: {e}", dir.display()),
            };
        }
    }
    let machine_toml = config_dir.join("machine.toml");
    if let Err(e) = std::fs::write(
        &machine_toml,
        "schema_version = 1\n\n[admission]\non_coordinator_unreachable = \"refuse\"\n",
    ) {
        return HookHealth::CouldNotExercise {
            why: format!("could not write {}: {e}", machine_toml.display()),
        };
    }
    let mut process = shell_command(command);
    process.env(paths::CONFIG_DIR_ENV, config_dir);
    process.env(paths::STATE_DIR_ENV, state_dir);
    match crate::procs::run_with_timeout(
        process,
        Duration::from_secs(10),
        Some(hook_probe_payload()),
        None,
        None,
    ) {
        Ok(end) => hook_health_from_probe(&end),
        Err(e) => HookHealth::CouldNotExercise {
            why: format!("could not run it: {e}"),
        },
    }
}

/// A shell invocation of a recorded hook command string, the way Claude
/// Code itself launches one: `sh -c` on Unix, `cmd /C` on Windows.
#[cfg(unix)]
fn shell_command(command: &str) -> std::process::Command {
    let mut process = std::process::Command::new("sh");
    process.arg("-c").arg(command);
    process
}

#[cfg(windows)]
fn shell_command(command: &str) -> std::process::Command {
    let mut process = std::process::Command::new("cmd");
    process.args(["/C", command]);
    process
}

/// The `PreToolUse` command relais itself would have recorded: a
/// `{"type":"command","command":"…"}` hook whose command ends in
/// ` hook` — the exact shape `install::settings::apply_hooks` writes,
/// read back rather than reconstructed, so this never drifts from what
/// install actually does. `PreToolUse` is the only phase that can still
/// refuse a tool call (`hook::decide`'s module doc), so it is the only
/// one worth exercising.
pub(crate) fn recorded_hook_command(settings_text: &str) -> Option<String> {
    let value: Value = serde_json::from_str(settings_text).ok()?;
    let entries = value.pointer("/hooks/PreToolUse")?.as_array()?;
    entries.iter().find_map(|entry| {
        entry.get("hooks")?.as_array()?.iter().find_map(|hook| {
            if hook.get("type")?.as_str()? != "command" {
                return None;
            }
            let command = hook.get("command")?.as_str()?;
            command
                .trim_end()
                .ends_with(" hook")
                .then(|| command.to_string())
        })
    })
}

/// The `timeout` recorded on the same `PreToolUse` leaf
/// [`recorded_hook_command`] finds — `Some(None)` when the leaf exists
/// but carries no `timeout` field at all, which a settings file written
/// before `timeout` existed leaves exactly that way.
pub(crate) fn recorded_pretooluse_timeout_secs(settings_text: &str) -> Option<Option<u64>> {
    let value: Value = serde_json::from_str(settings_text).ok()?;
    let entries = value.pointer("/hooks/PreToolUse")?.as_array()?;
    entries.iter().find_map(|entry| {
        entry.get("hooks")?.as_array()?.iter().find_map(|hook| {
            if hook.get("type")?.as_str()? != "command" {
                return None;
            }
            let command = hook.get("command")?.as_str()?;
            command
                .trim_end()
                .ends_with(" hook")
                .then(|| hook.get("timeout").and_then(Value::as_u64))
        })
    })
}

/// The hook cannot read its own handler timeout — nothing in a hook
/// payload carries it — so a hand-edited or stale settings.json silently
/// converts every capped spawn into a fail-open admission the moment the
/// recorded timeout stops covering the configured wait: the hook then
/// waits `queue_wait_secs` regardless, and the harness kills it before it
/// can withdraw and print, deciding nothing and running the agent anyway
/// (the false "expired hook fails open" SPEC §23 records). `doctor` is
/// the one place that CAN read both sides — the file and the machine
/// settings — so it is the one place that can catch it, as a FAILURE
/// rather than a warning: this is the one outcome the whole package
/// exists to prevent.
fn hook_timeout_finding(
    repo_dir: &Path,
    admission: &crate::policy::HookAdmissionSettings,
) -> Finding {
    let queue_wait = match admission.queue_behaviour() {
        crate::policy::QueueBehaviour::RefuseImmediately => Duration::ZERO,
        crate::policy::QueueBehaviour::WaitUpTo(wait) => wait,
    };
    let required = crate::install::settings::derived_pretooluse_timeout(queue_wait).as_secs();
    let mut candidates = vec![repo_dir.join(".claude").join("settings.json")];
    if let Ok(home) = paths::home_dir() {
        candidates.push(home.join(".claude").join("settings.json"));
    }
    let recorded = candidates.iter().find_map(|path| {
        let text = std::fs::read_to_string(path).ok()?;
        recorded_pretooluse_timeout_secs(&text).map(|timeout| (path.clone(), timeout))
    });
    match recorded {
        None => Finding {
            component: "hook-timeout",
            level: Level::Warn,
            detail: "no relais command is recorded on PreToolUse in any settings.json this \
                     repository can see, so there is no recorded timeout to check — see \
                     `hook-live`"
                .into(),
        },
        Some((path, Some(timeout))) if timeout >= required => Finding {
            component: "hook-timeout",
            level: Level::Ok,
            detail: format!(
                "{} records a PreToolUse timeout of {timeout}s, which covers the {required}s \
                 this machine's queue_wait_secs currently requires",
                path.display()
            ),
        },
        Some((path, Some(timeout))) => Finding {
            component: "hook-timeout",
            level: Level::Fail,
            detail: format!(
                "{} records a PreToolUse timeout of {timeout}s, but this machine's \
                 queue_wait_secs now requires at least {required}s — an expired hook fails \
                 OPEN (SPEC §23), so a queued spawn can be admitted without relais ever \
                 deciding; run `relais install --claude --hooks --write` to correct it, or \
                 lower `queue_wait_secs` under `[admission]` in machine.toml",
                path.display()
            ),
        },
        Some((path, None)) => Finding {
            component: "hook-timeout",
            level: Level::Fail,
            detail: format!(
                "{} records a PreToolUse hook with no timeout field at all — Claude Code then \
                 waits for it indefinitely (300s measured, and nothing suggests that is a \
                 ceiling), which is worse than any recorded number; run \
                 `relais install --claude --hooks --write` to add one",
                path.display()
            ),
        },
    }
}

/// `relais doctor` exercising the live hook (SPEC criteria), checked
/// against every settings.json scope this repository can see: the
/// project's own `.claude/settings.json` first, the user's
/// `~/.claude/settings.json` otherwise. Never part of `--probe-hooks`:
/// that command needs a real Claude Code session and costs money; this
/// spawns nothing but the hook binary itself.
fn hook_live_finding(repo_dir: &Path) -> Finding {
    let mut candidates = vec![repo_dir.join(".claude").join("settings.json")];
    if let Ok(home) = paths::home_dir() {
        candidates.push(home.join(".claude").join("settings.json"));
    }
    let recorded = candidates.iter().find_map(|path| {
        let text = std::fs::read_to_string(path).ok()?;
        recorded_hook_command(&text).map(|command| (path.clone(), command))
    });
    match recorded {
        None => Finding {
            component: "hook-live",
            level: Level::Warn,
            detail: "no relais command is recorded on PreToolUse in any settings.json this \
                     repository can see — wire it with \
                     `relais install --claude --hooks --write`"
                .into(),
        },
        Some((path, command)) => match probe_recorded_hook(&command) {
            HookHealth::Refused => Finding {
                component: "hook-live",
                level: Level::Ok,
                detail: format!(
                    "recorded in {} and exercised: it refused a fixture spawn against an \
                     unreachable coordinator, as configured",
                    path.display()
                ),
            },
            HookHealth::RecordedButDidNotRefuse { how } => Finding {
                component: "hook-live",
                level: Level::Fail,
                detail: format!(
                    "recorded in {} as `{command}`, but exercising it with a fixture spawn \
                     against an unreachable coordinator did not produce a deny ({how}) — this \
                     hook is wired in and enforces nothing",
                    path.display()
                ),
            },
            // Not a verdict on the hook: doctor could not run it, and
            // says so. Reporting this as "enforces nothing" would be a
            // claim about a run that never happened.
            HookHealth::CouldNotExercise { why } => Finding {
                component: "hook-live",
                level: Level::Warn,
                detail: format!(
                    "recorded in {} as `{command}`, but it could not be exercised, so whether \
                     it refuses is unknown: {why}",
                    path.display()
                ),
            },
        },
    }
}

/// The ledger line. A ledger that opens but whose schema version cannot
/// be read is NOT a pass: relais writes every run through this database,
/// and "schema unreadable" means it could not tell whether the file it is
/// about to write is the shape it expects (C4).
fn ledger_finding() -> Finding {
    let path = match paths::ledger_path() {
        Ok(path) => path,
        Err(e) => {
            return Finding {
                component: "ledger",
                level: Level::Fail,
                detail: e.to_string(),
            }
        }
    };
    match Ledger::open(&path) {
        Err(e) => Finding {
            component: "ledger",
            level: Level::Fail,
            detail: e.to_string(),
        },
        Ok(ledger) => ledger_schema_finding(&path, ledger.schema_version()),
    }
}

/// The verdict on a ledger that opened, given what reading its schema
/// version said. Split out from the path lookup so the unreadable arm has
/// a test that needs no database on disk.
pub(crate) fn ledger_schema_finding(
    path: &Path,
    schema: Result<u64, crate::ledger::LedgerError>,
) -> Finding {
    match schema {
        Ok(version) => Finding {
            component: "ledger",
            level: Level::Ok,
            detail: format!("schema v{version} at {}", path.display()),
        },
        Err(e) => Finding {
            component: "ledger",
            level: Level::Fail,
            detail: format!(
                "the ledger at {} opened but its schema version could not be read: {e}",
                path.display()
            ),
        },
    }
}

/// The retained run worktrees under the state directory, read from disk.
fn worktrees_finding_on_disk() -> Finding {
    let state_dir = match paths::state_dir() {
        Ok(dir) => dir,
        Err(e) => {
            return Finding {
                component: "worktrees",
                level: Level::Fail,
                detail: e.to_string(),
            }
        }
    };
    worktrees_finding(crate::workspace::retained_worktrees(&state_dir))
}

/// The verdict on the retained worktrees, given what the scan found.
///
/// A run's worktree is retired at its end (SPEC §8), so any still on
/// disk is an older release's, an interrupted run's, or a retirement
/// that failed — each a directory of build output nothing reads, and
/// one `git worktree list` entry in its repository. Never a blocker:
/// nothing about them stops a run. A scan that could not run is a
/// failure to say so, not a clean state.
pub(crate) fn worktrees_finding(
    retained: Result<Vec<crate::workspace::RetainedWorktree>, crate::workspace::WorkspaceError>,
) -> Finding {
    match retained {
        Ok(retained) if retained.is_empty() => Finding {
            component: "worktrees",
            level: Level::Ok,
            detail: "no run worktree is retained".into(),
        },
        Ok(retained) => {
            let bytes: u64 = retained.iter().map(|worktree| worktree.bytes).sum();
            Finding {
                component: "worktrees",
                level: Level::Warn,
                detail: format!(
                    "{} run worktree(s) retained, {bytes} bytes; `relais resume --retire` \
                     names what no candidate holds and removes them",
                    retained.len()
                ),
            }
        }
        Err(e) => Finding {
            component: "worktrees",
            level: Level::Fail,
            detail: format!("the retained worktrees could not be listed: {e}"),
        },
    }
}

/// The scratch directories `test_support::short_temp_dir` and the
/// integration suites' own world types leave under `/tmp` when a test
/// run is killed before its guard can drop — the guard
/// (`temp-dir-lifetime`) stops new ones; it does nothing for the
/// thousands a machine this old already carries.
fn strays_finding_on_disk() -> Finding {
    // BOTH roots a test helper can strand a directory under:
    // `short_temp_dir` goes to `/tmp` (a socket path's 104-byte cap), and
    // `temp_dir` to `std::env::temp_dir()`, which on macOS is
    // `/var/folders/...` — scanning only `/tmp` reported a clean machine
    // while the other root filled up, which is the "believed zero" this
    // finding exists to prevent. The same root twice is counted once.
    let tmp = PathBuf::from("/tmp");
    let env_tmp = std::env::temp_dir();
    let mut roots = vec![tmp];
    if !roots.contains(&env_tmp) {
        roots.push(env_tmp);
    }
    strays_finding(count_strays_under(&roots))
}

/// The counts across several roots, folded. Any root that cannot be read
/// fails the whole scan rather than being skipped: a partial count
/// reported as a total is the shape of answer this finding must not give.
fn count_strays_under(roots: &[PathBuf]) -> std::io::Result<(u64, u64)> {
    let mut count = 0;
    let mut bytes = 0;
    for root in roots {
        let (c, b) = count_strays(root)?;
        count += c;
        bytes += b;
    }
    Ok((count, bytes))
}

/// The verdict on `/tmp/{SCRATCH_PREFIX}*`, given what the scan found.
/// Never a blocker: a stray only costs disk. A scan that could not run
/// is a failure to say so, not a clean state.
pub(crate) fn strays_finding(strays: std::io::Result<(u64, u64)>) -> Finding {
    match strays {
        Ok((0, _)) => Finding {
            component: "temp-strays",
            level: Level::Ok,
            detail: format!(
                "no /tmp/{}* directory is left over",
                crate::test_support::SCRATCH_PREFIX
            ),
        },
        Ok((count, bytes)) => Finding {
            component: "temp-strays",
            level: Level::Warn,
            detail: format!(
                "{count} /tmp/{}* director{} left over from a killed test run, {bytes} \
                 bytes; safe to remove by hand",
                crate::test_support::SCRATCH_PREFIX,
                if count == 1 { "y" } else { "ies" }
            ),
        },
        // A count of leftover scratch directories is information, never a
        // blocker: `DoctorReport::failed()` treats `Fail` as one, and
        // this finding's own text tells a person the strays are "safe to
        // remove by hand". Failing doctor because a shared, churning
        // `/tmp` could not be read would stop a run over something that
        // is not about the run at all.
        Err(e) => Finding {
            component: "temp-strays",
            level: Level::Warn,
            detail: format!("/tmp could not be scanned for leftover test directories: {e}"),
        },
    }
}

/// The count and total bytes of `{SCRATCH_PREFIX}*` directories directly
/// under `root` — the one prefix `test_support::short_temp_dir`, the
/// integration suites' own world types and this scan all read from
/// [`crate::test_support::SCRATCH_PREFIX`], so this counts exactly what
/// any of them can strand. `pub`, not `pub(crate)`: an integration suite
/// under `tests/` proves its own world is countable by calling this
/// directly. A root that does not exist (no `/tmp` on this platform)
/// counts as none, not a failure.
/// The prefix `short_temp_dir` wrote before `SCRATCH_PREFIX` unified it
/// with the integration suites'. Counted alongside the current one so a
/// machine that ran relais in between is not told it is clean.
const LEGACY_SCRATCH_PREFIX: &str = "relais-";

pub fn count_strays(root: &Path) -> std::io::Result<(u64, u64)> {
    if !root.is_dir() {
        return Ok((0, 0));
    }
    let mut count = 0u64;
    let mut bytes = 0u64;
    for entry in std::fs::read_dir(root)? {
        // Per-entry errors are SKIPPED, not propagated. `/tmp` is shared
        // with every process on the machine, so an entry can vanish
        // between the listing and the stat through no fault of this
        // scan; treating that as a failed scan would report nothing
        // about the thing being counted. Undercounting by one stray is
        // the right trade against that.
        let Ok(entry) = entry else { continue };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        // The current prefix AND the one `short_temp_dir` used before it
        // was unified: a machine that ran this suite in between carries
        // `relais-*` directories, and a scan that stopped counting them
        // the moment the prefix changed would tell that machine it was
        // clean — the same false zero this finding exists to prevent.
        // Droppable once no machine can still be carrying them.
        let stray = name.starts_with(crate::test_support::SCRATCH_PREFIX)
            || name.starts_with(LEGACY_SCRATCH_PREFIX);
        if stray && file_type.is_dir() {
            count += 1;
            bytes += crate::workspace::dir_size(&entry.path()).unwrap_or(0);
        }
    }
    Ok((count, bytes))
}

/// The learned-artifact registry. An absent directory is the cold start,
/// not a fault: routing abstains to the conservative baseline.
fn registry_finding() -> Finding {
    match paths::registry_dir() {
        Err(e) => Finding {
            component: "registry",
            level: Level::Fail,
            detail: e.to_string(),
        },
        Ok(registry) if registry.is_dir() => Finding {
            component: "registry",
            level: Level::Ok,
            detail: format!("{} artifacts directory present", registry.display()),
        },
        Ok(registry) => Finding {
            component: "registry",
            level: Level::Warn,
            detail: format!(
                "no artifacts at {} yet (learned routing will abstain to the conservative \
                 baseline)",
                registry.display()
            ),
        },
    }
}

/// The coordinator. Not running is normal — it starts on the first
/// managed dispatch — so an unanswered socket is a `!`, never a `✗`.
fn coordinator_finding() -> Finding {
    let socket = match crate::coordinator::socket_path() {
        Ok(socket) => socket,
        Err(e) => {
            return Finding {
                component: "coordinator",
                level: Level::Fail,
                detail: e.to_string(),
            }
        }
    };
    match crate::coordinator::Client::new(socket.clone()).ping() {
        Ok(pid) => Finding {
            component: "coordinator",
            level: Level::Ok,
            detail: format!("coordinator pid {pid} answers on {}", socket.display()),
        },
        Err(_) if socket.exists() => Finding {
            component: "coordinator",
            level: Level::Warn,
            detail: format!(
                "socket {} exists but nobody answers; the next managed dispatch takes it over",
                socket.display()
            ),
        },
        Err(_) => Finding {
            component: "coordinator",
            level: Level::Warn,
            detail: "no coordinator running; it starts lazily on the first managed dispatch"
                .to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::claude::capabilities_from_help;
    use std::path::PathBuf;

    const HELP_2_1: &str = "usage: claude -p --model <model> --effort <level> \
         --output-format <format> --max-budget-usd <amount> \
         --disallowed-tools <tools...> --settings <file-or-json>";

    fn dependency(mode: &str) -> Dependency {
        toml::from_str::<std::collections::BTreeMap<String, Dependency>>(&format!(
            "d = \"{mode}\"\n"
        ))
        .expect("parses")
        .remove("d")
        .expect("d")
    }

    #[test]
    fn the_turn_ceiling_line_reports_what_the_harness_can_take() {
        // The installed Claude Code: SPEC §11 promises a turn ceiling it
        // has no flag for, so doctor says which ceilings are real.
        let finding = claude_code_finding(&capabilities_from_help("2.1.278".into(), HELP_2_1));
        assert_eq!(finding.level, Level::Ok);
        assert!(
            finding.detail.contains(
                "turn ceiling: not available on this Claude Code — attempts and wall time \
                 are enforced by the runner, turns are not"
            ),
            "{}",
            finding.detail
        );
        let older = claude_code_finding(&capabilities_from_help(
            "1.0".into(),
            "--model --output-format --max-turns",
        ));
        assert!(
            older
                .detail
                .contains("turn ceiling: enforced by the harness"),
            "{}",
            older.detail
        );
    }

    #[test]
    fn integrations_are_judged_at_the_declared_mode() {
        let missing = |_: &str| false;
        let present = |_: &str| true;
        let no_version = |_: &str| None;
        let version = |_: &str| Some("1.2.3".to_string());

        let required_missing = integration_finding(
            "aval",
            "aval",
            Some(&dependency("required")),
            &missing,
            &no_version,
        );
        assert_eq!(required_missing.level, Level::Fail);

        let optional_missing = integration_finding(
            "amont",
            "amont",
            Some(&dependency("optional")),
            &missing,
            &no_version,
        );
        assert_eq!(optional_missing.level, Level::Warn);
        assert!(
            optional_missing.detail.contains("not a pass"),
            "{}",
            optional_missing.detail
        );

        // `off` is not probed at all, and the line says so rather than
        // failing a repository that declared it does not use the tool.
        let off = integration_finding(
            "aval",
            "aval",
            Some(&dependency("off")),
            &missing,
            &no_version,
        );
        assert_eq!(off.level, Level::Ok);
        assert!(off.detail.contains("not checked"), "{}", off.detail);

        let undeclared =
            integration_finding("amont_agent", "amont-agent", None, &missing, &no_version);
        assert_eq!(undeclared.level, Level::Ok);
        assert!(
            undeclared.detail.contains("not declared"),
            "{}",
            undeclared.detail
        );

        let ok = integration_finding(
            "amont_agent",
            "amont-agent",
            Some(&dependency("required")),
            &present,
            &version,
        );
        assert_eq!(ok.level, Level::Ok);
        assert!(
            ok.detail.contains("`amont-agent` 1.2.3"),
            "the config key is amont_agent, the binary amont-agent: {}",
            ok.detail
        );
    }

    /// A lockfile with no setup declared warns and names the block; a
    /// declared setup is reported as ok; no lockfile says nothing.
    #[test]
    fn a_lockfile_without_a_declared_setup_is_a_warning_not_a_failure() {
        let toml = "schema_version = 1\n\
                    [[verification.profiles.default.commands]]\n\
                    argv = [\"npm\", \"test\"]\n";
        let policy = RepoPolicy::from_toml_str(toml).expect("policy parses");
        assert!(
            setup_finding(&policy, &[]).is_none(),
            "a Cargo or Go repository has nothing to install and gets no line"
        );
        let npm = crate::repo::ECOSYSTEMS
            .iter()
            .find(|e| e.lockfile == "package-lock.json")
            .expect("npm is an ecosystem");
        let finding = setup_finding(&policy, &[npm]).expect("a line");
        assert_eq!(finding.component, "setup");
        assert_eq!(
            finding.level,
            Level::Warn,
            "never a failure: a docs profile may need nothing"
        );
        assert!(
            finding.detail.contains("package-lock.json present"),
            "{}",
            finding.detail
        );
        assert!(finding.detail.contains("`default`"), "{}", finding.detail);
        assert!(
            finding.detail.contains("may be unavailable"),
            "{}",
            finding.detail
        );
        assert!(
            finding.detail.contains("argv = [\"npm\", \"ci\"]"),
            "{}",
            finding.detail
        );
        assert!(
            !finding.detail.contains('\n'),
            "one line: {}",
            finding.detail
        );

        let declared = RepoPolicy::from_toml_str(&format!(
            "schema_version = 1\n\
             [[verification.profiles.default.setup]]\n\
             argv = [\"npm\", \"ci\"]\n{}",
            toml.trim_start_matches("schema_version = 1\n")
        ))
        .expect("policy parses");
        let finding = setup_finding(&declared, &[npm]).expect("a line");
        assert_eq!(finding.level, Level::Ok, "{}", finding.detail);
    }

    #[test]
    fn an_enabled_trial_envelope_is_reported_as_inert() {
        let mut settings =
            MachineSettings::from_toml_str("schema_version = 1\n").expect("machine parses");
        assert!(
            trials_finding(&settings).is_none(),
            "nothing to say when the envelope is off"
        );
        settings.trials.enabled = true;
        let finding = trials_finding(&settings).expect("a line");
        assert_eq!(finding.level, Level::Warn);
        assert!(
            finding.detail.contains("NOT implemented in this release"),
            "{}",
            finding.detail
        );
        assert!(finding.detail.contains("no effect"), "{}", finding.detail);
    }

    #[test]
    fn relocated_directories_are_named_and_flagged() {
        let findings = directory_findings();
        let directories: Vec<&Finding> = findings
            .iter()
            .filter(|finding| finding.component == "directories")
            .collect();
        assert_eq!(directories.len(), 2, "config and state");
        for finding in directories {
            match paths::dir_override(paths::CONFIG_DIR_ENV).is_some()
                || paths::dir_override(paths::STATE_DIR_ENV).is_some()
            {
                // Under `cargo test` neither override is normally set;
                // the release scenarios set both, and then the line must
                // say the environment moved the machine authority.
                true if finding.level == Level::Warn => {
                    assert!(
                        finding.detail.contains("relocated by"),
                        "{}",
                        finding.detail
                    )
                }
                _ => assert!(
                    finding.detail.contains("directory is"),
                    "{}",
                    finding.detail
                ),
            }
        }
    }

    /// A ledger that opens but cannot say what schema it is was reported
    /// as `✓ ok` and exited 0 (C4): relais writes every run through this
    /// database, and "unreadable" is not a pass.
    #[test]
    fn an_unreadable_ledger_schema_is_a_failure_with_the_reason() {
        let path = Path::new("/somewhere/ledger.sqlite");
        let good = ledger_schema_finding(path, Ok(7));
        assert_eq!(good.level, Level::Ok);
        assert!(good.ok());
        assert!(good.detail.contains("schema v7"), "{}", good.detail);

        let bad = ledger_schema_finding(
            path,
            Err(crate::ledger::LedgerError::Corrupt {
                what: "the migration count".into(),
                detail: "no such table: schema_migrations".into(),
            }),
        );
        assert_eq!(bad.level, Level::Fail, "{}", bad.detail);
        assert!(!bad.ok());
        // The error text travels with the finding, or the reader has
        // nothing to act on.
        assert!(
            bad.detail.contains("no such table: schema_migrations"),
            "{}",
            bad.detail
        );
        assert!(
            DoctorReport {
                findings: vec![bad]
            }
            .failed(),
            "an unreadable schema makes `relais doctor` exit non-zero"
        );
    }

    /// `render` used to fall through `_ => "✗"`, so a level it did not
    /// recognise printed as a blocker while `failed()` ignored it. The
    /// enum makes the two agree by construction.
    #[test]
    fn every_level_renders_as_its_own_mark() {
        let marks: Vec<&str> = [Level::Ok, Level::Warn, Level::Fail]
            .iter()
            .map(|level| level.mark())
            .collect();
        assert_eq!(marks, vec!["✓", "!", "✗"]);
        let report = DoctorReport {
            findings: vec![
                Finding {
                    component: "a",
                    level: Level::Ok,
                    detail: "fine".into(),
                },
                Finding {
                    component: "b",
                    level: Level::Warn,
                    detail: "a gap".into(),
                },
            ],
        };
        assert!(!report.failed(), "a warning is not a blocker");
        let rendered = report.render();
        assert!(rendered.contains("✓ a"), "{rendered}");
        assert!(rendered.contains("! b"), "{rendered}");
        assert!(!rendered.contains("fix the ✗"), "{rendered}");
        // The JSON form carries the level as the same three words the
        // text form is built from.
        let json = serde_json::to_string(&report).expect("serializes");
        assert!(json.contains("\"level\":\"ok\""), "{json}");
        assert!(json.contains("\"level\":\"warn\""), "{json}");
    }

    /// Retained worktrees are a `!` that names the sweep and the size,
    /// never a blocker; none is a pass; a scan that could not run says so.
    #[test]
    fn retained_worktrees_are_a_warning_naming_the_sweep() {
        use crate::workspace::{RetainedWorktree, WorkspaceError};
        let none = worktrees_finding(Ok(Vec::new()));
        assert_eq!(none.level, Level::Ok, "{}", none.detail);
        let some = worktrees_finding(Ok(vec![
            RetainedWorktree {
                run_id: "run-1".into(),
                path: PathBuf::from("/state/worktrees/run-1/task"),
                bytes: 1000,
            },
            RetainedWorktree {
                run_id: "run-2".into(),
                path: PathBuf::from("/state/runs/run-2/worktree"),
                bytes: 24,
            },
        ]));
        assert_eq!(some.level, Level::Warn, "{}", some.detail);
        assert!(some.detail.contains("2 run worktree(s)"), "{}", some.detail);
        assert!(some.detail.contains("1024 bytes"), "{}", some.detail);
        assert!(
            some.detail.contains("relais resume --retire"),
            "{}",
            some.detail
        );
        let unscanned = worktrees_finding(Err(WorkspaceError::Git("boom".into())));
        assert_eq!(unscanned.level, Level::Fail);
        assert!(unscanned.detail.contains("boom"), "{}", unscanned.detail);
    }

    /// Leftover `/tmp/rl-*` directories are a `!` naming the count
    /// and the bytes, never a blocker; none is a pass; a scan that could
    /// not run says so.
    #[test]
    fn temp_strays_are_a_warning_naming_the_count_and_bytes() {
        let none = strays_finding(Ok((0, 0)));
        assert_eq!(none.level, Level::Ok, "{}", none.detail);
        let some = strays_finding(Ok((2, 1024)));
        assert_eq!(some.level, Level::Warn, "{}", some.detail);
        assert!(some.detail.contains("2 /tmp/rl-*"), "{}", some.detail);
        assert!(some.detail.contains("1024 bytes"), "{}", some.detail);
        let unscanned = strays_finding(Err(std::io::Error::other("boom")));
        assert_eq!(
            unscanned.level,
            Level::Warn,
            "a count of scratch directories is information, never a blocker: {}",
            unscanned.detail
        );
        assert!(unscanned.detail.contains("boom"), "{}", unscanned.detail);

        // The claim in this finding's own text — "safe to remove by
        // hand" — is only true if it cannot stop a run. `/tmp` is shared
        // with every process on the machine, so an unreadable entry
        // there says nothing about the work doctor was asked about.
        let report = DoctorReport {
            findings: vec![strays_finding(Err(std::io::Error::other("boom")))],
        };
        assert!(!report.failed(), "an unscannable /tmp must not fail doctor");
    }

    /// `count_strays` counts only `{SCRATCH_PREFIX}*` directories
    /// directly under the root, and a root that does not exist is none,
    /// not a failure.
    #[test]
    fn count_strays_finds_only_scratch_prefixed_directories() {
        let dir = crate::test_support::short_temp_dir("doctor-strays");
        let root = dir.to_path_buf();
        std::fs::create_dir(root.join("rl-a-1-1")).expect("a stray");
        std::fs::write(root.join("rl-a-1-1").join("f"), b"12345").expect("a file");
        std::fs::create_dir(root.join("rl-b-1-2")).expect("another stray");
        std::fs::write(root.join("not-rl"), b"ignored").expect("a non-stray file");
        // A directory `short_temp_dir` left before the prefix was
        // unified. A machine that ran relais between the two changes
        // carries these, and stopping counting them the moment the
        // prefix changed would report that machine clean.
        std::fs::create_dir(root.join("relais-old-1-3")).expect("a legacy stray");
        let (count, bytes) = count_strays(&root).expect("scan");
        assert_eq!(count, 3, "both the current prefix and the legacy one");
        assert_eq!(bytes, 5);
        assert_eq!(
            count_strays(&root.join("absent")).expect("absent is none"),
            (0, 0)
        );
    }

    #[test]
    fn recorded_hook_command_finds_a_pretooluse_relais_command_and_ignores_others() {
        let settings = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "Bash", "hooks": [{"type": "command", "command": "/usr/bin/lint"}]},
                    {"matcher": "Agent|Task", "hooks": [
                        {"type": "command", "command": "/opt/relais/bin/relais hook --probe --record /tmp/x"},
                        {"type": "command", "command": "/opt/relais/bin/relais hook"}
                    ]}
                ]
            }
        })
        .to_string();
        assert_eq!(
            recorded_hook_command(&settings),
            Some("/opt/relais/bin/relais hook".to_string())
        );
    }

    #[test]
    fn recorded_hook_command_is_none_without_a_plain_hook_invocation() {
        let settings = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"hooks": [{"type": "command", "command": "/opt/relais/bin/relais hook --probe --record /tmp/x"}]}
                ]
            }
        })
        .to_string();
        assert_eq!(recorded_hook_command(&settings), None);
        assert_eq!(recorded_hook_command("{}"), None);
        assert_eq!(recorded_hook_command("not json"), None);
    }

    #[test]
    fn recorded_pretooluse_timeout_secs_reads_the_same_leaf_recorded_hook_command_does() {
        let settings = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "Agent|Task", "hooks": [
                        {"type": "command", "command": "/opt/relais/bin/relais hook", "timeout": 19}
                    ]}
                ]
            }
        })
        .to_string();
        assert_eq!(recorded_pretooluse_timeout_secs(&settings), Some(Some(19)));
    }

    /// An entry an older relais wrote, before `timeout` existed, carries
    /// no such field at all: distinguished from "no relais command
    /// recorded" (`None`) by `Some(None)`.
    #[test]
    fn recorded_pretooluse_timeout_secs_is_some_none_for_an_older_entry_with_no_field() {
        let settings = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "Agent|Task", "hooks": [
                        {"type": "command", "command": "/opt/relais/bin/relais hook"}
                    ]}
                ]
            }
        })
        .to_string();
        assert_eq!(recorded_pretooluse_timeout_secs(&settings), Some(None));
        assert_eq!(recorded_pretooluse_timeout_secs("{}"), None);
    }

    /// A settings.json whose recorded timeout no longer covers the
    /// configured wait is a FAILURE, not a warning — the one outcome
    /// this whole package exists to prevent (SPEC §23: an expired hook
    /// fails open).
    #[test]
    fn hook_timeout_finding_fails_when_the_recorded_timeout_falls_short() {
        let dir = crate::test_support::temp_dir("doctor-hook-timeout-short");
        let claude_dir = dir.join(".claude");
        std::fs::create_dir_all(&claude_dir).expect("mkdir");
        std::fs::write(
            claude_dir.join("settings.json"),
            serde_json::json!({
                "hooks": {
                    "PreToolUse": [
                        {"matcher": "Agent|Task", "hooks": [
                            {"type": "command", "command": "/opt/relais/bin/relais hook", "timeout": 3}
                        ]}
                    ]
                }
            })
            .to_string(),
        )
        .expect("write");

        let admission = crate::policy::HookAdmissionSettings {
            queue_wait_secs: 30,
            ..Default::default()
        };
        let finding = hook_timeout_finding(&dir, &admission);
        assert_eq!(finding.level, Level::Fail, "{}", finding.detail);
        assert!(finding.detail.contains("3s"), "{}", finding.detail);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A recorded timeout that covers the currently configured wait is
    /// `Ok`.
    #[test]
    fn hook_timeout_finding_is_ok_when_the_recorded_timeout_covers_the_wait() {
        let dir = crate::test_support::temp_dir("doctor-hook-timeout-ok");
        let claude_dir = dir.join(".claude");
        std::fs::create_dir_all(&claude_dir).expect("mkdir");
        let admission = crate::policy::HookAdmissionSettings {
            queue_wait_secs: 2,
            ..Default::default()
        };
        let required = crate::install::settings::derived_pretooluse_timeout(
            match admission.queue_behaviour() {
                crate::policy::QueueBehaviour::WaitUpTo(wait) => wait,
                crate::policy::QueueBehaviour::RefuseImmediately => Duration::ZERO,
            },
        )
        .as_secs();
        std::fs::write(
            claude_dir.join("settings.json"),
            serde_json::json!({
                "hooks": {
                    "PreToolUse": [
                        {"matcher": "Agent|Task", "hooks": [
                            {"type": "command", "command": "/opt/relais/bin/relais hook", "timeout": required}
                        ]}
                    ]
                }
            })
            .to_string(),
        )
        .expect("write");

        let finding = hook_timeout_finding(&dir, &admission);
        assert_eq!(finding.level, Level::Ok, "{}", finding.detail);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn probe_end(ended: crate::procs::Ended, stdout: &str) -> crate::procs::ProcessEnd {
        crate::procs::ProcessEnd {
            ended,
            stdout: stdout.to_string(),
            stderr: String::new(),
            group: crate::procs::GroupKill::NothingLeft,
            captured: crate::procs::Captured::Complete,
        }
    }

    /// The outcomes doctor must tell apart: exit non-zero is never read
    /// as a refusal (a broken hook and a refusing hook must not look the
    /// same), a zero exit with no deny payload is the dead guard rather
    /// than a pass, and each non-refusal carries HOW it ended, because
    /// "timed out" and "exit 0, said nothing" send a person to different
    /// places.
    #[test]
    fn hook_health_from_probe_requires_both_a_clean_exit_and_a_deny_payload() {
        use crate::procs::Ended;

        let deny = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": "no",
            }
        })
        .to_string();
        assert_eq!(
            hook_health_from_probe(&probe_end(Ended::Exited(0), &deny)),
            HookHealth::Refused
        );
        assert_eq!(
            hook_health_from_probe(&probe_end(Ended::Exited(1), &deny)),
            HookHealth::RecordedButDidNotRefuse {
                how: "exit 1".to_string()
            },
            "a non-zero exit is never read as a refusal"
        );
        assert_eq!(
            hook_health_from_probe(&probe_end(Ended::Exited(0), "")),
            HookHealth::RecordedButDidNotRefuse {
                how: "exit 0".to_string()
            },
            "exit zero with nothing printed is a dead guard, not a pass"
        );
        assert_eq!(
            hook_health_from_probe(&probe_end(Ended::TimedOut, &deny)),
            HookHealth::RecordedButDidNotRefuse {
                how: "timed out".to_string()
            },
            "a hook that hung is reported as having hung, not as having said nothing"
        );
    }

    /// An exercise that could not be staged is `CouldNotExercise`, never
    /// the dead-guard verdict: doctor may only report what the hook did
    /// when the hook actually ran. Staged deterministically by putting a
    /// FILE where the scratch config directory has to go, so
    /// `create_dir_all` cannot succeed.
    #[test]
    fn an_exercise_that_could_not_be_staged_is_not_reported_as_a_dead_guard() {
        let base = crate::test_support::temp_dir("doctor-hook-unstageable");
        let blocked = base.join("config");
        std::fs::write(&blocked, "not a directory").expect("write");

        let health = stage_and_run("true", &blocked, &base.join("state"));
        match &health {
            HookHealth::CouldNotExercise { why } => {
                assert!(why.contains("could not create"), "{why}");
            }
            other => panic!("staging failed, so nothing ran: {other:?}"),
        }
        std::fs::remove_dir_all(&base).ok();
    }

    /// A recorded command that does not exist must not come back
    /// `Refused`. A shell reports it as exit 127 rather than a spawn
    /// failure, so which of the two remaining outcomes it lands in is a
    /// fact about the platform's shell; what is asserted here is the part
    /// that is relais's promise — it never reads as a working guard.
    #[test]
    fn a_command_that_does_not_exist_never_reads_as_a_refusal() {
        let health = probe_recorded_hook("/nonexistent/relais-does-not-exist hook");
        assert_ne!(
            health,
            HookHealth::Refused,
            "a command that does not exist cannot have refused anything"
        );
    }
}
