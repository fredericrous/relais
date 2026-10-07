//! The Claude Code harness prober (SPEC §8, §15).
//!
//! relais never launches Claude Code itself: every dispatch is a native
//! agent the relais plugin spawns (SPEC §23). What this module keeps is
//! the question every run and `doctor` ask first — which Claude Code is
//! installed and what it accepts. `claude --version` and `claude --help`
//! are probed once per process, and every fact is read off the installed
//! CLI's own output, never assumed from documentation: the harness id,
//! the version range, and the effort catalog.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::OnceLock;

use crate::backend::{BackendError, Capabilities, Harness, PermissionEnforcement};
use crate::procs::{run_with_timeout, Ended};
use crate::tooling::PROBE_TIMEOUT;

/// Why a capability probe produced no capabilities.
///
/// A probe that could not be run is not the same fact as a CLI that ran
/// and does not support a flag, and reading the first as the second is
/// how an unavailable harness reads as an unsupported one. Every
/// variant blocks the run; which it was is what `doctor` prints
/// (`errors.never-swallowed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeFailure {
    /// The probe process could not be started, timed out, or was
    /// cancelled.
    NotRun { args: String, detail: String },
    /// It ran and did not exit 0.
    Refused { args: String, ended: String },
    /// It exited 0 with nothing on either stream.
    Silent { args: String },
}

impl std::fmt::Display for ProbeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRun { args, detail } => {
                write!(f, "`claude {args}` did not run: {detail}")
            }
            Self::Refused { args, ended } => write!(f, "`claude {args}` {ended}"),
            Self::Silent { args } => {
                write!(f, "`claude {args}` exited 0 and printed nothing")
            }
        }
    }
}

impl std::error::Error for ProbeFailure {}

#[derive(Debug)]
pub struct ClaudeBackend {
    binary: PathBuf,
    /// Probed on first use and kept: `--version` and `--help` are facts
    /// about an installed CLI, and asking again before every dispatch
    /// spent two processes per launch to learn the same thing (audit V6).
    /// The failure is kept too, so a probe that errored is reported as
    /// an error rather than as a CLI without the flags.
    capabilities: OnceLock<Result<Capabilities, ProbeFailure>>,
}

impl ClaudeBackend {
    /// Discovery: `RELAIS_CLAUDE_BIN` wins, then PATH lookup. Missing
    /// binary is a blocked outcome, never a fallback (SPEC §6).
    pub fn discover() -> Result<Self, BackendError> {
        if let Some(explicit) = std::env::var_os("RELAIS_CLAUDE_BIN") {
            return Self::with_binary(PathBuf::from(explicit));
        }
        let found = crate::tooling::which("claude")
            .ok_or_else(|| BackendError::MissingBinary("claude".into()))?;
        Self::with_binary(found)
    }

    /// A backend at a named binary, resolved to an absolute path.
    ///
    /// The path is canonicalised because a relative one is probed
    /// against relais's cwd and could be executed against another
    /// directory's, so a repository that ships `node_modules/.bin/claude`
    /// could have its own binary run in place of the operator's (audit
    /// V13). A relative `RELAIS_CLAUDE_BIN` is refused outright rather
    /// than resolved, since nothing can say which directory the operator
    /// meant.
    pub fn with_binary(binary: PathBuf) -> Result<Self, BackendError> {
        if binary.is_relative() {
            return Err(BackendError::MissingBinary(format!(
                "{}: the Claude Code binary must be named by an absolute path — a relative one \
                 resolves against relais's working directory, not the one it runs in",
                binary.display()
            )));
        }
        let resolved = binary
            .canonicalize()
            .map_err(|e| BackendError::MissingBinary(format!("{}: {e}", binary.display())))?;
        if !resolved.is_file() {
            return Err(BackendError::MissingBinary(
                resolved.to_string_lossy().into(),
            ));
        }
        Ok(Self {
            binary: resolved,
            capabilities: OnceLock::new(),
        })
    }

