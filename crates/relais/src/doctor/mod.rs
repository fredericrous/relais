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
use std::path::Path;

use crate::adapter::{Backend, Capabilities};
use crate::policy::{Dependency, DependencyMode, MachineSettings, RepoPolicy};
use crate::{ledger::Ledger, paths};

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub component: &'static str,
    pub ok: bool,
    /// "ok" | "warn" | "fail"
    pub level: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub findings: Vec<Finding>,
}

impl DoctorReport {
    pub fn failed(&self) -> bool {
        self.findings.iter().any(|finding| finding.level == "fail")
    }

    pub fn render(&self) -> String {
        let mut out = String::from("relais doctor\n");
        for finding in &self.findings {
            let mark = match finding.level {
                "ok" => "✓",
                "warn" => "!",
                _ => "✗",
            };
            out.push_str(&format!(
                " {mark} {:<12} {}\n",
                finding.component, finding.detail
            ));
        }
        if self.failed() {
            out.push_str("\nfix the ✗ findings before running tasks\n");
        }
        out
    }
}

fn binary_on_path(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn check_command(name: &str, args: &[&str], findings: &mut Vec<Finding>, component: &'static str) {
    match binary_on_path(name) {
        None => findings.push(Finding {
            component,
            ok: false,
            level: "fail",
            detail: format!("`{name}` is not on PATH"),
        }),
        Some(binary) => {
            let output = std::process::Command::new(&binary).args(args).output();
            match output {
                Ok(output) if output.status.success() => findings.push(Finding {
                    component,
                    ok: true,
                    level: "ok",
                    detail: format!(
                        "`{}` — {}",
                        binary.to_string_lossy(),
                        String::from_utf8_lossy(&output.stdout).trim()
                    ),
                }),
                Ok(_) => findings.push(Finding {
                    component,
                    ok: false,
                    level: "fail",
                    detail: format!("`{name}` exists but failed to run"),
                }),
                Err(e) => findings.push(Finding {
                    component,
                    ok: false,
                    level: "fail",
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
            ok: true,
            level: "ok",
            detail: format!("not declared in [integrations]; `{name}` is not checked"),
        };
    };
    let bin = dependency
        .bin()
        .map(str::to_string)
        .unwrap_or_else(|| default_integration_bin(name));
    if dependency.mode() == DependencyMode::Off {
        return Finding {
            component,
            ok: true,
            level: "ok",
            detail: format!("declared `off` in [integrations]; `{bin}` is not checked"),
        };
    }
    if available(&bin) {
        return Finding {
            component,
            ok: true,
            level: "ok",
            detail: match version(&bin) {
                Some(version) => format!("{} — `{bin}` {version}", mode_word(dependency.mode())),
                None => format!(
                    "{} — `{bin}` is on PATH but would not answer `--version`",
                    mode_word(dependency.mode())
                ),
            },
        };
    }
    match dependency.mode() {
        DependencyMode::Required => Finding {
            component,
            ok: false,
            level: "fail",
            detail: format!(
                "required by [integrations] and `{bin}` is not on PATH; every run is blocked on it"
            ),
        },
        // Reported as a gap, never as a pass.
        _ => Finding {
            component,
            ok: false,
            level: "warn",
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
        ok: true,
        level: if caps.supports_model { "ok" } else { "warn" },
        detail,
    }
}

/// `[trials] enabled = true` promises randomized assignment with logged
/// propensities (SPEC §17). This release ships no replay command and no
/// propensity logging, so the flag does nothing at all. Saying so is the
/// whole point: a silent no-op reads as a running experiment.
pub(crate) fn trials_finding(settings: &MachineSettings) -> Option<Finding> {
    settings.trials.enabled.then(|| Finding {
        component: "trials",
        ok: false,
        level: "warn",
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
    match paths::home_dir_checked() {
        Ok(_) => {}
        Err(e) => findings.push(Finding {
            component: "home",
            ok: false,
            level: if paths::dir_override(paths::CONFIG_DIR_ENV).is_some()
                && paths::dir_override(paths::STATE_DIR_ENV).is_some()
            {
                // Both directories are given explicitly: nothing needs a
                // home directory, so this is informational.
                "warn"
            } else {
                "fail"
            },
            detail: e.to_string(),
        }),
    }
    for (env, label, carries) in [
        (
            paths::CONFIG_DIR_ENV,
            "config",
            "the machine authority (trust grants, spending ceilings, permissions)",
        ),
        (
            paths::STATE_DIR_ENV,
            "state",
            "the ledger, the artifact registry and every run's artifacts",
        ),
    ] {
        let (level, detail) = match paths::dir_override(env) {
            Some(dir) => (
                "warn",
                format!(
                    "{label} directory is {} — relocated by {env}, so {carries} come from there, \
                     not from the defaults under $HOME",
                    dir.display()
                ),
            ),
            None => (
                "ok",
                match paths::home_dir_checked() {
                    Ok(_) if label == "config" => {
                        format!("config directory is {}", paths::config_dir().display())
                    }
                    Ok(_) => format!("state directory is {}", paths::state_dir().display()),
                    Err(_) => format!("{label} directory cannot be resolved without HOME"),
                },
            ),
        };
        findings.push(Finding {
            component: "directories",
            ok: level == "ok",
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

    match crate::adapter::claude::ClaudeBackend::discover() {
        Err(e) => findings.push(Finding {
            component: "claude-code",
            ok: false,
            level: "fail",
            detail: e.to_string(),
        }),
        Ok(backend) => match backend.probe() {
            None => findings.push(Finding {
                component: "claude-code",
                ok: false,
                level: "fail",
                detail: "claude --version did not answer".into(),
            }),
            Some(caps) => findings.push(claude_code_finding(&caps)),
        },
    }

    let policy_path = repo_dir.join("relais.toml");
    let policy = match std::fs::read_to_string(&policy_path) {
        Err(_) => {
            findings.push(Finding {
                component: "relais.toml",
                ok: false,
                level: "fail",
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
                    ok: true,
                    level: "ok",
                    detail: format!(
                        "valid; models configured: {}",
                        if models.is_empty() { "none" } else { &models }
                    ),
                });
                if !policy.verification.profiles.is_empty() {
                    findings.push(Finding {
                        component: "verify",
                        ok: true,
                        level: "ok",
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
                        ok: false,
                        level: "fail",
                        detail: "no verification profiles; acceptance has nothing to run".into(),
                    });
                }
                Some(policy)
            }
            Err(e) => {
                findings.push(Finding {
                    component: "relais.toml",
                    ok: false,
                    level: "fail",
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
                    &|bin| crate::policy::binary_available(bin),
                    &|bin| crate::policy::integration_version(bin),
                ));
            }
        }
        None => findings.push(Finding {
            component: "integrations",
            ok: false,
            level: "warn",
            detail: "no readable relais.toml, so the declared integration modes are unknown \
                     and nothing was probed"
                .into(),
        }),
    }

    let machine_path = paths::machine_settings_path();
    match std::fs::read_to_string(&machine_path) {
        Err(_) => findings.push(Finding {
            component: "machine.toml",
            ok: false,
            level: "warn",
            detail: format!(
                "{} does not exist; every run will be blocked on the missing trust grant",
                machine_path.display()
            ),
        }),
        Ok(text) => match MachineSettings::from_toml_str(&text) {
            Ok(settings) => {
                findings.push(Finding {
                    component: "machine.toml",
                    ok: true,
                    level: "ok",
                    detail: format!("valid; {} trust grant(s)", settings.trust.len()),
                });
                if let Some(trials) = trials_finding(&settings) {
                    findings.push(trials);
                }
            }
            Err(e) => findings.push(Finding {
                component: "machine.toml",
                ok: false,
                level: "fail",
                detail: e.to_string(),
            }),
        },
    }

    match Ledger::open(&paths::ledger_path()) {
        Ok(ledger) => findings.push(Finding {
            component: "ledger",
            ok: true,
            level: "ok",
            detail: format!(
                "{} at {}",
                match ledger.schema_version() {
                    Ok(version) => format!("schema v{version}"),
                    Err(_) => "schema unreadable".to_string(),
                },
                paths::ledger_path().display()
            ),
        }),
        Err(e) => findings.push(Finding {
            component: "ledger",
            ok: false,
            level: "fail",
            detail: e.to_string(),
        }),
    }

    let registry = paths::registry_dir();
    findings.push(Finding {
        component: "registry",
        ok: true,
        level: if registry.is_dir() { "ok" } else { "warn" },
        detail: if registry.is_dir() {
            format!("{} artifacts directory present", registry.display())
        } else {
            format!(
                "no artifacts at {} yet (learned routing will abstain to the conservative baseline)",
                registry.display()
            )
        },
    });

    let socket = crate::coordinator::socket_path();
    let answered = crate::coordinator::Client::new(socket.clone()).ping();
    findings.push(Finding {
        component: "coordinator",
        ok: true,
        level: if answered.is_ok() { "ok" } else { "warn" },
        detail: match answered {
            Ok(pid) => format!("coordinator pid {pid} answers on {}", socket.display()),
            Err(_) if socket.exists() => format!(
                "socket {} exists but nobody answers; the next managed dispatch takes it over",
                socket.display()
            ),
            Err(_) => {
                "no coordinator running; it starts lazily on the first managed dispatch".to_string()
            }
        },
    });

    DoctorReport { findings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::claude::capabilities_from_help;

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
        assert_eq!(finding.level, "ok");
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
        assert_eq!(required_missing.level, "fail");

        let optional_missing = integration_finding(
            "amont",
            "amont",
            Some(&dependency("optional")),
            &missing,
            &no_version,
        );
        assert_eq!(optional_missing.level, "warn");
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
        assert_eq!(off.level, "ok");
        assert!(off.detail.contains("not checked"), "{}", off.detail);

        let undeclared =
            integration_finding("amont_agent", "amont-agent", None, &missing, &no_version);
        assert_eq!(undeclared.level, "ok");
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
        assert_eq!(ok.level, "ok");
        assert!(
            ok.detail.contains("`amont-agent` 1.2.3"),
            "the config key is amont_agent, the binary amont-agent: {}",
            ok.detail
        );
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
        assert_eq!(finding.level, "warn");
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
                true if finding.level == "warn" => {
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
}
