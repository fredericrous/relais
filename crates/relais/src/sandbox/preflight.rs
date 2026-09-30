//! Before any worker dispatch: can the OS sandbox be relied on here?
//!
//! Each question is a function of explicit inputs — a platform string, a
//! version string, a PATH lookup, the managed roots — so each case is
//! testable; the runner calls [`preflight`] with the real values. A
//! sandboxed worker is launched confined, or not launched: every answer
//! here that is not a clear yes is a [`Blocker`].

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{judge_managed, judge_user_config, managed_sources, Weakening};
use crate::policy::{sandbox_platform_refusal, BlockCode, Blocker};

/// The only Claude Code version the sandbox was measured on (S0). Newer
/// is accepted; older, or a version relais cannot read, is not.
pub const SANDBOX_MIN_HARNESS: &str = "2.1.285";

/// Programs the Linux sandbox is built from.
const LINUX_TOOLS: [&str; 2] = ["bwrap", "socat"];

/// Where Claude Code reads managed (administrator-owned) settings on
/// `platform`.
pub fn managed_root(platform: &str) -> &'static str {
    match platform {
        "macos" => "/Library/Application Support/ClaudeCode",
        _ => "/etc/claude-code",
    }
}

/// The host facts the sandbox preflight reads, behind one seam so a run
/// can be driven past the preflight in a test (the real host has no
/// bubblewrap on a CI runner, and its own `~/.claude.json`).
pub trait SandboxHost: Sync {
    /// A `std::env::consts::OS` value.
    fn platform(&self) -> &str;
    /// Is this program on PATH?
    fn on_path(&self, program: &str) -> bool;
    /// The managed-settings root for this platform.
    fn managed_root(&self) -> std::path::PathBuf;
    /// The test root that ADDS to the real one (`RELAIS_TEST_MANAGED_ROOT`).
    fn extra_managed_root(&self) -> Option<std::path::PathBuf>;
    /// `~/.claude.json` under `home`.
    fn user_config(&self, home: &Path) -> std::path::PathBuf;
    /// The file the verification records are kept in.
    fn verification_store(&self) -> Result<std::path::PathBuf, crate::paths::HomeUnset>;
}

/// The machine relais runs on.
pub struct RealSandboxHost;

impl SandboxHost for RealSandboxHost {
    fn platform(&self) -> &str {
        std::env::consts::OS
    }

    fn on_path(&self, program: &str) -> bool {
        crate::tooling::binary_available(program)
    }

    fn managed_root(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(managed_root(self.platform()))
    }

    fn extra_managed_root(&self) -> Option<std::path::PathBuf> {
        std::env::var_os("RELAIS_TEST_MANAGED_ROOT").map(std::path::PathBuf::from)
    }

    fn user_config(&self, home: &Path) -> std::path::PathBuf {
        home.join(".claude.json")
    }

    fn verification_store(&self) -> Result<std::path::PathBuf, crate::paths::HomeUnset> {
        crate::paths::state_dir().map(|dir| super::store_path(&dir))
    }
}

/// Everything the sandbox preflight reads.
pub struct PreflightInputs<'a> {
    /// A `std::env::consts::OS` value.
    pub platform: &'a str,
    /// What `claude --version` printed, when the probe got an answer.
    pub harness_version: Option<&'a str>,
    /// Is this program on PATH?
    pub on_path: &'a dyn Fn(&str) -> bool,
    /// The real managed root, and the test root that adds to it.
    pub managed_root: &'a Path,
    pub extra_managed_root: Option<&'a Path>,
    /// `~/.claude.json`.
    pub user_config: &'a Path,
    /// The task worktree the worker will run in and the repository root:
    /// the only two `projects.<path>` entries of `~/.claude.json` a worker
    /// session reads.
    pub worktree: &'a Path,
    pub repo_root: &'a Path,
}

/// The first reason the sandbox cannot be relied on, or `None`.
pub fn preflight(inputs: &PreflightInputs) -> Option<Blocker> {
    platform_blocker(inputs.platform)
        .or_else(|| harness_blocker(inputs.harness_version))
        .or_else(|| helpers_blocker(inputs.platform, inputs.on_path))
        .or_else(|| weakened_blocker(inputs))
}

fn unavailable(detail: String) -> Blocker {
    Blocker {
        code: BlockCode::SandboxUnavailable,
        detail,
    }
}

pub fn platform_blocker(platform: &str) -> Option<Blocker> {
    sandbox_platform_refusal(platform).map(|reason| unavailable(reason.to_string()))
}

