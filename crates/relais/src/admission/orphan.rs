//! Lost in-place workers (ADR-0008): the coordinator's half of the fence.
//!
//! A native worker whose session went silent was never told to stop. The
//! runner writes that fact into the in-place marker and reports it here. The
//! coordinator holds the report until the plugin's `SubagentStop` names the
//! worker's agent, then writes the stop confirmation into the marker, which
//! frees the tree. Elapsed time frees nothing.
//!
//! The table is pure state. What it decides to write is handed out as
//! [`OrphanEffects`], which the caller applies once the admission lock is
//! released, so no file is touched under it.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::workspace::inplace::{confirm_stopped, set_orphan_agent, StoppedBy};

/// How many remembered stops the table keeps, oldest first out. A stop is
/// only needed for an orphan reported after it arrived, which is a matter of
/// seconds.
const STOP_MEMORY: usize = 1024;

/// A lost worker, as the runner reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanReport {
    pub dispatch_id: String,
    pub session_id: String,
    /// The agent the worker ran as; `None` when none had bound.
    pub agent_id: Option<String>,
    /// The in-place marker the confirmation is written into.
    pub marker: PathBuf,
}

/// What the coordinator did with a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum OrphanRegistration {
    /// Held until a stop for the agent arrives (or the person vouches).
    Held,
    /// The worker had already stopped: the confirmation is being written.
    Confirmed,
}

/// The files a change to the table asks to be written.
#[derive(Debug, Default)]
pub struct OrphanEffects {
    marker_writes: Arc<Mutex<()>>,
    named: Vec<OrphanReport>,
    confirmed: Vec<OrphanReport>,
    journal: Option<JournalSnapshot>,
}

/// The held set as of one change, numbered so that a snapshot applied late
/// never overwrites a newer one already on disk.
#[derive(Debug)]
struct JournalSnapshot {
    path: PathBuf,
    held: Vec<OrphanReport>,
    generation: u64,
    newest_written: Arc<Mutex<u64>>,
}

impl OrphanEffects {
    pub fn is_empty(&self) -> bool {
        self.named.is_empty() && self.confirmed.is_empty() && self.journal.is_none()
    }

    /// Write the coordinator's agent into the markers that lacked one, the
    /// stop confirmations into their markers (stamped `at`) and the
    /// journal of held orphans. A failure is said on stderr: the marker still
    /// fences the tree, and the person's `--worker-stopped` still frees it.
    pub fn apply(self, at: &str) {
        // Both marker updates read, change and rewrite the file: one at a
        // time, so a later one never erases what an earlier one wrote.
        let _marker_writes = self
            .marker_writes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for report in &self.named {
            let Some(agent) = report.agent_id.as_deref() else {
                continue;
            };
            if let Err(e) = set_orphan_agent(&report.marker, &report.dispatch_id, agent) {
                eprintln!(
                    "relais: agent {agent} of dispatch {} was not written to {}: {e}",
                    report.dispatch_id,
                    report.marker.display()
                );
            }
        }
        for report in &self.confirmed {
            if let Err(e) =
                confirm_stopped(&report.marker, &report.dispatch_id, StoppedBy::Subagent, at)
            {
                eprintln!(
                    "relais: the stop of dispatch {} was not written to {}: {e}",
                    report.dispatch_id,
                    report.marker.display()
                );
            }
        }
        if let Some(JournalSnapshot {
            path,
            held,
            generation,
            newest_written,
        }) = self.journal
        {
            let mut newest = newest_written
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if generation <= *newest {
                return;
            }
            *newest = generation;
            let written = serde_json::to_string(&held)
                .map_err(std::io::Error::other)
                .and_then(|text| crate::fsutil::write_atomic(&path, &text));
            if let Err(e) = written {
                eprintln!(
                    "relais: the lost workers were not journalled to {}: {e}",
                    path.display()
                );
            }
        }
    }
}

/// The orphans a coordinator journalled before it stopped. A journal that
/// cannot be read is said and treated as empty: the markers still fence.
pub fn read_journal(path: &Path) -> Vec<OrphanReport> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            eprintln!("relais: {} cannot be read: {e}", path.display());
            return Vec::new();
        }
    };
    serde_json::from_str(&text).unwrap_or_else(|e| {
        eprintln!("relais: {} cannot be parsed: {e}", path.display());
        Vec::new()
    })
}

