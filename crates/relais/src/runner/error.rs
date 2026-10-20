//! The runner's own failure type and how a run ends for its driver.

use super::*;

/// How a run ended for its driver: by its own last transition, or by
/// another process that ended it first (whose outcome event stands).
pub(super) enum Finish {
    Own(RunOutcome),
    Superseded(RunOutcome),
    /// The run cannot be recorded at all (relais's storage is full and
    /// nothing could be persisted): `execute` returns this error.
    Exit(RunError),
}

/// The runner's own failure, as distinct from anything a worker did.
#[derive(Debug)]
pub enum RunError {
    Ledger(LedgerError),
    Io(std::io::Error),
    Workspace(WorkspaceError),
    /// The verification plan itself could not be carried out: a pattern
    /// that will not compile, a command with no time to run in.
    Verify(verify::VerifyError),
    /// No identifier could be minted for a run or a dispatch.
    Id(crate::ids::IdError),
    /// Another process ended the run (reaped, cancelled) while this one
    /// drove it: the transition was not written and the driver stops.
    Superseded(RunId),
    /// relais's own storage is full and nothing more could be persisted:
    /// the run's last durable ledger state is what `relais resume`
    /// reconciles from (SPEC §10). `relais run` exits with its own code.
    StorageFull {
        volume: String,
    },
    Other(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ledger(e) => write!(f, "ledger: {e}"),
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Workspace(e) => write!(f, "workspace: {e}"),
            Self::Verify(e) => write!(f, "verification: {e}"),
            Self::Id(e) => write!(f, "{e}"),
            Self::Superseded(run) => write!(
                f,
                "run {} was ended by another process (reaped or cancelled)",
                run.as_str()
            ),
            Self::StorageFull { volume } => write!(
                f,
                "relais's own storage on {volume} is full and the run could not be recorded; \
                 free space there, then `relais resume` the run to reconcile it"
            ),
            Self::Other(detail) => f.write_str(detail),
        }
    }
}

impl RunError {
    /// Does this error say relais's own write met a full disk?
    pub fn is_storage_full(&self) -> bool {
        match self {
            Self::Io(e) => crate::storage_full::is_full(e),
            Self::Ledger(e) => e.is_storage_full(),
            Self::Workspace(e) => e.is_storage_full(),
            Self::Verify(e) => e.is_storage_full(),
            Self::Id(_) | Self::Superseded(_) | Self::StorageFull { .. } | Self::Other(_) => false,
        }
    }
}

impl From<verify::VerifyError> for RunError {
    fn from(e: verify::VerifyError) -> Self {
        Self::Verify(e)
    }
}

impl std::error::Error for RunError {}

impl From<LedgerError> for RunError {
    fn from(e: LedgerError) -> Self {
        Self::Ledger(e)
    }
}

impl From<std::io::Error> for RunError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<WorkspaceError> for RunError {
    fn from(e: WorkspaceError) -> Self {
        Self::Workspace(e)
    }
}

impl From<crate::ids::IdError> for RunError {
    fn from(e: crate::ids::IdError) -> Self {
        Self::Id(e)
    }
}
