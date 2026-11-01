//! Holdouts the store declares for themselves (SPEC §4).
//!
//! At preflight relais asks the store for one commit, fetches that commit
//! into a cache keyed by its sha, and reads `<repo>/<name>/holdout.toml`
//! (every key optional) for each holdout directory of the repository. What
//! it finds is merged with machine.toml's declarations by
//! [`crate::policy::HoldoutSettings::with_discovered`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Deserialize;

use super::{clean_relative, fetch_tree, ls_remote};
use crate::policy::{
    CommandSpec, HoldoutAttach, HoldoutDecl, HoldoutSettings, DEFAULT_HOLDOUT_TIMEOUT_SECONDS,
};

/// The script of a holdout whose `holdout.toml` names no `path`.
const DEFAULT_SCRIPT: &str = "check.sh";

/// How many store commits the cache keeps.
const KEPT_COMMITS: usize = 5;

/// A `holdout.toml` the run is refused for: the file, and what is wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarationInvalid {
    /// The file inside the store, `<repo>/<name>/holdout.toml`.
    pub file: String,
    pub cause: String,
}

impl std::fmt::Display for DeclarationInvalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the holdout declaration {} in the store is invalid: {}",
            self.file, self.cause
        )
    }
}

/// What preflight learned from the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreView {
    /// The one commit the store's ref named, or why it could not be had.
    pub head: Result<String, String>,
    /// The holdouts the store declares for this repository, by name.
    pub declared: BTreeMap<String, HoldoutDecl>,
    /// The [`super::fingerprint`] of each declared holdout, by name.
    pub fingerprints: BTreeMap<String, String>,
    /// Why the store could not be read: its commit could not be had, or its
    /// files could not be fetched within the bound. When set, `head` is an
    /// error too and `declared` is empty.
    pub unread: Option<String>,
}

impl StoreView {
    fn without_files(head: Result<String, String>, unread: Option<String>) -> Self {
        Self {
            head,
            declared: BTreeMap::new(),
            fingerprints: BTreeMap::new(),
            unread,
        }
    }
}

/// Ask the store for its commit once, fetch it into
/// `<cache_root>/<sha>` and read the holdouts it declares for `repo`. The
/// whole of it, `ls-remote` included, is bounded by the settings'
/// `discovery_bound` (30 s unless a test injects another).
/// `run_id` names this fetch's own partial directory. A store that cannot
/// be asked or fetched declares nothing here; the view says why.
pub fn discover(
    settings: &HoldoutSettings,
    repo: &str,
    cache_root: &Path,
    run_id: &str,
) -> Result<StoreView, DeclarationInvalid> {
    let started = Instant::now();
    let store = settings.store.clone().unwrap_or_default();
    if store.is_empty() {
        return Ok(StoreView::without_files(
            Err("no `store` is set in [holdouts]".to_string()),
            None,
        ));
    }
    let bound = settings.discovery_bound;
    let head = ls_remote(&store, &settings.git_ref, bound);
    let sha = match &head {
        Ok(sha) if !repo.is_empty() => sha.clone(),
        Ok(_) => return Ok(StoreView::without_files(head, None)),
        Err(cause) => {
            let unread = Some(cause.clone());
            return Ok(StoreView::without_files(head, unread));
        }
    };
    let left = || bound.saturating_sub(started.elapsed());
    let target = cache_root.join(&sha);
    let partial = cache_root.join(format!("{sha}.partial.{run_id}"));
    let tree = match fetch_tree(&store, &settings.git_ref, &sha, &target, &partial, &left) {
        Ok(tree) => tree,
        Err(cause) => return Ok(StoreView::without_files(Err(cause.clone()), Some(cause))),
    };
    prune(cache_root, &sha);
    let declared = read_declarations(&tree, repo)?;
    let mut fingerprints = BTreeMap::new();
    for (name, decl) in &declared {
        let fingerprint =
            super::fingerprint(decl, &tree).map_err(|refused| DeclarationInvalid {
                file: format!("{repo}/{name}/holdout.toml"),
                cause: format!("its supplied contents were refused: {refused}"),
            })?;
        fingerprints.insert(name.clone(), fingerprint);
    }
    Ok(StoreView {
        head,
        declared,
        fingerprints,
        unread: None,
    })
}

