//! Copy-on-write cloning of dependency directories (ADR-0008).
//!
//! A fork of the caller's workspace needs the caller's `node_modules` or
//! `target`, and a byte copy of 6 GB is exactly what this exists to avoid.
//! So the only way a tree is duplicated here is a filesystem clone:
//! `clonefile(2)` on macOS, `FICLONE` per regular file on Linux. When the
//! filesystem cannot clone, the answer is [`CloneError::Unavailable`] and
//! the caller falls back to today's behaviour; this module NEVER falls back
//! to copying bytes, and a clone that fails halfway removes what it made.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

/// What a successful clone made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CloneStats {
    pub files: u64,
    pub dirs: u64,
    pub symlinks: u64,
}

#[derive(Debug)]
pub enum CloneError {
    /// The filesystem (or the pair of volumes) cannot clone. `reason`
    /// names the errno, `from`/`to` the paths that were tried.
    Unavailable {
        reason: String,
        from: PathBuf,
        to: PathBuf,
    },
    /// A symlink in the tree would point out of the repository or into
    /// the source workspace; the tree is not safe to move.
    UnsafeSymlink {
        link: PathBuf,
        target: PathBuf,
    },
    Io(io::Error),
}

impl std::fmt::Display for CloneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable { reason, from, to } => write!(
                f,
                "copy-on-write is unavailable between {} and {}: {reason}",
                from.display(),
                to.display()
            ),
            Self::UnsafeSymlink { link, target } => write!(
                f,
                "symlink {} -> {} leaves the repository, so the tree is not seeded",
                link.display(),
                target.display()
            ),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for CloneError {}

