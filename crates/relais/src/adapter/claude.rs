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
use std::sync::Mutex;
use std::time::{Duration, Instant};

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
    /// The probe process could not be started, or was cancelled.
    NotRun { args: String, detail: String },
    /// It did not answer within `after`: the machine may be busy. Not a
    /// fact about the CLI, so never cached and never read as an
    /// unsupported version.
    TimedOut { args: String, after: Duration },
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
            Self::TimedOut { args, after } => write!(
                f,
                "`claude {args}` did not answer within {}s (machine busy?)",
                after.as_secs_f64()
            ),
            Self::Refused { args, ended } => write!(f, "`claude {args}` {ended}"),
            Self::Silent { args } => {
                write!(f, "`claude {args}` exited 0 and printed nothing")
            }
        }
    }
}

impl std::error::Error for ProbeFailure {}

/// Whether this Claude Code is one the plugin supports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Support {
    Supported,
    /// Outside the range, unreadable or unanswering: the run is refused.
    Unsupported(String),
    /// The probe was late, not wrong: nothing says it does not work, and
    /// nothing says it does. The run warns and goes on.
    Unchecked(String),
}

/// The three-way verdict on an installed Claude Code. `printed` is what
/// `claude --version` printed; `failure` is why the probe could not say.
/// Only a probe that timed out is `Unchecked`: an answer outside the range,
/// an unparsable one, a non-zero exit, silence or a missing binary mean
/// nothing says it works (`crate::native::unsupported_claude_code_because`).
pub fn claude_code_support(printed: Option<&str>, failure: Option<&ProbeFailure>) -> Support {
    let answered = printed.is_some_and(|text| !text.trim().is_empty());
    if !answered {
        if let Some(late @ ProbeFailure::TimedOut { .. }) = failure {
            return Support::Unchecked(late.to_string());
        }
    }
    let shown = failure.map(ToString::to_string);
    match crate::native::unsupported_claude_code_because(printed, shown.as_deref()) {
        Some(refusal) => Support::Unsupported(refusal),
        None => Support::Supported,
    }
}

#[derive(Debug)]
pub struct ClaudeBackend {
    binary: PathBuf,
    /// Probed on first use and kept: `--version` and `--help` are facts
    /// about an installed CLI, and asking again before every dispatch
    /// spent two processes per launch to learn the same thing (audit V6).
    /// The failure is kept too, so a probe that errored is reported as
    /// an error rather than as a CLI without the flags. A probe that
    /// timed out is the exception: it says nothing about the CLI, so it
    /// is kept only for `timeout_retry`, then asked again.
    probed: Mutex<Probed>,
    /// How long one probe call may take.
    probe_timeout: Duration,
    /// How long a timed-out probe stands before the next use asks again:
    /// one run start must not wait out the timeout once per caller.
    timeout_retry: Duration,
}

/// How long a timed-out probe stands before the next use asks again.
pub const TIMEOUT_RETRY: Duration = Duration::from_secs(300);

