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
//! released, so no file is touched under it. What a marker is, and how it is
//! read and written, is the [`OrphanFence`] the caller injects.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// What admission needs of the markers that fence a tree. Declared here,
/// implemented beside the markers (`workspace::inplace::MarkerFence`) and
/// injected where the admission state is built, as the disk probe is.
pub trait OrphanFence: std::fmt::Debug + Send + Sync {
    /// Whether the marker a report names still fences its tree: it has the
    /// orphan, and nobody confirmed it stopped. A marker that cannot be read
    /// is kept, since the fence is not proved gone.
    fn still_standing(&self, report: &OrphanReport) -> bool;
    /// Record in the marker that the report's worker stopped (stamped
    /// `at`). `Ok(false)`: nothing to record.
    fn confirm_stopped(&self, report: &OrphanReport, at: &str) -> io::Result<bool>;
    /// Record in the marker the agent the coordinator knows for the report's
    /// worker. `Ok(false)`: nothing to record.
    fn name_agent(&self, report: &OrphanReport, agent: &str) -> io::Result<bool>;
    /// Whether `marker` is the one marker of `run`.
    fn owns_marker(&self, marker: &Path, run: &str) -> bool;
}

/// The fence of a state built without one: it knows no marker, so it
/// refuses every report and writes nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoFence;

impl OrphanFence for NoFence {
    fn still_standing(&self, _report: &OrphanReport) -> bool {
        true
    }

    fn confirm_stopped(&self, _report: &OrphanReport, _at: &str) -> io::Result<bool> {
        Ok(false)
    }

    fn name_agent(&self, _report: &OrphanReport, _agent: &str) -> io::Result<bool> {
        Ok(false)
    }

    fn owns_marker(&self, _marker: &Path, _run: &str) -> bool {
        false
    }
}

/// A lost worker, as the runner reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanReport {
    pub dispatch_id: String,
    pub session_id: String,
    /// The agent the worker ran as; `None` when none had bound.
    pub agent_id: Option<String>,
    /// The in-place marker the confirmation is written into.
    pub marker: PathBuf,
    /// The worker's stop was seen and its confirmation is owed to the
    /// marker. Set by the coordinator, never by the runner; journalled, so a
    /// restart still owes it.
    #[serde(default)]
    pub stop_seen: bool,
}

/// What the coordinator did with a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum OrphanRegistration {
    /// Held until a stop for the agent arrives (or the person vouches).
    Held,
    /// The worker had already stopped: the confirmation is being written.
    Confirmed,
    /// The report names a marker that is not the reported run's: nothing
    /// was held and nothing will be written to it.
    Refused { reason: String },
}

/// The files a change to the table asks to be written.
#[derive(Debug, Default)]
pub struct OrphanEffects {
    marker_writes: Arc<Mutex<()>>,
    fence: Option<Arc<dyn OrphanFence>>,
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
    /// fences the tree, the confirmation is retried while the report stays
    /// held, and the person's `--worker-stopped` frees it meanwhile.
    pub fn apply(self, at: &str) {
        let Some(fence) = self.fence.as_deref() else {
            return;
        };
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
            if let Err(e) = fence.name_agent(report, agent) {
                eprintln!(
                    "relais: agent {agent} of dispatch {} was not written to {}: {e}",
                    report.dispatch_id,
                    report.marker.display()
                );
            }
        }
        for report in &self.confirmed {
            if let Err(e) = fence.confirm_stopped(report, at) {
                eprintln!(
                    "relais: the stop of dispatch {} was not written to {}, to be retried: {e}",
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
/// cannot be read is said and treated as empty: the markers still fence. One
/// that cannot be parsed is renamed to `<name>.corrupt` first, so the next
/// journal write does not destroy it.
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
        let aside = corrupt_name(path);
        match std::fs::rename(path, &aside) {
            Ok(()) => eprintln!(
                "relais: {} cannot be parsed ({e}); kept as {}",
                path.display(),
                aside.display()
            ),
            Err(rename_error) => eprintln!(
                "relais: {} cannot be parsed ({e}) and could not be set aside: {rename_error}",
                path.display()
            ),
        }
        Vec::new()
    })
}

fn corrupt_name(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".corrupt");
    path.with_file_name(name)
}

