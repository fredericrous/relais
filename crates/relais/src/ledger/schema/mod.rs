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

/// Every actor spelling a ledger written before the typed [`Actor`] may
/// hold, and the actor it becomes. The v27 SQL `CASE` repeats this table;
/// a test runs each row through the migration so the two cannot drift.
pub const LEGACY_ACTORS: &[(&str, Actor)] = &[
    ("claude", Actor::Orchestrator),
    ("claude-orchestrator", Actor::Orchestrator),
    ("claude-session", Actor::Orchestrator),
    ("claude (for the person)", Actor::PersonViaOrchestrator),
    (
        "Frédéric Rousseau (via Claude Code)",
        Actor::PersonViaOrchestrator,
    ),
    ("fredericrous", Actor::Person),
    ("person", Actor::Person),
];

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

pub const LEDGER_SCHEMA_VERSION: u64 = 40;
