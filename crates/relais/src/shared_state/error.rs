//! What a [`super::SharedState`] call can come to besides its answer.

use std::path::PathBuf;

use super::types::ResourceId;
use crate::money::MicroUsd;

/// Why a shared-state call did not do what it was asked.
///
/// The first group is the shared layer's own (an-t10, an-t11). The
/// pass-through group carries today's local errors unchanged, so a call
/// site routed through [`super::LocalState`] says exactly what it said
/// before.
#[derive(Debug)]
pub enum SharedStateError {
    /// The fence moved: the hold expired or another acquire bumped the
    /// epoch. The holder stops new work on it (an-t11).
    LeaseLost { resource: ResourceId, epoch: u64 },
    /// A claim the caller held is no longer its own.
    ClaimLost { resource: ResourceId },
    /// A compare-and-set found another value than the one expected.
    CasConflict {
        expected: Option<String>,
        found: Option<String>,
    },
    /// The primary cannot be reached; the operation fails closed (an-t10).
    Unreachable { detail: String },
    /// The node's spend slice cannot cover the reservation (an-t10).
    SliceExhausted {
        requested: MicroUsd,
        remaining: MicroUsd,
    },
    /// The primary refused the call for a stated reason.
    Refused { reason: String },
    /// `machine.toml` selects `external`, and this build has no shared-state
    /// client (an-t06): relais refuses rather than split the cap.
    ExternalAbsent { machine_toml: PathBuf },
    /// No home directory, so no machine path resolves.
    Home(crate::paths::HomeUnset),
    /// A local file could not be read.
    Read {
        path: PathBuf,
        cause: std::io::Error,
    },
    /// A local lock could not be taken (not held by another: it failed).
    Lock {
        path: PathBuf,
        cause: std::io::Error,
    },
    /// A local file was read and is not valid.
    Invalid {
        path: PathBuf,
        cause: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The node's own ledger.
    Ledger(crate::ledger::LedgerError),
    /// The programme claim store.
    Claim(crate::programme::claim::ClaimError),
    /// The learned-artifact registry.
    Registry(crate::learn::registry::ArtifactError),
}

impl std::fmt::Display for SharedStateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SharedStateError::LeaseLost { resource, epoch } => {
                write!(
                    f,
                    "lease_lost: {resource} is no longer held at epoch {epoch}"
                )
            }
            SharedStateError::ClaimLost { resource } => {
                write!(f, "claim_lost: {resource} is held by another holder")
            }
            SharedStateError::CasConflict { expected, found } => write!(
                f,
                "cas_conflict: expected {}, found {}",
                expected.as_deref().unwrap_or("nothing"),
                found.as_deref().unwrap_or("nothing")
            ),
            SharedStateError::Unreachable { detail } => {
                write!(f, "the shared-state primary is unreachable: {detail}")
            }
            SharedStateError::SliceExhausted {
                requested,
                remaining,
            } => write!(
                f,
                "slice_exhausted: {} micro-USD requested, {} left in this node's slice",
                requested.to_micros(),
                remaining.to_micros()
            ),
            SharedStateError::Refused { reason } => write!(f, "refused: {reason}"),
            SharedStateError::ExternalAbsent { machine_toml } => write!(
                f,
                "machine.toml {} sets shared_state = \"external\", but this relais build \
                 has no shared-state client; set shared_state = \"local\" or install the \
                 build that has one",
                machine_toml.display()
            ),
            SharedStateError::Home(e) => write!(f, "{e}"),
            SharedStateError::Read { path, cause } => {
                write!(f, "cannot read {}: {cause}", path.display())
            }
            SharedStateError::Lock { path, cause } => {
                write!(f, "cannot lock {}: {cause}", path.display())
            }
            SharedStateError::Invalid { path, cause } => {
                write!(f, "{} is invalid: {cause}", path.display())
            }
            SharedStateError::Ledger(e) => write!(f, "{e}"),
            SharedStateError::Claim(e) => write!(f, "{e}"),
            SharedStateError::Registry(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SharedStateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SharedStateError::Home(e) => Some(e),
            SharedStateError::Read { cause, .. } | SharedStateError::Lock { cause, .. } => {
                Some(cause)
            }
            SharedStateError::Invalid { cause, .. } => Some(cause.as_ref()),
            SharedStateError::Ledger(e) => Some(e),
            SharedStateError::Claim(e) => Some(e),
            SharedStateError::Registry(e) => Some(e),
            SharedStateError::LeaseLost { .. }
            | SharedStateError::ClaimLost { .. }
            | SharedStateError::CasConflict { .. }
            | SharedStateError::Unreachable { .. }
            | SharedStateError::SliceExhausted { .. }
            | SharedStateError::Refused { .. }
            | SharedStateError::ExternalAbsent { .. } => None,
        }
    }
}

impl From<crate::ledger::LedgerError> for SharedStateError {
    fn from(e: crate::ledger::LedgerError) -> Self {
        SharedStateError::Ledger(e)
    }
}

impl From<crate::programme::claim::ClaimError> for SharedStateError {
    fn from(e: crate::programme::claim::ClaimError) -> Self {
        SharedStateError::Claim(e)
    }
}

impl From<crate::learn::registry::ArtifactError> for SharedStateError {
    fn from(e: crate::learn::registry::ArtifactError) -> Self {
        SharedStateError::Registry(e)
    }
}
