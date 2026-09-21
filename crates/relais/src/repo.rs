//! The repository a run belongs to, on disk.
//!
//! Finding the `relais.toml` that governs a directory, and writing the
//! starter one `relais init` produces. Both are filesystem operations on
//! a policy document, which is why they are here and not in `policy`:
//! that module decides authority from parsed text and touches nothing.

use crate::policy::INIT_TEMPLATE;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locate_walks_up_to_the_policy_and_stops_at_a_repository() {
        let base = std::env::temp_dir().join(format!("relais-locate-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
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
        let dir = std::env::temp_dir().join(format!("relais-init-{}", std::process::id()));
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
}
