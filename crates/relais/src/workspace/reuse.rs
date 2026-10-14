//! Local runs reuse the caller's workspace (ADR-0008).
//!
//! Three questions live here, each answered by plain functions:
//!
//! 1. WHICH workspace a run gets: [`resolve`] turns the flag, the
//!    environment, `CI`, the machine setting and git's own answer about the
//!    checkout into [`Workspace`] or a typed refusal.
//! 2. WHAT is seeded into a fork: [`detect_deps`] names the git-ignored
//!    dependency directories, [`seed`] clones them copy-on-write.
//! 3. WHEN a setup step may be skipped: [`step_key`] identifies a step's
//!    result, [`StampStore`] remembers what relais itself produced, and
//!    [`decide_skip`] says whether a seeded tree already has it. The
//!    default is that nothing is skipped.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::fsclone::{self, CloneError, CloneStats};
use crate::ids::{canonical_json_hash, sha256_hex};
use crate::policy::{CommandSpec, ReuseSpec, WorkspaceMode, NEVER_SEEDED_DIRS};
use crate::procs::{run_with_timeout, Ended};
use crate::workspace::git_command;

// ---------------------------------------------------------------- mode

/// Where a run works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Workspace {
    /// Today: a fresh worktree and verification trees, every setup step in
    /// each. Server and CI, replay and live trials.
    Isolated,
    /// A fresh worktree whose dependency directories are cloned
    /// copy-on-write from the caller's workspace.
    Fork,
    /// The caller's own linked worktree, edited directly.
    InPlace,
}

impl Workspace {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Isolated => "isolated",
            Self::Fork => "fork",
            Self::InPlace => "in_place",
        }
    }
}

/// What a person (or the plugin) asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requested {
    Auto,
    Fork,
    InPlace,
    Isolated,
}

impl Requested {
    /// `auto|fork|in-place|isolated`, as spelled on the command line and
    /// in `RELAIS_WORKSPACE`.
    pub fn parse(text: &str) -> Result<Self, ModeRefusal> {
        match text {
            "auto" => Ok(Self::Auto),
            "fork" => Ok(Self::Fork),
            "in-place" => Ok(Self::InPlace),
            "isolated" => Ok(Self::Isolated),
            other => Err(ModeRefusal::Unknown(other.to_string())),
        }
    }
}

/// Everything [`resolve`] reads, gathered by the caller so the decision is
/// a pure function.
#[derive(Debug, Clone, Copy)]
pub struct ModeInputs<'a> {
    pub flag: Option<Requested>,
    pub env_workspace: Option<&'a str>,
    pub ci: Option<&'a str>,
    pub machine: WorkspaceMode,
    /// `[workspace] in_place = false` in the repository's relais.toml.
    pub repo_opts_out_of_in_place: bool,
    /// `repo::is_linked_worktree`; `None` = git could not say.
    pub linked_worktree: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModeRefusal {
    /// A value that is none of the four (usage error, exit 2).
    Unknown(String),
    /// `RELAIS_WORKSPACE=isolated` or `CI` forces isolation and the flag
    /// asked for less (block, exit 3).
    WidenedPastTheEnvironment,
    /// `--workspace in-place` outside a linked worktree.
    PrimaryCheckout,
    /// `--workspace in-place` in a repository that opted out.
    InPlaceOptedOut,
}

/// `CI` is set, non-empty and not an explicit no.
fn ci_is_on(ci: Option<&str>) -> bool {
    ci.is_some_and(|value| !matches!(value.trim(), "" | "0" | "false"))
}

