//! Workspace-aware setup for a run (ADR-0008).
//!
//! In `Isolated` mode a run sets a tree up exactly as it always did. In a
//! `Fork` or `InPlace` run, the tree first receives the caller's dependency
//! directories copy-on-write, then every setup step still runs, except one
//! that declared `reuse` and proved, against the source's recorded result,
//! that its outputs are already there and good.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::policy::{CommandSpec, RepoPolicy};
use crate::protocol::{Event, Events};
use crate::verify::SetupGate;
use crate::workspace::inplace;
use crate::workspace::reuse::{
    self, decide_skip, step_key, RunReason, SkipDecision, StampStore, StepKey,
};

/// How long a source's dependency tree must have been still before it is
/// cloned, how often to look, and how long to wait for that at most. A
/// politeness toward an install in progress, never an identity (a rewrite
/// that preserves timestamps defeats it): every setup step still runs
/// afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quiet {
    pub still_for: Duration,
    pub poll: Duration,
    pub give_up: Duration,
}

impl Quiet {
    /// The window for a tree somebody else may be writing: the caller's
    /// workspace.
    pub const CALLER: Quiet = Quiet {
        still_for: Duration::from_secs(30),
        poll: Duration::from_secs(5),
        give_up: Duration::from_secs(60),
    };
}

/// Where a tree is seeded from, and what the caller knows of its writers.
#[derive(Debug, Clone, Copy)]
pub struct SeedSource<'a> {
    pub path: &'a Path,
    pub wait: SeedWait,
}

/// What a seed knows about the writers of its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedWait {
    /// Writers may be active: wait for the tree to be still.
    QuietFor(Quiet),
    /// The source is the run's own tree and every writer the run started
    /// has ended: there is nothing to wait for.
    WritersEnded,
}

const VERIFY_TIMEOUT: Duration = Duration::from_secs(120);

fn decision(events: &Events, what: &str, reason: String) {
    events.emit(Event::Decision {
        what: what.to_string(),
        reason,
    });
}

/// Seed `dest` from `source`: detect the dependency directories, wait for a
/// busy source to settle, clone copy-on-write. Says what happened as
/// decisions on the run; a filesystem that cannot clone costs nothing but
/// the time today's setup already takes.
///
/// A source an in-place run holds is not cloned: its tree may be half
/// written, and the run's marker exists for exactly as long as that is so.
/// `own_run` is the run seeding, whose own marker does not count (an
/// in-place run seeds its verification trees from its own worktree).
pub fn seed_tree(
    policy: &RepoPolicy,
    source: &Path,
    dest: &Path,
    own_run: Option<&str>,
    wait: SeedWait,
    sleep: &mut dyn FnMut(Duration),
    events: &Events,
) {
    let deps = reuse::detect_deps(source, &policy.workspace.deps);
    if deps.is_empty() {
        return;
    }
    if !source_is_free(source, own_run, events) {
        return;
    }
    for dep in &deps {
        let quiet = match wait {
            SeedWait::WritersEnded => {
                let place = if own_run.is_some() {
                    "in place"
                } else {
                    "fork"
                };
                decision(
                    events,
                    "workspace_seed_unwaited",
                    format!(
                        "{}: the run's own writers ended ({place} run)",
                        dep.display()
                    ),
                );
                continue;
            }
            SeedWait::QuietFor(quiet) => quiet,
        };
        let settled = reuse::wait_until_quiet(
            &source.join(dep),
            quiet.still_for,
            quiet.poll,
            quiet.give_up,
            sleep,
            &mut |waited| {
                decision(
                    events,
                    "workspace_seed_waiting",
                    format!(
                        "{} is still changing ({}s)",
                        dep.display(),
                        waited.as_secs()
                    ),
                );
            },
        );
        if !settled {
            decision(
                events,
                "workspace_seed_skipped",
                format!(
                    "{} kept changing for {}s: source busy",
                    dep.display(),
                    quiet.give_up.as_secs()
                ),
            );
            return;
        }
    }
    let report = reuse::seed(source, dest, &deps);
    // A run may have started while the clone was under way: a marker found
    // now means what was cloned may be half written, so it is thrown away.
    if !report.seeded.is_empty() && !source_is_free(source, own_run, events) {
        for (path, _) in &report.seeded {
            if let Err(e) = std::fs::remove_dir_all(dest.join(path)) {
                decision(
                    events,
                    "workspace_seed_discarded",
                    format!("{} could not be removed: {e}", path.display()),
                );
            }
        }
        decision(
            events,
            "workspace_seed_discarded",
            format!(
                "{} was taken by an in-place run while it was cloned",
                source.display()
            ),
        );
        return;
    }
    if let Some(reason) = &report.unavailable {
        decision(
            events,
            "workspace_cow_unavailable",
            format!(
                "{reason}; put RELAIS_STATE_DIR on the source's volume to reuse its dependencies"
            ),
        );
    }
    for (path, stats) in &report.seeded {
        decision(
            events,
            "workspace_seeded",
            format!("{} ({} files)", path.display(), stats.files),
        );
    }
    for (path, why) in &report.skipped {
        decision(
            events,
            "workspace_seed_skipped",
            format!("{}: {why}", path.display()),
        );
    }
}