/// The dispatches among `held` whose markers no longer fence their trees.
/// Reads files, so the caller holds no admission lock while it runs.
pub fn released_dispatches(held: &[OrphanReport], fence: &dyn OrphanFence) -> Vec<String> {
    held.iter()
        .filter(|report| !fence.still_standing(report))
        .map(|report| report.dispatch_id.clone())
        .collect()
}

#[derive(Debug, Default)]
pub(super) struct OrphanTable {
    named: Vec<OrphanReport>,
    /// Taken by every marker rewrite this table hands out.
    marker_writes: Arc<Mutex<()>>,
    /// Held orphans by dispatch. One whose stop was seen stays here, flagged,
    /// until its marker says it is confirmed.
    held: BTreeMap<String, OrphanReport>,
    /// Confirmations owed to markers, to be written by the next effects.
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

    /// Forget held orphans whose markers say they are freed, and owe a
    /// confirmation again to those whose stop was seen but whose marker
    /// still fences: the write failed, or a restart lost it.
    pub(super) fn release(&mut self, dispatches: &[String]) {
        for dispatch in dispatches {
            if self.held.remove(dispatch).is_some() {
                self.journal_dirty = true;
            }
        }
        let owed: Vec<OrphanReport> = self
            .held
            .values()
            .filter(|held| {
                held.stop_seen
                    && !self
                        .confirmed
                        .iter()
                        .any(|pending| pending.dispatch_id == held.dispatch_id)
            })
            .cloned()
            .collect();
        self.confirmed.extend(owed);
    }

    /// The coordinator knew the agent of a report that named none: the
    /// marker is to carry it too.
    pub(super) fn note_named(&mut self, report: OrphanReport) {
        self.named.push(report);
    }

    pub(super) fn held(&self) -> impl Iterator<Item = &OrphanReport> {
        self.held.values()
    }

    /// `agent` of `session` stopped: every held orphan of that agent is owed
    /// its confirmation.
    pub(super) fn note_stop(&mut self, session_id: &str, agent_id: &str) {
        for held in self.held.values_mut() {
            if held.session_id == session_id
                && held.agent_id.as_deref() == Some(agent_id)
                && !held.stop_seen
            {
                held.stop_seen = true;
                self.confirmed.push(held.clone());
                self.journal_dirty = true;
            }
        }
    }

    /// Register a lost worker. `prior_stop` is what the caller found in
    /// the coordinator's other records: a stop remembered for the agent, or
    /// the dispatch's native state already `Stopped`.
    pub(super) fn register(
        &mut self,
        mut report: OrphanReport,
        prior_stop: PriorStop,
    ) -> OrphanRegistration {
        report.stop_seen = prior_stop == PriorStop::Seen;
        let registration = if report.stop_seen {
            self.confirmed.push(report.clone());
            OrphanRegistration::Confirmed
        } else {
            OrphanRegistration::Held
        };
        self.held.insert(report.dispatch_id.clone(), report);
        self.journal_dirty = true;
        registration
    }

