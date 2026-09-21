//! What is installed on this machine (SPEC §5).
//!
//! Which programs answer on `PATH`, where they resolve to, and what
//! version they report. A fact about the machine, not a decision:
//! `policy` intersects declarations and never looks at `PATH`, and every
//! caller that needs the answer — `doctor`, `plan`, `run`, the backend's
//! own discovery, the baseline cache key — probes here and hands the
//! result to the pure functions as a value.
//!
//! One helper resolves a program name, on every platform: on Windows an
//! executable is `git.exe`, `claude.cmd` or a `.ps1` shim, and a lookup
//! that only tries the bare name finds nothing at all (audit C2). The
//! resolution itself is a pure function over a directory listing, so it
//! is tested on Unix too.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::policy::{BlockCode, Blocker, DependencyMode, RepoPolicy};
use crate::procs::{run_with_timeout, Ended};

/// How long a `--version`/`--help` probe may take. A probe is a question
/// about the machine, not work: an integration that will not answer in
/// this long is an integration that is not answering (audit V6).
///
/// Generous, because the answer decides whether a run happens at all: a
/// node-based CLI on a machine already running a build takes seconds to
/// start, and reporting THAT as "not installed" would block a run over a
/// busy laptop.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

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
            match dependency.mode() {
                DependencyMode::Required => {
                    let bin = dependency.bin().unwrap_or(default_bin(name));
                    if !binary_available(bin) {
                        blockers.push(Blocker {
                            code: BlockCode::IntegrationMissing,
                            detail: format!(
                                "required integration `{name}` is not available on PATH"
                            ),
                        });
                    }
                }
                // An optional integration that is missing is a gap the
                // report carries (`verify::integration_gaps`), never a
                // blocker; one that is off is not consulted at all.
                DependencyMode::Optional | DependencyMode::Off => {}
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

/// The executable suffixes a program name may carry on this platform.
/// Unix has none; Windows has `PATHEXT`, and a program is only
/// executable with one of them — `.COM;.EXE;.BAT;.CMD` at a minimum,
/// plus the `.ps1` shims npm-installed CLIs ship.
pub fn path_extensions(pathext: Option<&OsString>) -> Vec<String> {
    if !cfg!(windows) {
        return vec![String::new()];
    }
    // The exact name first: a program that already carries its suffix
    // must not resolve to `git.exe.exe`.
    let mut extensions = vec![String::new()];
    let declared = pathext
        .map(|raw| raw.to_string_lossy().into_owned())
        .unwrap_or_default();
    for extension in declared.split(';') {
        push_extension(&mut extensions, extension);
    }
    // The documented defaults, plus the `.cmd`/`.ps1` shims npm and pnpm
    // install CLIs as — executable through the shell, and not always in
    // a machine's PATHEXT.
    for extension in [".com", ".exe", ".bat", ".cmd", ".ps1"] {
        push_extension(&mut extensions, extension);
    }
    extensions
}

fn push_extension(into: &mut Vec<String>, extension: &str) {
    let extension = extension.trim().to_ascii_lowercase();
    if extension.starts_with('.') && !into.iter().any(|seen| seen == &extension) {
        into.push(extension);
    }
}

/// Resolve a program name against a list of directories, trying each
/// executable suffix in turn. Pure: the directories, the suffixes and the
/// existence test are all given, which is what makes the Windows
/// behaviour testable on a Unix machine.
pub fn resolve_in(
    name: &str,
    dirs: &[PathBuf],
    extensions: &[String],
    exists: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    for dir in dirs {
        for extension in extensions {
            let candidate = dir.join(format!("{name}{extension}"));
            if exists(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Where this machine's `PATH` resolves a program name, if anywhere.
pub fn which(name: &str) -> Option<PathBuf> {
    // A name that already carries a path is not a PATH lookup.
    if Path::new(name).components().count() > 1 {
        let named = PathBuf::from(name);
        return named.is_file().then_some(named);
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    let extensions = path_extensions(std::env::var_os("PATHEXT").as_ref());
    resolve_in(name, &dirs, &extensions, &|candidate| candidate.is_file())
}

/// Is this binary on PATH?
pub fn binary_available(bin: &str) -> bool {
    which(bin).is_some()
}

/// Is the integration's binary on PATH? By the integration's config name
/// (`amont_agent` → `amont-agent`), ignoring any `bin` override.
pub fn integration_available(name: &str) -> bool {
    let name = name.replace('-', "_");
    binary_available(default_bin(&name))
}

/// A program as this machine resolves it: where it lives and what it
/// says its version is. Both halves matter — two machines with the same
/// `cargo --version` and different `cargo` on PATH are two toolchains.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProgramVersion {
    pub program: String,
    pub path: String,
    pub version: String,
}

/// Why a program could not be versioned. Not an error in itself — it is
/// the reason a baseline verdict cannot be cached (SPEC §18).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionUnknown {
    /// Nothing on PATH answers to this name.
    NotOnPath { program: String },
    /// It is there, but `--version` did not end in a readable answer.
    NoAnswer { program: String, detail: String },
}

impl std::fmt::Display for VersionUnknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotOnPath { program } => {
                write!(f, "`{program}` is not on PATH")
            }
            Self::NoAnswer { program, detail } => {
                write!(f, "`{program} --version` did not answer: {detail}")
            }
        }
    }
}

impl std::error::Error for VersionUnknown {}

/// Resolve a program and ask it for its version, under the probe timeout
/// and the run's cancel flag. The pair (resolved path, reported version)
/// is what identifies a toolchain; either half missing means this
/// machine cannot say what would run.
pub fn program_version(
    program: &str,
    cancel: Option<&AtomicBool>,
) -> Result<ProgramVersion, VersionUnknown> {
    let path = which(program).ok_or_else(|| VersionUnknown::NotOnPath {
        program: program.to_string(),
    })?;
    let mut command = std::process::Command::new(&path);
    command.arg("--version");
    let end = run_with_timeout(command, PROBE_TIMEOUT, None, cancel, None).map_err(|e| {
        VersionUnknown::NoAnswer {
            program: program.to_string(),
            detail: e.to_string(),
        }
    })?;
    if end.ended != Ended::Exited(0) {
        return Err(VersionUnknown::NoAnswer {
            program: program.to_string(),
            detail: end.ended.describe(),
        });
    }
    let version = first_line(&end.stdout)
        .or_else(|| first_line(&end.stderr))
        .ok_or_else(|| VersionUnknown::NoAnswer {
            program: program.to_string(),
            detail: "it printed nothing".to_string(),
        })?;
    Ok(ProgramVersion {
        program: program.to_string(),
        path: path.to_string_lossy().into_owned(),
        version,
    })
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .next()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
}

/// `<tool> --version`'s first line, or `None` when the tool is absent or
/// will not answer — recorded in the context manifest, which is where a
/// run names the toolchain it was verified with.
pub fn integration_version(name: &str) -> Option<String> {
    program_version(default_bin(name), None)
        .ok()
        .map(|resolved| resolved.version)
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
        assert!(matches!(
            program_version("relais-no-such-binary-4f3a", None),
            Err(VersionUnknown::NotOnPath { .. })
        ));
    }

    /// C2: on Windows the file is `git.exe` or `claude.cmd`, never the
    /// bare name — a lookup that tries only the bare name finds nothing
    /// and `doctor` reports a machine with git installed as having none.
    /// The resolution is a pure function of a directory listing, so this
    /// runs everywhere.
    #[test]
    fn a_windows_program_resolves_through_its_pathext_suffix() {
        let dirs = vec![PathBuf::from("C:/tools"), PathBuf::from("C:/npm")];
        let listing = ["C:/tools/git.exe", "C:/npm/claude.cmd", "C:/npm/aval.ps1"];
        let exists = |candidate: &Path| listing.iter().any(|entry| Path::new(entry) == candidate);
        let extensions: Vec<String> = [".com", ".exe", ".bat", ".cmd", ".ps1"]
            .iter()
            .map(|ext| ext.to_string())
            .collect();
        let bare = vec![String::new()];

        assert_eq!(
            resolve_in("git", &dirs, &extensions, &exists),
            Some(PathBuf::from("C:/tools/git.exe"))
        );
        assert_eq!(
            resolve_in("claude", &dirs, &extensions, &exists),
            Some(PathBuf::from("C:/npm/claude.cmd")),
            "an npm-installed CLI is a .cmd shim"
        );
        assert_eq!(
            resolve_in("aval", &dirs, &extensions, &exists),
            Some(PathBuf::from("C:/npm/aval.ps1"))
        );
        assert_eq!(
            resolve_in("git", &dirs, &bare, &exists),
            None,
            "the extensionless lookup is what found nothing on Windows"
        );
        assert_eq!(resolve_in("nope", &dirs, &extensions, &exists), None);
    }

    /// The suffix list comes from `PATHEXT` where the platform has one,
    /// with the shim suffixes npm installs added, and is a single empty
    /// suffix everywhere else.
    #[test]
    fn the_suffix_list_is_the_platforms_own() {
        let declared = OsString::from(".COM;.EXE;.BAT");
        let extensions = path_extensions(Some(&declared));
        if cfg!(windows) {
            assert_eq!(extensions[0], "", "the exact name is tried first");
            assert!(extensions.iter().any(|ext| ext == ".exe"));
            assert!(extensions.iter().any(|ext| ext == ".cmd"), "{extensions:?}");
            assert!(extensions.iter().any(|ext| ext == ".ps1"), "{extensions:?}");
            assert!(path_extensions(None).iter().any(|ext| ext == ".exe"));
        } else {
            assert_eq!(extensions, vec![String::new()]);
        }
    }

    /// A program that is on PATH answers with a version and the path it
    /// resolved to: the pair is the toolchain identity a baseline cache
    /// key is built from (SPEC §18).
    #[test]
    fn a_program_on_path_reports_a_version_and_where_it_resolved() {
        let resolved = program_version("git", None).expect("git is installed for these tests");
        assert!(resolved.version.to_lowercase().contains("git"));
        assert!(resolved.path.ends_with("git") || resolved.path.contains("git"));
        assert_eq!(resolved.program, "git");
    }
}