    /// The binary this backend probes.
    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// The probe, cached, with the reason it failed when it did.
    /// `cancel` aborts a probe that hangs; the timeout bounds one that
    /// merely takes its time.
    fn capability_report(
        &self,
        cancel: Option<&AtomicBool>,
    ) -> &Result<Capabilities, ProbeFailure> {
        self.capabilities.get_or_init(|| self.probe_now(cancel))
    }

    /// What the installed CLI can be asked to do, or why that could not
    /// be established — the answer `doctor` reports.
    pub fn probe_report(&self) -> Result<Capabilities, ProbeFailure> {
        self.capability_report(None).clone()
    }

    fn capabilities(&self, cancel: Option<&AtomicBool>) -> Option<Capabilities> {
        // The failure is not dropped here: `probe_report` is what
        // `doctor` prints. This is the `Harness::probe` shape.
        self.capability_report(cancel).clone().ok()
    }

    fn probe_now(&self, cancel: Option<&AtomicBool>) -> Result<Capabilities, ProbeFailure> {
        let version = self.ask(&["--version"], cancel)?;
        // A CLI that answers `--version` but not `--help` cannot have its
        // flags checked: no capabilities means no backend, which blocks
        // (SPEC §6).
        let help = self.ask(&["--help"], cancel)?;
        Ok(capabilities_from_help(
            version.trim().to_string(),
            help.trim(),
        ))
    }

    /// One probe call: bounded, cancellable, and an error unless the CLI
    /// exited successfully with something to say.
    fn ask(&self, args: &[&str], cancel: Option<&AtomicBool>) -> Result<String, ProbeFailure> {
        let mut command = Command::new(&self.binary);
        command.args(args);
        let named = args.join(" ");
        let end = run_with_timeout(command, PROBE_TIMEOUT, None, cancel, None).map_err(|e| {
            ProbeFailure::NotRun {
                args: named.clone(),
                detail: e.to_string(),
            }
        })?;
        if end.ended != Ended::Exited(0) {
            return Err(ProbeFailure::Refused {
                args: named,
                ended: end.ended.describe(),
            });
        }
        let answer = format!("{}{}", end.stdout, end.stderr);
        if answer.trim().is_empty() {
            return Err(ProbeFailure::Silent { args: named });
        }
        Ok(answer)
    }
}

impl Harness for ClaudeBackend {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    fn probe(&self) -> Option<Capabilities> {
        self.capabilities(None)
    }
}

