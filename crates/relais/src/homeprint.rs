//! A fingerprint of the sensitive files in the home directory (SPEC §8).
//!
//! relais keeps no OS sandbox, and its scope check sees only the worktree
//! diff, so a worker or a baseline test run that rewrites `~/.gitconfig`
//! went unseen. A snapshot before and after a run turns that into a
//! warning. Nothing here decides anything: the result is a list of
//! changed paths, and no caller changes a run's outcome because of it.
//!
//! [`diff`] and [`report`] are calculations over snapshots and drifts.
//! [`snapshot`] is an action: it reads the filesystem (through a seam for
//! file contents), as does [`repo_config`], which asks git where a
//! repository's shared config lives.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::ids::sha256_hex;

/// A directory is expanded to at most this many files; past it the
/// directory is recorded as truncated, not walked on.
pub const MAX_FILES_PER_DIRECTORY: usize = 500;

/// A regular file over this many bytes is never read: it is recorded by
/// its size, modification time and mode instead (greenbelay#306 chose the
/// same), so a large file in `extra_paths` costs one `stat`, not a read.
pub const HASH_LIMIT: u64 = 1024 * 1024;

/// How long git may take to name the shared config directory.
const GIT_WAIT: Duration = Duration::from_secs(10);

/// The last lines of a warning that lists changed files, each short enough
/// for an 80-column terminal.
pub const FOLLOW_UP: [&str; 2] = [
    "check these files; if you did not change them, the run or its tests did:",
    "  rerun them with HOME set to a temporary directory",
];

/// The heading `status` and `explain` print above the changed files.
pub const STATUS_HEADING: &str = "home files changed while the run was active:";

/// Where the repository's `.git/config` is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoConfig {
    /// The shared config file, as git named it.
    At(PathBuf),
    /// Git could not say (no git, not a repository); the guessed path is
    /// recorded as unreadable.
    Unresolved(PathBuf),
}

/// A variable lookup, by name.
pub type EnvLookup = Box<dyn Fn(&str) -> Option<OsString> + Send + Sync>;

/// The home directory and the variable lookup the run engine watches,
/// built once at the CLI root and carried on `RunConfig`.
pub struct HomeWatch {
    /// `None` when no home directory could be resolved.
    pub home: Option<PathBuf>,
    pub env: EnvLookup,
}

impl HomeWatch {
    /// The process's own home and environment.
    pub fn from_process() -> Self {
        Self {
            home: crate::paths::resolve_home(|name| std::env::var_os(name)).ok(),
            env: Box::new(|name| std::env::var_os(name)),
        }
    }

    /// A fixed home: `HOME` answers with it and every other variable is
    /// unset, so a test reads nothing from the process.
    pub fn fixed(home: PathBuf) -> Self {
        let answer = home.clone().into_os_string();
        Self {
            home: Some(home),
            env: Box::new(move |name| (name == "HOME").then(|| answer.clone())),
        }
    }
}

/// Everything that decides which files are watched. Taken as values, so a
/// test never sets a process-wide variable.
pub struct Watch<'a> {
    pub home: &'a Path,
    pub var: &'a dyn Fn(&str) -> Option<OsString>,
    pub repo_config: RepoConfig,
    pub extra_paths: &'a [PathBuf],
}

/// What one watched path held.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    Absent,
    File {
        sha256: String,
        size: u64,
        mode: Option<u32>,
    },
    /// A regular file over [`HASH_LIMIT`], never read. One whose
    /// modification time cannot be read is recorded as unreadable.
    Large {
        modified_ns: u128,
        size: u64,
        mode: Option<u32>,
    },
    Link {
        target: PathBuf,
    },
    Unreadable,
}

/// The watched files at one moment: hashes only, never contents.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    entries: BTreeMap<PathBuf, Entry>,
    truncated: BTreeSet<PathBuf>,
}

