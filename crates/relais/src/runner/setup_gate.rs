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
use crate::workspace::reuse::{
    self, decide_skip, step_key, RunReason, SkipDecision, StampStore, StepKey,
};

/// How long a source's dependency tree must have been still before it is
/// cloned, and how long to wait for that at most. A politeness toward an
/// install in progress, never an identity (a rewrite that preserves
/// timestamps defeats it): every setup step still runs afterwards.
const QUIET_FOR: Duration = Duration::from_secs(30);
const GIVE_UP: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_secs(5);
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
pub fn seed_tree(policy: &RepoPolicy, source: &Path, dest: &Path, events: &Events) {
    let deps = reuse::detect_deps(source, &policy.workspace.deps);
    if deps.is_empty() {
        return;
    }
    for dep in &deps {
        let settled = reuse::wait_until_quiet(
            &source.join(dep),
            QUIET_FOR,
            POLL,
            GIVE_UP,
            &mut std::thread::sleep,
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
                    GIVE_UP.as_secs()
                ),
            );
            return;
        }
    }
    let report = reuse::seed(source, dest, &deps);
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
