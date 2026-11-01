//! Per-check timings (decisions `one-verdict-per-tree`, t09): one
//! `check_timings` row per run of a verification command, retries
//! included, so what skipping or caching a check would save can be
//! measured. A row holds instants and durations only; the check's log and
//! verdict stay in the evidence rows and the receipt.

use rusqlite::params;

use crate::ids::RunId;

use super::{Ledger, Result};

/// One run of one verification command, as the ledger keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckTimingRow<'a> {
    /// The log prefix the run was verified under: `base`, `attempt1`, ...
    pub phase: &'a str,
    pub label: &'a str,
    /// 0 for the first run, n for the n-th retry.
    pub attempt_index: u32,
    pub queued_at: &'a str,
    pub admitted_at: Option<&'a str>,
    pub started_at: Option<&'a str>,
    pub ended_at: &'a str,
    pub slot_wait_ms: u64,
    pub run_ms: u64,
    /// Reused, not run now: a report never counts it as work.
    pub cached: bool,
}

/// Per-label aggregate of the timings since a date: what `relais report`
/// prints.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CheckTimingStats {
    pub label: String,
    pub cached: bool,
    pub runs: u64,
    /// Rows with `attempt_index > 0`.
    pub retries: u64,
    pub run_ms_p50: u64,
    pub run_ms_p90: u64,
    pub slot_wait_ms_p50: u64,
    pub slot_wait_ms_p90: u64,
}

impl Ledger {
    /// Record one run of a check; the same (run, phase, label, attempt)
    /// again replaces the row, so a re-entered verification is safe.
    pub fn record_check_timing(&self, run_id: &RunId, row: &CheckTimingRow<'_>) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO check_timings
                (run_id, phase, label, attempt_index, queued_at, admitted_at, started_at,
                 ended_at, slot_wait_ms, run_ms, cached)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                run_id.as_str(),
                row.phase,
                row.label,
                i64::from(row.attempt_index),
                row.queued_at,
                row.admitted_at,
                row.started_at,
                row.ended_at,
                i64::try_from(row.slot_wait_ms).unwrap_or(i64::MAX),
                i64::try_from(row.run_ms).unwrap_or(i64::MAX),
                i64::from(row.cached),
            ],
        )?;
        Ok(())
    }

    /// Per-label, per-`cached` aggregates of every row whose run ended at or
    /// after `since` (RFC 3339), sorted by label then cached. Percentiles are
    /// nearest-rank over the rows of the group.
    pub fn check_timing_stats(&self, since: &str) -> Result<Vec<CheckTimingStats>> {
        let mut stmt = self.conn.prepare(
            "SELECT label, cached, attempt_index, run_ms, slot_wait_ms
               FROM check_timings
              WHERE ended_at >= ?1
              ORDER BY label, cached",
        )?;
        let rows = stmt
            .query_map(params![since], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)? != 0,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        // (label, cached) -> (attempt index, run ms, slot wait ms) per row.
        type Group = ((String, bool), Vec<(i64, u64, u64)>);
        let mut groups: Vec<Group> = Vec::new();
        for (label, cached, attempt, run_ms, wait_ms) in rows {
            let entry = (attempt, run_ms.max(0) as u64, wait_ms.max(0) as u64);
            match groups.last_mut() {
                Some((key, items)) if key.0 == label && key.1 == cached => items.push(entry),
                _ => groups.push(((label, cached), vec![entry])),
            }
        }
        Ok(groups
            .into_iter()
            .map(|((label, cached), items)| {
                let mut run: Vec<u64> = items.iter().map(|i| i.1).collect();
                let mut wait: Vec<u64> = items.iter().map(|i| i.2).collect();
                CheckTimingStats {
                    label,
                    cached,
                    runs: items.len() as u64,
                    retries: items.iter().filter(|i| i.0 > 0).count() as u64,
                    run_ms_p50: nearest_rank(&mut run, 50),
                    run_ms_p90: nearest_rank(&mut run, 90),
                    slot_wait_ms_p50: nearest_rank(&mut wait, 50),
                    slot_wait_ms_p90: nearest_rank(&mut wait, 90),
                }
            })
            .collect())
    }
}

/// The nearest-rank percentile `p` of `values` (sorted in place); 0 for
/// none.
fn nearest_rank(values: &mut [u64], p: usize) -> u64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let rank = (p * values.len()).div_ceil(100).max(1);
    values[rank - 1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TaskId;

    fn ledger_with_run() -> (Ledger, RunId, crate::test_support::TempDir) {
        let dir = crate::test_support::short_temp_dir("timings");
        let ledger = Ledger::open(&dir.to_path_buf().join("ledger.sqlite")).expect("ledger");
        let run = RunId::from_stored("run-timings");
        ledger
            .insert_run(
                &run,
                "/repo",
                None,
                &TaskId::from_stored("run-timings"),
                "rk",
            )
            .expect("run");
        (ledger, run, dir)
    }

    fn row<'a>(
        label: &'a str,
        attempt_index: u32,
        run_ms: u64,
        cached: bool,
    ) -> CheckTimingRow<'a> {
        CheckTimingRow {
            phase: "attempt1",
            label,
            attempt_index,
            queued_at: "2026-11-01T10:00:00.000Z",
            admitted_at: Some("2026-11-01T10:00:01.000Z"),
            started_at: Some("2026-11-01T10:00:01.000Z"),
            ended_at: "2026-11-01T10:01:00.000Z",
            slot_wait_ms: 1000,
            run_ms,
            cached,
        }
    }

    /// Rows aggregate per label and per `cached`, retries counted, and a
    /// row recorded twice replaces itself.
    #[test]
    fn timings_aggregate_per_label_and_cached() {
        let (ledger, run, _dir) = ledger_with_run();
        ledger
            .record_check_timing(&run, &row("make test", 0, 100, false))
            .expect("row");
        ledger
            .record_check_timing(&run, &row("make test", 1, 300, false))
            .expect("retry");
        ledger
            .record_check_timing(&run, &row("make test", 1, 300, false))
            .expect("again");
        let cached = CheckTimingRow {
            phase: "base",
            ..row("make test", 0, 50, true)
        };
        ledger.record_check_timing(&run, &cached).expect("cached");
        let stats = ledger
            .check_timing_stats("2026-11-01T00:00:00Z")
            .expect("stats");
        assert_eq!(stats.len(), 2, "{stats:?}");
        let live = &stats[0];
        assert!(!live.cached);
        assert_eq!((live.runs, live.retries), (2, 1));
        assert_eq!((live.run_ms_p50, live.run_ms_p90), (100, 300));
        assert_eq!(live.slot_wait_ms_p90, 1000);
        assert!(stats[1].cached);
        assert_eq!(stats[1].runs, 1);
        assert!(ledger
            .check_timing_stats("2026-11-02T00:00:00Z")
            .expect("later")
            .is_empty());
    }

    #[test]
    fn nearest_rank_is_the_ceiling_rank() {
        assert_eq!(nearest_rank(&mut [], 50), 0);
        assert_eq!(nearest_rank(&mut [7], 90), 7);
        assert_eq!(nearest_rank(&mut [1, 2, 3, 4], 50), 2);
        assert_eq!(nearest_rank(&mut [1, 2, 3, 4, 5, 6, 7, 8, 9, 10], 90), 9);
    }
}
