//! The credential floor: what a sandboxed worker may never read.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::backend::WORKER_ENV_PREFIXES;
use crate::paths::CLAUDE_CONFIG_DIR_ENV;
use crate::policy::{
    FLOOR_CARGO_DIR, FLOOR_CLAUDE_DIR, FLOOR_CONFIG_DIRS, FLOOR_DOCKER_DIR, FLOOR_HOME_DIRS,
    FLOOR_HOME_FILES, FLOOR_RELAIS_CONFIG, FLOOR_RELAIS_LEDGER,
};

/// Everything the floor is computed from. `env` is relais's OWN
/// environment (where its credentials were relocated to), not the
/// worker's; `launch_env_names` is what the worker's launch environment
/// carries.
pub struct FloorInputs<'a> {
    pub home: &'a Path,
    pub config_dir: &'a Path,
    pub ledger_path: &'a Path,
    pub env: &'a dyn Fn(&str) -> Option<String>,
    pub launch_env_names: &'a [String],
    pub extra_deny: &'a [PathBuf],
}

/// The floor: directory trees, single files, path globs and environment
/// names. Each list is sorted and free of duplicates.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Floor {
    pub dirs: Vec<PathBuf>,
    pub files: Vec<PathBuf>,
    pub globs: Vec<String>,
    pub env_names: Vec<String>,
}

/// The credential floor for one machine. A relocation variable ADDS its
/// path: the default stays in the floor, because a worker that can read
/// the default has been given a credential the operator did not move. That
/// holds for relais's own config dir and ledger too.
pub fn credential_floor(inputs: &FloorInputs) -> Floor {
    let home = inputs.home;
    let raw = |name: &str| (inputs.env)(name).filter(|value| !value.is_empty());
    let absolute = |name: &str| raw(name).map(PathBuf::from).filter(|p| p.is_absolute());
    // The default, then where the environment moved it to.
    let roots = |default: PathBuf, var: &str| {
        let mut roots = vec![default];
        roots.extend(absolute(var));
        roots
    };

    let mut dirs: Vec<PathBuf> = FLOOR_HOME_DIRS.iter().map(|d| home.join(d)).collect();
    for root in roots(home.join(".config"), "XDG_CONFIG_HOME") {
        dirs.extend(FLOOR_CONFIG_DIRS.iter().map(|d| root.join(d)));
    }
    dirs.push(home.join(FLOOR_RELAIS_CONFIG));
    dirs.push(inputs.config_dir.to_path_buf());
    for var in ["GNUPGHOME", "GH_CONFIG_DIR", "CLOUDSDK_CONFIG"] {
        dirs.extend(absolute(var));
    }

    let mut files: Vec<PathBuf> = FLOOR_HOME_FILES.iter().map(|f| home.join(f)).collect();
    for ((default_dir, names), var) in [
        (FLOOR_CARGO_DIR, "CARGO_HOME"),
        (FLOOR_DOCKER_DIR, "DOCKER_CONFIG"),
        (FLOOR_CLAUDE_DIR, CLAUDE_CONFIG_DIR_ENV),
    ] {
        for root in roots(home.join(default_dir), var) {
            files.extend(names.iter().map(|name| root.join(name)));
        }
    }
    if let Some(list) = raw("KUBECONFIG") {
        files.extend(
            list.split(':')
                .map(PathBuf::from)
                .filter(|p| p.is_absolute()),
        );
    }
    for var in [
        "AWS_SHARED_CREDENTIALS_FILE",
        "AWS_CONFIG_FILE",
        "GOOGLE_APPLICATION_CREDENTIALS",
    ] {
        files.extend(absolute(var));
    }

    // An operator's extra path is a tree when it is one, or when it does
    // not exist yet (the wider reading); only an existing non-directory
    // is a single file.
    for extra in inputs.extra_deny {
        if extra.exists() && !extra.is_dir() {
            files.push(extra.clone());
        } else {
            dirs.push(extra.clone());
        }
    }

    // `ledger.sqlite*` covers the -wal and -shm siblings.
    let ledger_glob = |path: &Path| format!("{}*", path.display());
    let ledgers = [
        home.join(FLOOR_RELAIS_LEDGER),
        inputs.ledger_path.to_path_buf(),
    ];
    let mut globs: Vec<String> = ledgers.iter().map(|path| ledger_glob(path)).collect();
    globs.extend(
        ledgers
            .iter()
            .filter_map(|path| alias(path))
            .map(|path| ledger_glob(&path)),
    );

    let env_names = inputs
        .launch_env_names
        .iter()
        .filter(|name| WORKER_ENV_PREFIXES.iter().any(|p| name.starts_with(p)))
        .cloned();

    Floor {
        dirs: with_aliases(dirs),
        files: with_aliases(files),
        globs: sorted(globs),
        env_names: sorted(env_names),
    }
}

