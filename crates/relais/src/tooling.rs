//! What is installed on this machine (SPEC §5).
//!
//! Which integration binaries answer on `PATH` and what version they
//! report. A fact about the machine, not a decision: `policy` intersects
//! declarations and never looks at `PATH`, and every caller that needs
//! the answer — `doctor`, `plan`, `run` — probes here and hands the
//! result to the pure functions as a value.

use crate::policy::{BlockCode, Blocker, DependencyMode, RepoPolicy};

/// Availability of required integrations at run time. Kept out of
/// `effective_authority` on purpose: the intersection stays a pure function
/// over policy files, while PATH probing belongs to doctor, plan and run.
pub fn probe_integrations(repo: &RepoPolicy) -> Vec<Blocker> {
    let mut blockers = Vec::new();
    for (name, dependency) in [
        ("aval", repo.integrations.aval.as_ref()),
        ("amont", repo.integrations.amont.as_ref()),
        ("amont_agent", repo.integrations.amont_agent.as_ref()),
    ] {
        if let Some(dependency) = dependency {
            if dependency.mode() == DependencyMode::Required {
                let bin = dependency.bin().unwrap_or(default_bin(name));
                if which_missing(bin) {
                    blockers.push(Blocker {
                        code: BlockCode::IntegrationMissing,
                        detail: format!("required integration `{name}` is not available on PATH"),
                    });
                }
            }
        }
    }
    blockers
}

/// The config key is `amont_agent`; the binary on PATH is `amont-agent`.
fn default_bin(name: &str) -> &str {
    match name {
        "amont_agent" => "amont-agent",
        other => other,
    }
}

fn which_missing(bin: &str) -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).all(|dir| !dir.join(bin).is_file())
}

/// Is this binary on PATH?
pub fn binary_available(bin: &str) -> bool {
    !which_missing(bin)
}

/// Is the integration's binary on PATH? By the integration's config name
/// (`amont_agent` → `amont-agent`), ignoring any `bin` override.
pub fn integration_available(name: &str) -> bool {
    let name = name.replace('-', "_");
    binary_available(default_bin(&name))
}

/// `<tool> --version`'s first line, or `None` when the tool is absent or
/// will not answer — recorded in the context manifest so a receipt names
/// the toolchain it was verified with.
pub fn integration_version(name: &str) -> Option<String> {
    let output = std::process::Command::new(default_bin(name))
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The config key carries an underscore; the binary on PATH carries
    /// a hyphen, and a probe against the wrong spelling reports every
    /// machine as missing the integration.
    #[test]
    fn the_agent_integrations_binary_name_is_hyphenated() {
        assert_eq!(default_bin("amont_agent"), "amont-agent");
        assert_eq!(default_bin("aval"), "aval");
    }

    /// A name no machine has on PATH is missing; the interpreter running
    /// this test is not.
    #[test]
    fn a_binary_is_missing_when_no_path_entry_holds_it() {
        assert!(!binary_available("relais-no-such-binary-4f3a"));
        assert!(integration_version("relais-no-such-binary-4f3a").is_none());
    }
}
