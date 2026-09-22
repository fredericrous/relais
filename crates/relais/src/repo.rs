//! The repository a run belongs to, on disk.
//!
//! Finding the `relais.toml` that governs a directory, and writing the
//! starter one `relais init` produces. Both are filesystem operations on
//! a policy document, which is why they are here and not in `policy`:
//! that module decides authority from parsed text and touches nothing.

use std::path::Path;

use crate::policy::{RepoIdentity, INIT_TEMPLATE};

/// Why no policy was found upward from a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocateError {
    /// The repository root (the nearest `.git`) was reached and holds no
    /// `relais.toml`; the path is that root, where `relais init` belongs.
    RepoWithoutPolicy(std::path::PathBuf),
    /// Neither a policy nor a repository between the start directory and
    /// the filesystem root; the path is the start directory.
    NotInRepository(std::path::PathBuf),
}

impl std::fmt::Display for LocateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RepoWithoutPolicy(root) => write!(
                f,
                "no relais.toml in the repository at {} — run `relais init` there",
                root.display()
            ),
            Self::NotInRepository(start) => write!(
                f,
                "no relais.toml in {} or any parent, and no repository above it — cd into the repository (or its task worktree) and run `relais init`",
                start.display()
            ),
        }
    }
}

impl std::error::Error for LocateError {}

/// The directory whose `relais.toml` governs `start`: the nearest
/// ancestor (including `start` itself) holding one. The policy is the
/// repository's, not the shell's, so a subdirectory or a crate inside the
/// repository resolves to the same root. The search stops at the nearest
/// `.git` (a directory, or the file a worktree carries): a nested
/// repository never inherits the policy of the one that contains it.
pub fn locate_repo_root(start: &std::path::Path) -> Result<std::path::PathBuf, LocateError> {
    let mut dir = start.to_path_buf();
    loop {
        if dir.join("relais.toml").is_file() {
            return Ok(dir);
        }
        if dir.join(".git").exists() {
            return Err(LocateError::RepoWithoutPolicy(dir));
        }
        if !dir.pop() {
            return Err(LocateError::NotInRepository(start.to_path_buf()));
        }
    }
}

/// Which repository this is, for binding a trust grant (SPEC §5).
///
/// The root is canonicalized, so `.`, a relative path and a symlinked
/// path all name one repository rather than three. When canonicalization
/// fails — a directory that was deleted or cannot be read — the path is
/// used as given: the identity is then narrower than it might be, which
/// costs a review and never grants one.
pub fn identity(root: &Path) -> RepoIdentity {
    let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    RepoIdentity::new(&canonical, origin_url(root))
}

/// The `origin` remote's URL, when git reports one.
///
/// Every way of not getting one — no such remote, no repository, git not
/// installed — answers `None`, because they are the same answer to the
/// question asked: this repository has no origin to bind to, so its
/// identity is its root path. The failure direction is safe: a grant
/// issued with an origin stops matching, and the run blocks on the
/// missing grant rather than running under someone else's review.
fn origin_url(root: &Path) -> Option<String> {
    let output = crate::workspace::git_command(root)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if url.is_empty() {
        None
    } else {
        Some(url)
    }
}

/// Write the template unless a policy already exists. Returns `false`
/// when the file was present; init never overwrites.
pub fn write_init_template(path: &std::path::Path) -> std::io::Result<bool> {
    use std::io::Write;
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(e) => return Err(e),
    };
    file.write_all(INIT_TEMPLATE.as_bytes())?;
    Ok(true)
}

/// An ecosystem that keeps its dependencies inside the tree, named by
/// the lockfile it leaves at the root, with the command that installs
/// exactly what the lockfile says. A verification worktree is a checkout
/// of one revision and nothing else, so a profile in such a repository
/// usually needs a setup step before its commands can run — `usually`,
/// because a documentation profile needs nothing, which is why relais
/// only ever SUGGESTS the block and never runs it uninvited (SPEC §7).
///
/// Go and Rust are absent on purpose: `go test` and `cargo test` fetch
/// into caches shared across worktrees, and rustup installs a pinned
/// toolchain itself. A `Cargo.lock` calls for nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ecosystem {
    pub name: &'static str,
    pub lockfile: &'static str,
    pub setup_argv: &'static [&'static str],
}

