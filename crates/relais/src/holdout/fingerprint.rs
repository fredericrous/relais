//! The fingerprint of a store holdout (SPEC §4): its complete effective
//! declaration plus the contents the declaration supplies through `path`
//! and `config`.
//!
//! Nothing here follows a symlink: every component is looked at with
//! `symlink_metadata`, and a link is refused before anything it points at
//! is opened.

use std::path::Path;

use super::clean_relative;
use crate::ids::sha256_hex;
use crate::policy::HoldoutDecl;

/// The formula a fingerprint was computed with; it prefixes the fingerprint.
pub const FINGERPRINT_VERSION: u32 = 1;

/// Supplied contents the store may not give: a symlink, a path outside the
/// store's tree, or something unreadable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuppliedRefusal {
    /// The path inside the store.
    pub path: String,
    pub cause: String,
}

impl std::fmt::Display for SuppliedRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}` {}", self.path, self.cause)
    }
}

/// One supplied file: its path under the supplied root, its mode and the
/// hash of its bytes.
type Entry = (String, u32, String);

/// The versioned fingerprint of `decl` and of the contents it supplies
/// from the store checked out at `tree`.
pub fn fingerprint(decl: &HoldoutDecl, tree: &Path) -> Result<String, SuppliedRefusal> {
    let mut supplied = Vec::new();
    let roles = [
        ("path", Some(decl.path.as_str())),
        ("config", decl.config.as_deref()),
    ];
    for (role, relative) in roles {
        if let Some(relative) = relative {
            supplied.push(serde_json::json!({
                "role": role,
                "path": relative,
                "files": supplied_files(tree, relative)?,
            }));
        }
    }
    let canonical = serde_json::json!({
        "fingerprint_version": FINGERPRINT_VERSION,
        "decl": decl,
        "supplied": supplied,
    });
    Ok(format!(
        "v{FINGERPRINT_VERSION}:{}",
        sha256_hex(canonical.to_string().as_bytes())
    ))
}

/// Refuse a symlink at any component of `relative` under `root`.
pub fn reject_symlinks_along(root: &Path, relative: &str) -> Result<(), SuppliedRefusal> {
    let mut current = root.to_path_buf();
    let mut seen = String::new();
    for component in relative.split('/') {
        current.push(component);
        if !seen.is_empty() {
            seen.push('/');
        }
        seen.push_str(component);
        let meta = std::fs::symlink_metadata(&current).map_err(|e| SuppliedRefusal {
            path: seen.clone(),
            cause: format!("could not be read: {e}"),
        })?;
        if meta.file_type().is_symlink() {
            return Err(symlink(&seen));
        }
    }
    Ok(())
}

fn symlink(path: &str) -> SuppliedRefusal {
    SuppliedRefusal {
        path: path.to_string(),
        cause: "is a symlink".to_string(),
    }
}

/// The files `relative` stands for, sorted by path: one entry for a file,
/// every file below it for a directory.
fn supplied_files(tree: &Path, relative: &str) -> Result<Vec<Entry>, SuppliedRefusal> {
    if !clean_relative(relative) {
        return Err(SuppliedRefusal {
            path: relative.to_string(),
            cause: "is not a clean relative path".to_string(),
        });
    }
    reject_symlinks_along(tree, relative)?;
    let top = tree.join(relative);
    let meta = std::fs::symlink_metadata(&top).map_err(|e| unreadable(relative, &e))?;
    let mut files = Vec::new();
    if meta.is_dir() {
        collect(&top, "", relative, &mut files)?;
    } else if meta.is_file() {
        files.push((String::new(), mode_of(&meta), hash_of(&top, relative)?));
    } else {
        return Err(SuppliedRefusal {
            path: relative.to_string(),
            cause: "is neither a regular file nor a directory".to_string(),
        });
    }
    files.sort();
    Ok(files)
}

fn collect(
    dir: &Path,
    inside: &str,
    shown: &str,
    files: &mut Vec<Entry>,
) -> Result<(), SuppliedRefusal> {
    let entries = std::fs::read_dir(dir).map_err(|e| unreadable(shown, &e))?;
    for entry in entries {
        let entry = entry.map_err(|e| unreadable(shown, &e))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == ".git" {
            continue;
        }
        let here = if inside.is_empty() {
            name.clone()
        } else {
            format!("{inside}/{name}")
        };
        let shown_here = format!("{shown}/{name}");
        let meta =
            std::fs::symlink_metadata(entry.path()).map_err(|e| unreadable(&shown_here, &e))?;
        let kind = meta.file_type();
        if kind.is_symlink() {
            return Err(symlink(&shown_here));
        } else if kind.is_dir() {
            collect(&entry.path(), &here, &shown_here, files)?;
        } else if kind.is_file() {
            files.push((here, mode_of(&meta), hash_of(&entry.path(), &shown_here)?));
        } else {
            return Err(SuppliedRefusal {
                path: shown_here,
                cause: "is neither a regular file nor a directory".to_string(),
            });
        }
    }
    Ok(())
}

fn hash_of(path: &Path, shown: &str) -> Result<String, SuppliedRefusal> {
    std::fs::read(path)
        .map(|bytes| sha256_hex(&bytes))
        .map_err(|e| unreadable(shown, &e))
}

fn unreadable(path: &str, error: &std::io::Error) -> SuppliedRefusal {
    SuppliedRefusal {
        path: path.to_string(),
        cause: format!("could not be read: {error}"),
    }
}

#[cfg(unix)]
fn mode_of(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn mode_of(_meta: &std::fs::Metadata) -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(path: &str) -> HoldoutDecl {
        serde_json::from_value(serde_json::json!({
            "path": path,
            "overlay": ".relais-holdout/x.sh",
            "command": { "argv": ["sh", "x"], "junit": "j.xml" },
            "config": "r/cfg.toml",
            "inputs": [],
            "attach": null,
        }))
        .expect("a declaration")
    }

    fn tree(files: &[(&str, &str)]) -> crate::test_support::TempDir {
        let dir = crate::test_support::temp_dir("fingerprint");
        for (path, content) in files {
            let file = dir.join(path);
            std::fs::create_dir_all(file.parent().expect("parent")).expect("dirs");
            std::fs::write(file, content).expect("write");
        }
        dir
    }

    fn base_files() -> Vec<(&'static str, &'static str)> {
        vec![("r/check.sh", "true\n"), ("r/cfg.toml", "a = 1\n")]
    }

    #[test]
    fn it_changes_with_any_declaration_field() {
        let dir = tree(&base_files());
        let base = decl("r/check.sh");
        let before = fingerprint(&base, &dir).expect("fingerprint");
        assert!(before.starts_with("v1:"), "{before}");
        assert_eq!(before, fingerprint(&base, &dir).expect("again"));

        let mut variants = Vec::new();
        let mut timeout = base.clone();
        timeout.command.timeout_seconds += 1;
        variants.push(timeout);
        let mut env = base.clone();
        env.command.env.insert("A".into(), "b".into());
        variants.push(env);
        let mut inputs = base.clone();
        inputs.inputs.push("Cargo.toml".into());
        variants.push(inputs);
        let mut attach = base.clone();
        attach.attach = Some(
            crate::policy::HoldoutAttach::parse(
                "attach",
                vec!["feature".into()],
                vec!["src/**".into()],
            )
            .expect("attach"),
        );
        variants.push(attach);
        for changed in variants {
            assert_ne!(
                before,
                fingerprint(&changed, &dir).expect("changed"),
                "{changed:?}"
            );
        }
        let mut with_input = base.clone();
        with_input.inputs.push("Cargo.toml".into());
        let with = fingerprint(&with_input, &dir).expect("with");
        with_input.inputs.clear();
        assert_ne!(with, fingerprint(&with_input, &dir).expect("without"));
    }

    #[test]
    fn it_changes_with_the_script_or_the_config_bytes() {
        let base = decl("r/check.sh");
        let dir = tree(&base_files());
        let before = fingerprint(&base, &dir).expect("fingerprint");
        std::fs::write(dir.join("r/check.sh"), "false\n").expect("write");
        let script = fingerprint(&base, &dir).expect("script changed");
        assert_ne!(before, script);
        std::fs::write(dir.join("r/cfg.toml"), "a = 2\n").expect("write");
        assert_ne!(script, fingerprint(&base, &dir).expect("config changed"));
    }

    #[test]
    fn a_directory_overlay_hashes_nested_bytes_and_modes_in_path_order() {
        let mut files = base_files();
        files.extend([("r/dir/b.sh", "b\n"), ("r/dir/sub/a.sh", "a\n")]);
        let dir = tree(&files);
        let overlay = decl("r/dir");
        let before = fingerprint(&overlay, &dir).expect("fingerprint");

        let other = tree(&files.iter().rev().copied().collect::<Vec<_>>());
        assert_eq!(before, fingerprint(&overlay, &other).expect("reordered"));

        std::fs::write(dir.join("r/dir/sub/a.sh"), "changed\n").expect("write");
        let bytes = fingerprint(&overlay, &dir).expect("bytes");
        assert_ne!(before, bytes);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file = dir.join("r/dir/b.sh");
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            assert_ne!(bytes, fingerprint(&overlay, &dir).expect("mode"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_anywhere_in_the_supplied_contents_is_refused_unread() {
        use std::os::unix::fs::symlink;
        let mut files = base_files();
        files.push(("r/dir/a.sh", "a\n"));
        let dir = tree(&files);
        let missing = dir.join("does-not-exist");
        symlink(&missing, dir.join("r/dir/link")).expect("link");
        let refused = fingerprint(&decl("r/dir"), &dir).expect_err("nested link");
        assert_eq!(refused.path, "r/dir/link");
        assert_eq!(refused.cause, "is a symlink");

        symlink(dir.join("r"), dir.join("alias")).expect("component link");
        let refused = fingerprint(&decl("alias/check.sh"), &dir).expect_err("component");
        assert_eq!(refused.path, "alias");

        symlink(dir.join("loop"), dir.join("loop")).expect("cycle");
        let refused = fingerprint(&decl("loop"), &dir).expect_err("cycle");
        assert_eq!(refused.path, "loop");
    }
}
