//! Machine-local directory conventions.
//!
//! Settings live under `~/.config/relais/`, mutable state (ledger, registry,
//! run artifacts) under `~/.local/state/relais/`. Constructed from the home
//! directory rather than a platform abstraction, so the paths are the same
//! everywhere and predictable in reports. Constructors that consume these
//! take explicit paths instead, which is what keeps tests isolated.
//!
//! `RELAIS_CONFIG_DIR` and `RELAIS_STATE_DIR` relocate both, which moves
//! the machine authority (trust grants, spending ceilings, permissions)
//! and the ledger with them. That is deliberate — the release scenarios
//! drive a whole isolated machine through them, and the worker's
//! environment is stripped of both so a nested `relais` cannot inherit a
//! relocated authority — but it is also invisible, so `doctor` names the
//! directories in effect and flags the ones the environment supplied.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The environment variables that relocate the two directories. Named
/// here so `doctor` and the paths themselves cannot disagree about the
/// spelling.
pub const CONFIG_DIR_ENV: &str = "RELAIS_CONFIG_DIR";
pub const STATE_DIR_ENV: &str = "RELAIS_STATE_DIR";

/// Neither `HOME` nor `USERPROFILE` is set, so there is no directory to
/// build the defaults from. An error, never a panic: the CLI prints it
/// and exits, and `doctor` reports it as a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HomeUnset;

impl std::fmt::Display for HomeUnset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "HOME is not set (nor USERPROFILE); relais keeps its settings under \
             $HOME/.config/relais and its state under $HOME/.local/state/relais. \
             Set HOME, or point RELAIS_CONFIG_DIR and RELAIS_STATE_DIR at explicit \
             directories",
        )
    }
}

impl std::error::Error for HomeUnset {}