/// What the installed CLI advertises, read from its `--help`. Flag
/// spellings are the ones Claude Code 2.x prints; a harness that spells
/// one differently is a harness without that capability.
pub fn capabilities_from_help(version: String, help: &str) -> Capabilities {
    let supports = |flag: &str| help.contains(flag);
    Capabilities {
        backend: "claude-code".into(),
        version: Some(version),
        supports_model: supports("--model"),
        accepted_efforts: crate::catalog::parse_cli_efforts(help),
        supports_max_turns: supports("--max-turns"),
        supports_output_format_json: supports("--output-format"),
        supports_budget: supports("--max-budget-usd"),
        supports_disallowed_tools: supports("--disallowed-tools"),
        supports_settings: supports("--settings"),
        // Permission lists are enforced by the harness itself; the
        // adapter reports observed enforcement until the
        // compatibility matrix verifies it per version.
        permission_enforcement: PermissionEnforcement::Observed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real `--help` fragment of 2.1.278: `--max-budget-usd`, no
    /// `--max-turns`, no `--budget`.
    const HELP_2_1: &str = "  --max-budget-usd <amount>  Maximum dollar amount\n  \
        --model <model>  Model\n  --effort <level>  Effort\n  \
        --output-format <format>  Output\n  --disallowedTools, --disallowed-tools <tools...>\n  \
        --settings <file-or-json>  Path\n";

    #[test]
    fn capabilities_come_from_the_installed_help_text() {
        let caps = capabilities_from_help("2.1.278".into(), HELP_2_1);
        assert!(caps.supports_model);
        assert_eq!(
            caps.accepted_efforts,
            crate::catalog::Fact::Unknown,
            "the flag is there and no list of levels is"
        );
        assert!(caps.supports_budget, "--max-budget-usd is the budget flag");
        assert!(!caps.supports_max_turns, "2.1.278 has no turn ceiling");
        assert!(caps.supports_disallowed_tools);
        assert!(caps.supports_settings);
        let old = capabilities_from_help("1.0".into(), "--model --budget --max-turns");
        assert!(!old.supports_budget, "`--budget` is not the budget flag");
        assert!(old.supports_max_turns);
        assert!(!old.supports_disallowed_tools);
    }

    #[test]
    fn the_effort_catalog_is_read_from_the_help() {
        let help = include_str!("../../tests/fixtures/help/claude-2.1.284.txt");
        let caps = capabilities_from_help("2.1.284".into(), help);
        let listed = crate::policy::EffortId::parse("medium").expect("an effort id");
        let unlisted = crate::policy::EffortId::parse("ultra").expect("an effort id");
        assert!(
            crate::backend::check_effort(&caps.accepted_efforts, &listed, "sonnet", None).is_ok()
        );
        assert!(
            crate::backend::check_effort(&caps.accepted_efforts, &unlisted, "sonnet", None)
                .is_err()
        );

        let none = capabilities_from_help(
            "2.1.0".into(),
            include_str!("../../tests/fixtures/help/claude-no-effort.txt"),
        );
        assert!(
            crate::backend::check_effort(&none.accepted_efforts, &listed, "sonnet", None).is_err(),
            "a CLI with no --effort flag refuses instead of dropping it"
        );

        let unlisted_levels = capabilities_from_help(
            "2.1.0".into(),
            include_str!("../../tests/fixtures/help/claude-effort-no-list.txt"),
        );
        assert_eq!(
            unlisted_levels.accepted_efforts,
            crate::catalog::Fact::Unknown
        );
        assert!(crate::backend::check_effort(
            &unlisted_levels.accepted_efforts,
            &listed,
            "sonnet",
            None
        )
        .is_ok());
    }

    #[test]
    fn the_turn_ceiling_capability_is_reported_not_assumed() {
        use crate::backend::TurnCeiling;
        assert_eq!(
            capabilities_from_help("2.1.278".into(), HELP_2_1).turn_ceiling(),
            TurnCeiling::Unavailable,
            "SPEC §11 promises a turn ceiling this harness cannot take"
        );
        assert_eq!(
            capabilities_from_help("1.0".into(), "--model --max-turns").turn_ceiling(),
            TurnCeiling::Harness
        );
        assert_eq!(TurnCeiling::Unavailable.as_str(), "unavailable");
        assert_eq!(TurnCeiling::Harness.as_str(), "harness");
    }

    /// V13: a binary named relatively is probed in one place and could be
    /// executed in another. It is refused, and an absolute one is
    /// canonicalised so both halves name the same file.
    #[test]
    fn the_backend_binary_is_absolute_or_refused() {
        let error = ClaudeBackend::with_binary(PathBuf::from("node_modules/.bin/claude"))
            .expect_err("a relative binary is refused");
        assert!(
            error.to_string().contains("absolute path"),
            "{error}: {error:?}"
        );

        let dir = crate::test_support::temp_dir("claude-bin");
        std::fs::create_dir_all(dir.join("bin")).expect("mkdir");
        let binary = dir.join("bin/claude");
        std::fs::write(&binary, "#!/bin/sh\nexit 0\n").expect("write");
        let backend = ClaudeBackend::with_binary(dir.join("bin/../bin/claude"))
            .expect("an absolute binary resolves");
        assert_eq!(
            backend.binary(),
            binary.canonicalize().expect("canonical").as_path(),
            "the path the probe used is the path that is run"
        );
        assert!(
            ClaudeBackend::with_binary(dir.join("bin/absent")).is_err(),
            "a binary that is not there is a missing binary, never a fallback"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
