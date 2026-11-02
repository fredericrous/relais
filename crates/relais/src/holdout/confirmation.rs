//! Confirm on change (SPEC §4): what the person reviews, what a
//! confirmation retains, and the refusal of a review that went stale.
//!
//! Nothing here touches the ledger or the network: the caller hands in the
//! confirmations it read and the [`StoreView`] it discovered, and gets back
//! values to print or to record.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::fingerprint::{fingerprint, supplied_contents, SuppliedFile, SuppliedRefusal};
use super::StoreView;
use crate::ids::to_hex;
use crate::policy::{DeclSource, HoldoutDecl, HoldoutSettings};

/// The more lines a diff would compare (old times new), the less it is worth
/// aligning them: past this the whole file is shown replaced.
// holds-until: scripts of a few hundred lines — a linear-space diff after
const DIFF_CELL_LIMIT: usize = 4_000_000;

/// One row of the ledger's `holdout_confirmations`: what the person
/// confirmed for a store holdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoldoutConfirmation {
    pub fingerprint: String,
    pub store_sha: String,
    /// The [`Snapshot`] as JSON.
    pub snapshot_json: String,
    pub confirmed_at: String,
}

/// One supplied file kept in a snapshot. The bytes are hex so any content
/// survives the JSON column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotFile {
    pub role: String,
    pub root: String,
    pub relative: String,
    pub mode: u32,
    pub content_hex: String,
}

impl SnapshotFile {
    fn from_supplied(file: &SuppliedFile) -> Self {
        Self {
            role: file.role.to_string(),
            root: file.root.clone(),
            relative: file.relative.clone(),
            mode: file.mode,
            content_hex: to_hex(&file.bytes),
        }
    }

    /// Where the file is in the store.
    pub fn shown_path(&self) -> String {
        if self.relative.is_empty() {
            self.root.clone()
        } else {
            format!("{}/{}", self.root, self.relative)
        }
    }

    fn bytes(&self) -> Vec<u8> {
        (0..self.content_hex.len() / 2)
            .filter_map(|at| u8::from_str_radix(self.content_hex.get(at * 2..at * 2 + 2)?, 16).ok())
            .collect()
    }
}

/// What a confirmation retains: the reviewed declaration and the contents
/// it supplies, at the store commit they were reviewed at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub store_sha: String,
    pub fingerprint: String,
    pub decl: HoldoutDecl,
    pub files: Vec<SnapshotFile>,
}

/// Where a store holdout stands against the confirmations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationState {
    /// Never confirmed.
    New,
    /// Confirmed once; the declaration or its supplied contents differ now.
    Changed,
    /// The confirmed fingerprint is the current one.
    Confirmed,
    /// A machine.toml table of the same name replaces the store's.
    Shadowed,
}

impl ConfirmationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Changed => "changed",
            Self::Confirmed => "confirmed",
            Self::Shadowed => "shadowed",
        }
    }
}

/// Where a store holdout stands: never confirmed, confirmed at another
/// fingerprint, or confirmed at `current`.
pub fn state_of(
    confirmation: Option<&HoldoutConfirmation>,
    current: Option<&str>,
) -> ConfirmationState {
    match confirmation {
        None => ConfirmationState::New,
        Some(_) if is_confirmed(confirmation, current) => ConfirmationState::Confirmed,
        Some(_) => ConfirmationState::Changed,
    }
}

/// Whether `fingerprint` is the one the person confirmed. A holdout frozen
/// without a fingerprint was never fingerprinted, so it is not confirmed.
pub fn is_confirmed(confirmation: Option<&HoldoutConfirmation>, fingerprint: Option<&str>) -> bool {
    match (confirmation, fingerprint) {
        (Some(confirmed), Some(current)) => confirmed.fingerprint == current,
        _ => false,
    }
}

/// One holdout discovered for the repository, as `relais doctor` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListedHoldout {
    pub name: String,
    /// `store` or `machine`: whose declaration is in force.
    pub source: &'static str,
    /// Where it attaches to a task that does not name it.
    pub attach: String,
    pub state: ConfirmationState,
}