pub fn harness_blocker(version: Option<&str>) -> Option<Blocker> {
    let Some(text) = version else {
        return Some(unavailable(format!(
            "the harness version could not be read, and the OS sandbox is only measured on \
             Claude Code {SANDBOX_MIN_HARNESS} or newer; fix the `claude` binary or turn \
             `[sandbox]` off"
        )));
    };
    let Some(found) = parse_version(text) else {
        return Some(unavailable(format!(
            "the harness version `{}` is not a version relais can compare, and the OS sandbox \
             is only measured on Claude Code {SANDBOX_MIN_HARNESS} or newer; upgrade `claude` \
             or turn `[sandbox]` off",
            text.trim()
        )));
    };
    let minimum = parse_version(SANDBOX_MIN_HARNESS).expect("the minimum is a version");
    (found < minimum).then(|| {
        unavailable(format!(
            "Claude Code {} is older than {SANDBOX_MIN_HARNESS}, the only version the OS \
             sandbox is measured on; upgrade `claude` or turn `[sandbox]` off",
            text.trim()
        ))
    })
}

/// `2.1.285` or `2.1.285 (Claude Code)` as numbers; anything else is
/// unknown, never a guess.
fn parse_version(text: &str) -> Option<[u64; 3]> {
    let mut parts = text.split_whitespace().next()?.split('.');
    let mut numbers = [0; 3];
    for number in &mut numbers {
        *number = parts.next()?.parse().ok()?;
    }
    parts.next().is_none().then_some(numbers)
}

pub fn helpers_blocker(platform: &str, on_path: &dyn Fn(&str) -> bool) -> Option<Blocker> {
    if platform != "linux" {
        return None;
    }
    let missing: Vec<&str> = LINUX_TOOLS
        .into_iter()
        .filter(|tool| !on_path(tool))
        .collect();
    (!missing.is_empty()).then(|| {
        unavailable(format!(
            "the Linux OS sandbox needs `bwrap` (bubblewrap) and `socat` on PATH; missing: \
             {}. Install them, or turn `[sandbox]` off",
            missing.join(", ")
        ))
    })
}

/// The bytes of every managed file the preflight judges, each with the path
/// it was read from: what a verification is keyed by, so a change to any
/// of them is a change to the configuration that was probed.
pub fn managed_bytes(
    root: &Path,
    extra_root: Option<&Path>,
) -> Result<Vec<(PathBuf, Vec<u8>)>, String> {
    let sources = managed_sources(root, extra_root)
        .map_err(|(path, error)| format!("{} cannot be listed: {error}", path.display()))?;
    sources
        .into_iter()
        .map(|source| match std::fs::read(&source) {
            Ok(bytes) => Ok((source, bytes)),
            Err(error) => Err(format!("{} cannot be read: {error}", source.display())),
        })
        .collect()
}

/// The managed roots and `~/.claude.json`, judged. A file that cannot be
/// read or parsed is a finding, not an absence: only `NotFound` means
/// there is nothing to judge.
pub fn weakened_blocker(inputs: &PreflightInputs) -> Option<Blocker> {
    weakenings(inputs).map(|found| {
        let listed: Vec<String> = found
            .iter()
            .map(|w| format!("{}: {} — {}", w.source.display(), w.key, w.reason))
            .collect();
        Blocker {
            code: BlockCode::SandboxWeakened,
            detail: format!(
                "the machine's Claude Code configuration would weaken the worker sandbox: {}. \
                 Remove those settings, or turn `[sandbox]` off",
                listed.join("; ")
            ),
        }
    })
}

/// Every way the managed roots and `~/.claude.json` weaken the sandbox, or
/// `None` when nothing does.
pub fn weakenings(inputs: &PreflightInputs) -> Option<Vec<Weakening>> {
    let mut found: Vec<Weakening> = Vec::new();
    let mut settings = Vec::new();
    let mut mcp = Vec::new();
    match managed_sources(inputs.managed_root, inputs.extra_managed_root) {
        Err((path, error)) => found.push(unreadable(path, &error.to_string())),
        Ok(sources) => {
            for source in sources {
                match read_json(&source) {
                    Ok(Some(doc)) if is_mcp_file(&source) => mcp.push((source, doc)),
                    Ok(Some(doc)) => settings.push((source, doc)),
                    Ok(None) => {}
                    Err(reason) => found.push(unreadable(source, &reason)),
                }
            }
        }
    }
    if let Err(weakenings) = judge_managed(&settings, &mcp) {
        found.extend(weakenings);
    }
    match read_json(inputs.user_config) {
        Ok(Some(doc)) => found.extend(judge_user_config(&doc, inputs.worktree, inputs.repo_root)),
        Ok(None) => {}
        Err(reason) => found.push(unreadable(inputs.user_config.to_path_buf(), &reason)),
    }
    (!found.is_empty()).then_some(found)
}

