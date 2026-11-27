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
pub const FOLLOW_UP: [&str; 3] = [
    "check these files; if you did not change them, the run, its tests or",
    "another session did:",
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

/// The hash of a file with one writer's own part taken out, and the
/// writer's name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReducedHash {
    writer: &'static str,
    sha256: String,
}

/// Which part of a watched file a known writer owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriterPart {
    /// The per-entry bookkeeping of `installed_plugins.json`.
    InstalledPlugins,
    /// The per-marketplace bookkeeping of `known_marketplaces.json`.
    KnownMarketplaces,
    /// The `[branch "…"]` sections of the repository's `.git/config`.
    BranchSections,
}

const PLUGIN_WRITER: &str = "claude code plugin refresh";
const BRANCH_WRITER: &str = "git branch tracking";

/// The fields Claude Code rewrites on a plugin or marketplace entry when a
/// session refreshes it. Every other field stays in the reduced hash, so a
/// new entry or a changed `source` is never explained.
const PLUGIN_ENTRY_KEYS: [&str; 5] = [
    "lastUpdated",
    "installedAt",
    "gitCommitSha",
    "version",
    "installPath",
];

impl WriterPart {
    fn writer(self) -> &'static str {
        match self {
            Self::InstalledPlugins | Self::KnownMarketplaces => PLUGIN_WRITER,
            Self::BranchSections => BRANCH_WRITER,
        }
    }

    /// The hash of `bytes` without this writer's part; `None` when the
    /// file does not parse as the format the part is cut from.
    fn reduced_hash(self, bytes: &[u8]) -> Option<ReducedHash> {
        let reduced = match self {
            Self::InstalledPlugins => without_installed_plugin_bookkeeping(bytes)?,
            Self::KnownMarketplaces => without_marketplace_bookkeeping(bytes)?,
            Self::BranchSections => without_branch_sections(bytes)?,
        };
        Some(ReducedHash {
            writer: self.writer(),
            sha256: sha256_hex(&reduced),
        })
    }
}

/// What one watched path held.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    Absent,
    File {
        sha256: String,
        size: u64,
        mode: Option<u32>,
        /// The hash of the file without its known writer's part, for the
        /// few files a tool rewrites in its normal work.
        reduced: Option<ReducedHash>,
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
    /// Why the path changed; absent on a record an older relais wrote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<Cause>,
}

/// Why a watched path changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Cause {
    /// A known tool's own upkeep: only the writer's part of the file changed.
    Writer { name: String },
    /// A tool call of one of the run's own workers writes the path.
    Call { tool: String, agent: String },
    /// Nothing known explains it. `calls_read` is false when the run had no
    /// worker dispatch, or a worker's transcript could not be read, so the
    /// worker's calls were not all checked.
    Unexplained { calls_read: bool },
}

/// A tool call of a worker that writes a watched path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerCall {
    pub tool: String,
    /// The full agent id.
    pub agent: String,
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

