//! Resource admission records: group bindings, run epochs and the
//! measured disk growth of checks.

use rusqlite::OptionalExtension;

use super::{Ledger, LedgerError, Result};

/// A lease tied to a process group, as the ledger keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupBindingRow {
    pub dispatch_id: String,
    pub run_id: String,
    pub session_id: String,
    pub runner: u32,
    pub runner_started: Option<String>,
    pub pgid: u32,
    pub leader_started: Option<String>,
    pub tokens: u32,
}

/// The ledger is where a check's measured growth is kept (SPEC §23).
impl crate::disk::Growth for Ledger {
    fn last(&self, label: &str) -> Option<u64> {
        match self.check_disk_growth(label) {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!("relais: the disk estimate of `{label}` could not be read: {e}");
                None
            }
        }
    }

    fn record(&self, label: &str, bytes: u64) {
        if let Err(e) = self.record_check_disk_growth(label, bytes) {
            eprintln!("relais: the disk growth of `{label}` could not be recorded: {e}");
        }
    }
}

impl Ledger {
    /// Record that a lease is tied to a process group (replacing an
    /// earlier row of the same lease).
    pub fn put_group_binding(&self, row: &GroupBindingRow) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO group_bindings
                 (dispatch_id, run_id, session_id, runner, runner_started,
                  pgid, leader_started, tokens, bound_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                row.dispatch_id,
                row.run_id,
                row.session_id,
                i64::from(row.runner),
                row.runner_started,
                i64::from(row.pgid),
                row.leader_started,
                i64::from(row.tokens),
                self.clock.now_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Forget a lease's group binding: it was given back. Idempotent.
    pub fn remove_group_binding(&self, dispatch_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM group_bindings WHERE dispatch_id = ?1",
            [dispatch_id],
        )?;
        Ok(())
    }

    /// Every group binding on record: the holds a restarted coordinator
    /// takes back.
    pub fn group_bindings(&self) -> Result<Vec<GroupBindingRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT dispatch_id, run_id, session_id, runner, runner_started,
                    pgid, leader_started, tokens
             FROM group_bindings ORDER BY bound_at, dispatch_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })?;
        let mut out = Vec::new();
        for raw in rows {
            let (
                dispatch_id,
                run_id,
                session_id,
                runner,
                runner_started,
                pgid,
                leader_started,
                tokens,
            ) = raw?;
            let corrupt = |what: &str, value: i64| LedgerError::Corrupt {
                what: format!("the {what} of group binding {dispatch_id}"),
                detail: format!("`{value}` is out of range"),
            };
            out.push(GroupBindingRow {
                runner: u32::try_from(runner).map_err(|_| corrupt("runner", runner))?,
                pgid: u32::try_from(pgid).map_err(|_| corrupt("pgid", pgid))?,
                tokens: u32::try_from(tokens).map_err(|_| corrupt("tokens", tokens))?,
                dispatch_id,
                run_id,
                session_id,
                runner_started,
                leader_started,
            });
        }
        Ok(out)
    }

    /// The bytes the check `label` grew its tree by the last time it ran,
    /// if it was ever measured.
    pub fn check_disk_growth(&self, label: &str) -> Result<Option<u64>> {
        let bytes: Option<i64> = self
            .conn
            .query_row(
                "SELECT bytes FROM check_disk_growth WHERE label = ?1",
                [label],
                |row| row.get(0),
            )
            .optional()?;
        Ok(bytes.map(|bytes| u64::try_from(bytes).unwrap_or(0)))
    }

    /// Remember how far the check `label` grew its tree this time; the
    /// next lease of the label reserves this much.
    pub fn record_check_disk_growth(&self, label: &str, bytes: u64) -> Result<()> {
        let tx = self.write_tx()?;
        tx.execute(
            "INSERT OR REPLACE INTO check_disk_growth (label, bytes, recorded_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![
                label,
                i64::try_from(bytes).unwrap_or(i64::MAX),
                self.clock.now_rfc3339()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Count one more start of this run's runner and say which one it is:
    /// 0 the first, 1 the first resume, and so on. Heavy lease ids carry
    /// it, so a resumed run never asks for the id of a lease that settled
    /// before the resume.
    pub fn next_run_epoch(&self, run_id: &str) -> Result<u32> {
        let tx = self.write_tx()?;
        let previous: Option<i64> = tx
            .query_row(
                "SELECT epoch FROM run_epochs WHERE run_id = ?1",
                [run_id],
                |row| row.get(0),
            )
            .optional()?;
        let epoch = previous.map_or(0, |epoch| epoch.saturating_add(1));
        tx.execute(
            "INSERT OR REPLACE INTO run_epochs (run_id, epoch) VALUES (?1, ?2)",
            rusqlite::params![run_id, epoch],
        )?;
        tx.commit()?;
        Ok(u32::try_from(epoch).unwrap_or(u32::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::temp_ledger;
    use super::*;

    #[test]
    fn a_checks_disk_growth_is_kept_per_label_and_replaced_by_the_latest() {
        let (ledger, _dir) = temp_ledger();
        assert_eq!(ledger.check_disk_growth("make test").expect("read"), None);
        ledger
            .record_check_disk_growth("make test", 10)
            .expect("record");
        ledger
            .record_check_disk_growth("make lint", 3)
            .expect("record");
        assert_eq!(
            ledger.check_disk_growth("make test").expect("read"),
            Some(10)
        );
        ledger
            .record_check_disk_growth("make test", 42)
            .expect("record");
        assert_eq!(
            ledger.check_disk_growth("make test").expect("read"),
            Some(42)
        );
        assert_eq!(
            ledger.check_disk_growth("make lint").expect("read"),
            Some(3)
        );
    }

    #[test]
    fn group_bindings_are_kept_until_removed_and_run_starts_are_counted() {
        let (ledger, _dir) = temp_ledger();
        let row = GroupBindingRow {
            dispatch_id: "run-1-heavy-base-cmd0-cmd0-1".into(),
            run_id: "run-1".into(),
            session_id: "tab".into(),
            runner: 4242,
            runner_started: Some("1.000001".into()),
            pgid: 4343,
            leader_started: None,
            tokens: 2,
        };
        ledger.put_group_binding(&row).expect("put");
        ledger.put_group_binding(&row).expect("put again replaces");
        assert_eq!(ledger.group_bindings().expect("rows"), vec![row.clone()]);
        ledger
            .remove_group_binding(&row.dispatch_id)
            .expect("remove");
        ledger
            .remove_group_binding(&row.dispatch_id)
            .expect("idempotent");
        assert!(ledger.group_bindings().expect("rows").is_empty());

        assert_eq!(ledger.next_run_epoch("run-1").expect("epoch"), 0);
        assert_eq!(ledger.next_run_epoch("run-1").expect("epoch"), 1);
        assert_eq!(ledger.next_run_epoch("run-2").expect("epoch"), 0);
    }
}