/// Resolve the home directory from a variable lookup. Taking the lookup
/// as an argument keeps the unset case testable: `std::env::set_var` is
/// process-global and racy under a threaded test runner.
fn resolve_home(var: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, HomeUnset> {
    var("HOME")
        .or_else(|| var("USERPROFILE"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or(HomeUnset)
}

/// The home directory, or the reason there is none. Fallible all the way
/// up: a library that exits the process leaves its caller no way to
/// report the cause, and `doctor` has to be able to print this as a
/// finding rather than die printing it (C10).
pub fn home_dir() -> Result<PathBuf, HomeUnset> {
    resolve_home(|name| std::env::var_os(name))
}

/// The directory an environment variable relocates, when it does. An
/// empty value is no value: it would otherwise resolve to the process's
/// working directory, which is a repository checkout, not machine state.
pub fn dir_override(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Where machine-owned settings live: the override when one is set, and
/// otherwise a directory under `$HOME`, which is why this is fallible.
pub fn config_dir() -> Result<PathBuf, HomeUnset> {
    match dir_override(CONFIG_DIR_ENV) {
        Some(dir) => Ok(dir),
        None => Ok(home_dir()?.join(".config").join("relais")),
    }
}

/// The machine authority: trust grants, spending ceilings, permissions.
pub fn machine_settings_path() -> Result<PathBuf, HomeUnset> {
    Ok(config_dir()?.join("machine.toml"))
}

/// Where mutable state lives: the ledger, the registry, run artifacts.
pub fn state_dir() -> Result<PathBuf, HomeUnset> {
    match dir_override(STATE_DIR_ENV) {
        Some(dir) => Ok(dir),
        None => Ok(home_dir()?.join(".local").join("state").join("relais")),
    }
}

/// The SQLite ledger (SPEC §12).
pub fn ledger_path() -> Result<PathBuf, HomeUnset> {
    Ok(state_dir()?.join("ledger.sqlite"))
}

/// The learned-artifact registry (SPEC §17).
pub fn registry_dir() -> Result<PathBuf, HomeUnset> {
    Ok(state_dir()?.join("registry"))
}

/// The names the state directory's layout is spelled with, so the runner
/// that writes it and the sweep that reads it back (`resume --retire`,
/// `doctor`) cannot disagree.
///
/// `runs/<run>/` holds a run's artifacts; `worktrees/<run>/<name>/` its
/// git worktrees (`task`, `integration`), a SIBLING of the artifacts so
/// a worker cannot reach the run's record by a relative path. Releases
/// before that split kept the worktree at `runs/<run>/worktree/`, and a
/// state directory may still hold some.
pub const RUNS_DIR: &str = "runs";
pub const WORKTREES_DIR: &str = "worktrees";
pub const LEGACY_WORKTREE_DIR: &str = "worktree";

/// One directory per run: receipts, patches, evidence.
pub fn runs_dir() -> Result<PathBuf, HomeUnset> {
    Ok(state_dir()?.join(RUNS_DIR))
}

/// Where `relais hook` journals every firing it answers, one JSON line
/// per firing (SPEC §23). A payload carries a transcript path and a
/// working directory that name a person's machine, so this file is
/// created owner-only and never shared with the socket or settings
/// files above.
pub fn hook_journal_path() -> Result<PathBuf, HomeUnset> {
    Ok(state_dir()?.join("hook_journal.jsonl"))
}

/// Claude Code's own config directory relocator (SPEC §11): a session's
/// transcripts live under it, at `<dir>/projects/<slug>/<session>.jsonl`.
/// Named for Claude Code's own variable, not relais's — this is the
/// other CLI's directory, not this one's.
pub const CLAUDE_CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

/// Where Claude Code writes session transcripts: `$CLAUDE_CONFIG_DIR` when
/// set, otherwise `$HOME/.claude`.
pub fn claude_projects_dir() -> Result<PathBuf, HomeUnset> {
    match dir_override(CLAUDE_CONFIG_DIR_ENV) {
        Some(dir) => Ok(dir.join("projects")),
        None => Ok(home_dir()?.join(".claude").join("projects")),
    }
}

/// A session's main transcript, found by searching the projects
/// directory for `<session_id>.jsonl` — the session id, not its project
/// slug, is what a run's `root_session` records, so the slug's own
/// directory name is not knowable ahead of the search. `None` when no
/// such file exists anywhere under `projects_dir`, which is a fact
/// ("transcript missing"), not an error.
pub fn find_transcript(projects_dir: &Path, session_id: &str) -> Option<PathBuf> {
    let target = format!("{session_id}.jsonl");
    let mut stack = vec![projects_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|name| name.to_str()) == Some(target.as_str()) {
                return Some(path);
            }
        }
    }
    None
}

/// A session's subagent transcripts: sibling files of its main
/// transcript, at `<slug>/<session>/subagents/agent-*.jsonl`. A subagent
/// turn is never a sidechain line inside the main file — it lives only
/// here — so importing a session's usage means reading every file this
/// returns too. A missing `subagents` directory (a session that spawned
/// no subagents) is not an error: it reads back as no subagent files,
/// the same as an empty one.
pub fn subagent_transcripts(main_transcript: &Path) -> Vec<PathBuf> {
    let (Some(stem), Some(parent)) = (
        main_transcript.file_stem().and_then(|s| s.to_str()),
        main_transcript.parent(),
    ) else {
        return Vec::new();
    };
    let subagents_dir = parent.join(stem).join("subagents");
    let Ok(entries) = std::fs::read_dir(&subagents_dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("agent-") && name.ends_with(".jsonl"))
        })
        .collect();
    files.sort();
    files
}