    /// What must be written now, and nothing the next call would repeat.
    pub(super) fn take_effects(
        &mut self,
        journal: Option<&Path>,
        fence: &Arc<dyn OrphanFence>,
    ) -> OrphanEffects {
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
            fence: Some(Arc::clone(fence)),
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
            stop_seen: false,
        }
    }

    fn stopped(mut report: OrphanReport) -> OrphanReport {
        report.stop_seen = true;
        report
    }

    fn fence() -> Arc<dyn OrphanFence> {
        Arc::new(NoFence)
    }

    #[test]
    fn a_stop_for_the_agent_owes_a_confirmation_and_the_orphan_stays_held() {
        let mut table = OrphanTable::default();
        assert_eq!(
            table.register(report("d1", Some("a1")), PriorStop::Unseen),
            OrphanRegistration::Held
        );
        table.note_stop("sess", "other");
        assert!(table.take_effects(None, &fence()).confirmed.is_empty());
        table.note_stop("sess", "a1");
        let effects = table.take_effects(None, &fence());
        assert_eq!(effects.confirmed, vec![stopped(report("d1", Some("a1")))]);
        assert_eq!(
            table.held().cloned().collect::<Vec<_>>(),
            vec![stopped(report("d1", Some("a1")))],
            "held until the marker says it is confirmed"
        );
        table.note_stop("sess", "a1");
        assert!(table.take_effects(None, &fence()).confirmed.is_empty());
    }

    #[test]
    fn a_confirmation_that_was_not_written_is_owed_again_while_the_marker_fences() {
        let mut table = OrphanTable::default();
        table.register(report("d1", Some("a1")), PriorStop::Unseen);
        table.note_stop("sess", "a1");
        assert_eq!(table.take_effects(None, &fence()).confirmed.len(), 1);
        table.release(&[]);
        assert_eq!(
            table.take_effects(None, &fence()).confirmed,
            vec![stopped(report("d1", Some("a1")))]
        );
        table.release(&["d1".to_string()]);
        assert_eq!(table.held().count(), 0);
        assert!(table.take_effects(None, &fence()).confirmed.is_empty());
    }

    #[test]
    fn a_stop_the_caller_found_confirms_at_registration_and_stays_held() {
        let mut table = OrphanTable::default();
        assert_eq!(
            table.register(report("d1", Some("a1")), PriorStop::Seen),
            OrphanRegistration::Confirmed
        );
        assert_eq!(table.held().count(), 1);
        assert_eq!(table.take_effects(None, &fence()).confirmed.len(), 1);
    }

    #[test]
    fn an_orphan_without_an_agent_is_confirmed_by_no_stop() {
        let mut table = OrphanTable::default();
        table.register(report("d1", None), PriorStop::Unseen);
        table.note_stop("sess", "a1");
        assert!(table.take_effects(None, &fence()).confirmed.is_empty());
        assert_eq!(table.held().count(), 1);
    }

    #[test]
    fn the_journal_is_written_only_when_the_held_set_changed() {
        let mut table = OrphanTable::default();
        let path = Path::new("/journal.json");
        assert!(table.take_effects(Some(path), &fence()).journal.is_none());
        table.register(report("d1", Some("a1")), PriorStop::Unseen);
        let snapshot = table
            .take_effects(Some(path), &fence())
            .journal
            .expect("journal");
        assert_eq!(snapshot.path, path);
        assert_eq!(snapshot.held, vec![report("d1", Some("a1"))]);
        assert!(table.take_effects(Some(path), &fence()).journal.is_none());
    }

    #[test]
    fn an_older_snapshot_applied_late_does_not_overwrite_a_newer_one() {
        let dir = crate::test_support::temp_dir("orphan-journal");
        let path = dir.join("journal.json");
        let mut table = OrphanTable::default();
        table.register(report("d1", Some("a1")), PriorStop::Unseen);
        let older = table.take_effects(Some(&path), &fence());
        table.register(report("d2", Some("a2")), PriorStop::Unseen);
        let newer = table.take_effects(Some(&path), &fence());
        newer.apply("2026-01-01T00:00:00+00:00");
        older.apply("2026-01-01T00:00:00+00:00");
        assert_eq!(read_journal(&path).len(), 2);
    }

    #[test]
    fn a_journal_that_does_not_parse_is_kept_aside_not_overwritten() {
        let dir = crate::test_support::temp_dir("orphan-journal-corrupt");
        let path = dir.join("inplace-orphans.json");
        std::fs::write(&path, "{ not json").expect("journal");
        assert!(read_journal(&path).is_empty());
        assert!(!path.exists());
        let aside = dir.join("inplace-orphans.json.corrupt");
        assert_eq!(std::fs::read_to_string(aside).expect("kept"), "{ not json");
    }
}