/// Precedence, once: `--workspace` > `RELAIS_WORKSPACE` > `CI` > the
/// machine setting > `auto`. The environment and `CI` can force isolation
/// and the flag cannot widen past that; a repository can only turn
/// in-place off.
pub fn resolve(inputs: ModeInputs<'_>) -> Result<Workspace, ModeRefusal> {
    let env_request = inputs.env_workspace.map(Requested::parse).transpose()?;
    let forced_isolated = env_request == Some(Requested::Isolated) || ci_is_on(inputs.ci);
    if forced_isolated {
        return match inputs.flag {
            Some(requested) if requested != Requested::Isolated => {
                Err(ModeRefusal::WidenedPastTheEnvironment)
            }
            _ => Ok(Workspace::Isolated),
        };
    }
    let machine_default = match inputs.machine {
        WorkspaceMode::Auto => Requested::Auto,
        WorkspaceMode::Isolated => Requested::Isolated,
    };
    let requested = inputs.flag.or(env_request).unwrap_or(machine_default);
    let linked = inputs.linked_worktree == Some(true);
    match requested {
        Requested::Isolated => Ok(Workspace::Isolated),
        Requested::Fork => Ok(Workspace::Fork),
        Requested::InPlace if !linked => Err(ModeRefusal::PrimaryCheckout),
        Requested::InPlace if inputs.repo_opts_out_of_in_place => Err(ModeRefusal::InPlaceOptedOut),
        Requested::InPlace => Ok(Workspace::InPlace),
        Requested::Auto if linked && !inputs.repo_opts_out_of_in_place => Ok(Workspace::InPlace),
        Requested::Auto => Ok(Workspace::Fork),
    }
}

// ---------------------------------------------------------------- deps

fn git_lines(dir: &Path, args: &[&str]) -> io::Result<Vec<String>> {
    let output = git_command(dir).args(args).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .flat_map(|chunk| chunk.lines())
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

/// Directory names detected when `[workspace] deps` says nothing.
const DETECTED_DEP_NAMES: &[&str] = &["node_modules", "target"];

/// How deep below the root a named dependency directory is looked for:
/// `packages/web/node_modules` is depth 3.
const DEP_SEARCH_DEPTH: usize = 3;

/// Is this relative path git-ignored, untracked, and a directory that can
/// be moved? Only such a path is ever seeded: a hostile `deps` entry cannot
/// copy source files, and an environment is never carried (ADR-0008).
fn is_seedable(root: &Path, relative: &Path) -> bool {
    let full = root.join(relative);
    if !full.is_dir() || full.join("pyvenv.cfg").exists() {
        return false;
    }
    let text = relative.to_string_lossy();
    if relative
        .components()
        .any(|part| NEVER_SEEDED_DIRS.contains(&part.as_os_str().to_string_lossy().as_ref()))
    {
        return false;
    }
    let ignored = git_command(root)
        .args(["check-ignore", "-q", "--", text.as_ref()])
        .status()
        .is_ok_and(|status| status.success());
    let tracked = git_lines(root, &["ls-files", "-z", "--", text.as_ref()])
        .map(|files| !files.is_empty())
        .unwrap_or(true);
    ignored && !tracked
}

/// The dependency directories of `root` worth seeding, as paths relative to
/// it. `declared` is `[workspace] deps`: a bare name is searched for to
/// [`DEP_SEARCH_DEPTH`], a path with a `/` is taken as it is; empty means
/// the detected names. The walk never descends into a directory it found or
/// into `.git`.
pub fn detect_deps(root: &Path, declared: &[String]) -> Vec<PathBuf> {
    let (names, exact): (Vec<&str>, Vec<&str>) = if declared.is_empty() {
        (DETECTED_DEP_NAMES.to_vec(), Vec::new())
    } else {
        declared
            .iter()
            .map(String::as_str)
            .partition(|dep| !dep.contains('/'))
    };
    let mut found: Vec<PathBuf> = exact.iter().map(PathBuf::from).collect();
    let mut pending = vec![(PathBuf::new(), 0usize)];
    while let Some((relative, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(root.join(&relative)) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if !kind.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let child = relative.join(&name);
            if names.contains(&name.as_str()) {
                found.push(child);
            } else if name != ".git" && depth + 1 < DEP_SEARCH_DEPTH {
                pending.push((child, depth + 1));
            }
        }
    }
    found.retain(|path| is_seedable(root, path));
    found.sort();
    found.dedup();
    found
}

// ---------------------------------------------------------------- seed

/// What seeding did, for the run's `workspace_resolved` event and receipt.
#[derive(Debug, Default)]
pub struct SeedReport {
    pub seeded: Vec<(PathBuf, CloneStats)>,
    /// `(path, why)`: a directory that was not seeded and the reason.
    pub skipped: Vec<(PathBuf, String)>,
    /// Set when the filesystem cannot clone; nothing was copied and the
    /// caller runs today's full setup.
    pub unavailable: Option<String>,
}

/// Clone each of `deps` from `source` into `dest`, copy-on-write. A
/// directory already present in `dest` is left alone. A filesystem that
/// cannot clone stops the whole seed with [`SeedReport::unavailable`]; an
/// unsafe tree (a symlink out of the repository) skips only that tree.
pub fn seed(source: &Path, dest: &Path, deps: &[PathBuf]) -> SeedReport {
    let mut report = SeedReport::default();
    let Some(sample) = first_regular_file(source) else {
        report.unavailable = Some("the source workspace has no file to probe with".into());
        return report;
    };
    if let Err(e) = fsclone::probe(&sample, dest) {
        report.unavailable = Some(e.to_string());
        return report;
    }
    for relative in deps {
        let target = dest.join(relative);
        if target.exists() {
            report
                .skipped
                .push((relative.clone(), "already present".into()));
            continue;
        }
        match fsclone::clone_tree(source, dest, &source.join(relative), &target) {
            Ok(stats) => report.seeded.push((relative.clone(), stats)),
            Err(e @ CloneError::UnsafeSymlink { .. }) => {
                report.skipped.push((relative.clone(), e.to_string()));
            }
            Err(e) => {
                report.unavailable = Some(e.to_string());
                return report;
            }
        }
    }
    report
}

fn first_regular_file(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path())
}

