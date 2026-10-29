//! The ledger's schema: the migration machinery that brings a ledger up to
//! this binary's version. The steps themselves are in `migrations`.

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

use super::{Actor, Clock, LedgerError, Result};

pub(super) mod migrations;
use migrations::MIGRATIONS;

/// The version of the step that types the actor; it needs a backup and a
/// pre-check that a SQL batch cannot do.
pub(super) const ACTOR_STEP: &str = "v27";

/// The step that adds the repository grant's records. A binary from before
/// it refuses a ledger past its own version, so a rollback restores the
/// copy this step leaves beside the ledger; runs recorded after the
/// upgrade are lost with it.
pub(super) const TRUST_STEP: &str = "v30";

/// How many migration steps this ledger has applied.
pub(super) fn applied_count(conn: &Connection) -> Result<u64> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
        row.get(0)
    })?;
    Ok(count as u64)
}

/// Bring a ledger up to this binary's schema.
///
/// Each step is ONE transaction covering its DDL batch and the
/// `schema_migrations` row that records it (P1). Two consequences, both
/// load-bearing: a crash in the middle of a step rolls the whole step
/// back, so the next open does not meet half a table and fail forever
/// with "table already exists"; and `BEGIN IMMEDIATE` makes two
/// processes opening a fresh ledger serialise on the write lock instead
/// of racing between the check and the apply — the second one re-checks
/// INSIDE its transaction and finds the step already applied.
pub(super) fn migrate(conn: &Connection, clock: &dyn Clock) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version TEXT PRIMARY KEY,
            applied_at TEXT NOT NULL
        )",
        [],
    )?;
    // Migrations are additive and never edited, so more applied steps
    // than this binary ships means a NEWER relais owns this ledger.
    // Refuse before touching it: its extra tables and columns are not
    // ours to write through.
    let applied = applied_count(conn)?;
    if applied > LEDGER_SCHEMA_VERSION {
        return Err(LedgerError::SchemaAhead {
            found: applied,
            known: LEDGER_SCHEMA_VERSION,
        });
    }
    for (version, sql) in MIGRATIONS {
        if *version == ACTOR_STEP {
            prepare_actor_step(conn)?;
        }
        if *version == TRUST_STEP {
            prepare_trust_step(conn)?;
        }
        apply_step(conn, version, sql, &clock.now_rfc3339())?;
    }
    Ok(())
}

/// Before v27 runs: refuse, naming the spelling, if `decisions` or
/// `human_signoffs` holds an actor the step does not map, and back the
/// ledger up with `VACUUM INTO` (a file copy can miss WAL pages), then
/// rename the copy into place. Does nothing when the step is already
/// applied or the ledger holds no answer yet (a fresh file).
pub(super) fn prepare_actor_step(conn: &Connection) -> Result<()> {
    let pending: Option<String> = conn
        .query_row(
            "SELECT version FROM schema_migrations WHERE version = ?1",
            [ACTOR_STEP],
            |row| row.get(0),
        )
        .optional()?;
    if pending.is_some() {
        return Ok(());
    }
    let has_table = |name: &str| -> Result<bool> {
        Ok(conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [name],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    };
    for table in ["decisions", "human_signoffs"] {
        if !has_table(table)? {
            continue;
        }
        let mut stmt = conn.prepare(&format!(
            "SELECT DISTINCT actor FROM {table} WHERE actor IS NOT NULL"
        ))?;
        let spellings = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if let Some(unmapped) = spellings
            .iter()
            .find(|text| Actor::from_stored(text).is_none())
        {
            return Err(LedgerError::Corrupt {
                what: format!("an actor in `{table}`"),
                detail: format!(
                    "`{unmapped}` is not a spelling schema step {ACTOR_STEP} maps to an actor; \
                     the ledger was left untouched"
                ),
            });
        }
    }
    // A ledger with no earlier answer has nothing to lose — and a fresh
    // file several openers are creating at once must not race to back
    // itself up.
    let mut rows = 0_i64;
    for table in ["decisions", "human_signoffs"] {
        if has_table(table)? {
            rows += conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })?;
        }
    }
    if rows == 0 {
        return Ok(());
    }
    let Some(path) = conn.path().filter(|p| !p.is_empty()) else {
        return Ok(());
    };
    let backup = format!("{path}.bak-{ACTOR_STEP}");
    let partial = format!("{backup}.partial");
    match std::fs::remove_file(&partial) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    conn.execute("VACUUM INTO ?1", [&partial])?;
    std::fs::rename(&partial, &backup)?;
    Ok(())
}

/// Before v30 runs: write a copy of the ledger file beside it
/// (`<ledger>.bak-v30`, by `VACUUM INTO`, because a file copy can miss WAL
/// pages), so a rollback to the previous relais has a ledger it can read.
/// Does nothing when the step is already applied or for an in-memory
/// ledger. A ledger that holds no run is skipped too, because a fresh file
/// several openers are creating at once, whose earlier steps this same
/// open just applied, must not race to back itself up; such a ledger may
/// still hold other rows (trust grants, learning rows), and those are not
/// copied. A copy left by an earlier
/// attempt is replaced, so the copy is always of the ledger as it was just
/// before the step.
pub(super) fn prepare_trust_step(conn: &Connection) -> Result<()> {
    let applied = |version: &str| -> Result<bool> {
        Ok(conn
            .query_row(
                "SELECT 1 FROM schema_migrations WHERE version = ?1",
                [version],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    };
    if applied(TRUST_STEP)? || applied_count(conn)? == 0 {
        return Ok(());
    }
    let runs: i64 = conn.query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))?;
    if runs == 0 {
        return Ok(());
    }
    let Some(path) = conn.path().filter(|p| !p.is_empty()) else {
        return Ok(());
    };
    let backup = format!("{path}.bak-{TRUST_STEP}");
    // A name of this process's own, so two openers upgrading at once do
    // not write the same partial file.
    let partial = format!("{backup}.partial-{}", std::process::id());
    match std::fs::remove_file(&partial) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    conn.execute("VACUUM INTO ?1", [&partial])?;
    std::fs::rename(&partial, &backup)?;
    Ok(())
}