/// The attach rule of a declaration, in words.
pub fn attach_text(decl: &HoldoutDecl) -> String {
    match &decl.attach {
        None => "none (named by a contract only)".to_string(),
        Some(attach) => format!(
            "task types {}; paths {}",
            attach
                .task_types
                .iter()
                .map(|kind| kind.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            attach.paths.join(", ")
        ),
    }
}

/// Every holdout discovered for `repo`: the effective declarations (store and
/// machine.toml merged as `HoldoutSettings::with_discovered` does), then the
/// store's declarations a machine.toml table turned off. `machine` is the
/// settings before the merge, `confirmed` the confirmations by name.
pub fn listing(
    machine: &HoldoutSettings,
    repo: &str,
    view: &StoreView,
    confirmed: &BTreeMap<String, HoldoutConfirmation>,
) -> Vec<ListedHoldout> {
    let effective = machine.with_discovered(repo, view.declared.clone());
    let mut listed: BTreeMap<String, ListedHoldout> = BTreeMap::new();
    for name in effective.declared_names(repo) {
        let Some(decl) = effective.declaration(repo, &name) else {
            continue;
        };
        let (source, state) = match effective.source_of(repo, &name) {
            DeclSource::Store => (
                "store",
                state_of(
                    confirmed.get(&name),
                    view.fingerprints.get(&name).map(String::as_str),
                ),
            ),
            DeclSource::Machine if view.declared.contains_key(&name) => {
                ("machine", ConfirmationState::Shadowed)
            }
            // The person's own text needs no confirmation (SPEC §4).
            DeclSource::Machine => ("machine", ConfirmationState::Confirmed),
        };
        listed.insert(
            name.clone(),
            ListedHoldout {
                name,
                source,
                attach: attach_text(decl),
                state,
            },
        );
    }
    for (name, decl) in &view.declared {
        listed.entry(name.clone()).or_insert_with(|| ListedHoldout {
            name: name.clone(),
            source: "store",
            attach: attach_text(decl),
            state: ConfirmationState::Shadowed,
        });
    }
    listed.into_values().collect()
}

/// How one supplied file, or the declaration itself, differs from the
/// confirmed snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileDiff {
    pub path: String,
    /// `added`, `removed` or `modified`.
    pub change: &'static str,
    /// Changed lines, `-` before and `+` after; one sentence for a binary
    /// file.
    pub text: String,
}

/// The shown path of the declaration in a review's diffs.
pub const DECLARATION_PATH: &str = "holdout declaration";

/// One store holdout as the person reviews it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HoldoutReview {
    pub name: String,
    pub state: ConfirmationState,
    /// The store commit the review was read at, for `--sha`.
    pub store_sha: Option<String>,
    /// For `--fingerprint`.
    pub fingerprint: Option<String>,
    /// The complete effective declaration; none when machine.toml's table
    /// replaces the store's.
    pub declaration: Option<HoldoutDecl>,
    /// Every difference from the confirmed snapshot; against nothing when
    /// the holdout is new.
    pub diffs: Vec<FileDiff>,
}

/// A review that could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewFailed {
    pub name: String,
    pub cause: String,
}

impl std::fmt::Display for ReviewFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "holdout `{}`: {}", self.name, self.cause)
    }
}