/// The agent id a subagent transcript belongs to: the `<id>` of
/// `agent-<id>.jsonl`. `None` for any other file name, a session's main
/// transcript included.
pub fn subagent_agent_id(transcript: &Path) -> Option<&str> {
    transcript
        .file_name()?
        .to_str()?
        .strip_prefix("agent-")?
        .strip_suffix(".jsonl")
        .filter(|id| !id.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(*value))
        }
    }

    #[test]
    fn home_comes_from_home_then_userprofile() {
        assert_eq!(
            resolve_home(lookup(&[("HOME", "/h"), ("USERPROFILE", "C:\\u")])),
            Ok(PathBuf::from("/h"))
        );
        // Windows spells it USERPROFILE; the crate builds there too.
        assert_eq!(
            resolve_home(lookup(&[("USERPROFILE", "C:\\u")])),
            Ok(PathBuf::from("C:\\u"))
        );
    }

    #[test]
    fn an_unset_home_is_an_error_not_a_panic() {
        let err = resolve_home(lookup(&[])).expect_err("no home");
        assert_eq!(err, HomeUnset);
        let rendered = err.to_string();
        assert!(rendered.contains("HOME is not set"), "{rendered}");
        // The message says what to do instead, because the two overrides
        // are the way out for a machine without a home directory.
        assert!(rendered.contains(CONFIG_DIR_ENV), "{rendered}");
        assert!(rendered.contains(STATE_DIR_ENV), "{rendered}");
    }

    #[test]
    fn an_empty_home_is_no_home() {
        assert_eq!(resolve_home(lookup(&[("HOME", "")])), Err(HomeUnset));
    }

    fn scratch(label: &str) -> crate::test_support::TempDir {
        crate::test_support::temp_dir(&format!("paths-{label}"))
    }

    #[test]
    fn find_transcript_searches_every_project_slug() {
        let dir = scratch("find-transcript");
        let slug_dir = dir.join("-some-project-slug");
        std::fs::create_dir_all(&slug_dir).expect("slug dir");
        let transcript = slug_dir.join("sess-1.jsonl");
        std::fs::write(&transcript, "{}\n").expect("transcript");
        assert_eq!(find_transcript(&dir, "sess-1"), Some(transcript));
        assert_eq!(find_transcript(&dir, "sess-missing"), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn subagent_transcripts_are_found_beside_the_main_file() {
        let dir = scratch("subagents");
        let slug_dir = dir.join("-some-project-slug");
        let subagents_dir = slug_dir.join("sess-1").join("subagents");
        std::fs::create_dir_all(&subagents_dir).expect("subagents dir");
        let main = slug_dir.join("sess-1.jsonl");
        std::fs::write(&main, "{}\n").expect("main transcript");
        let agent_one = subagents_dir.join("agent-1.jsonl");
        let agent_two = subagents_dir.join("agent-2.jsonl");
        std::fs::write(&agent_one, "{}\n").expect("agent 1");
        std::fs::write(&agent_two, "{}\n").expect("agent 2");
        // Not a subagent transcript: shares the directory but not the
        // naming convention import relies on.
        std::fs::write(subagents_dir.join("notes.txt"), "").expect("decoy");
        assert_eq!(subagent_transcripts(&main), vec![agent_one, agent_two]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_subagent_transcript_names_its_agent() {
        let agent = |path: &'static str| subagent_agent_id(Path::new(path));
        assert_eq!(
            agent("/p/-slug/s1/subagents/agent-a1b2.jsonl"),
            Some("a1b2")
        );
        assert_eq!(agent("/p/-slug/s1.jsonl"), None);
        assert_eq!(agent("/p/-slug/s1/subagents/agent-.jsonl"), None);
        assert_eq!(agent("/p/-slug/s1/subagents/notes.txt"), None);
    }

    #[test]
    fn a_missing_subagents_directory_is_not_an_error() {
        let dir = scratch("no-subagents");
        let slug_dir = dir.join("-some-project-slug");
        std::fs::create_dir_all(&slug_dir).expect("slug dir");
        let main = slug_dir.join("sess-1.jsonl");
        std::fs::write(&main, "{}\n").expect("main transcript");
        assert_eq!(subagent_transcripts(&main), Vec::<PathBuf>::new());
        std::fs::remove_dir_all(&dir).ok();
    }
}