/// Keep `keep` and the most recently written other trees up to
/// [`KEPT_COMMITS`]; remove the rest. A tree still being built (its name
/// holds `.partial.`) belongs to another preflight and is left alone.
fn prune(cache_root: &Path, keep: &str) {
    let entries = match std::fs::read_dir(cache_root) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!(
                "relais: the holdout cache {} could not be read: {e}",
                cache_root.display()
            );
            return;
        }
    };
    let mut others: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name != keep && !name.contains(".partial.")
        })
        .filter_map(|entry| {
            let written = entry.metadata().and_then(|meta| meta.modified()).ok()?;
            Some((written, entry.path()))
        })
        .collect();
    others.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in others.into_iter().skip(KEPT_COMMITS - 1) {
        if let Err(e) = std::fs::remove_dir_all(&path) {
            eprintln!(
                "relais: the cached store {} could not be removed: {e}",
                path.display()
            );
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawStoreDeclaration {
    path: Option<String>,
    overlay: Option<String>,
    command: Option<RawStoreCommand>,
    timeout_seconds: Option<u64>,
    config: Option<String>,
    #[serde(default)]
    inputs: Vec<String>,
    attach: Option<RawStoreAttach>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawStoreCommand {
    argv: Option<Vec<String>>,
    junit: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStoreAttach {
    task_types: Vec<String>,
    paths: Vec<String>,
}

/// Every holdout directory `<repo>/<name>/` of the fetched store: one with a
/// `holdout.toml` or with the default script.
fn read_declarations(
    tree: &Path,
    repo: &str,
) -> Result<BTreeMap<String, HoldoutDecl>, DeclarationInvalid> {
    let repo_dir = tree.join(repo);
    if std::fs::symlink_metadata(&repo_dir).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(DeclarationInvalid {
            file: repo.to_string(),
            cause: "the repository directory is a symlink".to_string(),
        });
    }
    let symlinked = symlinked_entries(&repo_dir);
    if let Some(name) = symlinked.first() {
        return Err(DeclarationInvalid {
            file: format!("{repo}/{name}"),
            cause: "it is a symlink".to_string(),
        });
    }
    let mut names: Vec<String> = match std::fs::read_dir(&repo_dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .filter(|name| !name.starts_with('.'))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            return Err(DeclarationInvalid {
                file: repo.to_string(),
                cause: format!("the directory could not be read: {e}"),
            })
        }
    };
    names.sort();
    let mut declared = BTreeMap::new();
    for name in names {
        if let Some(decl) = read_declaration(&repo_dir.join(&name), repo, &name)? {
            declared.insert(name, decl);
        }
    }
    Ok(declared)
}

/// One holdout directory: its declaration with every default applied, or
/// `None` when it holds neither a `holdout.toml` nor the default script.
fn read_declaration(
    dir: &Path,
    repo: &str,
    name: &str,
) -> Result<Option<HoldoutDecl>, DeclarationInvalid> {
    let file = format!("{repo}/{name}/holdout.toml");
    let invalid = |cause: String| DeclarationInvalid {
        file: file.clone(),
        cause,
    };
    let toml_path = dir.join("holdout.toml");
    let raw = match std::fs::symlink_metadata(&toml_path) {
        Ok(meta) if meta.file_type().is_file() => {
            let text = std::fs::read_to_string(&toml_path)
                .map_err(|e| invalid(format!("it could not be read: {e}")))?;
            toml::from_str::<RawStoreDeclaration>(&text).map_err(|e| invalid(e.to_string()))?
        }
        Ok(_) => return Err(invalid("it is not a regular file".to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if std::fs::symlink_metadata(dir.join(DEFAULT_SCRIPT)).is_err() {
                return Ok(None);
            }
            RawStoreDeclaration::default()
        }
        Err(e) => return Err(invalid(format!("it could not be read: {e}"))),
    };
    let clean = |relative: &str, what: &str| -> Result<(), DeclarationInvalid> {
        if clean_relative(relative) {
            return Ok(());
        }
        Err(invalid(format!(
            "{what} `{relative}` is not a clean relative path (no leading `/`, no `.` or `..` \
             segment)"
        )))
    };
    let present = |relative: &str, what: &str| -> Result<String, DeclarationInvalid> {
        clean(relative, what)?;
        match std::fs::symlink_metadata(dir.join(relative)) {
            Ok(meta) if !meta.file_type().is_symlink() => Ok(format!("{repo}/{name}/{relative}")),
            Ok(_) => Err(invalid(format!("{what} `{relative}` is a symlink"))),
            Err(e) => Err(invalid(format!(
                "{what} `{relative}` could not be read: {e}"
            ))),
        }
    };
    let path = present(raw.path.as_deref().unwrap_or(DEFAULT_SCRIPT), "path")?;
    let config = raw
        .config
        .as_deref()
        .map(|config| present(config, "config"))
        .transpose()?;
    let overlay = match raw.overlay {
        Some(overlay) => {
            clean(&overlay, "overlay")?;
            overlay
        }
        None => format!(".relais-holdout/{name}.sh"),
    };
    let command = raw.command.unwrap_or_default();
    let attach = raw
        .attach
        .map(|attach| HoldoutAttach::parse("attach", attach.task_types, attach.paths))
        .transpose()
        .map_err(invalid)?;
    Ok(Some(HoldoutDecl {
        path,
        command: CommandSpec {
            name: None,
            argv: command
                .argv
                .unwrap_or_else(|| vec!["sh".to_string(), overlay.clone()]),
            timeout_seconds: raw
                .timeout_seconds
                .unwrap_or(DEFAULT_HOLDOUT_TIMEOUT_SECONDS),
            junit: Some(
                command
                    .junit
                    .unwrap_or_else(|| format!("target/holdout/{name}-junit.xml")),
            ),
            reuse: None,
            weight: None,
            retry: None,
            env: command.env,
        },
        overlay,
        config,
        inputs: raw.inputs,
        attach,
    }))
}

/// The visible entries of `dir` that are symlinks, by name; an unreadable
/// directory has none here (the listing that follows reports it).
fn symlinked_entries(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_symlink()))
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    names
}