// ---------------------------------------------------------- quiescence

/// holds-until: a clone of a half-written install is observed that a check
/// of the first two levels called quiet. The check has no authority: every
/// setup step runs after a seed, and a `reuse` skip needs its own outputs
/// and `verify`, so a wrong answer here costs an install, never a verdict.
///
/// How deep the quietness check looks. A whole `node_modules` (333,000
/// entries on happier) took 37 s to walk on a loaded machine, more than the
/// politeness is worth; an install adds and removes packages at the top two
/// levels, which is a few thousand entries. The check has no authority over
/// whether a step runs, so a shallow look is the right size for it.
const QUIET_DEPTH: usize = 2;

/// The newest mtime and the entry count over the first [`QUIET_DEPTH`]
/// levels of a directory.
fn tree_state(dir: &Path) -> io::Result<(std::time::SystemTime, u64)> {
    let mut newest = std::time::UNIX_EPOCH;
    let mut count = 0u64;
    let mut pending = vec![(dir.to_path_buf(), 1usize)];
    while let Some((current, depth)) = pending.pop() {
        for entry in std::fs::read_dir(&current)? {
            let entry = entry?;
            count += 1;
            if let Ok(modified) = entry.metadata()?.modified() {
                newest = newest.max(modified);
            }
            if depth < QUIET_DEPTH && entry.file_type()?.is_dir() {
                pending.push((entry.path(), depth + 1));
            }
        }
    }
    Ok((newest, count))
}

/// A cheap fingerprint of a directory tree: the newest mtime and the entry
/// count over the whole walk. Used ONLY to decide when it is polite to
/// clone (not mid-install); it has no authority over whether a step runs,
/// because a timestamp-preserving rewrite defeats it.
pub fn tree_fingerprint(dir: &Path) -> io::Result<String> {
    let (newest, count) = tree_state(dir)?;
    let nanos = newest
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    Ok(format!("{nanos}:{count}"))
}