impl From<io::Error> for CloneError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Lexical containment of a symlink: does `target`, read from the
/// directory `link_dir` (both relative to the repository root), stay
/// inside the repository? An absolute target never does. A relative one
/// such as yarn's `../../packages/x` does, and then points at the
/// DESTINATION's own copy once the tree is cloned there.
pub fn symlink_is_contained(link_dir: &Path, target: &Path) -> bool {
    if target.is_absolute() {
        return false;
    }
    let mut depth: i64 = 0;
    let steps = link_dir.components().chain(target.components());
    for component in steps {
        match component {
            Component::Normal(_) => depth += 1,
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

/// Walk `dir` (a path under `repo_root`), counting what is there and
/// refusing any symlink that is not contained.
pub fn survey(repo_root: &Path, dir: &Path) -> Result<CloneStats, CloneError> {
    let mut stats = CloneStats::default();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        stats.dirs += 1;
        for entry in fs::read_dir(&current)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_symlink() {
                let target = fs::read_link(&path)?;
                let link_dir = current.strip_prefix(repo_root).unwrap_or(&current);
                if !symlink_is_contained(link_dir, &target) {
                    return Err(CloneError::UnsafeSymlink { link: path, target });
                }
                stats.symlinks += 1;
            } else if kind.is_dir() {
                pending.push(path);
            } else {
                stats.files += 1;
            }
        }
    }
    Ok(stats)
}

/// Can the filesystem clone from the volume of `source_file` to the volume
/// of `dest_dir`? Clones one EXISTING regular file of the source into
/// `dest_dir` and removes it; nothing is written to the source.
pub fn probe(source_file: &Path, dest_dir: &Path) -> Result<(), CloneError> {
    let probe_path = dest_dir.join(format!(".relais-cow-probe-{}", std::process::id()));
    let result = crate::procs::clone_file(source_file, &probe_path);
    let _ = fs::remove_file(&probe_path);
    result.map_err(|e| classify(e, source_file, dest_dir))
}

/// Clone the directory `src` (under `repo_root`) to `dst`, which must not
/// exist. Symlinks are surveyed first; on any failure `dst` is removed.
pub fn clone_tree(repo_root: &Path, src: &Path, dst: &Path) -> Result<CloneStats, CloneError> {
    let stats = survey(repo_root, src)?;
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Err(e) = clone_directory(src, dst) {
        let _ = fs::remove_dir_all(dst);
        return Err(classify(e, src, dst));
    }
    Ok(stats)
}

fn classify(e: io::Error, from: &Path, to: &Path) -> CloneError {
    if crate::procs::clone_unsupported(&e) {
        return CloneError::Unavailable {
            reason: e.to_string(),
            from: from.to_path_buf(),
            to: to.to_path_buf(),
        };
    }
    CloneError::Io(e)
}

/// Clone a directory: in one call where the platform has it, else file by
/// file, symlinks recreated verbatim and modes kept.
fn clone_directory(src: &Path, dst: &Path) -> io::Result<()> {
    if crate::procs::CLONES_DIRECTORIES {
        return crate::procs::clone_directory(src, dst);
    }
    fs::create_dir(dst)?;
    fs::set_permissions(dst, fs::metadata(src)?.permissions())?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if kind.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(fs::read_link(entry.path())?, &to)?;
            #[cfg(not(unix))]
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        } else if kind.is_dir() {
            clone_directory(&entry.path(), &to)?;
        } else {
            crate::procs::clone_file(&entry.path(), &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_dir;

    #[test]
    fn a_relative_link_inside_the_repository_is_contained() {
        assert!(symlink_is_contained(
            Path::new("node_modules/@scope"),
            Path::new("../../packages/x")
        ));
        assert!(symlink_is_contained(
            Path::new("node_modules/.bin"),
            Path::new("../foo/bin/x")
        ));
    }

    #[test]
    fn an_absolute_link_or_one_leaving_the_repository_is_not() {
        assert!(!symlink_is_contained(
            Path::new("node_modules"),
            Path::new("/etc/passwd")
        ));
        assert!(!symlink_is_contained(
            Path::new("node_modules"),
            Path::new("../../outside")
        ));
        assert!(!symlink_is_contained(Path::new(""), Path::new("../x")));
    }

    #[cfg(unix)]
    #[test]
    fn a_survey_counts_and_refuses_an_escaping_symlink() {
        let root = temp_dir("fsclone-survey");
        let deps = root.join("node_modules");
        fs::create_dir_all(deps.join("a")).expect("mkdir");
        fs::write(deps.join("a/index.js"), "x").expect("file");
        std::os::unix::fs::symlink("../a/index.js", deps.join("a/link")).expect("link");
        let stats = survey(&root, &deps).expect("contained");
        assert_eq!((stats.files, stats.symlinks), (1, 1));
        std::os::unix::fs::symlink("/etc", deps.join("abs")).expect("link");
        assert!(matches!(
            survey(&root, &deps),
            Err(CloneError::UnsafeSymlink { .. })
        ));
    }

    /// On a filesystem that clones (APFS, btrfs, XFS reflink) the tree
    /// appears with its content; on one that does not, the answer is
    /// `Unavailable` and NOTHING was copied. Either is a correct outcome;
    /// a byte copy never is.
    #[test]
    fn a_clone_either_lands_whole_or_reports_unavailable_and_leaves_nothing() {
        let root = temp_dir("fsclone-tree");
        let deps = root.join("node_modules");
        fs::create_dir_all(deps.join("pkg")).expect("mkdir");
        fs::write(deps.join("pkg/index.js"), "module.exports = 1;").expect("file");
        let dst = root.join("seeded/node_modules");
        match clone_tree(&root, &deps, &dst) {
            Ok(stats) => {
                assert_eq!(stats.files, 1);
                assert_eq!(
                    fs::read_to_string(dst.join("pkg/index.js")).expect("cloned"),
                    "module.exports = 1;"
                );
            }
            Err(CloneError::Unavailable { .. }) => {
                assert!(!dst.exists(), "an unavailable clone leaves no partial tree");
            }
            Err(other) => panic!("unexpected: {other}"),
        }
    }

    #[test]
    fn a_probe_leaves_neither_a_file_in_the_source_nor_one_in_the_destination() {
        let root = temp_dir("fsclone-probe");
        let src = root.join("src");
        let dest = root.join("dest");
        fs::create_dir_all(&src).expect("mkdir");
        fs::create_dir_all(&dest).expect("mkdir");
        fs::write(src.join("Cargo.toml"), "x").expect("file");
        let _ = probe(&src.join("Cargo.toml"), &dest);
        assert_eq!(fs::read_dir(&src).expect("ls").count(), 1);
        assert_eq!(fs::read_dir(&dest).expect("ls").count(), 0);
    }
}