fn sorted<T: Ord>(items: impl IntoIterator<Item = T>) -> Vec<T> {
    items
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Each path, and its symlink-resolved form when that differs.
fn with_aliases(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let aliases: Vec<PathBuf> = paths.iter().filter_map(|p| alias(p)).collect();
    sorted(paths.into_iter().chain(aliases))
}

/// `path` with its longest existing prefix canonicalised, when that
/// differs from `path`. The prefix, not the whole path: a credential file
/// that does not exist yet still sits behind a symlinked home.
fn alias(path: &Path) -> Option<PathBuf> {
    let (existing, canonical) = path
        .ancestors()
        .find_map(|dir| std::fs::canonicalize(dir).ok().map(|c| (dir, c)))?;
    let rest = path.strip_prefix(existing).ok()?;
    let resolved = canonical.join(rest);
    (resolved != path).then_some(resolved)
}

// Unix only: the fixtures are Unix absolute paths (`/h/.ssh`), which are
// not absolute on Windows, and the OS sandbox these paths feed exists on
// macOS and Linux only.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_support::temp_dir;

    fn floor_with(home: &Path, env: &[(&str, &str)], launch: &[&str], extra: &[PathBuf]) -> Floor {
        let lookup = |name: &str| {
            env.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        };
        let launch: Vec<String> = launch.iter().map(|s| s.to_string()).collect();
        credential_floor(&FloorInputs {
            home,
            config_dir: &home.join(".config/relais"),
            ledger_path: &home.join(".local/state/relais/ledger.sqlite"),
            env: &lookup,
            launch_env_names: &launch,
            extra_deny: extra,
        })
    }

    fn has_dir(floor: &Floor, path: &str) -> bool {
        floor.dirs.contains(&PathBuf::from(path))
    }

    fn has_file(floor: &Floor, path: &str) -> bool {
        floor.files.contains(&PathBuf::from(path))
    }

    #[test]
    fn with_no_overrides_the_floor_lists_each_default() {
        let floor = floor_with(Path::new("/nonexistent/h"), &[], &[], &[]);
        for dir in [
            ".ssh",
            ".gnupg",
            ".aws",
            ".config/gh",
            ".config/gcloud",
            ".config/mtls",
            ".kube",
            "Library/Keychains",
            ".config/relais",
        ] {
            assert!(has_dir(&floor, &format!("/nonexistent/h/{dir}")), "{dir}");
        }
        for file in [
            ".netrc",
            ".npmrc",
            ".pypirc",
            ".docker/config.json",
            ".cargo/credentials",
            ".cargo/credentials.toml",
            ".claude/.credentials.json",
            ".git-credentials",
            ".claude.json",
        ] {
            assert!(
                has_file(&floor, &format!("/nonexistent/h/{file}")),
                "{file}"
            );
        }
        assert_eq!(
            floor.globs,
            vec!["/nonexistent/h/.local/state/relais/ledger.sqlite*".to_string()]
        );
        assert!(floor.env_names.is_empty());
    }

    #[test]
    fn relocated_config_and_ledger_keep_the_defaults() {
        let home = Path::new("/nonexistent/h");
        let floor = credential_floor(&FloorInputs {
            home,
            config_dir: Path::new("/r/config"),
            ledger_path: Path::new("/r/state/ledger.sqlite"),
            env: &|_| None,
            launch_env_names: &[],
            extra_deny: &[],
        });
        assert!(has_dir(&floor, "/r/config"));
        assert!(has_dir(&floor, "/nonexistent/h/.config/relais"));
        assert!(floor.globs.contains(&"/r/state/ledger.sqlite*".to_string()));
        assert!(floor
            .globs
            .contains(&"/nonexistent/h/.local/state/relais/ledger.sqlite*".to_string()));
    }

    #[test]
    fn the_default_paths_are_all_in_the_floor() {
        let home = Path::new("/nonexistent/h");
        let floor = floor_with(home, &[], &[], &[]);
        for path in crate::policy::default_floor_paths(home) {
            let listed = floor.dirs.contains(&path)
                || floor.files.contains(&path)
                || floor.globs.contains(&format!("{}*", path.display()));
            assert!(listed, "{}", path.display());
        }
    }

    #[test]
    fn each_relocation_adds_its_path_and_keeps_the_default() {
        let home = "/nonexistent/h";
        let dir_cases = [
            ("GNUPGHOME", "/r/gnupg", "/r/gnupg"),
            ("GH_CONFIG_DIR", "/r/gh", "/r/gh"),
            ("CLOUDSDK_CONFIG", "/r/gcloud", "/r/gcloud"),
            ("XDG_CONFIG_HOME", "/r/xdg", "/r/xdg/gh"),
            ("XDG_CONFIG_HOME", "/r/xdg", "/r/xdg/gcloud"),
            ("XDG_CONFIG_HOME", "/r/xdg", "/r/xdg/mtls"),
        ];
        for (var, value, expected) in dir_cases {
            let floor = floor_with(Path::new(home), &[(var, value)], &[], &[]);
            assert!(has_dir(&floor, expected), "{var} adds {expected}");
            assert!(
                has_dir(&floor, "/nonexistent/h/.ssh"),
                "{var} keeps defaults"
            );
            assert!(
                has_dir(&floor, "/nonexistent/h/.config/gh"),
                "{var} keeps gh"
            );
        }
        let file_cases = [
            ("CARGO_HOME", "/r/cargo", "/r/cargo/credentials"),
            ("CARGO_HOME", "/r/cargo", "/r/cargo/credentials.toml"),
            ("DOCKER_CONFIG", "/r/docker", "/r/docker/config.json"),
            (
                "CLAUDE_CONFIG_DIR",
                "/r/claude",
                "/r/claude/.credentials.json",
            ),
            ("KUBECONFIG", "/r/k1:/r/k2", "/r/k1"),
            ("KUBECONFIG", "/r/k1:/r/k2", "/r/k2"),
            ("AWS_SHARED_CREDENTIALS_FILE", "/r/aws-c", "/r/aws-c"),
            ("AWS_CONFIG_FILE", "/r/aws-f", "/r/aws-f"),
            ("GOOGLE_APPLICATION_CREDENTIALS", "/r/g.json", "/r/g.json"),
        ];
        for (var, value, expected) in file_cases {
            let floor = floor_with(Path::new(home), &[(var, value)], &[], &[]);
            assert!(has_file(&floor, expected), "{var} adds {expected}");
            assert!(
                has_file(&floor, "/nonexistent/h/.netrc"),
                "{var} keeps defaults"
            );
            assert!(
                has_file(&floor, "/nonexistent/h/.cargo/credentials"),
                "{var} keeps the cargo default"
            );
        }
    }

    #[test]
    fn a_relative_or_empty_relocation_is_no_path() {
        let floor = floor_with(
            Path::new("/nonexistent/h"),
            &[
                ("GNUPGHOME", "rel"),
                ("CARGO_HOME", ""),
                ("KUBECONFIG", "a:b"),
            ],
            &[],
            &[],
        );
        assert!(!floor.dirs.iter().any(|d| d.is_relative()));
        assert!(!floor.files.iter().any(|f| f.is_relative()));
    }

    #[test]
    fn extra_deny_paths_join_the_floor() {
        let scratch = temp_dir("floor-extra");
        let file = scratch.join("secret.txt");
        std::fs::write(&file, "x").expect("write");
        let floor = floor_with(
            Path::new("/nonexistent/h"),
            &[],
            &[],
            &[scratch.to_path_buf(), file.clone()],
        );
        assert!(floor.dirs.contains(&scratch.to_path_buf()));
        assert!(floor.files.contains(&file));
    }

    #[test]
    fn a_symlink_alias_appears_in_both_forms() {
        let scratch = temp_dir("floor-alias");
        let real = scratch.join("real");
        std::fs::create_dir_all(real.join(".ssh")).expect("mkdir");
        let link = scratch.join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        let canonical = std::fs::canonicalize(&real).expect("canonical");

        let floor = floor_with(&link, &[], &[], &[]);
        assert!(floor.dirs.contains(&link.join(".ssh")), "the spelled form");
        assert!(
            floor.dirs.contains(&canonical.join(".ssh")),
            "the resolved form"
        );
        assert!(
            floor.files.contains(&canonical.join(".netrc")),
            "a file that does not exist yet still resolves through the link"
        );
        assert!(floor.globs.contains(&format!(
            "{}*",
            link.join(".local/state/relais/ledger.sqlite").display()
        )));
        assert!(floor.globs.contains(&format!(
            "{}*",
            canonical
                .join(".local/state/relais/ledger.sqlite")
                .display()
        )));
    }

    #[test]
    fn env_names_keep_only_credential_families() {
        let floor = floor_with(
            Path::new("/nonexistent/h"),
            &[],
            &[
                "PATH",
                "HOME",
                "ANTHROPIC_API_KEY",
                "AWS_PROFILE",
                "LANG",
                "AWS_PROFILE",
            ],
            &[],
        );
        assert_eq!(floor.env_names, vec!["ANTHROPIC_API_KEY", "AWS_PROFILE"]);
    }
}