/// Wait until `dir` stopped changing for `quiet_for`, polling every
/// `poll`, for at most `give_up`. `sleep` and `progress(waited)` are
/// injected so the wait is testable and reports every ten seconds.
pub fn wait_until_quiet(
    dir: &Path,
    quiet_for: Duration,
    poll: Duration,
    give_up: Duration,
    sleep: &mut dyn FnMut(Duration),
    progress: &mut dyn FnMut(Duration),
) -> bool {
    // Already still for `quiet_for`: nothing changed since before the
    // window began, so there is nothing to wait out. A future-dated file
    // reads as "changed just now" and waits the full window.
    if let Ok((newest, _)) = tree_state(dir) {
        if std::time::SystemTime::now()
            .duration_since(newest)
            .is_ok_and(|age| age >= quiet_for)
        {
            return true;
        }
    }
    let mut waited = Duration::ZERO;
    let mut stable = Duration::ZERO;
    // An unreadable tree is a changing one: two failures in a row must not
    // compare equal and read as stable.
    let mut last = tree_fingerprint(dir).ok();
    while stable < quiet_for {
        if waited >= give_up {
            return false;
        }
        sleep(poll);
        waited += poll;
        let now = tree_fingerprint(dir).ok();
        if now.is_some() && now == last {
            stable += poll;
        } else {
            stable = Duration::ZERO;
            last = now;
        }
        if waited.as_secs().is_multiple_of(10) {
            progress(waited);
        }
    }
    true
}

// ---------------------------------------------------------- step reuse

/// The identity of one setup step's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepKey(pub String);

fn compile_inputs(patterns: &[String]) -> Result<globset::GlobSet, globset::Error> {
    let mut builder = globset::GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(
            globset::GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()?,
        );
    }
    builder.build()
}

/// sha256 of {the step's argv, the content of every TRACKED file matching
/// `reuse.inputs`, the toolchain of `argv[0]`, the keys of all earlier
/// setup steps}. Tracked files only: `**/package.json` would otherwise
/// match every package inside `node_modules`.
pub fn step_key(
    tree: &Path,
    step: &CommandSpec,
    reuse: &ReuseSpec,
    toolchain: &str,
    earlier: &[StepKey],
) -> io::Result<StepKey> {
    let matcher = compile_inputs(&reuse.inputs).map_err(io::Error::other)?;
    let mut files: Vec<String> = git_lines(tree, &["ls-files", "-z"])?
        .into_iter()
        .filter(|path| matcher.is_match(path))
        .collect();
    files.sort();
    let mut contents = BTreeMap::new();
    for file in files {
        let bytes = std::fs::read(tree.join(&file))?;
        contents.insert(file, sha256_hex(&bytes));
    }
    Ok(StepKey(canonical_json_hash(&serde_json::json!({
        "argv": step.argv,
        "inputs": contents,
        "toolchain": toolchain,
        "earlier": earlier.iter().map(|key| &key.0).collect::<Vec<_>>(),
    }))))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub key: StepKey,
    pub at_unix: u64,
}

/// What relais itself produced, per workspace and setup step, so a seeded
/// tree can be compared with its source. A hand-installed tree has no
/// stamp, and its steps simply run.
#[derive(Debug, Clone)]
pub struct StampStore {
    dir: PathBuf,
}

impl StampStore {
    pub fn new(state_dir: &Path) -> Self {
        Self {
            dir: state_dir.join("workspace-stamps"),
        }
    }

    fn file(&self, workspace: &Path) -> PathBuf {
        let canonical =
            std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
        let digest = sha256_hex(canonical.to_string_lossy().as_bytes());
        self.dir.join(format!("{digest}.json"))
    }

    fn read(&self, workspace: &Path) -> BTreeMap<usize, Stamp> {
        std::fs::read_to_string(self.file(workspace))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn get(&self, workspace: &Path, step: usize) -> Option<Stamp> {
        self.read(workspace).remove(&step)
    }

    pub fn put(&self, workspace: &Path, step: usize, key: StepKey, at_unix: u64) -> io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let mut all = self.read(workspace);
        all.insert(step, Stamp { key, at_unix });
        let text = serde_json::to_string(&all).map_err(io::Error::other)?;
        crate::fsutil::write_atomic(&self.file(workspace), &text)
    }
}