/// How a watched path differs between two snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Change {
    Added,
    Removed,
    Modified,
    /// A read failed on one side only: the file was not checked.
    Unreadable,
    /// A directory passed the file cap on one side only: part of it was
    /// not checked.
    Truncated,
    /// A read failed on both sides: nothing was compared, so nothing
    /// drifted, and the path is reported apart from the changes.
    NotWatched,
    /// A directory passed the file cap on both sides: the files past the
    /// cap are not compared. Reported apart from the changes.
    NotFullyChecked,
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Added => f.write_str("added"),
            Self::Removed => f.write_str("removed"),
            Self::Modified => f.write_str("modified"),
            Self::Unreadable => f.write_str("not checked: unreadable"),
            Self::Truncated => {
                write!(f, "not checked: more than {MAX_FILES_PER_DIRECTORY} files")
            }
            Self::NotWatched => f.write_str("unreadable on both sides"),
            Self::NotFullyChecked => {
                write!(f, "more than {MAX_FILES_PER_DIRECTORY} files on both sides")
            }
        }
    }
}

/// One drifted path, as stored on the run. The path is absolute; [`report`]
/// shortens it to `~/…` when it prints. A record an older relais wrote
/// already holds the `~/…` form, which prints unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Drift {
    pub path: String,
    pub change: Change,
}

impl Drift {
    /// The line for a path that was not compared at all; `None` for a
    /// path that changed.
    fn notice_line(&self) -> Option<String> {
        match self.change {
            Change::NotWatched => Some(format!("not watched (unreadable): {}", self.path)),
            Change::NotFullyChecked => Some(format!(
                "not fully checked: {} (more than {MAX_FILES_PER_DIRECTORY} files)",
                self.path
            )),
            Change::Added
            | Change::Removed
            | Change::Modified
            | Change::Unreadable
            | Change::Truncated => None,
        }
    }
}

/// The lines that report `drifts`: under `heading`, one indented line per
/// changed path and the follow-up; then one line per path that was not
/// compared. Empty when there is nothing to say.
/// A path under `home` prints as `~/…` on unix; with no `home`, every
/// path prints as stored.
pub fn report(drifts: &[Drift], heading: &str, home: Option<&Path>) -> Vec<String> {
    let drifts: Vec<Drift> = drifts
        .iter()
        .map(|drift| Drift {
            path: home.map_or_else(
                || drift.path.clone(),
                |home| shown(Path::new(&drift.path), home),
            ),
            change: drift.change,
        })
        .collect();
    let changed: Vec<&Drift> = drifts
        .iter()
        .filter(|drift| drift.notice_line().is_none())
        .collect();
    let mut lines = Vec::new();
    if !changed.is_empty() {
        lines.push(heading.to_string());
        lines.extend(
            changed
                .iter()
                .map(|drift| format!("  {} ({})", drift.path, drift.change)),
        );
        lines.extend(FOLLOW_UP.iter().map(|line| (*line).to_string()));
    }
    lines.extend(drifts.iter().filter_map(Drift::notice_line));
    lines
}

/// Whether `path` may be listed in `[fingerprint] extra_paths`: absolute,
/// or under `~/` (or `~\` on Windows).
pub fn is_valid_extra_path(path: &Path) -> bool {
    path.is_absolute()
        || path.to_str().is_some_and(|text| {
            text.starts_with("~/") || (cfg!(windows) && text.starts_with("~\\"))
        })
}

/// `extra_paths` with a leading `~/` (or `~\` on Windows) made relative
/// to `home`.
pub fn expand_extra_paths(paths: &[PathBuf], home: &Path) -> Vec<PathBuf> {
    paths
        .iter()
        .map(|path| match path.strip_prefix("~") {
            Ok(rest) if is_valid_extra_path(path) => home.join(rest),
            _ => path.clone(),
        })
        .collect()
}

/// The home directory paths print relative to: the process's `HOME`
/// (else `USERPROFILE`); `None` when neither is set.
pub fn print_home() -> Option<PathBuf> {
    crate::paths::resolve_home(|name| std::env::var_os(name)).ok()
}