/// The lines that report `drifts`: under `heading`, the paths nothing
/// explains first (a record with no cause is among them), then the paths a
/// worker's call wrote, each as an indented line with its cause below it,
/// then one count line per known writer, and the follow-up when a path of
/// the first two groups was listed; then one line per path that was not
/// compared. Empty when there is nothing to say.
/// A path under `home` prints as `~/…` on unix; with no `home`, every
/// path prints as stored.
pub fn report(drifts: &[Drift], heading: &str, home: Option<&Path>) -> Vec<String> {
    let shown_path = |drift: &Drift| {
        home.map_or_else(
            || drift.path.clone(),
            |home| shown(Path::new(&drift.path), home),
        )
    };
    let mut unexplained = Vec::new();
    let mut by_worker = Vec::new();
    let mut by_writer: BTreeMap<&str, usize> = BTreeMap::new();
    for drift in drifts.iter().filter(|drift| drift.notice_line().is_none()) {
        let line = format!("  {} ({})", shown_path(drift), drift.change);
        match &drift.cause {
            None => unexplained.push(vec![line]),
            Some(Cause::Unexplained { calls_read: true }) => {
                unexplained.push(vec![line, "    unexplained".to_string()]);
            }
            Some(Cause::Unexplained { calls_read: false }) => unexplained.push(vec![
                line,
                "    unexplained (no worker transcript read)".to_string(),
            ]),
            Some(Cause::Call { tool, agent }) => {
                by_worker.push(vec![line, format!("    by {tool} in agent {agent}")]);
            }
            Some(Cause::Writer { name }) => *by_writer.entry(name.as_str()).or_default() += 1,
        }
    }
    let mut lines = Vec::new();
    if !(unexplained.is_empty() && by_worker.is_empty() && by_writer.is_empty()) {
        lines.push(heading.to_string());
        let needs_follow_up = !(unexplained.is_empty() && by_worker.is_empty());
        lines.extend(unexplained.into_iter().chain(by_worker).flatten());
        lines.extend(
            by_writer
                .iter()
                .map(|(name, count)| format!("  {count} by known writer: {name}")),
        );
        if needs_follow_up {
            lines.extend(FOLLOW_UP.iter().map(|line| (*line).to_string()));
        }
    }
    lines.extend(drifts.iter().filter_map(|drift| {
        let mut shown_drift = drift.clone();
        shown_drift.path = shown_path(drift);
        shown_drift.notice_line()
    }));
    lines
}

/// Give each drift its cause. A worker's call that writes the path comes
/// first (a worker that writes only a writer's part is still the run's own
/// write); then a known writer, when both sides are the same regular file
/// with the same mode, both have a reduced hash, the reduced hashes are
/// equal and the full hash changed; else the drift is unexplained.
/// `calls_read` says whether every worker transcript was read. A path that
/// was not compared at all has no cause.
pub fn attribute(
    drifts: Vec<Drift>,
    before: &Snapshot,
    after: &Snapshot,
    call_for: &dyn Fn(&Path) -> Option<WorkerCall>,
    calls_read: bool,
) -> Vec<Drift> {
    drifts
        .into_iter()
        .map(|drift| {
            let cause = if drift.notice_line().is_some() {
                None
            } else {
                let path = PathBuf::from(&drift.path);
                Some(if let Some(call) = call_for(&path) {
                    Cause::Call {
                        tool: call.tool,
                        agent: call.agent,
                    }
                } else if let Some(name) = known_writer(before, after, &path) {
                    Cause::Writer {
                        name: name.to_string(),
                    }
                } else {
                    Cause::Unexplained { calls_read }
                })
            };
            Drift { cause, ..drift }
        })
        .collect()
}

/// The writer whose own part is all that changed at `path`.
fn known_writer(before: &Snapshot, after: &Snapshot, path: &Path) -> Option<&'static str> {
    match (before.entries.get(path)?, after.entries.get(path)?) {
        (
            Entry::File {
                sha256: was,
                mode: was_mode,
                reduced: Some(was_reduced),
                ..
            },
            Entry::File {
                sha256: is,
                mode: is_mode,
                reduced: Some(is_reduced),
                ..
            },
        ) if was != is && was_mode == is_mode && was_reduced == is_reduced => {
            Some(was_reduced.writer)
        }
        _ => None,
    }
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
    /// The part of the file a known writer owns, for a single file.
    part: Option<WriterPart>,
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
        part: None,
    }
}