/// Why a step runs, or that it may be skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipDecision {
    Skip,
    Run(RunReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunReason {
    NoReuseDeclared,
    NoStampForSource,
    KeyDiffers,
    OutputMissing(String),
    VerifyFailed(String),
}

impl std::fmt::Display for RunReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoReuseDeclared => write!(f, "the step declares no reuse"),
            Self::NoStampForSource => write!(f, "the source workspace has no recorded result"),
            Self::KeyDiffers => write!(f, "the step's inputs differ from the source's"),
            Self::OutputMissing(path) => write!(f, "declared output {path} is missing or empty"),
            Self::VerifyFailed(detail) => write!(f, "the verify command failed: {detail}"),
        }
    }
}

fn non_empty(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => {
            std::fs::read_dir(path).is_ok_and(|mut it| it.next().is_some())
        }
        Ok(meta) => meta.len() > 0,
        Err(_) => false,
    }
}

/// May `step` be skipped in `tree`, a tree seeded from `source`? Only when
/// (a) it declares `reuse`, (b) `key` equals the key stored for the source,
/// (c) every declared output exists and is non-empty in `tree`, and (d)
/// `reuse.verify` exits 0 there. Anything else runs the step.
pub fn decide_skip(
    tree: &Path,
    source: &Path,
    index: usize,
    step: &CommandSpec,
    key: &StepKey,
    stamps: &StampStore,
    verify_timeout: Duration,
) -> SkipDecision {
    let Some(reuse) = &step.reuse else {
        return SkipDecision::Run(RunReason::NoReuseDeclared);
    };
    match stamps.get(source, index) {
        None => return SkipDecision::Run(RunReason::NoStampForSource),
        Some(stamp) if &stamp.key != key => return SkipDecision::Run(RunReason::KeyDiffers),
        Some(_) => {}
    }
    if let Some(missing) = reuse.outputs.iter().find(|out| !non_empty(&tree.join(out))) {
        return SkipDecision::Run(RunReason::OutputMissing(missing.clone()));
    }
    let Some((program, args)) = reuse.verify.split_first() else {
        return SkipDecision::Run(RunReason::VerifyFailed("no verify command".into()));
    };
    let mut command = Command::new(program);
    command.args(args).current_dir(tree);
    match run_with_timeout(command, verify_timeout, None, None) {
        Ok(end) if end.ended == Ended::Exited(0) => SkipDecision::Skip,
        Ok(end) => SkipDecision::Run(RunReason::VerifyFailed(end.ended.describe())),
        Err(e) => SkipDecision::Run(RunReason::VerifyFailed(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{temp_dir, TempDir};

    fn inputs(flag: Option<Requested>) -> ModeInputs<'static> {
        ModeInputs {
            flag,
            env_workspace: None,
            ci: None,
            machine: WorkspaceMode::Auto,
            repo_opts_out_of_in_place: false,
            linked_worktree: Some(false),
        }
    }

    #[test]
    fn auto_forks_a_primary_checkout_and_edits_a_linked_worktree_in_place() {
        assert_eq!(resolve(inputs(None)), Ok(Workspace::Fork));
        let linked = ModeInputs {
            linked_worktree: Some(true),
            ..inputs(None)
        };
        assert_eq!(resolve(linked), Ok(Workspace::InPlace));
        let unknown = ModeInputs {
            linked_worktree: None,
            ..inputs(None)
        };
        assert_eq!(resolve(unknown), Ok(Workspace::Fork));
    }

    #[test]
    fn explicit_in_place_is_refused_rather_than_downgraded() {
        assert_eq!(
            resolve(inputs(Some(Requested::InPlace))),
            Err(ModeRefusal::PrimaryCheckout)
        );
        let opted_out = ModeInputs {
            linked_worktree: Some(true),
            repo_opts_out_of_in_place: true,
            ..inputs(Some(Requested::InPlace))
        };
        assert_eq!(resolve(opted_out), Err(ModeRefusal::InPlaceOptedOut));
        let auto_opted_out = ModeInputs {
            flag: None,
            ..opted_out
        };
        assert_eq!(resolve(auto_opted_out), Ok(Workspace::Fork));
    }

    #[test]
    fn ci_and_the_environment_force_isolation_and_the_flag_cannot_widen_it() {
        let ci = ModeInputs {
            ci: Some("true"),
            ..inputs(None)
        };
        assert_eq!(resolve(ci), Ok(Workspace::Isolated));
        let widened = ModeInputs {
            ci: Some("1"),
            ..inputs(Some(Requested::Fork))
        };
        assert_eq!(
            resolve(widened),
            Err(ModeRefusal::WidenedPastTheEnvironment)
        );
        let narrowed = ModeInputs {
            ci: Some("1"),
            ..inputs(Some(Requested::Isolated))
        };
        assert_eq!(resolve(narrowed), Ok(Workspace::Isolated));
        for off in ["", "0", "false"] {
            assert_eq!(
                resolve(ModeInputs {
                    ci: Some(off),
                    ..inputs(None)
                }),
                Ok(Workspace::Fork)
            );
        }
        let env = ModeInputs {
            env_workspace: Some("isolated"),
            ..inputs(None)
        };
        assert_eq!(resolve(env), Ok(Workspace::Isolated));
    }

    #[test]
    fn the_machine_setting_isolates_and_the_flag_overrides_it() {
        let machine = ModeInputs {
            machine: WorkspaceMode::Isolated,
            ..inputs(None)
        };
        assert_eq!(resolve(machine), Ok(Workspace::Isolated));
        let flagged = ModeInputs {
            flag: Some(Requested::Fork),
            ..machine
        };
        assert_eq!(resolve(flagged), Ok(Workspace::Fork));
    }

    #[test]
    fn an_unknown_environment_value_is_a_usage_error() {
        let bad = ModeInputs {
            env_workspace: Some("everywhere"),
            ..inputs(None)
        };
        assert_eq!(resolve(bad), Err(ModeRefusal::Unknown("everywhere".into())));
        assert_eq!(Requested::parse("in-place"), Ok(Requested::InPlace));
    }

    fn repo_with_ignored_deps() -> TempDir {
        let root = temp_dir("reuse-deps");
        let git = |args: &[&str]| {
            let out = git_command(&root).args(args).output().expect("git");
            assert!(out.status.success(), "{args:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(root.join(".gitignore"), "node_modules/\ntarget/\n.venv/\n")
            .expect("ignore");
        std::fs::write(root.join("package.json"), "{}").expect("pkg");
        for dir in [
            "node_modules/a",
            "packages/web/node_modules/b",
            "target/debug",
            ".venv/lib",
            "src",
        ] {
            std::fs::create_dir_all(root.join(dir)).expect("mkdir");
            std::fs::write(root.join(dir).join("f"), "x").expect("file");
        }
        std::fs::write(root.join(".venv/pyvenv.cfg"), "home = /usr").expect("cfg");
        root
    }

    #[test]
    fn detection_finds_ignored_dependency_dirs_at_depth_and_never_an_environment() {
        let root = repo_with_ignored_deps();
        let found = detect_deps(&root, &[]);
        assert_eq!(
            found,
            vec![
                PathBuf::from("node_modules"),
                PathBuf::from("packages/web/node_modules"),
                PathBuf::from("target"),
            ]
        );
    }

    #[test]
    fn a_declared_dir_must_still_be_ignored_untracked_and_not_an_environment() {
        let root = repo_with_ignored_deps();
        let declared = vec![
            "src".to_string(),
            ".venv".to_string(),
            "node_modules".to_string(),
        ];
        assert_eq!(
            detect_deps(&root, &declared),
            vec![
                PathBuf::from("node_modules"),
                PathBuf::from("packages/web/node_modules")
            ],
            "a bare name matches at any depth; src is tracked and .venv is an environment"
        );
    }

    #[test]
    fn a_tree_untouched_for_the_window_needs_no_waiting() {
        let root = temp_dir("reuse-old");
        let file = root.join("a");
        std::fs::write(&file, "1").expect("file");
        let long_ago = std::time::SystemTime::now() - Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .expect("open")
            .set_modified(long_ago)
            .expect("mtime");
        // The directory's own mtime is not read; only entries are.
        let mut slept = Duration::ZERO;
        let quiet = wait_until_quiet(
            &root,
            Duration::from_secs(30),
            Duration::from_secs(10),
            Duration::from_secs(60),
            &mut |d| slept += d,
            &mut |_| {},
        );
        assert!(quiet);
        assert_eq!(slept, Duration::ZERO);
    }

    #[test]
    fn a_quiet_tree_returns_at_once_and_a_busy_one_gives_up() {
        let root = temp_dir("reuse-quiet");
        std::fs::write(root.join("a"), "1").expect("file");
        let mut slept = Duration::ZERO;
        let quiet = wait_until_quiet(
            &root,
            Duration::from_secs(30),
            Duration::from_secs(10),
            Duration::from_secs(60),
            &mut |d| slept += d,
            &mut |_| {},
        );
        assert!(quiet);
        assert_eq!(slept, Duration::from_secs(30));
        let mut n = 0u32;
        let busy_root = root.to_path_buf();
        let busy = wait_until_quiet(
            &root,
            Duration::from_secs(30),
            Duration::from_secs(10),
            Duration::from_secs(60),
            &mut |_| {
                n += 1;
                std::fs::write(busy_root.join(format!("f{n}")), "x").expect("write");
            },
            &mut |_| {},
        );
        assert!(!busy, "a tree that keeps changing is not quiet");
    }

    fn step_with_reuse(verify: &[&str]) -> CommandSpec {
        CommandSpec {
            name: None,
            argv: vec!["install".into()],
            timeout_seconds: 60,
            junit: None,
            reuse: Some(ReuseSpec {
                inputs: vec!["lock".into(), "**/package.json".into(), "patches/**".into()],
                outputs: vec!["node_modules".into()],
                verify: verify.iter().map(|s| s.to_string()).collect(),
            }),
        }
    }

    fn tracked_tree() -> TempDir {
        let root = temp_dir("reuse-key");
        let git = |args: &[&str]| {
            let out = git_command(&root).args(args).output().expect("git");
            assert!(out.status.success(), "{args:?}");
        };
        git(&["init", "-q"]);
        std::fs::create_dir_all(root.join("patches")).expect("mkdir");
        std::fs::write(root.join("lock"), "v1").expect("lock");
        std::fs::write(root.join("package.json"), "{}").expect("pkg");
        std::fs::write(root.join("patches/p.patch"), "p1").expect("patch");
        git(&["add", "-A"]);
        root
    }

    #[test]
    fn the_key_moves_with_every_input_the_result_depends_on() {
        let root = tracked_tree();
        let step = step_with_reuse(&["true"]);
        let reuse = step.reuse.clone().expect("reuse");
        let key = |toolchain: &str, earlier: &[StepKey]| {
            step_key(&root, &step, &reuse, toolchain, earlier).expect("key")
        };
        let base = key("yarn 1.22", &[]);
        assert_eq!(base, key("yarn 1.22", &[]), "deterministic");
        assert_ne!(base, key("yarn 1.23", &[]), "toolchain");
        assert_ne!(
            base,
            key("yarn 1.22", &[StepKey("earlier".into())]),
            "an earlier step"
        );
        for (file, content) in [
            ("lock", "v2"),
            ("package.json", "{\"a\":1}"),
            ("patches/p.patch", "p2"),
        ] {
            let before = key("yarn 1.22", &[]);
            std::fs::write(root.join(file), content).expect("edit");
            assert_ne!(before, key("yarn 1.22", &[]), "{file}");
        }
        let mut other = step.clone();
        other.argv = vec!["install".into(), "--frozen".into()];
        assert_ne!(
            key("yarn 1.22", &[]),
            step_key(&root, &other, &reuse, "yarn 1.22", &[]).expect("key"),
            "argv"
        );
    }

    #[test]
    fn untracked_files_never_enter_the_key_so_node_modules_cannot_swamp_it() {
        let root = tracked_tree();
        let step = step_with_reuse(&["true"]);
        let reuse = step.reuse.clone().expect("reuse");
        let before = step_key(&root, &step, &reuse, "t", &[]).expect("key");
        std::fs::create_dir_all(root.join("node_modules/dep")).expect("mkdir");
        std::fs::write(root.join("node_modules/dep/package.json"), "{\"x\":1}").expect("file");
        assert_eq!(
            before,
            step_key(&root, &step, &reuse, "t", &[]).expect("key")
        );
    }

    #[test]
    fn a_step_is_skipped_only_when_stamp_outputs_and_verify_all_hold() {
        let source = tracked_tree();
        let tree = tracked_tree();
        let state = temp_dir("reuse-state");
        let stamps = StampStore::new(&state);
        let step = step_with_reuse(&["true"]);
        let key = StepKey("k".into());
        let decide = |step: &CommandSpec, key: &StepKey| {
            decide_skip(
                &tree,
                &source,
                0,
                step,
                key,
                &stamps,
                Duration::from_secs(20),
            )
        };
        let plain = CommandSpec {
            reuse: None,
            ..step.clone()
        };
        assert_eq!(
            decide(&plain, &key),
            SkipDecision::Run(RunReason::NoReuseDeclared)
        );
        assert_eq!(
            decide(&step, &key),
            SkipDecision::Run(RunReason::NoStampForSource)
        );
        stamps
            .put(&source, 0, StepKey("other".into()), 1)
            .expect("stamp");
        assert_eq!(
            decide(&step, &key),
            SkipDecision::Run(RunReason::KeyDiffers)
        );
        stamps.put(&source, 0, key.clone(), 1).expect("stamp");
        assert_eq!(
            decide(&step, &key),
            SkipDecision::Run(RunReason::OutputMissing("node_modules".into()))
        );
        std::fs::create_dir_all(tree.join("node_modules")).expect("mkdir");
        assert_eq!(
            decide(&step, &key),
            SkipDecision::Run(RunReason::OutputMissing("node_modules".into())),
            "an empty output is missing"
        );
        std::fs::write(tree.join("node_modules/x"), "x").expect("file");
        assert_eq!(decide(&step, &key), SkipDecision::Skip);
        let failing = step_with_reuse(&["false"]);
        assert!(matches!(
            decide(&failing, &key),
            SkipDecision::Run(RunReason::VerifyFailed(_))
        ));
    }

    /// What seeding costs on a real tree: one survey (symlink check and
    /// count), one fingerprint walk, one more after. Read-only:
    /// `RELAIS_PROBE_DIR=<repo>/node_modules cargo test -- --ignored a_real_tree_costs`.
    #[test]
    #[ignore = "reads RELAIS_PROBE_DIR and prints timings; not a pass/fail check"]
    fn a_real_tree_costs() {
        let Ok(dir) = std::env::var("RELAIS_PROBE_DIR") else {
            return;
        };
        let dir = PathBuf::from(dir);
        let root = dir
            .parent()
            .expect("a directory below a repository root")
            .to_path_buf();
        let started = std::time::Instant::now();
        let stats = crate::fsclone::survey(&root, &dir).expect("survey");
        println!("survey: {:?} in {:?}", stats, started.elapsed());
        let started = std::time::Instant::now();
        let fingerprint = tree_fingerprint(&dir).expect("fingerprint");
        println!("fingerprint {fingerprint} in {:?}", started.elapsed());
    }

    #[test]
    fn a_seed_clones_what_it_can_and_never_copies_bytes_when_it_cannot() {
        let source = repo_with_ignored_deps();
        let dest = temp_dir("reuse-seed-dest");
        let deps = detect_deps(&source, &[]);
        let report = seed(&source, &dest, &deps);
        if let Some(why) = &report.unavailable {
            assert!(report.seeded.is_empty(), "{why}");
            assert!(
                !dest.join("node_modules").exists(),
                "an unavailable seed copies nothing"
            );
        } else {
            assert_eq!(report.seeded.len(), deps.len());
            assert!(dest.join("node_modules/a/f").exists());
            assert!(dest.join("packages/web/node_modules/b/f").exists());
        }
    }
}
