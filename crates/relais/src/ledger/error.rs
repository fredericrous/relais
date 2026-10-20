//! The ledger's error type and the parsers that turn stored text into typed values.

use crate::lifecycle::{RunPurpose, State};
use crate::money::CostCompleteness;
use crate::policy::Tier;
use crate::route::RoutedBy;

#[derive(Debug)]
pub enum LedgerError {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    /// A stored row this binary cannot read: an unknown state string, a
    /// receipt that is not JSON. A ledger is external state — truncated,
    /// hand-edited or written by another version — so reading one is a
    /// fallible operation, never an assertion about what SQLite holds.
    Corrupt {
        what: String,
        detail: String,
    },
    /// The ledger carries more applied migrations than this binary knows:
    /// a newer relais wrote it, and writing it back could lose what that
    /// version recorded.
    SchemaAhead {
        found: u64,
        known: u64,
    },
    /// A read-only open met a ledger on another schema than this binary's,
    /// older or newer. A reader that cannot migrate cannot tell what the
    /// columns mean, so it does not read.
    SchemaMismatch {
        found: u64,
        known: u64,
    },
}

impl LedgerError {
    /// Does this error say the disk is full (SPEC §10)?
    pub fn is_storage_full(&self) -> bool {
        match self {
            LedgerError::Io(e) => crate::storage_full::is_full(e),
            LedgerError::Sqlite(e) => crate::storage_full::sqlite_full(e),
            _ => false,
        }
    }
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerError::Sqlite(e) => write!(f, "ledger: {e}"),
            LedgerError::Io(e) => write!(f, "ledger: {e}"),
            LedgerError::Corrupt { what, detail } => {
                write!(f, "ledger: {what} cannot be read: {detail}")
            }
            LedgerError::SchemaAhead { found, known } => write!(
                f,
                "ledger: {found} applied migration(s), this relais knows {known} — \
                 a newer relais wrote this ledger; upgrade relais or point \
                 RELAIS_STATE_DIR at another one"
            ),
            LedgerError::SchemaMismatch { found, known } => write!(
                f,
                "ledger: {found} applied migration(s), this relais reads {known} — \
                 not read; the next relais that writes it migrates it"
            ),
        }
    }
}

impl std::error::Error for LedgerError {}

impl From<rusqlite::Error> for LedgerError {
    fn from(e: rusqlite::Error) -> Self {
        LedgerError::Sqlite(e)
    }
}

impl From<std::io::Error> for LedgerError {
    fn from(e: std::io::Error) -> Self {
        LedgerError::Io(e)
    }
}

pub(super) type Result<T> = std::result::Result<T, LedgerError>;

/// A state string as the ledger stored it. An unrecognised one is a
/// corrupt row — a value from a newer relais, a truncated write, a hand
/// edit — and the caller is told, rather than the state silently reading
/// as "no such run" or the process aborting mid-report.
pub(super) fn parse_state(stored: &str) -> Result<State> {
    State::parse(stored).map_err(|unknown| LedgerError::Corrupt {
        what: unknown.what.into(),
        detail: unknown.to_string(),
    })
}

/// A tier string as an attempt row stored it.
pub(super) fn parse_tier(stored: &str) -> Result<Tier> {
    Tier::parse(stored).ok_or_else(|| LedgerError::Corrupt {
        what: "attempt tier".into(),
        detail: format!("`{stored}` is not a tier this relais knows"),
    })
}

/// A `routed_by` string as a dispatch row stored it. An unparseable value
/// is a corrupt row, never silently read as `ConservativeBaseline` — that
/// would assert a decision was actually made when the string just names
/// nothing this relais knows.
pub(super) fn parse_routed_by(stored: &str) -> Result<RoutedBy> {
    RoutedBy::parse(stored).ok_or_else(|| LedgerError::Corrupt {
        what: "dispatch routed_by".into(),
        detail: format!("`{stored}` is not a routing decision this relais knows"),
    })
}

/// A run purpose string as a run row stored it.
pub(super) fn parse_run_purpose(stored: &str) -> Result<RunPurpose> {
    RunPurpose::parse(stored).map_err(|unknown| LedgerError::Corrupt {
        what: unknown.what.into(),
        detail: unknown.to_string(),
    })
}

/// A cost completeness string as a trial row stored it — the same
/// quoted-JSON spelling `record_usage` writes (SPEC §11): `NULL` means
/// "not settled yet", never `Unknown`, which is a positive claim that
/// settlement happened and reported no usage.
pub(super) fn parse_cost_completeness(stored: &str) -> Result<CostCompleteness> {
    serde_json::from_str(stored).map_err(|e| LedgerError::Corrupt {
        what: "trial cost completeness".into(),
        detail: e.to_string(),
    })
}
