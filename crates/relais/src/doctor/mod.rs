//! Environment diagnosis (SPEC §3: `relais doctor`).
//!
//! Checks git, the Claude Code binary and its capability report, the
//! aval/amont/amont-agent integrations, policy files, the ledger, the
//! artifact registry and the coordinator socket. Findings distinguish
//! hard blockers from warnings; nothing here repairs anything by itself,
//! and a missing optional integration is reported, not counted as fine.

use serde::Serialize;
use std::path::Path;

use crate::adapter::Backend;
use crate::policy::{MachineSettings, RepoPolicy};
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

/// Probe the environment relative to one repository directory (the cwd in
/// practice) and the machine-owned settings.
pub fn doctor(repo_dir: &Path) -> DoctorReport {
    let mut findings = Vec::new();

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
            Some(caps) => {
                let mut detail = format!("version {}", caps.version.as_deref().unwrap_or("?"));
                if !caps.supports_model {
                    detail.push_str("; WARNING: --model not advertised in --help");
                }
                if !caps.supports_output_format_json {
                    detail.push_str("; WARNING: --output-format json not advertised");
                }
                findings.push(Finding {
                    component: "claude-code",
                    ok: true,
                    level: if caps.supports_model { "ok" } else { "warn" },
                    detail,
                });
            }
        },
    }

    for (name, component) in [
        ("aval", "aval"),
        ("amont", "amont"),
        ("amont-agent", "amont-agent"),
    ] {
        check_command(name, &["--version"], &mut findings, component);
    }

    let policy_path = repo_dir.join("relais.toml");
    match std::fs::read_to_string(&policy_path) {
        Err(_) => findings.push(Finding {
            component: "relais.toml",
            ok: false,
            level: "fail",
            detail: format!(
                "no relais.toml in {} — run `relais init`",
                repo_dir.display()
            ),
        }),
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
            }
            Err(e) => findings.push(Finding {
                component: "relais.toml",
                ok: false,
                level: "fail",
                detail: e.to_string(),
            }),
        },
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
            Ok(settings) => findings.push(Finding {
                component: "machine.toml",
                ok: true,
                level: "ok",
                detail: format!("valid; {} trust grant(s)", settings.trust.len()),
            }),
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
