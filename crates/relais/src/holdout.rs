//! Holdout evidence, resolved at preflight (ADR-0045, SPEC §4).
//!
//! A holdout is an acceptance test kept outside the candidate's repository.
//! This module only checks that a holdout criterion can be settled later: the
//! declaration exists in machine.toml, names a JUnit report and a clean
//! overlay path, the overlay is free at the base revision, and the store
//! resolves to one commit. Nothing here fetches or runs a holdout, and
//! nothing it learns is shown to a worker.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::acceptance::{AcceptanceEntry, Evidence};
use crate::ids::sha256_hex;
use crate::policy::{BlockCode, HoldoutDecl, HoldoutSettings};
use crate::workspace::Git;

/// How long `git ls-remote` may take before the store counts as unreadable.
const LS_REMOTE_TIMEOUT: Duration = Duration::from_secs(30);

/// A holdout whose store resolved: everything phase 3 needs to run it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedHoldout {
    pub name: String,
    pub repo: String,
    pub store: String,
    pub sha: String,
    pub decl: HoldoutDecl,
    pub digest: String,
}

/// What the context manifest keeps of a resolved holdout: no store URL,
/// path or overlay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldoutRecord {
    pub name: String,
    pub sha: String,
    pub digest: String,
}

impl ResolvedHoldout {
    pub fn record(&self) -> HoldoutRecord {
        HoldoutRecord {
            name: self.name.clone(),
            sha: self.sha.clone(),
            digest: self.digest.clone(),
        }
    }
}

/// Why a holdout criterion is refused at preflight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoldoutRefusal {
    Unknown {
        declared: Vec<String>,
    },
    NoJunit,
    BadOverlayPath(String),
    OverlayExists {
        overlay: String,
        base_sha: String,
    },
    BaseUnreadable {
        base_sha: String,
        cause: String,
    },
    Unreadable {
        store: String,
        git_ref: String,
        stderr_last_line: String,
    },
}

impl HoldoutRefusal {
    pub fn code(&self) -> BlockCode {
        match self {
            Self::Unknown { .. } => BlockCode::HoldoutUnknown,
            Self::NoJunit => BlockCode::HoldoutNoJunit,
            Self::BadOverlayPath(_) => BlockCode::HoldoutBadOverlay,
            Self::OverlayExists { .. } => BlockCode::HoldoutOverlayExists,
            Self::BaseUnreadable { .. } => BlockCode::BaseUnresolvable,
            Self::Unreadable { .. } => BlockCode::HoldoutUnreadable,
        }
    }

    fn describe(&self, name: &str, machine_toml: &str) -> String {
        match self {
            Self::Unknown { declared } => format!(
                "holdout `{name}` is not declared in {machine_toml} [holdouts] for this \
                 repository (declared: {})",
                if declared.is_empty() {
                    "none".to_string()
                } else {
                    declared.join(", ")
                }
            ),
            Self::NoJunit => format!(
                "holdout `{name}` declares no `command.junit`: a holdout settles only through a \
                 parsed JUnit report"
            ),
            Self::BadOverlayPath(path) => format!(
                "holdout `{name}` overlay `{path}` is not a clean repo-relative path (no leading \
                 `/`, no `.` or `..` segment)"
            ),
            Self::OverlayExists { overlay, base_sha } => format!(
                "holdout `{name}` overlay `{overlay}` already exists at base {base_sha}: a \
                 holdout never replaces a file the repository has"
            ),
            Self::BaseUnreadable { base_sha, cause } => format!(
                "holdout `{name}`: base {base_sha} could not be searched for the overlay: {cause}"
            ),
            Self::Unreadable {
                store,
                git_ref,
                stderr_last_line,
            } => format!(
                "holdout `{name}` store `{store}` does not resolve `{git_ref}`: \
                 {stderr_last_line}; retry with `GIT_TERMINAL_PROMPT=0 git ls-remote {store} \
                 {git_ref}`"
            ),
        }
    }
}

/// A refusal of one holdout criterion, with the sentence that names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoldoutBlock {
    pub code: BlockCode,
    pub detail: String,
}