impl Ecosystem {
    /// The `[[verification.profiles.<name>.setup]]` block that installs
    /// this ecosystem's dependencies, as a user would paste it.
    pub fn setup_block(&self, profile: &str) -> String {
        let argv = self
            .setup_argv
            .iter()
            .map(|arg| format!("{arg:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("[[verification.profiles.{profile}.setup]]\nargv = [{argv}]")
    }
}

pub const ECOSYSTEMS: &[Ecosystem] = &[
    Ecosystem {
        name: "npm",
        lockfile: "package-lock.json",
        setup_argv: &["npm", "ci"],
    },
    Ecosystem {
        name: "npm",
        lockfile: "npm-shrinkwrap.json",
        setup_argv: &["npm", "ci"],
    },
    Ecosystem {
        name: "pnpm",
        lockfile: "pnpm-lock.yaml",
        setup_argv: &["pnpm", "install", "--frozen-lockfile"],
    },
    Ecosystem {
        name: "yarn",
        lockfile: "yarn.lock",
        setup_argv: &["yarn", "install", "--immutable"],
    },
    Ecosystem {
        name: "bun",
        lockfile: "bun.lock",
        setup_argv: &["bun", "install", "--frozen-lockfile"],
    },
    Ecosystem {
        name: "bun",
        lockfile: "bun.lockb",
        setup_argv: &["bun", "install", "--frozen-lockfile"],
    },
    Ecosystem {
        name: "uv",
        lockfile: "uv.lock",
        setup_argv: &["uv", "sync", "--frozen"],
    },
    Ecosystem {
        name: "poetry",
        lockfile: "poetry.lock",
        setup_argv: &["poetry", "install", "--sync"],
    },
    Ecosystem {
        name: "pipenv",
        lockfile: "Pipfile.lock",
        setup_argv: &["pipenv", "sync"],
    },
    Ecosystem {
        name: "bundler",
        lockfile: "Gemfile.lock",
        setup_argv: &["bundle", "install"],
    },
    Ecosystem {
        name: "composer",
        lockfile: "composer.lock",
        setup_argv: &["composer", "install"],
    },
];

/// The ecosystems whose lockfile sits at the repository root, in table
/// order. A repository carrying two lockfiles (a `package-lock.json`
/// beside a `pnpm-lock.yaml`) is reported twice: relais does not know
/// which one is live, and saying so is more honest than picking.
pub fn lockfiles(repo_dir: &Path) -> Vec<&'static Ecosystem> {
    ECOSYSTEMS
        .iter()
        .filter(|ecosystem| repo_dir.join(ecosystem.lockfile).is_file())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lockfile_at_the_root_names_its_installer() {
        let dir = temp_dir("lockfiles");
        assert!(
            lockfiles(&dir).is_empty(),
            "no lockfile, nothing to suggest"
        );
        std::fs::write(dir.join("package-lock.json"), "{}").expect("lockfile");
        std::fs::write(dir.join("Cargo.lock"), "").expect("cargo lock");
        let found = lockfiles(&dir);
        assert_eq!(found.len(), 1, "Cargo.lock calls for no setup: {found:?}");
        assert_eq!(found[0].setup_argv, ["npm", "ci"]);
        assert_eq!(
            found[0].setup_block("default"),
            "[[verification.profiles.default.setup]]\nargv = [\"npm\", \"ci\"]"
        );
        std::fs::create_dir_all(dir.join("apps/web")).expect("mkdir");
        std::fs::write(dir.join("apps/web/pnpm-lock.yaml"), "").expect("nested");
        assert_eq!(
            lockfiles(&dir).len(),
            1,
            "a nested lockfile is a workspace member's; the root's setup is the root's"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Every lockfile that earns a setup suggestion is also a
    /// verification input: a candidate that edits one changes what the
    /// setup installs, and that is the user's decision (SPEC §10).
    #[test]
    fn every_ecosystem_lockfile_is_a_verification_input() {
        for ecosystem in ECOSYSTEMS {
            assert!(
                crate::verify::POLICY_VERIFICATION_INPUTS.contains(&ecosystem.lockfile),
                "{} is suggested for setup but is not a verification input",
                ecosystem.lockfile
            );
        }
    }

    /// Pid AND an in-process counter, pre-cleaned: two tests of this
    /// file run in parallel threads of one process, and a pid-only name
    /// makes them share a directory (P12).
    fn temp_dir(label: &str) -> std::path::PathBuf {
        crate::test_support::temp_dir(label)
    }

    #[test]
    fn locate_walks_up_to_the_policy_and_stops_at_a_repository() {
        let base = temp_dir("locate");
        // outer/: a repository with a policy; outer/crates/x: a subdirectory.
        let outer = base.join("outer");
        std::fs::create_dir_all(outer.join("crates/x")).expect("mkdir");
        std::fs::write(outer.join(".git"), "gitdir: elsewhere").expect("worktree .git file");
        std::fs::write(outer.join("relais.toml"), "schema_version = 1\n").expect("policy");
        assert_eq!(
            locate_repo_root(&outer),
            Ok(outer.clone()),
            "the root itself"
        );
        assert_eq!(
            locate_repo_root(&outer.join("crates/x")),
            Ok(outer.clone()),
            "a subdirectory resolves to the repository's policy"
        );
        // outer/inner: a nested repository without a policy must not
        // inherit outer's.
        let inner = outer.join("inner");
        std::fs::create_dir_all(inner.join(".git")).expect("nested repo");
        std::fs::create_dir_all(inner.join("src")).expect("mkdir");
        assert_eq!(
            locate_repo_root(&inner.join("src")),
            Err(LocateError::RepoWithoutPolicy(inner.clone())),
            "the nearest .git bounds the search and names where init belongs"
        );
        // loose/: no repository at all between here and base (base has
        // no .git either, and neither does the temp dir's lineage here).
        let loose = base.join("loose/deeper");
        std::fs::create_dir_all(&loose).expect("mkdir");
        match locate_repo_root(&loose) {
            Err(LocateError::NotInRepository(start)) => assert_eq!(start, loose),
            other => panic!("expected NotInRepository, got {other:?}"),
        }
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn init_never_overwrites() {
        let dir = temp_dir("init");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("relais.toml");
        assert!(write_init_template(&path).expect("first write"));
        std::fs::write(&path, "schema_version = 1").expect("user edit");
        assert!(!write_init_template(&path).expect("second write"));
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "schema_version = 1",
            "init must not clobber an existing policy"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The identity a trust grant is bound to: a canonical root, and an
    /// origin only when git actually reports one.
    #[test]
    fn identity_canonicalizes_the_root_and_tolerates_no_origin() {
        let dir = temp_dir("identity");
        std::fs::create_dir_all(dir.join("nested")).expect("mkdir");
        let canonical = std::fs::canonicalize(&dir).expect("canonicalize");
        let through_dots = dir.join("nested/..");
        assert_eq!(
            identity(&through_dots),
            identity(&canonical),
            "one repository, one identity"
        );
        // No repository here, so no origin: the root alone identifies it.
        assert_eq!(identity(&canonical).label(), canonical.to_string_lossy());
        std::fs::remove_dir_all(&dir).ok();
    }
}