/// The review of every holdout the store declares for the repository.
/// `shadowed` holds the names a machine.toml table replaces or turns off.
pub fn review(
    view: &StoreView,
    shadowed: &BTreeSet<String>,
    confirmed: &BTreeMap<String, HoldoutConfirmation>,
) -> Result<Vec<HoldoutReview>, ReviewFailed> {
    let mut reviews = Vec::new();
    for (name, decl) in &view.declared {
        if shadowed.contains(name) {
            reviews.push(HoldoutReview {
                name: name.clone(),
                state: ConfirmationState::Shadowed,
                store_sha: None,
                fingerprint: None,
                declaration: None,
                diffs: Vec::new(),
            });
            continue;
        }
        let failed = |cause: String| ReviewFailed {
            name: name.clone(),
            cause,
        };
        let current = view.fingerprints.get(name).cloned();
        let previous = confirmed.get(name);
        let state = state_of(previous, current.as_deref());
        let diffs = match (&view.tree, state) {
            (_, ConfirmationState::Confirmed) => Vec::new(),
            (Some(tree), _) => {
                let files = supplied_contents(decl, tree).map_err(|e| failed(e.to_string()))?;
                let before = match previous {
                    Some(held) => Some(
                        serde_json::from_str::<Snapshot>(&held.snapshot_json).map_err(|e| {
                            failed(format!("its retained snapshot is unreadable: {e}"))
                        })?,
                    ),
                    None => None,
                };
                diffs_against(before.as_ref(), decl, &files)
            }
            (None, _) => Vec::new(),
        };
        reviews.push(HoldoutReview {
            name: name.clone(),
            state,
            store_sha: view.head.as_ref().ok().cloned(),
            fingerprint: current,
            declaration: Some(decl.clone()),
            diffs,
        });
    }
    Ok(reviews)
}

fn diffs_against(
    before: Option<&Snapshot>,
    decl: &HoldoutDecl,
    files: &[SuppliedFile],
) -> Vec<FileDiff> {
    let mut diffs = Vec::new();
    let pretty = |decl: &HoldoutDecl| serde_json::to_string_pretty(decl).unwrap_or_default();
    let old_decl = before.map(|held| pretty(&held.decl));
    let new_decl = pretty(decl);
    if old_decl.as_deref() != Some(new_decl.as_str()) {
        diffs.push(FileDiff {
            path: DECLARATION_PATH.to_string(),
            change: if old_decl.is_some() {
                "modified"
            } else {
                "added"
            },
            text: line_diff(old_decl.as_deref().unwrap_or(""), &new_decl),
        });
    }
    let new_files: Vec<SnapshotFile> = files.iter().map(SnapshotFile::from_supplied).collect();
    let old_files: &[SnapshotFile] = before.map_or(&[], |held| held.files.as_slice());
    for file in &new_files {
        let shown = file.shown_path();
        match old_files.iter().find(|held| held.shown_path() == shown) {
            None => diffs.push(FileDiff {
                path: shown,
                change: "added",
                text: content_diff(&[], &file.bytes()),
            }),
            Some(held) if held.content_hex != file.content_hex || held.mode != file.mode => {
                let mut text = String::new();
                if held.mode != file.mode {
                    text.push_str(&format!("mode {:o} -> {:o}\n", held.mode, file.mode));
                }
                text.push_str(&content_diff(&held.bytes(), &file.bytes()));
                diffs.push(FileDiff {
                    path: shown,
                    change: "modified",
                    text,
                });
            }
            Some(_) => {}
        }
    }
    for held in old_files {
        let shown = held.shown_path();
        if !new_files.iter().any(|file| file.shown_path() == shown) {
            diffs.push(FileDiff {
                path: shown,
                change: "removed",
                text: content_diff(&held.bytes(), &[]),
            });
        }
    }
    diffs
}

fn content_diff(old: &[u8], new: &[u8]) -> String {
    match (std::str::from_utf8(old), std::str::from_utf8(new)) {
        (Ok(old), Ok(new)) => line_diff(old, new),
        _ => format!(
            "binary content, {} bytes -> {} bytes\n",
            old.len(),
            new.len()
        ),
    }
}

