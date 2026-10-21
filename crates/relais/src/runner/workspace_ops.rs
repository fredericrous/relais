//! The run's workspace: in-place entry and exit, trees and retirement.

use super::setup_gate;
use super::{
    retirement_detail, AtomicBool, BlockCode, Event, InPlaceRun, Path, PathBuf, Reason, RunEngine,
    RunError, RunOutcome, TaskWorktree, Terminal, WorktreeEnd,
};
use crate::{verify, workspace};

impl<'a> RunEngine<'a> {
    /// Take the caller's worktree for this run and snapshot it (ADR-0008).
    /// The snapshot, not `HEAD`, becomes the base of every diff, scope
    /// check and verification tree, so uncommitted work the person had
    /// is neither the worker's nor lost. A refusal carries its block code.
    pub(super) fn enter_in_place(
        &mut self,
        base_sha: String,
    ) -> Result<String, (BlockCode, String)> {
        use crate::workspace::inplace::{start_snapshot, Lease, LeaseRefusal};
        let lease = Lease::acquire(self.config.repo_dir, self.run_id.as_str()).map_err(|e| match e {
            LeaseRefusal::Busy { holder } => (
                BlockCode::WorkspaceBusy,
                format!(
                    "run {} holds this worktree; wait for its outcome (relais status {})",
                    holder.as_deref().unwrap_or("?"),
                    holder.as_deref().unwrap_or("<run>")
                ),
            ),
            LeaseRefusal::Interrupted { run } => (
                BlockCode::WorkspaceHasInterruptedRun,
                format!(
                    "run {run} was interrupted here with edits applied; run: relais resume --retire {run}"
                ),
            ),
            LeaseRefusal::UnreadableMarker { path, why } => (
                BlockCode::WorkspaceHasInterruptedRun,
                format!(
                    "a run died in this worktree and left {}, which cannot be read ({why}); look at the tree, and delete that file once you have",
                    path.display()
                ),
            ),
            LeaseRefusal::Io(e) => (BlockCode::WorktreeUnavailable, e.to_string()),
        })?;
        let start = start_snapshot(&lease).map_err(|e| {
            (
                BlockCode::WorktreeUnavailable,
                format!("the start snapshot failed: {e}"),
            )
        })?;
        if start.head_sha != base_sha {
            return Err((
                BlockCode::BaseUnresolvable,
                format!(
                    "the contract's base resolves to {} but this worktree is at {}; an in-place run works from the worktree's own HEAD",
                    &base_sha[..base_sha.len().min(12)],
                    &start.head_sha[..start.head_sha.len().min(12)]
                ),
            ));
        }
        self.events.emit(Event::Decision {
            what: "workspace_in_place".into(),
            reason: format!(
                "in place in {}: do not edit it or stage anything in it until the outcome arrives (lease held){}",
                self.config.repo_dir.display(),
                if start.dirty.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; the start snapshot includes {} uncommitted path(s)",
                        start.dirty.len()
                    )
                }
            ),
        });
        let sha = start.sha.clone();
        self.inplace = Some(InPlaceRun {
            lease: Some(lease),
            start,
        });
        Ok(sha)
    }

    /// The run's worker tree when it works in place: the caller's own
    /// worktree, against a private index. The marker is written here,
    /// before anything can edit the tree, so a run that dies from now on
    /// is found and finished by `resume --retire`.
    pub(super) fn take_in_place_worktree(&mut self) -> Option<Result<TaskWorktree, String>> {
        use crate::workspace::inplace::{Marker, Stage};
        let state = self.inplace.as_ref()?;
        let lease = state.lease.as_ref()?;
        let start = &state.start;
        let marker = Marker {
            run: self.run_id.as_str().to_string(),
            stage: Stage::Running,
            head_symbolic: start.head_symbolic.clone(),
            head_sha: start.head_sha.clone(),
            start: start.sha.clone(),
            index_listing: start.index_listing.clone(),
            target: None,
            exported: None,
        };
        Some(
            lease
                .write_marker(&marker)
                .map(|()| TaskWorktree {
                    path: self.config.repo_dir.to_path_buf(),
                    base_sha: start.sha.clone(),
                    index: Some(lease.relais_index()),
                })
                .map_err(|e| format!("the in-place marker could not be written: {e}")),
        )
    }

    /// The end of an in-place run: decide what the worktree must be (the
    /// accepted change, or how it was found), then export what no
    /// candidate holds, apply it, and release the worktree. The directory
    /// is never removed. An interrupted run keeps its marker and its
    /// edits for `relais resume --retire`.
    pub(super) fn finish_in_place(
        &mut self,
        worktree: &TaskWorktree,
        outcome: &RunOutcome,
    ) -> Result<(), RunError> {
        use crate::workspace::inplace::{
            finalize, ApplyError, IndexOutcome, IndexPolicy, Outcome, Target, TargetKind,
            Unrestored,
        };
        let Some(lease) = self.inplace.as_mut().and_then(|state| state.lease.take()) else {
            return Ok(());
        };
        if matches!(outcome.terminal.worktree_end(), WorktreeEnd::Keep) {
            self.events.emit(Event::Stderr {
                text: format!(
                    "worktree {} kept with the run's edits in it; `relais resume --retire {}` finishes it",
                    worktree.path.display(),
                    self.run_id
                ),
            });
            return Ok(());
        }
        let tree_of = |rev: &str| {
            self.config
                .git
                .run(&worktree.path, &["rev-parse", &format!("{rev}^{{tree}}")])
                .map(|tree| tree.trim().to_string())
        };
        let target = match &outcome.terminal {
            Terminal::Accepted(receipt) => tree_of(&receipt.candidate_sha).map(|tree| Target {
                kind: TargetKind::Accepted,
                tree,
            }),
            _ => tree_of(&worktree.base_sha).map(|tree| Target {
                kind: TargetKind::Start,
                tree,
            }),
        };
        let target = match target {
            Ok(target) => target,
            Err(e) => {
                return self.transition(
                    self.state,
                    Reason::WorktreeNotReleased,
                    serde_json::json!({
                        "worktree": worktree.path.to_string_lossy(),
                        "error": e.to_string(),
                        "detail": "the tree the worktree must end as could not be read; it is as the run left it",
                    }),
                );
            }
        };
        let kind = target.kind;
        let artifacts = self.artifacts.clone();
        let run_id = self.run_id.as_str().to_string();
        let mut export = || {
            workspace::export_unkept(worktree, &run_id, &artifacts)
                .map(|exported| {
                    exported
                        .map(|exported| exported.reference)
                        .unwrap_or_else(|| "none".to_string())
                })
                .map_err(std::io::Error::other)
        };
        match finalize(lease, target, IndexPolicy::Restore, &mut export) {
            Ok(Outcome::Applied { index }) => {
                self.say_index(&index, worktree);
                self.transition(
                    self.state,
                    Reason::WorktreeRetired,
                    serde_json::json!({
                        "worktree": worktree.path.to_string_lossy(),
                        "in_place": true,
                        "index": match &index {
                            IndexOutcome::Untouched => "untouched",
                            IndexOutcome::Restored => "restored",
                            IndexOutcome::Changed(_) => "changed",
                        },
                        "restore_error": match &index {
                            IndexOutcome::Changed(Unrestored::Failed(why)) => Some(why.as_str()),
                            _ => None,
                        },
                        "left": match kind {
                            TargetKind::Accepted => "the accepted change",
                            TargetKind::Start => "the tree as it was found",
                        },
                        "detail": "the directory is kept",
                    }),
                )
            }
            Ok(Outcome::HeadMoved { was, now }) => {
                self.events.emit(Event::Stderr {
                    text: format!(
                        "HEAD moved from {} to {} during the run; relais did not touch the branch, review it yourself",
                        &was[..was.len().min(12)],
                        &now[..now.len().min(12)]
                    ),
                });
                self.transition(
                    self.state,
                    Reason::WorktreeNotReleased,
                    serde_json::json!({
                        "worktree": worktree.path.to_string_lossy(),
                        "code": BlockCode::InPlaceHeadMoved.as_str(),
                        "was": was,
                        "now": now,
                        "detail": "a worker committed on the person's branch; the tree and the branch were left as they are",
                    }),
                )
            }
            Err(e) => {
                let code = match &e {
                    ApplyError::Collisions(_) => BlockCode::RestoreBlockedIgnoredCollision,
                    _ => BlockCode::WorktreeUnavailable,
                };
                self.transition(
                    self.state,
                    Reason::WorktreeNotReleased,
                    serde_json::json!({
                        "worktree": worktree.path.to_string_lossy(),
                        "code": code.as_str(),
                        "error": e.to_string(),
                        "detail": "the worktree is as the run left it; `relais resume --retire` retries once the paths named are dealt with",
                    }),
                )
            }
        }
    }

    /// Say, as a decision on the run, what became of the person's git index.
    pub(super) fn say_index(
        &self,
        index: &crate::workspace::inplace::IndexOutcome,
        worktree: &TaskWorktree,
    ) {
        use crate::workspace::inplace::{IndexOutcome, Unrestored};
        let (what, reason) = match index {
            IndexOutcome::Untouched => return,
            IndexOutcome::Restored => (
                "real_index_restored",
                format!(
                    "the worker changed your git index in {}; it is back as it was when the run began",
                    worktree.path.display()
                ),
            ),
            IndexOutcome::Changed(unrestored) => (
                "real_index_changed",
                format!(
                    "the worker changed your git index in {} and it could not be restored{}; the original is kept in the git directory as {}.index.orig",
                    worktree.path.display(),
                    match unrestored {
                        Unrestored::Failed(why) => format!(" ({why})"),
                        Unrestored::NotAsked => String::new(),
                    },
                    self.run_id
                ),
            ),
        };
        self.events.emit(Event::Decision {
            what: what.into(),
            reason,
        });
    }

    /// The tree the worker edits, when it exists: the caller's own worktree
    /// for an in-place run, else this run's task worktree. Candidate
    /// verification trees are seeded from it.
    pub(super) fn task_tree(&self) -> Option<PathBuf> {
        if self.inplace.is_some() {
            return Some(self.config.repo_dir.to_path_buf());
        }
        Some(self.worktrees.join("task")).filter(|task| task.is_dir())
    }

    /// Set a tree up before the profile's commands run in it. `Isolated` is
    /// exactly [`verify::run_setup`]. A `Fork` or `InPlace` run first seeds
    /// `dir` with the dependency directories of `seed_from` copy-on-write,
    /// then runs every step, skipping only one that declared `reuse` and
    /// proved its outputs (ADR-0008).
    pub(super) fn setup_tree(
        &self,
        dir: &Path,
        seed_from: Option<&Path>,
        profile: &crate::policy::VerificationProfile,
        logs_dir: &Path,
        prefix: &str,
        cancel: Option<&AtomicBool>,
    ) -> Result<Vec<verify::CheckOutcome>, verify::VerifyError> {
        use crate::workspace::reuse::{StampStore, Workspace};
        if self.config.workspace == Workspace::Isolated {
            return verify::run_setup_gated_within(
                dir,
                profile,
                logs_dir,
                prefix,
                &self.events,
                cancel,
                &mut verify::RunEveryStep,
                &self.heavy(),
            );
        }
        let source = seed_from.filter(|source| *source != dir);
        if let Some(source) = source {
            let own_run = self.inplace.is_some().then(|| self.run_id.as_str());
            setup_gate::seed_tree(self.config.repo_policy, source, dir, own_run, &self.events);
        }
        let state = self
            .config
            .artifacts_dir
            .parent()
            .unwrap_or(&self.config.artifacts_dir);
        let stamps = StampStore::new(state);
        let mut gate = setup_gate::ReuseGate::new(dir, source, &stamps, &self.events);
        verify::run_setup_gated_within(
            dir,
            profile,
            logs_dir,
            prefix,
            &self.events,
            cancel,
            &mut gate,
            &self.heavy(),
        )
    }

    /// Say, once, that the commands this repository runs changed since the
    /// last run that saw them (ADR-0007): a line on stderr and a
    /// `commands_changed` decision on the run. Whether it is said once
    /// is the store's: a recording one (`relais run`) stores what it saw,
    /// so the next run is silent. A replay never gets here. A store that
    /// fails is said and the run goes on, since a warning that cannot be
    /// kept must not stop work.
    pub(super) fn say_command_changes(&self, repo_key: &str) {
        match crate::trust::observe(self.config.declarations, repo_key, self.config.repo_policy) {
            Ok(Some(change)) => {
                eprintln!("{}", change.text());
                self.events.emit(Event::Decision {
                    what: "commands_changed".into(),
                    reason: change.summary(),
                });
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("relais: the commands this repository ran under were not kept: {e}")
            }
        }
    }

    /// Retire the run's worktree at its terminal state (SPEC §8): a run
    /// delivers a NAMED candidate — the ref and the patch — and the
    /// directory goes once everything tracked is exported, which is
    /// what `workspace::retire` guarantees before it removes anything.
    /// An accepted run's tree is already its accepted candidate, so
    /// nothing more is exported; a run that ended any other way keeps
    /// whatever its last worker wrote under `…/final`. One permanent
    /// `git worktree list` entry per run, and the build output every
    /// worker leaves behind, are what used to accumulate (audit B15).
    ///
    /// An interrupted run's state is uncertain (SPEC §12): a writer may
    /// still be in the tree. Its worktree is retired here only once
    /// every dispatch the ledger still holds live is provably gone —
    /// the judgement `relais resume --retire` makes, made at the end
    /// instead of by a person later. A run whose own worker ended
    /// (its dispatch closed before the run was judged) has no live
    /// dispatch and is retired like any other end; one whose dispatch
    /// may still write — a live process, a native agent of a session
    /// whose plugin still says hello, an outcome nothing knows — keeps
    /// its tree for `resume --retire`. Two interrupted runs of
    /// 2026-10-08 kept 6.8 GB of installed dependencies for a day this
    /// way, with nothing left writing them.
    ///
    /// A retirement that fails is on the record and changes nothing
    /// about how the run ended: the directory is still there, and so
    /// is everything in it.
    pub(crate) fn retire_worktree(
        &mut self,
        worktree: &TaskWorktree,
        outcome: &RunOutcome,
    ) -> Result<(), RunError> {
        if self.inplace.is_some() {
            return self.finish_in_place(worktree, outcome);
        }
        match outcome.terminal.worktree_end() {
            WorktreeEnd::Keep => {
                // A write lease on the tree is a writer, whatever the
                // ledger says of dispatches (SPEC §23): a straggler that
                // held it past the deadline is still in there.
                let key = worktree.path.to_string_lossy().into_owned();
                let lease_holder = self
                    .config
                    .gate
                    .and_then(|gate| gate.write_lease_holder(&key).ok().flatten());
                let judged = self.judge_live_dispatches()?;
                let may_still_write = match lease_holder {
                    Some(holder) => Some(format!("the write lease is held by {holder}")),
                    None if !judged.all_provably_dead() => Some(judged.detail()),
                    None => None,
                };
                if let Some(why) = may_still_write {
                    self.events.emit(Event::Stderr {
                        text: format!(
                            "worktree {} kept: a dispatch may still write it ({why}); `relais \
                             resume --retire {}` retires it once that is settled",
                            worktree.path.display(),
                            self.run_id
                        ),
                    });
                    return Ok(());
                }
            }
            WorktreeEnd::Retire => {}
        }
        match workspace::retire(worktree, self.run_id.as_str(), &self.artifacts) {
            Ok(retirement) => self.transition(
                self.state,
                Reason::WorktreeRetired,
                retirement_detail(&worktree.path, &retirement),
            ),
            Err(e) => self.transition(
                self.state,
                Reason::WorktreeNotReleased,
                serde_json::json!({
                    "worktree": worktree.path.to_string_lossy(),
                    "error": e.to_string(),
                    "detail": "the worktree could not be retired; whatever it holds is still in it",
                }),
            ),
        }
    }
}