/// Whether the marker a report names still fences its tree: it has the
/// orphan, and nobody confirmed it stopped. A marker that cannot be read is
/// kept, since the fence is not proved gone.
pub fn still_standing(report: &OrphanReport) -> bool {
    match std::fs::read_to_string(&report.marker) {
        Ok(text) => serde_json::from_str::<crate::workspace::inplace::Marker>(&text)
            .ok()
            .is_none_or(|marker| {
                marker.orphan.is_some_and(|orphan| {
                    orphan.dispatch == report.dispatch_id && orphan.standing()
                })
            }),
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

/// The dispatches among `held` whose markers no longer fence their trees.
/// Reads files, so the caller holds no admission lock while it runs.
pub fn released_dispatches(held: &[OrphanReport]) -> Vec<String> {
    held.iter()
        .filter(|report| !still_standing(report))
        .map(|report| report.dispatch_id.clone())
        .collect()
}

#[derive(Debug, Default)]
pub(super) struct OrphanTable {
    named: Vec<OrphanReport>,
    /// Taken by every marker rewrite this table hands out.
    marker_writes: Arc<Mutex<()>>,
    /// Held orphans by dispatch.
    held: BTreeMap<String, OrphanReport>,
    /// Stops seen, `(session, agent)`, newest last: an orphan reported after
    /// its worker's stop finds the stop here.
    stops: BTreeSet<(String, String)>,
    stop_order: VecDeque<(String, String)>,
    confirmed: Vec<OrphanReport>,
    journal_dirty: bool,
    /// Snapshots handed out so far; each journal write carries its number.
    journal_generation: u64,
    journal_newest_written: Arc<Mutex<u64>>,
}

/// Whether the caller found the worker's stop already recorded elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PriorStop {
    Seen,
    Unseen,
}

impl OrphanTable {
    /// Take back the orphans a previous coordinator journalled.
    pub(super) fn adopt(&mut self, reports: Vec<OrphanReport>) {
        for report in reports {
            self.held.insert(report.dispatch_id.clone(), report);
        }
    }

    /// Forget held orphans whose markers say they are freed.
    pub(super) fn release(&mut self, dispatches: &[String]) {
        for dispatch in dispatches {
            if self.held.remove(dispatch).is_some() {
                self.journal_dirty = true;
            }
        }
    }

    /// The coordinator knew the agent of a report that named none: the
    /// marker is to carry it too.
    pub(super) fn note_named(&mut self, report: OrphanReport) {
        self.named.push(report);
    }

    pub(super) fn held(&self) -> impl Iterator<Item = &OrphanReport> {
        self.held.values()
    }

    /// `agent` of `session` stopped: every held orphan of that agent is
    /// confirmed, and the stop is remembered for an orphan reported later.
    pub(super) fn note_stop(&mut self, session_id: &str, agent_id: &str) {
        let key = (session_id.to_string(), agent_id.to_string());
        if self.stops.insert(key.clone()) {
            self.stop_order.push_back(key);
            while self.stop_order.len() > STOP_MEMORY {
                if let Some(oldest) = self.stop_order.pop_front() {
                    self.stops.remove(&oldest);
                }
            }
        }
        let matching: Vec<String> = self
            .held
            .values()
            .filter(|held| {
                held.session_id == session_id && held.agent_id.as_deref() == Some(agent_id)
            })
            .map(|held| held.dispatch_id.clone())
            .collect();
        for dispatch_id in matching {
            if let Some(report) = self.held.remove(&dispatch_id) {
                self.confirmed.push(report);
                self.journal_dirty = true;
            }
        }
    }

    pub(super) fn has_stopped(&self, session_id: &str, agent_id: &str) -> bool {
        self.stops
            .contains(&(session_id.to_string(), agent_id.to_string()))
    }

    /// Register a lost worker. `prior_stop` is what the caller found in
    /// the coordinator's other records: a stop remembered for the agent, or
    /// the dispatch's native state already `Stopped`.
    pub(super) fn register(
        &mut self,
        report: OrphanReport,
        prior_stop: PriorStop,
    ) -> OrphanRegistration {
        let stopped = prior_stop == PriorStop::Seen
            || report
                .agent_id
                .as_deref()
                .is_some_and(|agent| self.has_stopped(&report.session_id, agent));
        if stopped {
            self.confirmed.push(report);
            return OrphanRegistration::Confirmed;
        }
        self.held.insert(report.dispatch_id.clone(), report);
        self.journal_dirty = true;
        OrphanRegistration::Held
    }

    /// What must be written now, and nothing the next call would repeat.
    pub(super) fn take_effects(&mut self, journal: Option<&Path>) -> OrphanEffects {
        let journal_dirty = std::mem::take(&mut self.journal_dirty);
        let snapshot = journal.filter(|_| journal_dirty).map(|path| {
            self.journal_generation += 1;
            JournalSnapshot {
                path: path.to_path_buf(),
                held: self.held.values().cloned().collect(),
                generation: self.journal_generation,
                newest_written: Arc::clone(&self.journal_newest_written),
            }
        });
        OrphanEffects {
            marker_writes: Arc::clone(&self.marker_writes),
            named: std::mem::take(&mut self.named),
            confirmed: std::mem::take(&mut self.confirmed),
            journal: snapshot,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(dispatch: &str, agent: Option<&str>) -> OrphanReport {
        OrphanReport {
            dispatch_id: dispatch.into(),
            session_id: "sess".into(),
            agent_id: agent.map(str::to_string),
            marker: PathBuf::from("/nonexistent/marker.json"),
        }
    }

    #[test]
    fn a_stop_for_the_agent_confirms_the_held_orphan_once() {
        let mut table = OrphanTable::default();
        assert_eq!(
            table.register(report("d1", Some("a1")), PriorStop::Unseen),
            OrphanRegistration::Held
        );
        table.note_stop("sess", "other");
        assert!(table.take_effects(None).confirmed.is_empty());
        table.note_stop("sess", "a1");
        let effects = table.take_effects(None);
        assert_eq!(effects.confirmed, vec![report("d1", Some("a1"))]);
        assert_eq!(table.held().count(), 0);
        table.note_stop("sess", "a1");
        assert!(table.take_effects(None).is_empty());
    }

    #[test]
    fn a_stop_seen_before_the_report_confirms_it_at_registration() {
        let mut table = OrphanTable::default();
        table.note_stop("sess", "a1");
        assert_eq!(
            table.register(report("d1", Some("a1")), PriorStop::Unseen),
            OrphanRegistration::Confirmed
        );
        assert_eq!(table.held().count(), 0);
    }

    #[test]
    fn an_orphan_without_an_agent_is_confirmed_by_no_stop() {
        let mut table = OrphanTable::default();
        table.register(report("d1", None), PriorStop::Unseen);
        table.note_stop("sess", "a1");
        assert!(table.take_effects(None).confirmed.is_empty());
        assert_eq!(table.held().count(), 1);
    }

    #[test]
    fn the_journal_is_written_only_when_the_held_set_changed() {
        let mut table = OrphanTable::default();
        let path = Path::new("/journal.json");
        assert!(table.take_effects(Some(path)).journal.is_none());
        table.register(report("d1", Some("a1")), PriorStop::Unseen);
        let snapshot = table.take_effects(Some(path)).journal.expect("journal");
        assert_eq!(snapshot.path, path);
        assert_eq!(snapshot.held, vec![report("d1", Some("a1"))]);
        assert!(table.take_effects(Some(path)).journal.is_none());
    }

    #[test]
    fn an_older_snapshot_applied_late_does_not_overwrite_a_newer_one() {
        let dir = crate::test_support::temp_dir("orphan-journal");
        let path = dir.join("journal.json");
        let mut table = OrphanTable::default();
        table.register(report("d1", Some("a1")), PriorStop::Unseen);
        let older = table.take_effects(Some(&path));
        table.register(report("d2", Some("a2")), PriorStop::Unseen);
        let newer = table.take_effects(Some(&path));
        newer.apply("2026-01-01T00:00:00+00:00");
        older.apply("2026-01-01T00:00:00+00:00");
        assert_eq!(read_journal(&path).len(), 2);
    }
}