/// The lines that differ, `-` for each one only `old` has and `+` for each
/// one only `new` has, found through the longest common subsequence.
fn line_diff(old: &str, new: &str) -> String {
    let before: Vec<&str> = old.lines().collect();
    let after: Vec<&str> = new.lines().collect();
    let mut out = String::new();
    if before.len().saturating_mul(after.len()) > DIFF_CELL_LIMIT {
        before
            .iter()
            .for_each(|line| out.push_str(&format!("-{line}\n")));
        after
            .iter()
            .for_each(|line| out.push_str(&format!("+{line}\n")));
        return out;
    }
    let width = after.len() + 1;
    let mut common = vec![0usize; (before.len() + 1) * width];
    for i in (0..before.len()).rev() {
        for j in (0..after.len()).rev() {
            common[i * width + j] = if before[i] == after[j] {
                common[(i + 1) * width + j + 1] + 1
            } else {
                common[(i + 1) * width + j].max(common[i * width + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < before.len() || j < after.len() {
        if i < before.len() && j < after.len() && before[i] == after[j] {
            i += 1;
            j += 1;
        } else if j < after.len()
            && (i == before.len() || common[i * width + j + 1] >= common[(i + 1) * width + j])
        {
            out.push_str(&format!("+{}\n", after[j]));
            j += 1;
        } else {
            out.push_str(&format!("-{}\n", before[i]));
            i += 1;
        }
    }
    out
}

/// Why an acceptance was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptRefusal {
    /// The store could not be read.
    StoreUnread(String),
    /// The store declares no such holdout for this repository.
    NotDeclared(String),
    /// The store moved, or the declaration or its contents changed, since
    /// the review the request carries.
    Stale {
        reviewed_sha: String,
        current_sha: String,
        reviewed_fingerprint: String,
        current_fingerprint: String,
    },
    /// The supplied contents were refused when the snapshot was read.
    Supplied(SuppliedRefusal),
}

impl std::fmt::Display for AcceptRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StoreUnread(cause) => write!(f, "the holdout store could not be read: {cause}"),
            Self::NotDeclared(name) => {
                write!(
                    f,
                    "the store declares no holdout `{name}` for this repository"
                )
            }
            Self::Stale {
                reviewed_sha,
                current_sha,
                reviewed_fingerprint,
                current_fingerprint,
            } => write!(
                f,
                "the review is stale: it was read at store commit {reviewed_sha} with \
                 fingerprint {reviewed_fingerprint}, the store is now at {current_sha} with \
                 {current_fingerprint}; review it again"
            ),
            Self::Supplied(refused) => write!(f, "supplied contents were refused: {refused}"),
        }
    }
}

/// The snapshot to retain for `name`, only when the store is still at the
/// reviewed commit `sha` and the holdout still has the reviewed
/// `fingerprint`. Contents fetched since are never substituted: a request
/// that does not match what is there now is refused.
pub fn accept(
    view: &StoreView,
    name: &str,
    sha: &str,
    fingerprint_reviewed: &str,
) -> Result<Snapshot, AcceptRefusal> {
    if let Some(cause) = &view.unread {
        return Err(AcceptRefusal::StoreUnread(cause.clone()));
    }
    let current_sha = view
        .head
        .as_ref()
        .map_err(|cause| AcceptRefusal::StoreUnread(cause.clone()))?;
    let decl = view
        .declared
        .get(name)
        .ok_or_else(|| AcceptRefusal::NotDeclared(name.to_string()))?;
    let current = view.fingerprints.get(name).cloned().unwrap_or_default();
    let stale = || AcceptRefusal::Stale {
        reviewed_sha: sha.to_string(),
        current_sha: current_sha.clone(),
        reviewed_fingerprint: fingerprint_reviewed.to_string(),
        current_fingerprint: current.clone(),
    };
    if current_sha != sha || current != fingerprint_reviewed {
        return Err(stale());
    }
    let tree = view
        .tree
        .as_ref()
        .ok_or_else(|| AcceptRefusal::StoreUnread("no fetched tree".to_string()))?;
    let files = supplied_contents(decl, tree).map_err(AcceptRefusal::Supplied)?;
    let again = fingerprint(decl, tree).map_err(AcceptRefusal::Supplied)?;
    if again != fingerprint_reviewed {
        return Err(stale());
    }
    Ok(Snapshot {
        store_sha: sha.to_string(),
        fingerprint: fingerprint_reviewed.to_string(),
        decl: decl.clone(),
        files: files.iter().map(SnapshotFile::from_supplied).collect(),
    })
}