fn unreadable(source: PathBuf, reason: &str) -> Weakening {
    Weakening {
        source,
        key: "$".to_string(),
        reason: format!("cannot be read ({reason}); relais fails closed"),
    }
}

fn is_mcp_file(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name == "managed-mcp.json")
}

/// The JSON in `path`; `None` when there is no such file.
fn read_json(path: &Path) -> Result<Option<Value>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("not valid JSON: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_dir;

    fn everything(_: &str) -> bool {
        true
    }

    #[test]
    fn a_platform_without_the_sandbox_is_unavailable_with_what_to_do() {
        let blocker = platform_blocker("windows").expect("blocked");
        assert_eq!(blocker.code, BlockCode::SandboxUnavailable);
        assert!(blocker.detail.contains("macOS and Linux only"));
        assert!(blocker.detail.contains("turn `[sandbox]` off"));
        assert_eq!(platform_blocker("macos"), None);
        assert_eq!(platform_blocker("linux"), None);
    }

    #[test]
    fn the_harness_must_be_at_least_the_measured_version() {
        for ok in [
            "2.1.285",
            "2.1.285 (Claude Code)",
            "2.1.300",
            "2.2.0",
            "3.0.0",
        ] {
            assert_eq!(harness_blocker(Some(ok)), None, "{ok}");
        }
        // Numeric, not textual: `2.1.1000` is newer and `2.1.30` is older.
        assert_eq!(harness_blocker(Some("2.1.1000")), None);
        for old in ["2.1.284", "2.1.30", "1.9.999", "2.0.285"] {
            let blocker = harness_blocker(Some(old)).expect(old);
            assert_eq!(blocker.code, BlockCode::SandboxUnavailable, "{old}");
            assert!(blocker.detail.contains(SANDBOX_MIN_HARNESS), "{old}");
        }
    }

    #[test]
    fn an_unknown_harness_version_blocks() {
        for unknown in [None, Some(""), Some("banana"), Some("2.1"), Some("2.1.x")] {
            let blocker = harness_blocker(unknown).expect("blocked");
            assert_eq!(blocker.code, BlockCode::SandboxUnavailable, "{unknown:?}");
        }
    }

    #[test]
    fn linux_needs_bwrap_and_socat_and_macos_does_not() {
        let only = |have: &'static str| move |name: &str| name == have;
        assert_eq!(helpers_blocker("linux", &everything), None);
        let no_socat = helpers_blocker("linux", &only("bwrap")).expect("blocked");
        assert!(no_socat.detail.contains("missing: socat"), "{no_socat:?}");
        let no_bwrap = helpers_blocker("linux", &only("socat")).expect("blocked");
        assert!(no_bwrap.detail.contains("missing: bwrap"), "{no_bwrap:?}");
        let neither = helpers_blocker("linux", &|_: &str| false).expect("blocked");
        assert_eq!(neither.code, BlockCode::SandboxUnavailable);
        assert!(neither.detail.contains("bwrap, socat"));
        assert_eq!(helpers_blocker("macos", &|_: &str| false), None);
    }

    /// `preflight` over two temp roots and a user config path.
    fn weakened(real: &Path, extra: Option<&Path>, user_config: &Path) -> Option<Blocker> {
        weakened_blocker(&PreflightInputs {
            platform: "macos",
            harness_version: Some(SANDBOX_MIN_HARNESS),
            on_path: &everything,
            managed_root: real,
            extra_managed_root: extra,
            user_config,
            worktree: Path::new("/p"),
            repo_root: Path::new("/repo"),
        })
    }

    #[test]
    fn nothing_to_read_is_nothing_weakened() {
        let root = temp_dir("preflight-empty");
        assert_eq!(weakened(&root, None, &root.join(".claude.json")), None);
    }

    #[test]
    fn a_managed_source_that_cannot_be_read_fails_closed() {
        let root = temp_dir("preflight-unreadable");
        // A file where the drop-in directory should be: the listing fails.
        std::fs::write(root.join("managed-settings.d"), "").expect("write");
        let blocker = weakened(&root, None, &root.join(".claude.json")).expect("blocked");
        assert_eq!(blocker.code, BlockCode::SandboxWeakened);
        assert!(blocker.detail.contains("managed-settings.d"), "{blocker:?}");
        assert!(blocker.detail.contains("fails closed"), "{blocker:?}");
    }

    #[test]
    fn a_managed_file_that_is_not_json_fails_closed() {
        let root = temp_dir("preflight-garbage");
        std::fs::write(root.join("managed-settings.json"), "{not json").expect("write");
        let blocker = weakened(&root, None, &root.join(".claude.json")).expect("blocked");
        assert_eq!(blocker.code, BlockCode::SandboxWeakened);
        assert!(
            blocker.detail.contains("managed-settings.json"),
            "{blocker:?}"
        );
    }

    #[test]
    fn a_weakening_managed_setting_reaches_the_message_with_source_key_and_reason() {
        let root = temp_dir("preflight-managed");
        std::fs::write(
            root.join("managed-settings.json"),
            r#"{"sandbox": {"allowUnsandboxedCommands": true}, "hooks": {}}"#,
        )
        .expect("write");
        let blocker = weakened(&root, None, &root.join(".claude.json")).expect("blocked");
        assert_eq!(blocker.code, BlockCode::SandboxWeakened);
        let source = root.join("managed-settings.json");
        assert!(
            blocker.detail.contains(&format!(
                "{}: sandbox.allowUnsandboxedCommands — is accepted only as false",
                source.display()
            )),
            "{blocker:?}"
        );
        assert!(
            blocker.detail.contains("hooks — is not an accepted key"),
            "each weakening is listed: {blocker:?}"
        );
    }

    #[test]
    fn a_managed_mcp_server_reaches_the_message() {
        let root = temp_dir("preflight-mcp");
        std::fs::write(
            root.join("managed-mcp.json"),
            r#"{"mcpServers": {"x": {"command": "x"}}}"#,
        )
        .expect("write");
        let blocker = weakened(&root, None, &root.join(".claude.json")).expect("blocked");
        assert!(blocker.detail.contains("managed-mcp.json"), "{blocker:?}");
        assert!(blocker.detail.contains("mcpServers"), "{blocker:?}");
    }

    #[test]
    fn a_weakening_user_config_reaches_the_message() {
        let root = temp_dir("preflight-user");
        let user = root.join(".claude.json");
        std::fs::write(
            &user,
            r#"{"permissions": {"allow": ["Bash"]},
                "projects": {"/p": {"allowedTools": ["Bash(rm:*)"]}}}"#,
        )
        .expect("write");
        let blocker = weakened(&root, None, &user).expect("blocked");
        assert_eq!(blocker.code, BlockCode::SandboxWeakened);
        assert!(blocker.detail.contains("permissions.allow"), "{blocker:?}");
        assert!(
            blocker.detail.contains("projects./p.allowedTools"),
            "{blocker:?}"
        );
    }

    #[test]
    fn a_user_config_that_is_not_json_fails_closed() {
        let root = temp_dir("preflight-user-garbage");
        let user = root.join(".claude.json");
        std::fs::write(&user, "nope").expect("write");
        let blocker = weakened(&root, None, &user).expect("blocked");
        assert!(blocker.detail.contains("fails closed"), "{blocker:?}");
    }

    #[test]
    fn the_test_root_is_read_in_addition_to_the_real_one() {
        let real = temp_dir("preflight-real");
        let extra = temp_dir("preflight-extra");
        let user = real.join(".claude.json");
        assert_eq!(weakened(&real, Some(&extra), &user), None);

        std::fs::write(
            extra.join("managed-settings.json"),
            r#"{"env": {"X": "1"}}"#,
        )
        .expect("write");
        let from_extra = weakened(&real, Some(&extra), &user).expect("the extra root counts");
        assert!(from_extra.detail.contains(&*extra.to_string_lossy()));

        std::fs::remove_file(extra.join("managed-settings.json")).expect("rm");
        std::fs::write(real.join("managed-settings.json"), r#"{"env": {"X": "1"}}"#)
            .expect("write");
        assert!(
            weakened(&real, Some(&extra), &user).is_some(),
            "the extra root never replaces the real one"
        );
    }

    #[test]
    fn preflight_reports_the_platform_before_anything_else() {
        let root = temp_dir("preflight-order");
        let inputs = PreflightInputs {
            platform: "windows",
            harness_version: None,
            on_path: &|_: &str| false,
            managed_root: &root,
            extra_managed_root: None,
            user_config: &root.join(".claude.json"),
            worktree: Path::new("/p"),
            repo_root: Path::new("/repo"),
        };
        let blocker = preflight(&inputs).expect("blocked");
        assert!(blocker.detail.contains("macOS and Linux only"));
    }

    #[test]
    fn the_real_managed_roots_are_the_documented_ones() {
        assert_eq!(
            managed_root("macos"),
            "/Library/Application Support/ClaudeCode"
        );
        assert_eq!(managed_root("linux"), "/etc/claude-code");
    }
}