/// The distinct holdout names the criteria declare.
pub fn holdout_names(entries: &[AcceptanceEntry]) -> Vec<String> {
    let mut names: Vec<String> = entries
        .iter()
        .filter_map(|entry| match entry.evidence() {
            Some(Evidence::Holdout { name }) => Some(name.clone()),
            Some(
                Evidence::Check { .. }
                | Evidence::Test { .. }
                | Evidence::LlmReview
                | Evidence::HumanSignOff
                | Evidence::AmontGate { .. },
            )
            | None => None,
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Resolve every holdout criterion, stopping at the first refusal.
pub fn resolve_all(
    entries: &[AcceptanceEntry],
    settings: &HoldoutSettings,
    repo: &str,
    git: &dyn Git,
    repo_dir: &Path,
    base_sha: &str,
    machine_toml: &str,
) -> Result<Vec<ResolvedHoldout>, HoldoutBlock> {
    holdout_names(entries)
        .iter()
        .map(|name| {
            resolve(settings, repo, name, git, repo_dir, base_sha).map_err(|refusal| HoldoutBlock {
                code: refusal.code(),
                detail: refusal.describe(name, machine_toml),
            })
        })
        .collect()
}

/// Resolve one holdout. The order of the checks is the order of the
/// refusals in SPEC §4.
pub fn resolve(
    settings: &HoldoutSettings,
    repo: &str,
    name: &str,
    git: &dyn Git,
    repo_dir: &Path,
    base_sha: &str,
) -> Result<ResolvedHoldout, HoldoutRefusal> {
    let decl = settings
        .declaration(repo, name)
        .ok_or_else(|| HoldoutRefusal::Unknown {
            declared: settings.declared_names(repo),
        })?;
    if decl.command.junit.is_none() {
        return Err(HoldoutRefusal::NoJunit);
    }
    if !clean_relative(&decl.overlay) {
        return Err(HoldoutRefusal::BadOverlayPath(decl.overlay.clone()));
    }
    match git.run(
        repo_dir,
        &["ls-tree", "--name-only", base_sha, "--", &decl.overlay],
    ) {
        Ok(listing) if !listing.trim().is_empty() => {
            return Err(HoldoutRefusal::OverlayExists {
                overlay: decl.overlay.clone(),
                base_sha: base_sha.to_string(),
            })
        }
        Ok(_) => {}
        Err(cause) => {
            return Err(HoldoutRefusal::BaseUnreadable {
                base_sha: base_sha.to_string(),
                cause: cause.to_string(),
            })
        }
    }
    let store = settings.store.clone().unwrap_or_default();
    let unreadable = |stderr_last_line: String| HoldoutRefusal::Unreadable {
        store: store.clone(),
        git_ref: settings.git_ref.clone(),
        stderr_last_line,
    };
    if store.is_empty() {
        return Err(unreadable("no `store` is set in [holdouts]".to_string()));
    }
    let sha = ls_remote(&store, &settings.git_ref).map_err(unreadable)?;
    let digest = digest_of(repo, name, &store, &sha, decl);
    Ok(ResolvedHoldout {
        name: name.to_string(),
        repo: repo.to_string(),
        store,
        sha,
        decl: decl.clone(),
        digest,
    })
}

fn clean_relative(path: &str) -> bool {
    !path.trim().is_empty()
        && !path.starts_with('/')
        && !Path::new(path).is_absolute()
        && !path
            .split(['/', '\\'])
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
}

fn digest_of(repo: &str, name: &str, store: &str, sha: &str, decl: &HoldoutDecl) -> String {
    let canonical = serde_json::json!({
        "repo": repo,
        "name": name,
        "store": store,
        "sha": sha,
        "path": decl.path,
        "overlay": decl.overlay,
        "argv": decl.command.argv,
        "junit": decl.command.junit,
        "config": decl.config,
    });
    sha256_hex(canonical.to_string().as_bytes())
}

/// `git ls-remote <store> <ref>` that cannot prompt and cannot hang: no
/// terminal prompt, ssh in batch mode, stdin from /dev/null, bounded by
/// [`LS_REMOTE_TIMEOUT`]. The one sha the ref names, or the reason it
/// could not be had.
fn ls_remote(store: &str, git_ref: &str) -> Result<String, String> {
    let mut command = Command::new("git");
    command
        .args(["ls-remote", store, git_ref])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    let output = crate::procs::run_with_timeout(command, LS_REMOTE_TIMEOUT, None, None)
        .map_err(|e| format!("git ls-remote could not be run: {e}"))?;
    match output.ended {
        crate::procs::Ended::Exited(0) => {}
        crate::procs::Ended::Exited(_) | crate::procs::Ended::Signalled => {
            return Err(last_line(&output.stderr, "git ls-remote failed"))
        }
        crate::procs::Ended::TimedOut | crate::procs::Ended::Cancelled => {
            return Err(format!(
                "git ls-remote did not answer within {} s",
                LS_REMOTE_TIMEOUT.as_secs()
            ))
        }
    }
    let stdout = output.stdout;
    let mut shas: Vec<&str> = stdout
        .lines()
        .filter(|line| !line.ends_with("^{}"))
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    shas.sort_unstable();
    shas.dedup();
    match shas.as_slice() {
        [only] => Ok((*only).to_string()),
        [] => Err(format!("`{git_ref}` names no ref in the store")),
        many => Err(format!(
            "`{git_ref}` names {} different commits",
            many.len()
        )),
    }
}

fn last_line(text: &str, fallback: &str) -> String {
    text.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(fallback)
        .trim()
        .to_string()
}

/// The machine.toml a person edits to declare a holdout, for a refusal's detail.
pub fn machine_toml_label() -> String {
    crate::paths::machine_settings_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "machine.toml".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::CommandSpec;

    fn decl(junit: Option<&str>, overlay: &str) -> HoldoutDecl {
        HoldoutDecl {
            path: "p.rs".into(),
            overlay: overlay.into(),
            command: serde_json::from_value::<CommandSpec>(
                serde_json::json!({ "argv": ["true"], "junit": junit }),
            )
            .expect("a command spec"),
            config: None,
        }
    }

    fn settings(store: &str, decl: HoldoutDecl) -> HoldoutSettings {
        let mut settings = HoldoutSettings {
            store: Some(store.into()),
            ..HoldoutSettings::default()
        };
        settings
            .repos
            .entry("r".into())
            .or_default()
            .insert("x".into(), decl);
        settings
    }

    fn never_exists() -> crate::workspace::FakeGit {
        crate::workspace::FakeGit::new(|_, _| Ok(String::new()))
    }

    fn refused(settings: &HoldoutSettings, git: &dyn Git) -> BlockCode {
        resolve(settings, "r", "x", git, Path::new("."), "abc")
            .expect_err("refused")
            .code()
    }

    #[test]
    fn each_refusal_has_its_own_code() {
        let git = never_exists();
        assert_eq!(
            refused(&HoldoutSettings::default(), &git),
            BlockCode::HoldoutUnknown
        );
        assert_eq!(
            refused(&settings("s", decl(None, "t/x.rs")), &git),
            BlockCode::HoldoutNoJunit
        );
        for bad in ["/abs.rs", "a/../b.rs", "./a.rs", ""] {
            assert_eq!(
                refused(&settings("s", decl(Some("j.xml"), bad)), &git),
                BlockCode::HoldoutBadOverlay,
                "{bad}"
            );
        }
        let present = crate::workspace::FakeGit::new(|_, _| Ok("t/x.rs\n".into()));
        assert_eq!(
            refused(&settings("s", decl(Some("j.xml"), "t/x.rs")), &present),
            BlockCode::HoldoutOverlayExists
        );
        assert_eq!(
            refused(
                &settings(
                    "file:///nonexistent/holdout-store",
                    decl(Some("j.xml"), "t/x.rs")
                ),
                &git
            ),
            BlockCode::HoldoutUnreadable
        );
    }

    #[test]
    fn a_git_failure_is_not_an_absent_overlay() {
        let broken = crate::workspace::FakeGit::new(|_, _| {
            Err(crate::workspace::WorkspaceError::Git("bad object".into()))
        });
        assert_eq!(
            refused(&settings("s", decl(Some("j.xml"), "t/x.rs")), &broken),
            BlockCode::BaseUnresolvable
        );
    }

    #[test]
    fn the_unknown_refusal_lists_the_declared_names() {
        let mut declared = settings("s", decl(Some("j.xml"), "t/x.rs"));
        declared
            .repos
            .get_mut("r")
            .expect("repo")
            .insert("y".into(), decl(Some("j.xml"), "t/y.rs"));
        let refusal = resolve(&declared, "r", "z", &never_exists(), Path::new("."), "abc")
            .expect_err("refused");
        assert_eq!(
            refusal,
            HoldoutRefusal::Unknown {
                declared: vec!["x".into(), "y".into()]
            }
        );
        let detail = refusal.describe("z", "/home/me/.config/relais/config.toml");
        assert!(detail.contains("x, y"), "{detail}");
        assert!(
            detail.contains("/home/me/.config/relais/config.toml"),
            "{detail}"
        );
    }
}
