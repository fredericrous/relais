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
    /// Absolute links into the source workspace, moved to the same place in
    /// the destination (package managers link workspace packages this way).
    pub relocated: u64,
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
/// refusing any symlink that is not contained: a relative link must stay
/// inside the repository, an absolute one must point into `repo_root`
/// itself (it is then relocated to the destination's own copy). A link to
/// anywhere else would keep pointing outside the tree it was cloned into.
pub fn survey(repo_root: &Path, dir: &Path) -> Result<CloneStats, CloneError> {
    survey_links(repo_root, dir).map(|(stats, _)| stats)
}

/// [`survey`], also returning the absolute links (paths relative to `dir`)
/// that the clone must relocate, so the clone does not walk the tree again.
fn survey_links(repo_root: &Path, dir: &Path) -> Result<(CloneStats, Vec<PathBuf>), CloneError> {
    let mut stats = CloneStats::default();
    let mut absolute = Vec::new();
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
                let inside = if target.is_absolute() {
                    target.starts_with(repo_root)
                } else {
                    symlink_is_contained(link_dir, &target)
                };
                if !inside {
                    return Err(CloneError::UnsafeSymlink { link: path, target });
                }
                if target.is_absolute() {
                    absolute.push(path.strip_prefix(dir).unwrap_or(&path).to_path_buf());
                }
                stats.symlinks += 1;
            } else if kind.is_dir() {
                pending.push(path);
            } else {
                stats.files += 1;
            }
        }
    }
    Ok((stats, absolute))
}

/// Can the filesystem clone from the volume of `source_file` to the volume
/// of `dest_dir`? Clones one EXISTING regular file of the source into
/// `dest_dir` and removes it; nothing is written to the source.
pub fn probe(source_file: &Path, dest_dir: &Path) -> Result<(), CloneError> {
    let probe_path = dest_dir.join(format!(".relais-cow-probe-{}", std::process::id()));
    let result = crate::procs::clone_file(source_file, &probe_path);
    // The probe file is a one-byte scratch; the clone's own result is what
    // is reported, and a leftover with this process id is overwritten next time.
    let _ = fs::remove_file(&probe_path);
    result.map_err(|e| classify(e, source_file, dest_dir))
}

/// Clone the directory `src` (under `repo_root`) to `dst` (under
/// `dest_root`), which must not exist. Symlinks are surveyed first, and an
/// absolute link into `repo_root` is moved to the same place under
/// `dest_root`; on any failure `dst` is removed.
pub fn clone_tree(
    repo_root: &Path,
    dest_root: &Path,
    src: &Path,
    dst: &Path,
) -> Result<CloneStats, CloneError> {
    let (mut stats, absolute) = survey_links(repo_root, src)?;
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Err(e) = clone_directory(src, dst) {
        // Remove the partial tree; the clone's error is the one reported.
        let _ = fs::remove_dir_all(dst);
        return Err(classify(e, src, dst));
    }
    match relocate_links(repo_root, dest_root, dst, &absolute) {
        Ok(moved) => stats.relocated = moved,
        Err(e) => {
            // Same: the relocation error is the one reported.
            let _ = fs::remove_dir_all(dst);
            return Err(CloneError::Io(e));
        }
    }
    Ok(stats)
}

/// Point each of `links` (paths under `dir`, found by the survey) at the
/// same place under `to_root` instead of `from_root`. Only the link inodes
/// are rewritten.
fn relocate_links(
    from_root: &Path,
    to_root: &Path,
    dir: &Path,
    links: &[PathBuf],
) -> io::Result<u64> {
    let mut moved = 0;
    for link in links {
        let path = dir.join(link);
        let target = fs::read_link(&path)?;
        if let Ok(rest) = target.strip_prefix(from_root) {
            fs::remove_file(&path)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(to_root.join(rest), &path)?;
            #[cfg(not(unix))]
            return Err(io::Error::from(io::ErrorKind::Unsupported));
            moved += 1;
        }
    }
    Ok(moved)
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

    /// happier's `node_modules/react-native` is an ABSOLUTE link to
    /// `apps/ui/node_modules/react-native` in the same checkout. Refusing it
    /// would skip seeding on the repository this exists for; keeping it
    /// would leave the clone pointing back at the source. It is moved to
    /// the destination's own path, and a link to anywhere else is refused.
    #[cfg(unix)]
    #[test]
    fn an_absolute_link_into_the_workspace_is_relocated_into_the_destination() {
        let source = temp_dir("fsclone-reloc-src");
        let dest = temp_dir("fsclone-reloc-dst");
        let deps = source.join("node_modules");
        fs::create_dir_all(&deps).expect("mkdir");
        fs::create_dir_all(source.join("apps/ui/node_modules/rn")).expect("mkdir");
        std::os::unix::fs::symlink(source.join("apps/ui/node_modules/rn"), deps.join("rn"))
            .expect("link");
        let out = dest.join("node_modules");
        match clone_tree(&source, &dest, &deps, &out) {
            Ok(stats) => {
                assert_eq!(stats.relocated, 1);
                assert_eq!(
                    fs::read_link(out.join("rn")).expect("still a link"),
                    dest.join("apps/ui/node_modules/rn"),
                    "the clone points at the destination's own copy, not back at the source"
                );
            }
            Err(CloneError::Unavailable { .. }) => {}
            Err(other) => panic!("{other}"),
        }
        std::os::unix::fs::symlink("/usr/bin", deps.join("elsewhere")).expect("link");
        assert!(matches!(
            clone_tree(&source, &dest, &deps, &dest.join("again")),
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
        match clone_tree(&root, &root, &deps, &dst) {
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

    /// Free kilobytes on the volume holding `dir`, as `df -k` reports it.
    fn free_kb(dir: &Path) -> u64 {
        let out = std::process::Command::new("df")
            .args(["-k"])
            .arg(dir)
            .output()
            .expect("df");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .nth(1)
            .and_then(|line| line.split_whitespace().nth(3))
            .and_then(|kb| kb.parse().ok())
            .expect("df -k prints the available kilobytes")
    }

    /// The point of the module: cloning a large tree adds almost no disk.
    /// Free space is volume-wide, so a parallel build makes the figure
    /// noisy; run it alone on a copy-on-write volume (APFS, btrfs, XFS
    /// reflink): `cargo test -- --ignored a_large_clone_adds`.
    #[test]
    #[ignore = "measures volume-wide free space; run alone on a copy-on-write volume"]
    fn a_large_clone_adds_almost_no_disk() {
        let root = temp_dir("fsclone-size");
        let deps = root.join("node_modules");
        fs::create_dir_all(&deps).expect("mkdir");
        let block = vec![7u8; 4 * 1024 * 1024];
        for n in 0..32 {
            fs::write(deps.join(format!("f{n}.bin")), &block).expect("file"); // 128 MiB
        }
        let before = free_kb(&root);
        let dst = root.join("seed/node_modules");
        match clone_tree(&root, &root, &deps, &dst) {
            Ok(stats) => assert_eq!(stats.files, 32),
            Err(CloneError::Unavailable { .. }) => return,
            Err(other) => panic!("{other}"),
        }
        let added_kb = before.saturating_sub(free_kb(&root));
        assert!(
            added_kb < 8 * 1024 + 128 * 1024 / 50,
            "a 128 MiB clone added {added_kb} KiB; the bound is 8 MiB plus 2 percent"
        );
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
