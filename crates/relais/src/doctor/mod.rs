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
                "granted for {} by {} on {}",
                identity.label(),
                grant.reviewed_by,
                grant.granted_at
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
                    "no grant for this declaration in {} (key {key}); `relais plan` prints the \
                     block to review and paste. {}",
                    identity.label(),
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
            Ok(caps) => findings.push(claude_code_finding(&caps)),
        },
    }

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
    findings.push(coordinator_finding());

    DoctorReport { findings }
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
}