fn with_part(target: Target, part: WriterPart) -> Target {
    Target {
        part: Some(part),
        ..target
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
            part: None,
        },
        with_part(
            single(claude.join("plugins").join("installed_plugins.json")),
            WriterPart::InstalledPlugins,
        ),
        with_part(
            single(claude.join("plugins").join("known_marketplaces.json")),
            WriterPart::KnownMarketplaces,
        ),
        Target {
            path: home.join(".ssh"),
            recursive: true,
            keep: keep_ssh,
            part: None,
        },
    ];
    if cfg!(windows) {
        targets.push(single(home.join("_netrc")));
    }
    match &watch.repo_config {
        RepoConfig::At(path) | RepoConfig::Unresolved(path) => {
            targets.push(with_part(single(path.clone()), WriterPart::BranchSections))
        }
    }
    targets.extend(watch.extra_paths.iter().map(|path| Target {
        path: path.clone(),
        recursive: true,
        keep: keep_all,
        part: None,
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
fn entry_of(
    path: &Path,
    read: &dyn Fn(&Path) -> io::Result<Vec<u8>>,
    part: Option<WriterPart>,
) -> Option<Entry> {
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
            size: bytes.len() as u64,
            mode: mode_of(&metadata),
            reduced: part.and_then(|part| part.reduced_hash(&bytes)),
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
        let Some(entry) = entry_of(&path, read, None) else {
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
        } else {
            let is_repo_config = matches!(
                &watch.repo_config,
                RepoConfig::At(path) if *path == target.path
            );
            let entry = if is_repo_config {
                // The tool's own keys are left out of what is hashed.
                entry_of(
                    &target.path,
                    &|path| read(path).map(|bytes| without_tool_keys(&bytes)),
                    target.part,
                )
            } else {
                entry_of(&target.path, read, target.part)
            };
            if let Some(entry) = entry {
                taken.entries.insert(target.path, entry);
            }
        }
    }
    taken
}

/// Keys of the `[greengate]` section that greengate writes to the shared
/// `.git/config` in its normal work: a trust record when a `greengate.conf`
/// is accepted (one per checkout, trimmed to sixteen) and the identity its
/// commit hook learns. They are not a run reaching outside its worktree, so
/// they stay out of the hash. Every other key stays in it: a change to a
/// setting that decides which gates run is still a drift.
const TOOL_KEYS: [&str; 2] = ["trusted", "knownidentity"];

/// The header a config line opens: what stands between `[` and `]`,
/// lowercased and trimmed, subsection included (`greengate "x"`, so it is
/// never `greengate`). `None` for any other line.
fn section_opened(line: &[u8]) -> Option<String> {
    let rest = line.trim_ascii_start().strip_prefix(b"[")?;
    let end = rest
        .iter()
        .position(|byte| *byte == b']')
        .unwrap_or(rest.len());
    let header = String::from_utf8_lossy(&rest[..end]);
    Some(header.trim().to_ascii_lowercase())
}

/// Whether a line inside `[greengate]` sets one of [`TOOL_KEYS`].
fn is_tool_key(line: &[u8]) -> bool {
    let line = line.trim_ascii_start();
    let end = line
        .iter()
        .position(|byte| matches!(byte, b'=' | b' ' | b'\t' | b'\r' | b'\n'))
        .unwrap_or(line.len());
    let key = String::from_utf8_lossy(&line[..end]).to_ascii_lowercase();
    TOOL_KEYS.contains(&key.as_str())
}

/// `config` without the [`TOOL_KEYS`] lines of its `[greengate]` section;
/// everything else, byte for byte.
fn without_tool_keys(config: &[u8]) -> Vec<u8> {
    let mut kept = Vec::with_capacity(config.len());
    let mut in_greengate = false;
    for line in config.split_inclusive(|byte| *byte == b'\n') {
        if let Some(name) = section_opened(line) {
            in_greengate = name == "greengate";
        } else if in_greengate && is_tool_key(line) {
            continue;
        }
        kept.extend_from_slice(line);
    }
    kept
}

/// `config` without its `[branch "…"]` sections, which git rewrites to
/// keep track of branches; `None` when a section header is not closed.
fn without_branch_sections(config: &[u8]) -> Option<Vec<u8>> {
    let mut kept = Vec::with_capacity(config.len());
    let mut in_branch = false;
    for line in config.split_inclusive(|byte| *byte == b'\n') {
        if let Some(name) = section_opened(line) {
            if !line.contains(&b']') {
                return None;
            }
            in_branch = name
                .split_once(char::is_whitespace)
                .is_some_and(|(section, rest)| {
                    section == "branch" && rest.trim_start().starts_with('"')
                });
        }
        if !in_branch {
            kept.extend_from_slice(line);
        }
    }
    Some(kept)
}

/// `entry` without the fields of [`PLUGIN_ENTRY_KEYS`], when it is an
/// object; any other value unchanged.
fn without_plugin_entry_keys(entry: serde_json::Value) -> serde_json::Value {
    match entry {
        serde_json::Value::Object(fields) => serde_json::Value::Object(
            fields
                .into_iter()
                .filter(|(key, _)| !PLUGIN_ENTRY_KEYS.contains(&key.as_str()))
                .collect(),
        ),
        other => other,
    }
}

/// `installed_plugins.json` without the bookkeeping of each install in
/// `plugins.<name>[i]`; the top-level `version` stays.
fn without_installed_plugin_bookkeeping(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut document: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let root = document.as_object_mut()?;
    if let Some(plugins) = root
        .get_mut("plugins")
        .and_then(serde_json::Value::as_object_mut)
    {
        for installs in plugins.values_mut() {
            for install in installs.as_array_mut().into_iter().flatten() {
                *install = without_plugin_entry_keys(install.take());
            }
        }
    }
    serde_json::to_vec(&document).ok()
}

/// `known_marketplaces.json` without the bookkeeping of each top-level
/// marketplace entry.
fn without_marketplace_bookkeeping(bytes: &[u8]) -> Option<Vec<u8>> {
    let document: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let serde_json::Value::Object(marketplaces) = document else {
        return None;
    };
    let reduced: serde_json::Map<_, _> = marketplaces
        .into_iter()
        .map(|(name, marketplace)| (name, without_plugin_entry_keys(marketplace)))
        .collect();
    serde_json::to_vec(&reduced).ok()
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
                cause: None,
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
            cause: None,
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

    fn repo_config_drift(before: &str, after: &str) -> Vec<Drift> {
        let home = crate::test_support::temp_dir("homeprint-repo-config");
        let config = home.join("repo").join(".git").join("config");
        std::fs::create_dir_all(config.parent().expect("parent")).expect("mkdir");
        std::fs::write(&config, before).expect("write");
        let first = snapshot(&watch(&home, &no_env, &[]), &real_read);
        std::fs::write(&config, after).expect("write");
        let second = snapshot(&watch(&home, &no_env, &[]), &real_read);
        diff(&first, &second)
    }

    #[test]
    fn greengate_trust_and_identity_writes_to_the_repo_config_are_no_drift() {
        let before = "[core]\n\tbare = false\n[GreenGate]\n\tattest = true\n\ttrusted = aaa\n";
        let after = "[core]\n\tbare = false\n[GreenGate]\n\tattest = true\n\ttrusted = aaa\n\
                     \tTrusted = bbb\n\tknownIdentity = x <y@z>\n";
        assert_eq!(repo_config_drift(before, after), Vec::new());
    }

    #[test]
    fn another_greengate_key_changing_is_still_drift() {
        let before = "[greengate]\n\ttrusted = aaa\n\tattest = true\n";
        let after = "[greengate]\n\ttrusted = aaa\n\tattest = false\n";
        assert_eq!(repo_config_drift(before, after).len(), 1);
    }

    #[test]
    fn a_trusted_key_in_a_greengate_subsection_is_still_drift() {
        for header in ["[greengate \"x\"]", "[greengate.x]"] {
            let before = format!("[greengate]\n\tattest = true\n{header}\n\tattest = true\n");
            let after = format!("{before}\ttrusted = b\n\tknownIdentity = c\n");
            assert_eq!(repo_config_drift(&before, &after).len(), 1, "{header}");
        }
    }

    #[test]
    fn a_trusted_key_in_another_section_is_still_drift() {
        let before = "[greengate]\n\tattest = true\n[core]\n\thooksPath = a\n";
        let after = "[greengate]\n\tattest = true\n[core]\n\thooksPath = a\n\ttrusted = b\n";
        assert_eq!(repo_config_drift(before, after).len(), 1);
    }

    #[test]
    fn a_change_beside_greengate_keys_is_still_drift() {
        let before = "[greengate]\n\ttrusted = aaa\n[user]\n\tname = a\n";
        let after = "[greengate]\n\ttrusted = aaa\n[user]\n\tname = a\n\
                     [url \"ssh://evil/\"]\n\tinsteadOf = https://x/\n";
        let drifts = repo_config_drift(before, after);
        assert_eq!(drifts.len(), 1, "{drifts:?}");
        assert_eq!(drifts[0].change, Change::Modified);
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
                FOLLOW_UP[1].to_string(),
                FOLLOW_UP[2].to_string()
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
            cause: None,
        }];
        assert_eq!(
            report(&drifts, "home files changed:", Some(Path::new("/h"))),
            vec![
                "home files changed:".to_string(),
                "  ~/.gitconfig (modified)".to_string(),
                FOLLOW_UP[0].to_string(),
                FOLLOW_UP[1].to_string(),
                FOLLOW_UP[2].to_string()
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
            cause: None,
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
                cause: None,
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
                cause: None,
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

    fn no_call(_: &Path) -> Option<WorkerCall> {
        None
    }

    /// The causes of the drifts between a home whose plugin file held
    /// `before` and then `after`, with no worker call.
    fn plugin_causes(file: &str, before: &str, after: &str) -> Vec<(String, Option<Cause>)> {
        let home = crate::test_support::temp_dir("homeprint-plugin");
        let path = home.join(".claude").join("plugins").join(file);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, before).expect("write");
        let first = snapshot(&watch(&home, &no_env, &[]), &real_read);
        std::fs::write(&path, after).expect("write");
        let second = snapshot(&watch(&home, &no_env, &[]), &real_read);
        attribute(diff(&first, &second), &first, &second, &no_call, true)
            .into_iter()
            .map(|drift| (drift.path, drift.cause))
            .collect()
    }

    fn writer(name: &str) -> Option<Cause> {
        Some(Cause::Writer { name: name.into() })
    }

    const MARKETPLACES: &str = r#"{"a":{"source":{"repo":"x/a"},"lastUpdated":"1"}}"#;

    #[test]
    fn a_last_updated_only_change_is_the_plugin_writers() {
        let after = r#"{"a":{"source":{"repo":"x/a"},"lastUpdated":"2"}}"#;
        let causes = plugin_causes("known_marketplaces.json", MARKETPLACES, after);
        assert_eq!(causes.len(), 1, "{causes:?}");
        assert_eq!(causes[0].1, writer("claude code plugin refresh"));
    }

    #[test]
    fn a_new_marketplace_entry_or_a_changed_source_is_unexplained() {
        let added = r#"{"a":{"source":{"repo":"x/a"},"lastUpdated":"1"},"b":{"lastUpdated":"1"}}"#;
        let moved = r#"{"a":{"source":{"repo":"x/evil"},"lastUpdated":"2"}}"#;
        for after in [added, moved] {
            let causes = plugin_causes("known_marketplaces.json", MARKETPLACES, after);
            assert_eq!(
                causes[0].1,
                Some(Cause::Unexplained { calls_read: true }),
                "{after}"
            );
        }
    }

    #[test]
    fn installed_plugins_bookkeeping_is_the_writers_and_a_new_plugin_is_not() {
        let before = r#"{"version":2,"plugins":{"p":[{"scope":"user","version":"1","installPath":"/a","gitCommitSha":"x","installedAt":"1","lastUpdated":"1"}]}}"#;
        let after = r#"{"version":2,"plugins":{"p":[{"scope":"user","version":"2","installPath":"/b","gitCommitSha":"y","installedAt":"2","lastUpdated":"2"}]}}"#;
        let causes = plugin_causes("installed_plugins.json", before, after);
        assert_eq!(causes[0].1, writer("claude code plugin refresh"));
        let top_level = after.replace(r#""version":2,"#, r#""version":3,"#);
        let causes = plugin_causes("installed_plugins.json", after, &top_level);
        assert_eq!(causes[0].1, Some(Cause::Unexplained { calls_read: true }));
        let scoped = after.replace(r#""scope":"user""#, r#""scope":"project""#);
        let causes = plugin_causes("installed_plugins.json", after, &scoped);
        assert_eq!(causes[0].1, Some(Cause::Unexplained { calls_read: true }));
    }

    #[test]
    fn unparsable_json_has_no_writer() {
        let causes = plugin_causes("known_marketplaces.json", "{not json", "{still not");
        assert_eq!(causes[0].1, Some(Cause::Unexplained { calls_read: true }));
    }

    #[cfg(unix)]
    #[test]
    fn a_last_updated_bump_with_a_chmod_is_unexplained() {
        use std::os::unix::fs::PermissionsExt;
        let home = crate::test_support::temp_dir("homeprint-plugin-chmod");
        let path = home
            .join(".claude")
            .join("plugins")
            .join("known_marketplaces.json");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, MARKETPLACES).expect("write");
        let first = snapshot(&watch(&home, &no_env, &[]), &real_read);
        std::fs::write(&path, MARKETPLACES.replace("\"1\"", "\"2\"")).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        let second = snapshot(&watch(&home, &no_env, &[]), &real_read);
        let drifts = attribute(diff(&first, &second), &first, &second, &no_call, true);
        assert_eq!(
            drifts[0].cause,
            Some(Cause::Unexplained { calls_read: true })
        );
    }

    #[test]
    fn a_branch_section_only_change_is_git_branch_tracking() {
        let before = "[core]\n\tbare = false\n";
        let after = "[core]\n\tbare = false\n[branch \"t\"]\n\tremote = origin\n";
        let drifts = repo_config_drift(before, after);
        assert_eq!(drifts.len(), 1, "{drifts:?}");
        assert_eq!(drifts[0].cause, None, "diff alone attributes nothing");
        let attributed = attributed_repo_config(before, after);
        assert_eq!(attributed[0].cause, writer("git branch tracking"));
        let other = "[core]\n\tbare = true\n[branch \"t\"]\n\tremote = origin\n";
        let attributed = attributed_repo_config(before, other);
        assert_eq!(
            attributed[0].cause,
            Some(Cause::Unexplained { calls_read: true })
        );
    }

    fn attributed_repo_config(before: &str, after: &str) -> Vec<Drift> {
        let home = crate::test_support::temp_dir("homeprint-repo-config-cause");
        let config = home.join("repo").join(".git").join("config");
        std::fs::create_dir_all(config.parent().expect("parent")).expect("mkdir");
        std::fs::write(&config, before).expect("write");
        let first = snapshot(&watch(&home, &no_env, &[]), &real_read);
        std::fs::write(&config, after).expect("write");
        let second = snapshot(&watch(&home, &no_env, &[]), &real_read);
        attribute(diff(&first, &second), &first, &second, &no_call, true)
    }

    #[test]
    fn a_worker_call_outranks_a_writer() {
        let before = "[core]\n\tbare = false\n";
        let after = "[core]\n\tbare = false\n[branch \"t\"]\n\tremote = origin\n";
        let home = crate::test_support::temp_dir("homeprint-call-first");
        let config = home.join("repo").join(".git").join("config");
        std::fs::create_dir_all(config.parent().expect("parent")).expect("mkdir");
        std::fs::write(&config, before).expect("write");
        let first = snapshot(&watch(&home, &no_env, &[]), &real_read);
        std::fs::write(&config, after).expect("write");
        let second = snapshot(&watch(&home, &no_env, &[]), &real_read);
        let call = |_: &Path| {
            Some(WorkerCall {
                tool: "Bash".into(),
                agent: "agent-1".into(),
            })
        };
        let drifts = attribute(diff(&first, &second), &first, &second, &call, true);
        assert_eq!(
            drifts[0].cause,
            Some(Cause::Call {
                tool: "Bash".into(),
                agent: "agent-1".into()
            })
        );
    }

    fn drift_with(path: &str, change: Change, cause: Option<Cause>) -> Drift {
        Drift {
            path: path.into(),
            change,
            cause,
        }
    }

    // unix: the fixture spells home paths with `/` and `~/` (Windows prints them
    // absolute and joins with `\`).
    #[cfg(unix)]
    #[test]
    fn the_report_lists_unexplained_then_worker_named_then_one_count_line_per_writer() {
        let drifts = [
            drift_with(
                "/h/.claude/plugins/known_marketplaces.json",
                Change::Modified,
                writer("claude code plugin refresh"),
            ),
            drift_with(
                "/h/repo/.git/config",
                Change::Modified,
                writer("git branch tracking"),
            ),
            drift_with(
                "/h/.claude/plugins/installed_plugins.json",
                Change::Modified,
                writer("claude code plugin refresh"),
            ),
            drift_with(
                "/h/.netrc",
                Change::Added,
                Some(Cause::Call {
                    tool: "Write".into(),
                    agent: "a30abb7e-1234-4abc".into(),
                }),
            ),
            drift_with(
                "/h/.zshrc",
                Change::Modified,
                Some(Cause::Unexplained { calls_read: false }),
            ),
            drift_with("/h/.gitconfig", Change::Modified, None),
            drift_with(
                "/h/.ssh/config",
                Change::Modified,
                Some(Cause::Unexplained { calls_read: true }),
            ),
        ];
        assert_eq!(
            report(&drifts, "heading", Some(Path::new("/h"))),
            vec![
                "heading",
                "  ~/.zshrc (modified)",
                "    unexplained (no worker transcript read)",
                "  ~/.gitconfig (modified)",
                "  ~/.ssh/config (modified)",
                "    unexplained",
                "  ~/.netrc (added)",
                "    by Write in agent a30abb7e-1234-4abc",
                "  2 by known writer: claude code plugin refresh",
                "  1 by known writer: git branch tracking",
                FOLLOW_UP[0],
                FOLLOW_UP[1],
                FOLLOW_UP[2],
            ]
        );
    }

    #[test]
    fn only_known_writers_print_the_heading_and_the_count_line() {
        let drifts = [drift_with(
            "/h/repo/.git/config",
            Change::Modified,
            writer("git branch tracking"),
        )];
        assert_eq!(
            report(&drifts, "heading", None),
            vec!["heading", "  1 by known writer: git branch tracking"]
        );
    }

    #[test]
    fn every_report_line_fits_80_columns_with_a_17_character_agent() {
        let drifts = [
            drift_with(
                "~/.netrc",
                Change::Modified,
                Some(Cause::Call {
                    tool: "NotebookEdit".into(),
                    agent: "a30abb7e12345678a".into(),
                }),
            ),
            drift_with(
                "~/.gitconfig",
                Change::Modified,
                Some(Cause::Unexplained { calls_read: false }),
            ),
            drift_with(
                "~/.claude/plugins/known_marketplaces.json",
                Change::Modified,
                writer("claude code plugin refresh"),
            ),
        ];
        let lines = report(&drifts, STATUS_HEADING, None);
        assert!(
            lines.iter().all(|line| line.chars().count() <= 80),
            "{lines:?}"
        );
    }

    #[test]
    fn an_old_record_still_reads_and_a_cause_round_trips() {
        let old: Drift =
            serde_json::from_str(r#"{"path":"~/.gitconfig","change":"modified"}"#).expect("old");
        assert_eq!(old.cause, None);
        assert_eq!(
            serde_json::to_string(&old).expect("json"),
            r#"{"path":"~/.gitconfig","change":"modified"}"#
        );
        let caused = Drift {
            cause: Some(Cause::Unexplained { calls_read: true }),
            ..old
        };
        let text = serde_json::to_string(&caused).expect("json");
        assert!(
            text.contains(r#""cause":{"kind":"unexplained","calls_read":true}"#),
            "{text}"
        );
        assert_eq!(serde_json::from_str::<Drift>(&text).expect("back"), caused);
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