/// Is `source` free to clone from? Says why not as a decision.
fn source_is_free(source: &Path, own_run: Option<&str>, events: &Events) -> bool {
    match inplace::held_by(source, own_run) {
        Ok(None) => true,
        Ok(Some(run)) => {
            decision(
                events,
                "workspace_seed_skipped",
                format!("{} is held by in-place run {run}", source.display()),
            );
            false
        }
        Err(e) => {
            decision(
                events,
                "workspace_seed_skipped",
                format!(
                    "{} could not be checked for a running in-place run: {e}",
                    source.display()
                ),
            );
            false
        }
    }
}

/// The gate [`crate::verify::run_setup_gated`] consults for a seeded tree.
pub struct ReuseGate<'a> {
    pub tree: &'a Path,
    /// The workspace the tree was seeded from, whose recorded results a
    /// skip is compared with. `None` = nothing to compare, nothing skipped.
    pub source: Option<&'a Path>,
    pub stamps: &'a StampStore,
    pub events: &'a Events,
    earlier: Vec<StepKey>,
    pending: Option<StepKey>,
}

impl<'a> ReuseGate<'a> {
    pub fn new(
        tree: &'a Path,
        source: Option<&'a Path>,
        stamps: &'a StampStore,
        events: &'a Events,
    ) -> Self {
        Self {
            tree,
            source,
            stamps,
            events,
            earlier: Vec::new(),
            pending: None,
        }
    }

    /// The toolchain of the step's program, or `None` when it cannot be
    /// versioned: an unidentified toolchain never authorizes a skip.
    fn toolchain(step: &CommandSpec) -> Option<String> {
        let program = step.argv.first()?;
        let version = crate::tooling::program_version(program, None).ok()?;
        Some(format!("{} {}", version.program, version.version))
    }

    fn key(&self, step: &CommandSpec) -> Option<StepKey> {
        let reuse = step.reuse.as_ref()?;
        let toolchain = Self::toolchain(step)?;
        step_key(self.tree, step, reuse, &toolchain, &self.earlier).ok()
    }

    fn record(&self, index: usize, key: &StepKey) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        if let Err(e) = self.stamps.put(self.tree, index, key.clone(), now) {
            eprintln!("relais: the result of setup step {index} was not recorded: {e}");
        }
    }

    /// A step with no `reuse` still shapes what later steps build on, so its
    /// argv joins their keys.
    fn plain_key(step: &CommandSpec) -> StepKey {
        StepKey(crate::ids::canonical_json_hash(
            &serde_json::json!({ "argv": step.argv }),
        ))
    }
}

impl SetupGate for ReuseGate<'_> {
    fn skip(&mut self, index: usize, step: &CommandSpec) -> bool {
        self.pending = None;
        if step.reuse.is_none() {
            self.earlier.push(Self::plain_key(step));
            return false;
        }
        let Some(key) = self.key(step) else {
            decision(
                self.events,
                "workspace_setup_ran",
                format!("step {index}: its inputs or toolchain could not be identified"),
            );
            self.earlier.push(Self::plain_key(step));
            return false;
        };
        self.pending = Some(key.clone());
        let Some(source) = self.source else {
            return false;
        };
        match decide_skip(
            self.tree,
            source,
            index,
            step,
            &key,
            self.stamps,
            VERIFY_TIMEOUT,
        ) {
            SkipDecision::Skip => {
                decision(
                    self.events,
                    "workspace_setup_skipped",
                    format!(
                        "step {index}: key {} matches {}",
                        &key.0[..12],
                        source.display()
                    ),
                );
                // The tree now holds a result proven equal to the source's,
                // so trees seeded from this one may compare with it.
                self.record(index, &key);
                self.earlier.push(key);
                self.pending = None;
                true
            }
            SkipDecision::Run(reason) => {
                if reason != RunReason::NoStampForSource {
                    decision(
                        self.events,
                        "workspace_setup_ran",
                        format!("step {index}: {reason}"),
                    );
                }
                false
            }
        }
    }

    fn ran(&mut self, index: usize, _step: &CommandSpec, succeeded: bool) {
        let Some(key) = self.pending.take() else {
            return;
        };
        if !succeeded {
            return;
        }
        self.record(index, &key);
        self.earlier.push(key);
    }
}