/// One migration step, all or nothing.
pub(super) fn apply_step(conn: &Connection, version: &str, sql: &str, now: &str) -> Result<()> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let already: Option<String> = tx
        .query_row(
            "SELECT version FROM schema_migrations WHERE version = ?1",
            [version],
            |row| row.get(0),
        )
        .optional()?;
    if already.is_none() {
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            params![version, now],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub const LEDGER_SCHEMA_VERSION: u64 = 45;

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::super::*;
    use super::*;
    use crate::ids::{RunId, TaskId, TrialId};
    use crate::lifecycle::State;
    use crate::money::MicroUsd;
    use crate::outcome::{Outcome, OutcomeDetail, OutcomeKind};
    use crate::policy::Tier;
    use rusqlite::params;

    /// The v6 migration's backfill names a state by its quoted, stored
    /// spelling; `State::awaits_a_person` is the single definition of
    /// which states those are. This is the converse of `lifecycle`'s own
    /// test: that one asserts what the enum method answers, this one
    /// asserts the migration names no state the method disagrees with,
    /// so the two cannot drift apart silently.
    #[test]
    fn the_v6_migration_names_exactly_the_states_that_await_a_person() {
        let (_, sql) = MIGRATIONS
            .iter()
            .find(|(version, _)| *version == "v6")
            .expect("v6 exists");
        for state in State::ALL {
            let quoted = format!("'{}'", state.as_str());
            assert_eq!(
                sql.contains(&quoted),
                state.awaits_a_person(),
                "state {state} disagrees with the migration text"
            );
        }
    }

    #[test]
    fn v3_turns_the_zeros_that_stood_for_unknown_into_null() {
        let dir = temp_dir("v3");
        let path = dir.join("ledger.sqlite");
        // A v2 ledger, as 0.1.1 wrote it: NOT NULL cost, unknown stored as 0.
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version TEXT PRIMARY KEY, applied_at TEXT NOT NULL);",
            )
            .expect("migrations table");
            for (version, sql) in &MIGRATIONS[..2] {
                conn.execute_batch(sql).expect("v1/v2");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, 'then')",
                    [version],
                )
                .expect("mark");
            }
            conn.execute_batch(
                r#"INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                   VALUES ('run-old', '/r', 'accepted', 't', 't');
                   INSERT INTO usage_events (event_id, run_id, cost_micros, cost_kind, completeness, at)
                   VALUES ('e-known', 'run-old', 500, '"api_spend"', '"actual"', 't'),
                          ('e-unknown', 'run-old', 0, '"api_spend"', '"unknown"', 't');"#,
            )
            .expect("old rows");
        }
        let ledger = Ledger::open(&path).expect("migrates");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        let unknown: Option<i64> = ledger
            .conn
            .query_row(
                "SELECT cost_micros FROM usage_events WHERE event_id = 'e-unknown'",
                [],
                |row| row.get(0),
            )
            .expect("row");
        assert_eq!(unknown, None);
        let known: Option<i64> = ledger
            .conn
            .query_row(
                "SELECT cost_micros FROM usage_events WHERE event_id = 'e-known'",
                [],
                |row| row.get(0),
            )
            .expect("row");
        assert_eq!(known, Some(500), "reported cost survives the rebuild");
        assert_eq!(
            ledger.run_cost(&run("run-old")).expect("cost"),
            MicroUsd::from_micros(500)
        );
        // The index came back with the table.
        let indexed: i64 = ledger
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_usage_run'",
                [],
                |row| row.get(0),
            )
            .expect("index");
        assert_eq!(indexed, 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// P1: a migration step is one transaction over its DDL batch and
    /// the row that records it. A failure inside the batch leaves the
    /// ledger exactly as it was — not half a v3 that every later open
    /// trips over with "table already exists".
    #[test]
    fn a_failed_migration_step_leaves_nothing_behind() {
        let dir = temp_dir("partial");
        let path = dir.join("ledger.sqlite");
        let conn = Connection::open(&path).expect("open");
        conn.execute(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version TEXT PRIMARY KEY, applied_at TEXT NOT NULL)",
            [],
        )
        .expect("migrations table");
        // A step that creates a table and then fails, the shape of a
        // crash in the middle of the real v3 rebuild.
        let broken = "CREATE TABLE half_built (id INTEGER);
                      INSERT INTO nowhere_at_all (id) VALUES (1);";
        assert!(
            apply_step(&conn, "v99", broken, "now").is_err(),
            "the step must fail"
        );
        let built: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'half_built'",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(built, 0, "the table the step created was rolled back");
        assert_eq!(
            applied_count(&conn).expect("count"),
            0,
            "and the step is not recorded as applied"
        );
        // The same step, fixed, still applies afterwards.
        apply_step(&conn, "v99", "CREATE TABLE half_built (id INTEGER);", "now")
            .expect("a working step applies");
        assert_eq!(applied_count(&conn).expect("count"), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn migrations_are_idempotent_and_additive() {
        let (ledger, dir) = temp_ledger();
        assert_eq!(
            ledger.schema_version().expect("count"),
            LEDGER_SCHEMA_VERSION
        );
        drop(ledger);
        let reopened = Ledger::open(&dir.join("ledger.sqlite")).expect("reopen");
        assert_eq!(
            reopened.schema_version().expect("count"),
            LEDGER_SCHEMA_VERSION,
            "no double-apply"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The v15 migration only adds a new table and its indexes — nothing
    /// it does touches an existing row, so an upgrade from a pre-v15
    /// ledger keeps every row exactly as it stood before.
    #[test]
    fn upgrading_to_v15_keeps_every_existing_row() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-pre-v15"), "/repo", None, &task("pre"), "rk")
            .expect("run");
        drop(ledger);
        let reopened = Ledger::open(&dir.join("ledger.sqlite")).expect("reopen applies v15");
        assert_eq!(
            reopened.schema_version().expect("count"),
            LEDGER_SCHEMA_VERSION
        );
        let (id, _, status, _) = reopened
            .runs_since("2000-01-01T00:00:00Z")
            .expect("runs")
            .into_iter()
            .find(|(id, ..)| *id == run("run-pre-v15"))
            .expect("the pre-v15 row is still there");
        assert_eq!(id, run("run-pre-v15"));
        assert_eq!(status, State::Prepared.as_str());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_ledger_from_a_newer_relais_is_refused() {
        let (ledger, dir) = temp_ledger();
        drop(ledger);
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("raw open");
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES ('v99', 'then')",
                [],
            )
            .expect("a migration this relais does not ship");
        }
        match Ledger::open(&path) {
            Err(LedgerError::SchemaAhead { found, known }) => {
                assert_eq!(
                    (found, known),
                    (LEDGER_SCHEMA_VERSION + 1, LEDGER_SCHEMA_VERSION)
                );
                let message = LedgerError::SchemaAhead { found, known }.to_string();
                assert!(message.contains(&found.to_string()), "{message}");
                assert!(message.contains(&known.to_string()), "{message}");
            }
            other => panic!(
                "a newer ledger must be refused, got {:?}",
                other.map(|_| ())
            ),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_v1_ledger_upgrades_additively_and_keeps_its_rows() {
        let (ledger, dir) = temp_ledger();
        drop(ledger);
        let path = dir.join("ledger.sqlite");
        // Rewind to v1: drop the v2 columns' migration record and the
        // columns themselves, as a ledger written by an older relais
        // would look.
        {
            let conn = Connection::open(&path).expect("raw open");
            conn.execute_batch(
                "DROP INDEX idx_runs_parent;
                 ALTER TABLE runs DROP COLUMN parent_run;
                 ALTER TABLE runs DROP COLUMN package_id;
                 DELETE FROM schema_migrations WHERE version = 'v2';
                 INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                 VALUES ('old-run', '/r', 'accepted', '2026-01-01T00:00:00+00:00',
                         '2026-01-01T00:00:00+00:00');",
            )
            .expect("rewind");
        }
        let upgraded = Ledger::open(&path).expect("upgrade");
        assert_eq!(
            upgraded.schema_version().expect("count"),
            LEDGER_SCHEMA_VERSION
        );
        assert_eq!(
            upgraded.run_status(&run("old-run")).expect("status"),
            Some(State::Accepted),
            "existing rows survive the additive migration"
        );
        assert!(upgraded
            .child_runs(&run("old-run"))
            .expect("children")
            .is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A v3 ledger — the shape every relais installed before the task
    /// spine wrote — upgrades to v4 keeping every row, every run's cost,
    /// AND gets a `task-legacy-<root run id>` task backfilled onto its
    /// whole tree: the root and the package it decomposed into.
    /// A ledger already migrated to v4 — every machine that ran the
    /// task-spine release — gains the outcome columns from v5. The
    /// columns first landed as an EDIT to the applied v4 step, where
    /// `apply_step` skips them, so they existed only in tests, which
    /// always start from nothing (found in review of
    /// run-65c1620e17aec-1000062a1).
    /// A ledger already at v6 — every machine that ran `relais decide`
    /// before this step — has its open decisions repointed at the
    /// transition that RAISED them. v6 backfilled from the run's last
    /// transition, which for a retired run is the `worktree_retired`
    /// row: `approve` reads the transition `raised_at` names, so it
    /// would have found no gaps on a run that has them.
    #[test]
    fn a_v6_backfilled_decision_is_repointed_at_its_raising_transition() {
        let dir = crate::test_support::temp_dir("ledger-v6-to-v7");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("ledger.sqlite");
        {
            // v6 exactly as it shipped: every step but the last.
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            conn.execute_batch(
                "INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                 VALUES ('run-w', '/repo', 'needs_decision', '2026-01-01T00:00:00+00:00',
                         '2026-01-01T00:02:00+00:00');",
            )
            .ok();
            // Pinned to v6, not `MIGRATIONS.len() - 1`: a step appended
            // after v7 must not silently move this fixture past the
            // v6-to-v7 boundary this test exists to prove.
            for (version, sql) in &MIGRATIONS[..6] {
                conn.execute_batch(sql).expect("step");
                if version == &"v1" {
                    // The run, then the wait, then the retirement that
                    // follows every real wait.
                    conn.execute_batch(
                        "INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                         VALUES ('run-w', '/repo', 'needs_decision',
                                 '2026-01-01T00:00:00+00:00', '2026-01-01T00:02:00+00:00');
                         INSERT INTO transitions (run_id, from_state, to_state, reason, detail_json, at)
                         VALUES ('run-w', 'verifying', 'needs_decision', 'verification_gap',
                                 '{\"gaps\":[\"declared-check: status `inert`\"]}',
                                 '2026-01-01T00:01:00+00:00');
                         INSERT INTO transitions (run_id, from_state, to_state, reason, detail_json, at)
                         VALUES ('run-w', 'needs_decision', 'needs_decision', 'worktree_retired',
                                 '{\"reference\":\"refs/relais/candidates/run-w/final\"}',
                                 '2026-01-01T00:02:00+00:00');",
                    )
                    .expect("fixture rows");
                }
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "2026-01-01T00:00:00+00:00"],
                )
                .expect("record");
            }
            let raised: String = conn
                .query_row(
                    "SELECT raised_reason FROM decisions WHERE run = 'run-w'",
                    [],
                    |row| row.get(0),
                )
                .expect("v6 backfilled a row");
            assert_eq!(
                raised, "worktree_retired",
                "v6 read the run's LAST transition — the defect this step corrects"
            );
        }

        let ledger = Ledger::open(&path).expect("upgrade to v7");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        let decision = ledger
            .decision_of_run(&RunId::from_stored("run-w"))
            .expect("query")
            .expect("still open");
        assert_eq!(
            decision.raised_reason, "verification_gap",
            "the row now names the transition that RAISED the wait"
        );
        assert_eq!(decision.raised_at, "2026-01-01T00:01:00+00:00");
        // Best effort: the fixture is a temp dir; a leftover costs
        // nothing but disk, and the next run pre-cleans it.
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_v4_ledger_gains_the_outcome_columns() {
        let dir = crate::test_support::temp_dir("ledger-v4-to-v5");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("ledger.sqlite");
        {
            // v4 exactly as it shipped: apply every step but the last.
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            // Pinned to v4, not `MIGRATIONS.len() - 1`: a step appended
            // after v5 must not silently move this fixture past the
            // v4-to-v5 boundary this test exists to prove.
            for (version, sql) in &MIGRATIONS[..4] {
                conn.execute_batch(sql).expect("step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "2026-09-22T00:00:00+00:00"],
                )
                .expect("record");
            }
        }
        let ledger = Ledger::open(&path).expect("upgrade to v5");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        // The proof: a write that needs the new columns succeeds on a
        // ledger that was already v4.
        let columns: Vec<String> = ledger
            .conn
            .prepare("SELECT name FROM pragma_table_info('outcomes')")
            .expect("pragma")
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("rows");
        assert!(
            columns.iter().any(|c| c == "task_id"),
            "v5 adds task_id to an already-v4 ledger: {columns:?}"
        );
        assert!(
            columns.iter().any(|c| c == "candidate_sha"),
            "and candidate_sha: {columns:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A ledger pinned at exactly v9 — every machine that ran the
    /// human-signoff release and no later one — upgrades to v10 keeping
    /// its outcome rows, and can then record one with no candidate: the
    /// case `relais feedback` gained for a person-approved run that
    /// never wrote a receipt.
    #[test]
    fn a_v9_ledger_upgrades_to_v10_and_accepts_an_outcome_with_no_candidate() {
        let dir = crate::test_support::temp_dir("ledger-v9-to-v10");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("ledger.sqlite");
        {
            // v9 exactly as it shipped: apply every step but the last.
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            for (version, sql) in &MIGRATIONS[..9] {
                conn.execute_batch(sql).expect("step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "2026-09-22T00:00:00+00:00"],
                )
                .expect("record");
            }
            conn.execute(
                "INSERT INTO outcomes (run_id, task_id, candidate_sha, kind, detail_json, at)
                 VALUES ('run-old', 'task-old', 'sha-old', 'accepted_unchanged', '{}', ?1)",
                params!["2026-09-22T00:00:00+00:00"],
            )
            .expect("a row written before v10");
        }
        let ledger = Ledger::open(&path).expect("upgrade to v10");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        // The pre-existing row survived the rebuild.
        let survived: String = ledger
            .conn
            .query_row(
                "SELECT candidate_sha FROM outcomes WHERE run_id = 'run-old'",
                [],
                |row| row.get(0),
            )
            .expect("row read back");
        assert_eq!(survived, "sha-old");
        // The proof: a run accepted by a person, with no receipt, can
        // record an outcome that names no candidate.
        ledger
            .insert_run(
                &RunId::from_stored("run-a"),
                "/repo",
                None,
                &TaskId::from_stored("task-a"),
                "rk",
            )
            .expect("run");
        let outcome = Outcome::new(
            OutcomeKind::AcceptedUnchanged,
            OutcomeDetail {
                candidate_sha: None,
                strategy: crate::outcome::Strategy {
                    tier: Tier::Implementation,
                    models: vec!["sonnet".into()],
                    escalated: false,
                },
                correction_magnitude: None,
                evidence: vec![],
                actor: "a person".into(),
                note: None,
                source: None,
            },
        )
        .expect("valid outcome");
        ledger
            .record_outcome(
                &RunId::from_stored("run-a"),
                &TaskId::from_stored("task-a"),
                &outcome,
            )
            .expect("an outcome with no candidate records");
        let latest = ledger
            .latest_outcome(&TaskId::from_stored("task-a"))
            .expect("latest")
            .expect("recorded");
        assert_eq!(latest.outcome.detail.candidate_sha, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_v10_ledger_upgrades_to_v11_keeping_every_evidence_row_readable() {
        let dir = crate::test_support::temp_dir("ledger-v10-to-v11");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("ledger.sqlite");
        {
            // v10 exactly as it shipped: apply every step but the last.
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            for (version, sql) in &MIGRATIONS[..10] {
                conn.execute_batch(sql).expect("step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "2026-09-22T00:00:00+00:00"],
                )
                .expect("record");
            }
            conn.execute(
                "INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                 VALUES ('run-old', '/repo', 'accepted',
                         '2026-09-22T00:00:00+00:00', '2026-09-22T00:00:00+00:00')",
                [],
            )
            .expect("a run written before v11");
            // v10's evidence row shape: no tool/external_id/subject/
            // criterion_id columns exist yet.
            conn.execute(
                "INSERT INTO evidence (run_id, attempt_id, kind, path, sha256, at)
                 VALUES ('run-old', NULL, 'check_log', '/tmp/old.log', 'deadbeef',
                         '2026-09-22T00:00:00+00:00')",
                [],
            )
            .expect("a row written before v11");
        }
        let ledger = Ledger::open(&path).expect("upgrade to v11");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        let evidence = ledger.evidence(&run("run-old")).expect("evidence reads");
        assert_eq!(
            evidence,
            vec![EvidenceRow {
                kind: EvidenceKind::CheckLog,
                path: "/tmp/old.log".into(),
                sha256: Some("deadbeef".into()),
                tool: None,
                external_id: None,
                subject: None,
                criterion_id: None,
            }],
            "a pre-v11 row reads back under its typed kind, the new columns absent"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_v3_ledger_upgrades_to_v4_keeping_every_row_and_cost() {
        let dir = temp_dir("v3-to-v4");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version TEXT PRIMARY KEY, applied_at TEXT NOT NULL);",
            )
            .expect("migrations table");
            for (version, sql) in &MIGRATIONS[..3] {
                conn.execute_batch(sql).expect("v1/v2/v3");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, 'then')",
                    [version],
                )
                .expect("mark");
            }
            conn.execute_batch(
                r#"INSERT INTO runs (id, repo_path, status, created_at, updated_at, parent_run, package_id)
                   VALUES ('root-old', '/r', 'accepted', 't1', 't1', NULL, NULL),
                          ('pkg-old', '/r', 'accepted', 't2', 't2', 'root-old', 'a');
                   INSERT INTO usage_events (event_id, run_id, cost_micros, cost_kind, completeness, at)
                   VALUES ('e-root', 'root-old', 300, '"api_spend"', '"actual"', 't1'),
                          ('e-pkg', 'pkg-old', 700, '"api_spend"', '"actual"', 't2');"#,
            )
            .expect("v3 rows");
        }
        let root_cost_before = MicroUsd::from_micros(1000);
        let ledger = Ledger::open(&path).expect("upgrades to v4");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        assert_eq!(
            ledger.run_status(&run("root-old")).expect("status"),
            Some(State::Accepted)
        );
        assert_eq!(
            ledger.run_cost(&run("root-old")).expect("cost"),
            root_cost_before,
            "every run's cost survives the upgrade"
        );
        let root_task = ledger
            .task_of_run(&run("root-old"))
            .expect("task")
            .expect("backfilled");
        assert_eq!(root_task.as_str(), "task-legacy-root-old");
        assert_eq!(
            ledger.task_of_run(&run("pkg-old")).expect("task"),
            Some(root_task.clone()),
            "the package inherits its root's backfilled task"
        );
        let mut runs = ledger.runs_of_task(&root_task).expect("runs of task");
        runs.sort();
        assert_eq!(runs, vec![run("pkg-old"), run("root-old")]);
        let tasks = ledger
            .tasks_since("2000-01-01T00:00:00+00:00")
            .expect("tasks");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].task_id, root_task);
        assert_eq!(tasks[0].origin, TaskOrigin::Backfilled);
        assert_eq!(tasks[0].first_run, run("root-old"));
        // A migration cannot compute a repository key, so a backfilled
        // task says what it actually has. `legacy:` is unmistakably not
        // a key, so grouping by `repo_key` never merges a legacy task
        // into a repository's real bucket on the strength of a path.
        assert!(
            tasks[0].repo_key.starts_with("legacy:"),
            "a backfilled task names what it has: {}",
            tasks[0].repo_key
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A ledger frozen at exactly v12 upgrades to v13 without losing any
    /// existing row, and the new column is readable on it.
    #[test]
    fn v13_adds_routed_by_without_touching_existing_rows() {
        let dir = temp_dir("v12-to-v13");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            // Pinned to v12, not `MIGRATIONS.len() - 1`: a step appended
            // after this test is written must not silently change what
            // "at v12" means here.
            for (version, sql) in &MIGRATIONS[..12] {
                conn.execute_batch(sql).expect("apply step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "then"],
                )
                .expect("record step");
            }
            conn.execute(
                "INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                 VALUES ('run-old', '/repo', 'accepted', 'now', 'now')",
                [],
            )
            .expect("a run written before v13");
            conn.execute(
                "INSERT INTO dispatches
                    (dispatch_id, run_id, intent_json, state,
                     reserved_micros, created_at, updated_at)
                 VALUES ('pre-v13', 'run-old', '{}', 'intent', 0, 'now', 'now')",
                [],
            )
            .expect("pre-v13 row");
        }
        let ledger = Ledger::open(&path).expect("upgrade to current schema");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        assert_eq!(
            ledger
                .first_dispatch_routed_by(&run("run-old"))
                .expect("routed_by"),
            None,
            "the upgrade adds the column; it does not retroactively fill it in"
        );
        assert_eq!(
            ledger
                .first_dispatch_intent(&run("run-old"))
                .expect("intent")
                .expect("row survives the upgrade"),
            serde_json::json!({}),
            "the pre-existing row is preserved, not dropped or rewritten"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// v0.26.0 ledgers stop at main's v38 (`landing_searches`). The
    /// admission steps were written as v37/v38 on their branch and renamed
    /// v39/v40 when it was rebased; a released ledger must take them on
    /// top, keep what it had, and end at the current version.
    #[test]
    fn a_v38_ledger_takes_the_admission_steps_on_top() {
        let dir = temp_dir("v38-to-v40");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            assert_eq!(MIGRATIONS[37].0, "v38", "pinned to main's v38");
            for (version, sql) in &MIGRATIONS[..38] {
                conn.execute_batch(sql).expect("apply step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "then"],
                )
                .expect("record step");
            }
            conn.execute(
                "INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                 VALUES ('run-old', '/repo', 'accepted', 'now', 'now')",
                [],
            )
            .expect("a run written at v38");
            conn.execute(
                "INSERT INTO landing_searches (run_id, searched_to, searched_at)
                 VALUES ('run-old', 'abc', 'now')",
                [],
            )
            .expect("a v38 row");
        }
        let ledger = Ledger::open(&path).expect("upgrade to current schema");
        assert_eq!(ledger.schema_version().expect("version"), 45);
        assert_eq!(LEDGER_SCHEMA_VERSION, 45);
        let tables: Vec<String> = ledger
            .conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table'
                 AND name IN ('group_bindings', 'run_epochs', 'check_disk_growth',
                              'landing_searches')
                 ORDER BY name",
            )
            .expect("prepare")
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<std::result::Result<_, _>>()
            .expect("names");
        assert_eq!(
            tables,
            [
                "check_disk_growth",
                "group_bindings",
                "landing_searches",
                "run_epochs"
            ]
        );
        let kept: i64 = ledger
            .conn
            .query_row("SELECT COUNT(*) FROM landing_searches", [], |row| {
                row.get(0)
            })
            .expect("count");
        assert_eq!(kept, 1, "the v38 row survives the upgrade");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// v41 adds the verification snapshot table beside what a v40 ledger
    /// holds: every v40 row survives and the version is 41.
    #[test]
    fn a_v40_ledger_takes_the_snapshot_table_and_keeps_its_rows() {
        let dir = temp_dir("v40-to-v41");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            assert_eq!(MIGRATIONS[39].0, "v40", "pinned to v40");
            for (version, sql) in &MIGRATIONS[..40] {
                conn.execute_batch(sql).expect("apply step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "then"],
                )
                .expect("record step");
            }
            conn.execute(
                "INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                 VALUES ('run-old', '/repo', 'accepted', 'now', 'now')",
                [],
            )
            .expect("a run written at v40");
        }
        let ledger = Ledger::open(&path).expect("upgrade to v41");
        assert_eq!(ledger.schema_version().expect("version"), 45);
        assert_eq!(LEDGER_SCHEMA_VERSION, 45);
        let runs: i64 = ledger
            .conn
            .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
            .expect("count");
        assert_eq!(runs, 1, "the v40 row survives the upgrade");
        let snapshots: i64 = ledger
            .conn
            .query_row("SELECT COUNT(*) FROM verification_snapshots", [], |row| {
                row.get(0)
            })
            .expect("the table exists");
        assert_eq!(snapshots, 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// v42 adds the successor intent's review column and the parts manifest
    /// beside what a v41 ledger holds: every v41 row survives, an intent
    /// written before reads back with no review mode, and the version is 42.
    #[test]
    fn a_v41_ledger_takes_the_review_parts_columns_and_keeps_its_rows() {
        let dir = temp_dir("v41-to-v42");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            assert_eq!(MIGRATIONS[40].0, "v41", "pinned to v41");
            for (version, sql) in &MIGRATIONS[..41] {
                conn.execute_batch(sql).expect("apply step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "then"],
                )
                .expect("record step");
            }
            conn.execute(
                "INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                 VALUES ('run-old', '/repo', 'accepted', 'now', 'now')",
                [],
            )
            .expect("a run written at v41");
            conn.execute(
                "INSERT INTO successor_intents
                    (intent_id, decision_id, predecessor, contract_digest, contract_json,
                     scope_delta_json, state, created_at)
                 VALUES ('intent-old', 'run-old', 'run-old', 'd', '{}', '[]', 'pending', 'now')",
                [],
            )
            .expect("an intent written at v41");
        }
        let ledger = Ledger::open(&path).expect("upgrade to v42");
        assert_eq!(ledger.schema_version().expect("version"), 45);
        assert_eq!(LEDGER_SCHEMA_VERSION, 45);
        let runs: i64 = ledger
            .conn
            .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
            .expect("count");
        assert_eq!(runs, 1, "the v41 row survives the upgrade");
        let review: Option<String> = ledger
            .conn
            .query_row(
                "SELECT review FROM successor_intents WHERE intent_id = 'intent-old'",
                [],
                |row| row.get(0),
            )
            .expect("the intent survives");
        assert_eq!(review, None, "an earlier intent has no review mode");
        let parts: i64 = ledger
            .conn
            .query_row("SELECT COUNT(*) FROM review_parts", [], |row| row.get(0))
            .expect("the table exists");
        assert_eq!(parts, 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// v43 adds the holdout definitions table beside what a v42 ledger
    /// holds: every v42 row survives and the table starts empty.
    #[test]
    fn a_v42_ledger_takes_the_holdout_definitions_table_empty() {
        let dir = temp_dir("v42-to-v43");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            assert_eq!(MIGRATIONS[41].0, "v42", "pinned to v42");
            for (version, sql) in &MIGRATIONS[..42] {
                conn.execute_batch(sql).expect("apply step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "then"],
                )
                .expect("record step");
            }
            conn.execute(
                "INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                 VALUES ('run-old', '/repo', 'accepted', 'now', 'now')",
                [],
            )
            .expect("a run written at v42");
        }
        let ledger = Ledger::open(&path).expect("upgrade to v43");
        assert_eq!(ledger.schema_version().expect("version"), 45);
        let runs: i64 = ledger
            .conn
            .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
            .expect("count");
        assert_eq!(runs, 1, "the v42 row survives the upgrade");
        let definitions: i64 = ledger
            .conn
            .query_row("SELECT COUNT(*) FROM holdout_definitions", [], |row| {
                row.get(0)
            })
            .expect("the table exists");
        assert_eq!(definitions, 0);
        std::fs::remove_dir_all(&dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
    }

    /// v44 adds the holdout outcome and exposure tables beside what a v43
    /// ledger holds: every v43 row survives and both tables start empty.
    #[test]
    fn a_v43_ledger_takes_the_holdout_count_tables_empty() {
        let dir = temp_dir("v43-to-v44");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            assert_eq!(MIGRATIONS[42].0, "v43", "pinned to v43");
            for (version, sql) in &MIGRATIONS[..43] {
                conn.execute_batch(sql).expect("apply step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "then"],
                )
                .expect("record step");
            }
            conn.execute(
                "INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                 VALUES ('run-old', '/repo', 'accepted', 'now', 'now')",
                [],
            )
            .expect("a run written at v43");
        }
        let ledger = Ledger::open(&path).expect("upgrade to v44");
        assert_eq!(ledger.schema_version().expect("version"), 45);
        let runs: i64 = ledger
            .conn
            .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
            .expect("count");
        assert_eq!(runs, 1, "the v43 row survives the upgrade");
        for table in ["holdout_outcomes", "holdout_exposures"] {
            let rows: i64 = ledger
                .conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("the table exists");
            assert_eq!(rows, 0, "{table} starts empty");
        }
        std::fs::remove_dir_all(&dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
    }

    /// v45 backfills `answered` onto ended, unaborted, file-free question
    /// and review rows only, and its undo SQL restores them.
    #[test]
    fn v45_backfills_only_ended_file_free_question_and_review_rows() {
        let dir = temp_dir("v44-to-v45");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            assert_eq!(MIGRATIONS[44].0, "v45", "pinned to v45");
            for (version, sql) in &MIGRATIONS[..44] {
                conn.execute_batch(sql).expect("apply step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "then"],
                )
                .expect("record step");
            }
            let rows: [(&str, Option<&str>, &str, &str, &str); 8] = [
                (
                    "question",
                    Some("2026-10-01T00:00:00Z"),
                    "[]",
                    "[]",
                    "unknown",
                ),
                (
                    "review",
                    Some("2026-10-01T00:00:00Z"),
                    "[]",
                    r#"["respawned"]"#,
                    "unknown",
                ),
                (
                    "question",
                    Some("2026-10-01T00:00:00Z"),
                    "[]",
                    r#"["aborted"]"#,
                    "unknown",
                ),
                ("question", None, "[]", "[]", "unknown"),
                ("debug", Some("2026-10-01T00:00:00Z"), "[]", "[]", "unknown"),
                (
                    "question",
                    Some("2026-10-01T00:00:00Z"),
                    r#"["abc"]"#,
                    "[]",
                    "unknown",
                ),
                (
                    "question",
                    Some("2026-10-01T00:00:00Z"),
                    "[]",
                    "[]",
                    "corrected",
                ),
                (
                    "review",
                    Some("2026-10-01T00:00:00Z"),
                    "[]",
                    r#"["aborted_late"]"#,
                    "unknown",
                ),
            ];
            for (index, (kind, ended, files, inferred, outcome)) in rows.iter().enumerate() {
                conn.execute(
                    "INSERT INTO router_tasks (session, task_id, agent_id, started_at, ended_at,
                        class_kind, outcome, outcome_rank, inferred_json, files_json,
                        escalations, exhausted, turns, recorded_at, updated_at, outcome_source,
                        completed_at_end)
                     VALUES ('s', ?1, '', '2026-10-01T00:00:00Z', ?2, ?3, ?4, 0, ?5, ?6,
                        0, 0, 1, 'now', 'now', 'own', ?4)",
                    params![format!("t{index}"), ended, kind, outcome, inferred, files],
                )
                .expect("a task written at v44");
            }
        }
        let ledger = Ledger::open(&path).expect("upgrade to v45");
        assert_eq!(ledger.schema_version().expect("version"), 45);
        let answered = |ledger: &Ledger| -> Vec<String> {
            let mut stmt = ledger
                .conn
                .prepare(
                    "SELECT task_id FROM router_tasks WHERE outcome = 'answered' ORDER BY task_id",
                )
                .expect("prepare");
            stmt.query_map([], |row| row.get(0))
                .expect("query")
                .collect::<std::result::Result<Vec<String>, _>>()
                .expect("rows")
        };
        assert_eq!(answered(&ledger), ["t0", "t1", "t7"]);
        let marked: i64 = ledger
            .conn
            .query_row(
                "SELECT COUNT(*) FROM router_tasks WHERE outcome_source = 'backfill_v45'",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(marked, 3);
        let ends = |ledger: &Ledger, wanted: &str| -> Vec<String> {
            let mut stmt = ledger
                .conn
                .prepare(
                    "SELECT task_id FROM router_tasks WHERE completed_at_end = ?1 ORDER BY task_id",
                )
                .expect("prepare");
            stmt.query_map([wanted], |row| row.get(0))
                .expect("query")
                .collect::<std::result::Result<Vec<String>, _>>()
                .expect("rows")
        };
        assert_eq!(ends(&ledger, "answered"), ["t0", "t1", "t7"]);
        assert_eq!(ends(&ledger, "corrected"), ["t6"]);
        ledger
            .conn
            .execute(
                "UPDATE router_tasks
                    SET outcome = 'unknown', outcome_source = 'own',
                        completed_at_end = CASE WHEN completed_at_end = 'answered'
                                                THEN 'unknown' ELSE completed_at_end END
                  WHERE outcome_source = 'backfill_v45'",
                [],
            )
            .expect("undo");
        assert!(ends(&ledger, "answered").is_empty());
        assert_eq!(ends(&ledger, "unknown").len(), 7);
        assert!(answered(&ledger).is_empty());
        let unknown: i64 = ledger
            .conn
            .query_row(
                "SELECT COUNT(*) FROM router_tasks WHERE outcome = 'unknown' AND outcome_source = 'own'",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(unknown, 7);
        std::fs::remove_dir_all(&dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
    }

    /// v45 adds the parent link and unmatched-check columns, and a task
    /// written at v44 reads back with their defaults.
    #[test]
    fn v45_adds_the_parent_and_unmatched_check_columns_with_their_defaults() {
        let dir = temp_dir("v44-to-v45-columns");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            for (version, sql) in &MIGRATIONS[..44] {
                conn.execute_batch(sql).expect("apply step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "then"],
                )
                .expect("record step");
            }
            conn.execute(
                "INSERT INTO router_tasks (session, task_id, agent_id, started_at, ended_at,
                    class_kind, outcome, outcome_rank, inferred_json, files_json,
                    escalations, exhausted, turns, recorded_at, updated_at, outcome_source,
                    completed_at_end)
                 VALUES ('s', 't0', 'a', '2026-10-01T00:00:00Z', NULL, 'debug', 'unknown', 0,
                    '[]', '[]', 0, 0, 1, 'now', 'now', 'own', NULL)",
                [],
            )
            .expect("a task written at v44");
        }
        let ledger = Ledger::open(&path).expect("upgrade to v45");
        assert_eq!(ledger.schema_version().expect("version"), 45);
        let (parent, recorded, unmatched): (Option<String>, i64, Option<String>) = ledger
            .conn
            .query_row(
                "SELECT parent_task_id, parent_link_recorded, unmatched_checks_json
                   FROM router_tasks WHERE task_id = 't0'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("the three columns exist");
        assert_eq!((parent, recorded, unmatched), (None, 0, None));
        std::fs::remove_dir_all(&dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
    }

    /// A ledger frozen at exactly v13 upgrades to v14 without losing any
    /// existing row: the `trials` table, `runs.purpose` and
    /// `dispatches.trial_id` all appear, and every pre-existing row
    /// reads back as ordinary work with no trial id — never guessed
    /// into either.
    #[test]
    fn v14_adds_trials_purpose_and_trial_id_without_touching_existing_rows() {
        let dir = temp_dir("v13-to-v14");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            // Pinned to v13, not `MIGRATIONS.len() - 1`: a step appended
            // after this test is written must not silently change what
            // "at v13" means here.
            for (version, sql) in &MIGRATIONS[..13] {
                conn.execute_batch(sql).expect("apply step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "then"],
                )
                .expect("record step");
            }
            conn.execute(
                "INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                 VALUES ('run-old', '/repo', 'accepted', 'now', 'now')",
                [],
            )
            .expect("a run written before v14");
            conn.execute(
                "INSERT INTO dispatches
                    (dispatch_id, run_id, intent_json, state,
                     reserved_micros, created_at, updated_at)
                 VALUES ('pre-v14', 'run-old', '{}', 'intent', 0, 'now', 'now')",
                [],
            )
            .expect("pre-v14 row");
        }
        let ledger = Ledger::open(&path).expect("upgrade to current schema");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        assert_eq!(
            ledger.trials_by_task(&task("nonexistent")).expect("trials"),
            vec![],
            "the trials table exists and is empty on a fresh upgrade"
        );
        assert_eq!(
            ledger.run_purpose(&run("run-old")).expect("purpose"),
            None,
            "the upgrade adds the column; it does not retroactively fill it in"
        );
        assert_eq!(
            ledger
                .first_dispatch_trial_id(&run("run-old"))
                .expect("trial_id"),
            None
        );
        assert_eq!(
            ledger
                .first_dispatch_intent(&run("run-old"))
                .expect("intent")
                .expect("row survives the upgrade"),
            serde_json::json!({}),
            "the pre-existing row is preserved, not dropped or rewritten"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A ledger frozen at exactly v16 upgrades to v17 keeping every trial
    /// row: `arm_run_id` appears and reads `None` on the rows written
    /// before it, never filled in by a guess.
    #[test]
    fn v17_adds_arm_run_id_and_keeps_every_existing_trial() {
        let dir = temp_dir("v16-to-v17");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            for (version, sql) in &MIGRATIONS[..16] {
                conn.execute_batch(sql).expect("apply step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "then"],
                )
                .expect("record step");
            }
            conn.execute(
                "INSERT INTO trials
                    (trial_id, task_id, source_run_id, incumbent_recipe_id,
                     arm_recipe_id, arm_index, assignment_probability, seed,
                     base_sha, contract_hash, verification_profile_hash,
                     workspace_isolation, created_at)
                 VALUES ('trial-old', 'task-old', 'run-old', 'inc', 'arm', 0, 1.0, 0,
                         'sha', 'ch', 'ph', 'worktree', 'now')",
                [],
            )
            .expect("a trial written before v17");
        }
        let ledger = Ledger::open(&path).expect("upgrade to current schema");
        let rows = ledger.trials_by_task(&task("old")).expect("trials");
        assert_eq!(rows.len(), 1, "the pre-existing trial survives");
        assert_eq!(rows[0].source_run_id, run("run-old"));
        assert_eq!(rows[0].arm_run_id, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A ledger frozen at v26 upgrades to v28: the three driver columns
    /// appear, NULL on the row written before them.
    #[test]
    fn v28_adds_the_driver_columns_to_an_older_ledger() {
        let dir = temp_dir("v26-to-v28");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            // Up to v26: v27 (the typed actor) and v28 run in `open`.
            for (version, sql) in &MIGRATIONS[..26] {
                conn.execute_batch(sql).expect("apply step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "then"],
                )
                .expect("record step");
            }
            conn.execute(
                "INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                 VALUES ('run-old', '/r', 'prepared', 'then', 'then')",
                [],
            )
            .expect("a run written before v28");
        }
        let ledger = Ledger::open(&path).expect("upgrade");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        assert_eq!(driver_of(&ledger, "run-old"), (None, None, None));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// ADR-0007: the step that adds the repository grant's records first
    /// writes a copy of the ledger beside it (what a rollback restores),
    /// then applies; a run records the grant that admitted it, which
    /// `legacy_grant_runs_since` counts; a repository's declaration is an
    /// upsert.
    #[test]
    fn v30_copies_the_ledger_first_then_records_trust_status_and_declarations() {
        let dir = temp_dir("v29-to-v30");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            for (version, sql) in &MIGRATIONS[..29] {
                conn.execute_batch(sql).expect("apply step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "then"],
                )
                .expect("record step");
            }
            conn.execute(
                "INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                 VALUES ('run-old', '/r', 'prepared', '2020-01-01T00:00:00Z', 'then')",
                [],
            )
            .expect("a run written before v30");
        }
        let ledger = Ledger::open(&path).expect("upgrade");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );

        let copy = format!("{}.bak-v30", path.display());
        let before = Connection::open(&copy).expect("the copy opens as a ledger");
        assert_eq!(
            applied_count(&before).expect("count"),
            29,
            "as it was before"
        );
        let old_runs: i64 = before
            .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
            .expect("the copy holds the run");
        assert_eq!(old_runs, 1);
        assert!(
            before.prepare("SELECT trust_status FROM runs").is_err(),
            "the copy is from before the step"
        );

        // The status a run records, and the count doctor reads.
        let old = RunId::from_stored("run-old".to_string());
        ledger
            .set_run_trust_status(&old, "legacy_exact")
            .expect("status");
        assert_eq!(
            ledger
                .legacy_grant_runs_since("2019-01-01T00:00:00Z")
                .expect("count"),
            1
        );
        assert_eq!(
            ledger
                .legacy_grant_runs_since("2021-01-01T00:00:00Z")
                .expect("count"),
            0,
            "a run older than the window is not counted"
        );
        ledger
            .set_run_trust_status(&old, "repository")
            .expect("status");
        assert_eq!(
            ledger
                .legacy_grant_runs_since("2019-01-01T00:00:00Z")
                .expect("count"),
            0
        );

        // A repository's declaration is upserted, one row per repository.
        assert_eq!(ledger.repo_declaration("rk").expect("read"), None);
        ledger
            .upsert_repo_declaration("rk", "h1", "{}")
            .expect("first");
        ledger
            .upsert_repo_declaration("rk", "h2", "{\"a\":1}")
            .expect("second");
        let stored = ledger.repo_declaration("rk").expect("read").expect("a row");
        assert_eq!(stored.declaration_hash, "h2");
        assert_eq!(stored.declaration_json, "{\"a\":1}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A ledger frozen at v26 upgrades to v29: a task written before it
    /// has no type, never a guessed one, and a trial no design.
    #[test]
    fn v29_adds_task_types_and_the_trial_design_without_touching_old_rows() {
        let dir = temp_dir("v26-to-v29");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            for (version, sql) in &MIGRATIONS[..26] {
                conn.execute_batch(sql).expect("apply step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "then"],
                )
                .expect("record step");
            }
            conn.execute(
                "INSERT INTO tasks (task_id, repo_key, first_run, origin, created_at)
                 VALUES ('task-old', 'rk', 'run-old', 'created', 'then')",
                [],
            )
            .expect("a task written before v29");
        }
        let ledger = Ledger::open(&path).expect("upgrade");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        assert_eq!(
            ledger
                .task_type_of(&task("old"))
                .expect("reads the new columns"),
            None
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A ledger frozen at v31 upgrades to v32: a trial written before it
    /// keeps every column and reads back unjudged, and, like the other
    /// additive steps, the step leaves no backup copy.
    #[test]
    fn v32_adds_the_ground_truth_columns_and_keeps_every_trial() {
        let dir = temp_dir("v31-to-v32");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            for (version, sql) in &MIGRATIONS[..31] {
                conn.execute_batch(sql).expect("apply step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "then"],
                )
                .expect("record step");
            }
            conn.execute(
                "INSERT INTO trials (trial_id, task_id, source_run_id, incumbent_recipe_id,
                    arm_recipe_id, arm_index, assignment_probability, seed, base_sha,
                    contract_hash, verification_profile_hash, workspace_isolation,
                    outcome, accepted_without_escalation, created_at)
                 VALUES ('trial-old', 'task-old', 'run-old', 'inc', 'arm', 0, 1.0, 1, 'b',
                    'c', 'p', 'w', 'rejected', 0, 'then')",
                [],
            )
            .expect("a trial written before v32");
        }
        let ledger = Ledger::open(&path).expect("upgrade");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        let old = ledger
            .trial(&TrialId::from_stored("trial-old"))
            .expect("reads")
            .expect("the old trial survives");
        assert_eq!(old.outcome, Some(TrialOutcome::Rejected));
        assert_eq!(old.arm_recipe_id, "arm");
        assert_eq!(old.ground_truth, None);
        assert!(
            !dir.join("ledger.sqlite.bak-v32").exists(),
            "an additive step takes no backup, like v29 and v31"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_actor_step_maps_every_legacy_spelling_keeps_the_original_and_backs_up() {
        let dir = crate::test_support::temp_dir("ledger-v26-to-v27");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("ledger.sqlite");
        let spellings: Vec<&str> = LEGACY_ACTORS.iter().map(|(old, _)| *old).collect();
        assert_eq!(spellings.len(), 7, "the seven spellings of the live ledger");
        v26_ledger_with(&path, &spellings);
        let ledger = Ledger::open(&path).expect("upgrade to v27");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        for (i, (old, expected)) in LEGACY_ACTORS.iter().enumerate() {
            for (table, key) in [("decisions", "run"), ("human_signoffs", "run_id")] {
                let (actor, raw, via): (String, String, String) = ledger
                    .conn
                    .query_row(
                        &format!(
                            "SELECT actor, actor_raw, recorded_via FROM {table} WHERE {key} = ?1"
                        ),
                        [format!("run-{i}")],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .expect("row");
                assert_eq!(actor, expected.as_str(), "{table}: {old}");
                assert_eq!(raw, *old, "{table}: the original is kept");
                assert_eq!(via, "legacy", "{table}");
            }
        }
        let nulls: i64 = ledger
            .conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM decisions WHERE recorded_via IS NULL)
                      + (SELECT COUNT(*) FROM human_signoffs WHERE recorded_via IS NULL)",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(nulls, 0, "every existing row is stamped legacy");
        let open_actor: Option<String> = ledger
            .conn
            .query_row(
                "SELECT actor FROM decisions WHERE run = 'run-open'",
                [],
                |row| row.get(0),
            )
            .expect("open row");
        assert_eq!(open_actor, None, "an open decision still has no actor");
        let backup = dir.join("ledger.sqlite.bak-v27");
        assert!(backup.is_file(), "the VACUUM INTO backup is written");
        let old = Connection::open(&backup).expect("backup opens");
        let kept: String = old
            .query_row(
                "SELECT actor FROM decisions WHERE run = 'run-0'",
                [],
                |row| row.get(0),
            )
            .expect("backup row");
        assert_eq!(
            kept, "claude",
            "the backup holds the pre-migration spelling"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_actor_step_refuses_an_unmapped_spelling_by_name_and_changes_nothing() {
        let dir = crate::test_support::temp_dir("ledger-v26-unmapped");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("ledger.sqlite");
        v26_ledger_with(&path, &["claude", "a stranger"]);
        let refused = match Ledger::open(&path) {
            Ok(_) => panic!("an unmapped spelling must refuse"),
            Err(e) => e.to_string(),
        };
        assert!(refused.contains("a stranger"), "{refused}");
        assert!(
            !dir.join("ledger.sqlite.bak-v27").exists(),
            "no backup for a refusal"
        );
        let conn = Connection::open(&path).expect("open");
        let applied: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("count");
        assert_eq!(applied, 26, "the ledger stays at v26");
        std::fs::remove_dir_all(&dir).ok();
    }
}