#[derive(Debug, Default)]
struct Probed {
    /// An answer, or a failure that is a fact about the CLI.
    settled: Option<Result<Capabilities, ProbeFailure>>,
    /// The last probe that timed out, and when.
    timed_out: Option<(Instant, ProbeFailure)>,
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
            probed: Mutex::new(Probed::default()),
            probe_timeout: PROBE_TIMEOUT,
            timeout_retry: TIMEOUT_RETRY,
        })
    }

    /// The same backend with another probe timeout; tests use it to avoid
    /// waiting out `PROBE_TIMEOUT`.
    #[must_use]
    pub fn with_probe_timeout(mut self, probe_timeout: Duration) -> Self {
        self.probe_timeout = probe_timeout;
        self
    }

    /// The same backend with another `timeout_retry`.
    #[must_use]
    pub fn with_timeout_retry(mut self, timeout_retry: Duration) -> Self {
        self.timeout_retry = timeout_retry;
        self
    }

    /// The binary this backend probes.
    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// The probe, cached, with the reason it failed when it did.
    /// `cancel` aborts a probe that hangs; the timeout bounds one that
    /// merely takes its time.
    fn capability_report(&self, cancel: Option<&AtomicBool>) -> Result<Capabilities, ProbeFailure> {
        let mut probed = self
            .probed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(report) = probed.settled.as_ref() {
            return report.clone();
        }
        if let Some((at, failure)) = probed.timed_out.as_ref() {
            if at.elapsed() < self.timeout_retry {
                return Err(failure.clone());
            }
        }
        let report = self.probe_now(cancel);
        match &report {
            Err(failure @ ProbeFailure::TimedOut { .. }) => {
                probed.timed_out = Some((Instant::now(), failure.clone()));
            }
            _ => {
                probed.timed_out = None;
                probed.settled = Some(report.clone());
            }
        }
        report
    }

    /// Why the probe could not say in time, when that is why it has no
    /// answer: the version is unchecked, not unsupported.
    pub fn probe_timed_out(&self) -> Option<ProbeFailure> {
        let probed = self
            .probed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match probed.settled {
            Some(_) => None,
            None => probed
                .timed_out
                .as_ref()
                .map(|(_, failure)| failure.clone()),
        }
    }

    /// What the installed CLI can be asked to do, or why that could not
    /// be established — the answer `doctor` reports.
    pub fn probe_report(&self) -> Result<Capabilities, ProbeFailure> {
        self.capability_report(None)
    }

    fn capabilities(&self, cancel: Option<&AtomicBool>) -> Option<Capabilities> {
        // The failure is not dropped here: `probe_report` is what
        // `doctor` prints. This is the `Harness::probe` shape.
        self.capability_report(cancel).ok()
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
        let end = run_with_timeout(command, self.probe_timeout, None, cancel).map_err(|e| {
            ProbeFailure::NotRun {
                args: named.clone(),
                detail: e.to_string(),
            }
        })?;
        if end.ended == Ended::TimedOut {
            return Err(ProbeFailure::TimedOut {
                args: named,
                after: self.probe_timeout,
            });
        }
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

    fn probe_timed_out(&self) -> Option<String> {
        ClaudeBackend::probe_timed_out(self).map(|failure| failure.to_string())
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

    #[test]
    fn only_a_late_probe_is_unchecked_every_other_unreadable_answer_is_unsupported() {
        use std::time::Duration;
        let late = ProbeFailure::TimedOut {
            args: "--version".into(),
            after: Duration::from_secs(30),
        };
        assert!(matches!(
            claude_code_support(None, Some(&late)),
            Support::Unchecked(_)
        ));
        assert_eq!(
            claude_code_support(Some("2.1.291 (Claude Code)"), None),
            Support::Supported
        );
        for outside in ["2.0.0", "2.2.0", "not a version", ""] {
            assert!(
                matches!(
                    claude_code_support(Some(outside), None),
                    Support::Unsupported(_)
                ),
                "{outside}"
            );
        }
        let exited = ProbeFailure::Refused {
            args: "--version".into(),
            ended: "exit 1".into(),
        };
        let silent = ProbeFailure::Silent {
            args: "--version".into(),
        };
        let missing = ProbeFailure::NotRun {
            args: "--version".into(),
            detail: "No such file".into(),
        };
        for failure in [&exited, &silent, &missing] {
            assert!(
                matches!(
                    claude_code_support(None, Some(failure)),
                    Support::Unsupported(_)
                ),
                "{failure}"
            );
        }
        assert!(matches!(
            claude_code_support(None, None),
            Support::Unsupported(_)
        ));
    }

    /// A fake `claude` that answers `--version` only after `delay`.
    #[cfg(unix)]
    fn fake_claude(dir: &Path, script: &str) -> ClaudeBackend {
        use std::os::unix::fs::PermissionsExt;
        let binary = dir.join("claude");
        std::fs::write(&binary, script).expect("write");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        // The first exec of a fresh script can take longer than the
        // probe timeout (Gatekeeper); run it once before the probe.
        crate::test_support::prime_script(&binary);
        ClaudeBackend::with_binary(binary)
            .expect("backend")
            .with_probe_timeout(Duration::from_millis(200))
    }

    /// A fake `claude` that never answers in time, counting its starts.
    #[cfg(unix)]
    fn slow_claude(dir: &Path, count: &Path) -> ClaudeBackend {
        fake_claude(
            dir,
            &format!(
                "#!/bin/sh\n[ \"$1\" = prime ] && exit 0\necho x >> {}\nexec sleep 5\n",
                count.display()
            ),
        )
    }

    #[cfg(unix)]
    fn starts(count: &Path) -> usize {
        std::fs::read_to_string(count)
            .expect("count")
            .lines()
            .count()
    }

    #[cfg(unix)]
    #[test]
    fn a_probe_that_times_out_is_typed_and_stands_until_the_retry() {
        let dir = crate::test_support::temp_dir("claude-slow");
        let count = dir.join("count");
        let backend = slow_claude(&dir, &count);
        let first = backend.probe_report().expect_err("timed out");
        assert!(matches!(first, ProbeFailure::TimedOut { .. }), "{first:?}");
        assert!(first.to_string().contains("did not answer within 0.2s"));
        assert!(matches!(
            claude_code_support(None, Some(&first)),
            Support::Unchecked(_)
        ));
        assert_eq!(Harness::probe_timed_out(&backend), Some(first.to_string()));
        // Every caller of one run start gets the same answer, not a
        // second wait.
        assert_eq!(Harness::probe(&backend), None);
        assert_eq!(starts(&count), 1, "within the retry, no second probe");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_probe_that_timed_out_is_asked_again_after_the_retry() {
        let dir = crate::test_support::temp_dir("claude-slow-retry");
        let count = dir.join("count");
        let backend = slow_claude(&dir, &count).with_timeout_retry(Duration::ZERO);
        backend.probe_report().expect_err("timed out");
        let second = backend.probe_report().expect_err("timed out again");
        assert!(matches!(second, ProbeFailure::TimedOut { .. }));
        assert_eq!(starts(&count), 2, "past the retry, the binary runs again");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_probe_that_fails_otherwise_is_cached() {
        let dir = crate::test_support::temp_dir("claude-exit1");
        let count = dir.join("count");
        let backend = fake_claude(
            &dir,
            &format!(
                "#!/bin/sh\n[ \"$1\" = prime ] && exit 0\necho x >> {}\nexit 1\n",
                count.display()
            ),
        );
        let first = backend.probe_report().expect_err("refused");
        assert!(matches!(first, ProbeFailure::Refused { .. }), "{first:?}");
        backend.probe_report().expect_err("refused");
        assert_eq!(starts(&count), 1, "other failures stay cached");
        assert_eq!(Harness::probe_timed_out(&backend), None);
        std::fs::remove_dir_all(&dir).ok();
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