/// How a path is printed: `~/…` under home on unix, absolute otherwise.
fn shown(path: &Path, home: &Path) -> String {
    if cfg!(unix) {
        if let Ok(rest) = path.strip_prefix(home) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

/// A watched file or directory, and which names inside a directory count.
struct Target {
    path: PathBuf,
    recursive: bool,
    keep: fn(&str) -> bool,
}

fn keep_all(_: &str) -> bool {
    true
}

/// `ssh` appends to `known_hosts*` in the normal course of work.
fn keep_ssh(name: &str) -> bool {
    !name.starts_with("known_hosts")
}

fn keep_claude_settings(name: &str) -> bool {
    name.starts_with("settings") && name.ends_with(".json")
}

fn single(path: PathBuf) -> Target {
    Target {
        path,
        recursive: false,
        keep: keep_all,
    }
}

/// The default watched set plus the machine's extras (SPEC §8).
fn targets(watch: &Watch<'_>) -> Vec<Target> {
    let home = watch.home;
    let claude = (watch.var)("CLAUDE_CONFIG_DIR")
        .filter(|value| !value.is_empty())
        .map_or_else(|| home.join(".claude"), PathBuf::from);
    let xdg_config = (watch.var)("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map_or_else(|| home.join(".config"), PathBuf::from);
    let mut targets = vec![
        single(home.join(".gitconfig")),
        single(xdg_config.join("git").join("config")),
        single(home.join(".netrc")),
        Target {
            path: claude.clone(),
            recursive: false,
            keep: keep_claude_settings,
        },
        single(claude.join("plugins").join("installed_plugins.json")),
        single(claude.join("plugins").join("known_marketplaces.json")),
        Target {
            path: home.join(".ssh"),
            recursive: true,
            keep: keep_ssh,
        },
    ];
    if cfg!(windows) {
        targets.push(single(home.join("_netrc")));
    }
    match &watch.repo_config {
        RepoConfig::At(path) | RepoConfig::Unresolved(path) => targets.push(single(path.clone())),
    }
    targets.extend(watch.extra_paths.iter().map(|path| Target {
        path: path.clone(),
        recursive: true,
        keep: keep_all,
    }));
    targets
}

#[cfg(unix)]
fn mode_of(metadata: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(metadata.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn mode_of(_: &std::fs::Metadata) -> Option<u32> {
    None
}

/// What a path holds, or `None` for what is not watched: a directory is
/// walked by its caller, and a FIFO, socket or device is skipped, since
/// opening a FIFO blocks and sockets come and go.
fn entry_of(path: &Path, read: &dyn Fn(&Path) -> io::Result<Vec<u8>>) -> Option<Entry> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Some(Entry::Absent),
        Err(_) => return Some(Entry::Unreadable),
    };
    if metadata.is_symlink() {
        return Some(match std::fs::read_link(path) {
            Ok(target) => Entry::Link { target },
            Err(_) => Entry::Unreadable,
        });
    }
    if !metadata.is_file() {
        return None;
    }
    if metadata.len() > HASH_LIMIT {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok());
        return Some(match modified {
            Some(since) => Entry::Large {
                modified_ns: since.as_nanos(),
                size: metadata.len(),
                mode: mode_of(&metadata),
            },
            None => Entry::Unreadable,
        });
    }
    Some(match read(path) {
        Ok(bytes) => Entry::File {
            sha256: sha256_hex(&bytes),
            size: metadata.len(),
            mode: mode_of(&metadata),
        },
        Err(_) => Entry::Unreadable,
    })
}

/// What [`walk`] found under one directory.
struct Walked {
    entries: BTreeMap<PathBuf, Entry>,
    /// Files counted so far, against [`MAX_FILES_PER_DIRECTORY`].
    files: usize,
    /// Whether more files remained past the cap.
    truncated: bool,
}

/// The files of a directory in byte-sorted order, subdirectories walked
/// depth first when `recursive`; stops at the cap. `files_so_far` is what
/// the enclosing directories already counted.
fn walk(
    target: &Target,
    dir: &Path,
    read: &dyn Fn(&Path) -> io::Result<Vec<u8>>,
    files_so_far: usize,
) -> Walked {
    let mut walked = Walked {
        entries: BTreeMap::new(),
        files: files_so_far,
        truncated: false,
    };
    let Ok(listing) = std::fs::read_dir(dir) else {
        walked.entries.insert(dir.to_path_buf(), Entry::Unreadable);
        return walked;
    };
    let mut names: Vec<OsString> = listing
        .filter_map(|item| item.ok())
        .map(|item| item.file_name())
        .collect();
    names.sort();
    for name in names {
        if !(target.keep)(&name.to_string_lossy()) {
            continue;
        }
        let path = dir.join(&name);
        let is_dir = std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_dir());
        if is_dir {
            if target.recursive {
                let inner = walk(target, &path, read, walked.files);
                walked.entries.extend(inner.entries);
                walked.files = inner.files;
                if inner.truncated {
                    walked.truncated = true;
                    return walked;
                }
            }
            continue;
        }
        let Some(entry) = entry_of(&path, read) else {
            continue;
        };
        if walked.files == MAX_FILES_PER_DIRECTORY {
            walked.truncated = true;
            return walked;
        }
        walked.files += 1;
        walked.entries.insert(path, entry);
    }
    walked
}

/// Snapshot the watched files. `read` is the seam through which file
/// contents arrive; a failure there is recorded as unreadable.
pub fn snapshot(watch: &Watch<'_>, read: &dyn Fn(&Path) -> io::Result<Vec<u8>>) -> Snapshot {
    let mut taken = Snapshot::default();
    for target in targets(watch) {
        if matches!(&watch.repo_config, RepoConfig::Unresolved(path) if *path == target.path) {
            taken.entries.insert(target.path, Entry::Unreadable);
            continue;
        }
        let is_dir =
            std::fs::symlink_metadata(&target.path).is_ok_and(|metadata| metadata.is_dir());
        if is_dir {
            let walked = walk(&target, &target.path, read, 0);
            taken.entries.extend(walked.entries);
            if walked.truncated {
                taken.truncated.insert(target.path);
            }
        } else if let Some(entry) = entry_of(&target.path, read) {
            taken.entries.insert(target.path, entry);
        }
    }
    taken
}

/// What changed between two snapshots, in path order. A path one side
/// does not hold counts as absent there.
pub fn diff(before: &Snapshot, after: &Snapshot) -> Vec<Drift> {
    let paths: BTreeSet<&PathBuf> = before.entries.keys().chain(after.entries.keys()).collect();
    let mut drifts = Vec::new();
    for path in paths {
        let was = before.entries.get(path).unwrap_or(&Entry::Absent);
        let is = after.entries.get(path).unwrap_or(&Entry::Absent);
        let change = match (was, is) {
            (Entry::Unreadable, Entry::Unreadable) => Some(Change::NotWatched),
            (Entry::Unreadable, _) | (_, Entry::Unreadable) => Some(Change::Unreadable),
            (Entry::Absent, Entry::Absent) => None,
            (Entry::Absent, _) => Some(Change::Added),
            (_, Entry::Absent) => Some(Change::Removed),
            _ => (was != is).then_some(Change::Modified),
        };
        if let Some(change) = change {
            drifts.push(Drift {
                path: path.display().to_string(),
                change,
            });
        }
    }
    for path in before.truncated.union(&after.truncated) {
        let change = if before.truncated.contains(path) && after.truncated.contains(path) {
            Change::NotFullyChecked
        } else {
            Change::Truncated
        };
        drifts.push(Drift {
            path: path.display().to_string(),
            change,
        });
    }
    drifts
}

/// The repository's shared `.git/config`, by `git rev-parse
/// --git-common-dir` (the bare form prints a relative `.git`), so a linked
/// worktree watches the config it shares. Bounded by [`GIT_WAIT`].
pub fn repo_config(root: &Path) -> RepoConfig {
    let guess = root.join(".git").join("config");
    let spawned = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = spawned else {
        return RepoConfig::Unresolved(guess);
    };
    let deadline = Instant::now() + GIT_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return RepoConfig::Unresolved(guess),
            Ok(None) if Instant::now() >= deadline => {
                child.kill().ok(); // abandoning the child: a failed kill leaves nothing to do
                child.wait().ok(); // reaping only; the exit status is not wanted
                return RepoConfig::Unresolved(guess);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        if io::Read::read_to_string(&mut stdout, &mut output).is_err() {
            return RepoConfig::Unresolved(guess);
        }
    }
    let line = output.trim();
    if line.is_empty() {
        return RepoConfig::Unresolved(guess);
    }
    RepoConfig::At(PathBuf::from(line).join("config"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watch<'a>(
        home: &'a Path,
        var: &'a dyn Fn(&str) -> Option<OsString>,
        extra: &'a [PathBuf],
    ) -> Watch<'a> {
        Watch {
            home,
            var,
            repo_config: RepoConfig::At(home.join("repo").join(".git").join("config")),
            extra_paths: extra,
        }
    }

    fn no_env(_: &str) -> Option<OsString> {
        None
    }

    fn real_read(path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }

    fn change_of(drifts: &[Drift], path: &str) -> Option<Change> {
        drifts
            .iter()
            .find(|drift| drift.path == path)
            .map(|drift| drift.change)
    }

    #[test]
    fn an_unchanged_home_has_no_drift() {
        let home = crate::test_support::temp_dir("homeprint-same");
        std::fs::write(home.join(".gitconfig"), "[user]\n").expect("write");
        let first = snapshot(&watch(&home, &no_env, &[]), &real_read);
        let second = snapshot(&watch(&home, &no_env, &[]), &real_read);
        assert_eq!(diff(&first, &second), Vec::new());
    }

    #[test]
    fn modified_added_and_removed_files_are_stored_absolute_and_printed_with_a_tilde() {
        let home = crate::test_support::temp_dir("homeprint-changes");
        std::fs::write(home.join(".gitconfig"), "one").expect("write");
        std::fs::write(home.join(".netrc"), "machine").expect("write");
        let before = snapshot(&watch(&home, &no_env, &[]), &real_read);
        std::fs::write(home.join(".gitconfig"), "two").expect("write");
        std::fs::remove_file(home.join(".netrc")).expect("remove");
        std::fs::create_dir_all(home.join(".ssh")).expect("mkdir");
        std::fs::write(home.join(".ssh").join("config"), "Host *").expect("write");
        let after = snapshot(&watch(&home, &no_env, &[]), &real_read);
        let drifts = diff(&before, &after);
        let stored = |path: PathBuf| path.display().to_string();
        assert_eq!(
            change_of(&drifts, &stored(home.join(".gitconfig"))),
            Some(Change::Modified)
        );
        assert_eq!(
            change_of(&drifts, &stored(home.join(".netrc"))),
            Some(Change::Removed)
        );
        assert_eq!(
            change_of(&drifts, &stored(home.join(".ssh").join("config"))),
            Some(Change::Added)
        );
        assert_eq!(drifts.len(), 3, "{drifts:?}");
        let lines = report(&drifts, "heading", Some(&home));
        assert!(
            lines.contains(&format!("  {} (modified)", shown_gitconfig(&home))),
            "{lines:?}"
        );
    }

    #[test]
    fn known_hosts_is_not_watched() {
        let home = crate::test_support::temp_dir("homeprint-known-hosts");
        std::fs::create_dir_all(home.join(".ssh")).expect("mkdir");
        let before = snapshot(&watch(&home, &no_env, &[]), &real_read);
        std::fs::write(home.join(".ssh").join("known_hosts"), "host key").expect("write");
        std::fs::write(home.join(".ssh").join("known_hosts.old"), "host key").expect("write");
        let after = snapshot(&watch(&home, &no_env, &[]), &real_read);
        assert_eq!(diff(&before, &after), Vec::new());
    }

    #[test]
    fn a_read_error_is_reported_as_not_checked() {
        let home = crate::test_support::temp_dir("homeprint-unreadable");
        std::fs::write(home.join(".gitconfig"), "one").expect("write");
        let failing = |path: &Path| -> io::Result<Vec<u8>> {
            if path.ends_with(".gitconfig") {
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "injected"))
            } else {
                std::fs::read(path)
            }
        };
        let before = snapshot(&watch(&home, &no_env, &[]), &real_read);
        let after = snapshot(&watch(&home, &no_env, &[]), &failing);
        let drifts = diff(&before, &after);
        assert_eq!(drifts.len(), 1, "{drifts:?}");
        assert_eq!(drifts[0].change, Change::Unreadable);
        assert_eq!(
            report(&drifts, "heading", Some(&home)),
            vec![
                "heading".to_string(),
                format!("  {} (not checked: unreadable)", shown_gitconfig(&home)),
                FOLLOW_UP[0].to_string(),
                FOLLOW_UP[1].to_string()
            ]
        );
    }

    #[test]
    fn an_extra_directory_is_walked() {
        let home = crate::test_support::temp_dir("homeprint-extra");
        let extra = home.join("tool");
        std::fs::create_dir_all(extra.join("nested")).expect("mkdir");
        std::fs::write(extra.join("nested").join("a.toml"), "a").expect("write");
        let extras = [extra.clone()];
        let before = snapshot(&watch(&home, &no_env, &extras), &real_read);
        std::fs::write(extra.join("nested").join("a.toml"), "b").expect("write");
        let after = snapshot(&watch(&home, &no_env, &extras), &real_read);
        let drifts = diff(&before, &after);
        assert_eq!(drifts.len(), 1, "{drifts:?}");
        assert_eq!(drifts[0].change, Change::Modified);
    }

    #[test]
    fn a_directory_past_the_cap_is_cut_the_same_way_twice() {
        let home = crate::test_support::temp_dir("homeprint-cap");
        let extra = home.join("many");
        std::fs::create_dir_all(&extra).expect("mkdir");
        for index in 0..(MAX_FILES_PER_DIRECTORY + 100) {
            std::fs::write(extra.join(format!("f{index:04}")), "x").expect("write");
        }
        let extras = [extra];
        let first = snapshot(&watch(&home, &no_env, &extras), &real_read);
        let second = snapshot(&watch(&home, &no_env, &extras), &real_read);
        let kept = first
            .entries
            .keys()
            .filter(|path| path.starts_with(home.join("many")))
            .count();
        assert_eq!(kept, MAX_FILES_PER_DIRECTORY);
        assert!(first.truncated.contains(&home.join("many")));
        let drifts = diff(&first, &second);
        assert_eq!(drifts.len(), 1, "{drifts:?}");
        assert_eq!(drifts[0].change, Change::NotFullyChecked);
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_in_a_watched_directory_does_not_hang_the_snapshot() {
        let home = crate::test_support::temp_dir("homeprint-fifo");
        let extra = home.join("tool");
        std::fs::create_dir_all(&extra).expect("mkdir");
        let made = Command::new("mkfifo")
            .arg(extra.join("pipe"))
            .status()
            .expect("mkfifo runs");
        assert!(made.success());
        std::fs::write(extra.join("plain"), "x").expect("write");
        let extras = [extra.clone()];
        let taken = snapshot(&watch(&home, &no_env, &extras), &real_read);
        assert!(taken.entries.contains_key(&extra.join("plain")));
        assert!(!taken.entries.contains_key(&extra.join("pipe")));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_recorded_by_target_and_not_followed() {
        let home = crate::test_support::temp_dir("homeprint-link");
        let secret = home.join("elsewhere");
        std::fs::write(&secret, "one").expect("write");
        std::os::unix::fs::symlink(&secret, home.join(".gitconfig")).expect("symlink");
        let before = snapshot(&watch(&home, &no_env, &[]), &real_read);
        std::fs::write(&secret, "two").expect("write");
        let after = snapshot(&watch(&home, &no_env, &[]), &real_read);
        assert_eq!(diff(&before, &after), Vec::new());
        assert!(matches!(
            before.entries.get(&home.join(".gitconfig")),
            Some(Entry::Link { .. })
        ));
    }

    #[test]
    fn the_userprofile_home_and_the_environment_choose_the_watched_paths() {
        let home = PathBuf::from("/h");
        let var = |name: &str| match name {
            "XDG_CONFIG_HOME" => Some(OsString::from("/xdg")),
            "CLAUDE_CONFIG_DIR" => Some(OsString::from("/claude")),
            _ => None,
        };
        let paths: Vec<PathBuf> = targets(&watch(&home, &var, &[]))
            .into_iter()
            .map(|target| target.path)
            .collect();
        assert!(paths.contains(&PathBuf::from("/xdg/git/config")));
        assert!(paths.contains(&PathBuf::from("/claude/plugins/installed_plugins.json")));
        assert!(paths.contains(&PathBuf::from("/h/.gitconfig")));
        assert!(paths.contains(&PathBuf::from("/h/.ssh")));
        let fallback: Vec<PathBuf> = targets(&watch(&home, &no_env, &[]))
            .into_iter()
            .map(|target| target.path)
            .collect();
        assert!(fallback.contains(&PathBuf::from("/h/.config/git/config")));
        assert!(fallback.contains(&PathBuf::from("/h/.claude/plugins/known_marketplaces.json")));
    }

    #[test]
    fn an_unresolved_repo_config_is_recorded_unreadable() {
        let home = crate::test_support::temp_dir("homeprint-unresolved");
        let guess = home.join("repo").join(".git").join("config");
        let watching = Watch {
            home: &home,
            var: &no_env,
            repo_config: RepoConfig::Unresolved(guess.clone()),
            extra_paths: &[],
        };
        let taken = snapshot(&watching, &real_read);
        assert_eq!(taken.entries.get(&guess), Some(&Entry::Unreadable));
    }

    #[test]
    fn a_record_stored_with_a_tilde_prints_unchanged() {
        // A record an older relais wrote holds the printed `~/…` form.
        let drifts = [Drift {
            path: "~/.gitconfig".into(),
            change: Change::Modified,
        }];
        assert_eq!(
            report(&drifts, "home files changed:", Some(Path::new("/h"))),
            vec![
                "home files changed:".to_string(),
                "  ~/.gitconfig (modified)".to_string(),
                FOLLOW_UP[0].to_string(),
                FOLLOW_UP[1].to_string()
            ]
        );
    }

    #[test]
    fn a_path_unreadable_on_both_sides_is_not_a_drift() {
        let home = crate::test_support::temp_dir("homeprint-both-unreadable");
        std::fs::write(home.join(".gitconfig"), "one").expect("write");
        let failing = |path: &Path| -> io::Result<Vec<u8>> {
            if path.ends_with(".gitconfig") {
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "injected"))
            } else {
                std::fs::read(path)
            }
        };
        let before = snapshot(&watch(&home, &no_env, &[]), &failing);
        let after = snapshot(&watch(&home, &no_env, &[]), &failing);
        let drifts = diff(&before, &after);
        assert_eq!(drifts.len(), 1, "{drifts:?}");
        assert_eq!(drifts[0].change, Change::NotWatched);
        let lines = report(&drifts, "heading", Some(&home));
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].starts_with("not watched (unreadable): "),
            "{lines:?}"
        );
        assert!(lines[0].ends_with(".gitconfig"), "{lines:?}");
    }

    #[test]
    fn a_directory_cut_on_both_sides_is_reported_as_not_fully_checked() {
        let home = PathBuf::from("/h");
        let cut = Snapshot {
            entries: BTreeMap::new(),
            truncated: BTreeSet::from([home.join("many")]),
        };
        let drifts = diff(&cut, &cut);
        assert_eq!(
            report(&drifts, "heading", Some(&home)),
            vec![format!(
                "not fully checked: {} (more than {MAX_FILES_PER_DIRECTORY} files)",
                shown(&home.join("many"), &home)
            )]
        );
    }

    #[test]
    fn with_no_home_a_report_prints_the_stored_path() {
        let stored = absolute_tool_path()
            .join(".gitconfig")
            .display()
            .to_string();
        let drifts = [Drift {
            path: stored.clone(),
            change: Change::Modified,
        }];
        assert_eq!(
            report(&drifts, "heading", None)[1],
            format!("  {stored} (modified)")
        );
    }

    #[test]
    fn a_file_over_the_hash_limit_is_never_read_and_its_change_still_shows() {
        let home = crate::test_support::temp_dir("homeprint-large");
        let extra = home.join("tool");
        std::fs::create_dir_all(&extra).expect("mkdir");
        let big = extra.join("big.bin");
        let size = usize::try_from(HASH_LIMIT).expect("fits") + 1;
        std::fs::write(&big, vec![b'a'; size]).expect("write");
        std::fs::write(extra.join("small"), "x").expect("write");
        let refusing = |path: &Path| -> io::Result<Vec<u8>> {
            assert!(!path.ends_with("big.bin"), "a large file was read");
            std::fs::read(path)
        };
        let extras = [extra.clone()];
        let before = snapshot(&watch(&home, &no_env, &extras), &refusing);
        assert!(matches!(
            before.entries.get(&big),
            Some(Entry::Large { size: s, .. }) if *s == HASH_LIMIT + 1
        ));
        assert!(matches!(
            before.entries.get(&extra.join("small")),
            Some(Entry::File { .. })
        ));
        std::fs::write(&big, vec![b'b'; size + 1]).expect("rewrite");
        let after = snapshot(&watch(&home, &no_env, &extras), &refusing);
        assert_eq!(
            diff(&before, &after),
            vec![Drift {
                path: big.display().to_string(),
                change: Change::Modified,
            }]
        );
    }

    #[test]
    fn a_large_file_touched_with_the_same_content_shows_as_modified() {
        let home = crate::test_support::temp_dir("homeprint-large-touch");
        let extra = home.join("tool");
        std::fs::create_dir_all(&extra).expect("mkdir");
        let big = extra.join("big.bin");
        let size = usize::try_from(HASH_LIMIT).expect("fits") + 1;
        std::fs::write(&big, vec![b'a'; size]).expect("write");
        let extras = [extra.clone()];
        let before = snapshot(&watch(&home, &no_env, &extras), &real_read);
        let later = std::fs::metadata(&big)
            .expect("stat")
            .modified()
            .expect("mtime")
            + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&big)
            .expect("open")
            .set_modified(later)
            .expect("touch");
        let after = snapshot(&watch(&home, &no_env, &extras), &real_read);
        assert_eq!(
            diff(&before, &after),
            vec![Drift {
                path: big.display().to_string(),
                change: Change::Modified,
            }]
        );
    }

    #[test]
    fn a_leading_tilde_slash_is_expanded_against_the_home() {
        let home = PathBuf::from("/h");
        let absolute = absolute_tool_path();
        let paths = [PathBuf::from("~/.config/relais"), absolute.clone()];
        assert_eq!(
            expand_extra_paths(&paths, &home),
            vec![PathBuf::from("/h/.config/relais"), absolute.clone()]
        );
        assert!(is_valid_extra_path(Path::new("~/x")));
        assert!(is_valid_extra_path(&absolute));
        assert!(!is_valid_extra_path(Path::new("x/y")));
        assert!(!is_valid_extra_path(Path::new("~other/x")));
    }

    /// An absolute path on this platform: `/abs/tool` is not absolute on
    /// Windows, where a path needs a drive.
    fn absolute_tool_path() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\abs\tool")
        } else {
            PathBuf::from("/abs/tool")
        }
    }

    /// How `~/.gitconfig` under `home` prints: `~/…` on unix, absolute on
    /// Windows, where a path is printed so it can be pasted.
    fn shown_gitconfig(home: &Path) -> String {
        if cfg!(unix) {
            "~/.gitconfig".to_string()
        } else {
            home.join(".gitconfig").display().to_string()
        }
    }
}
