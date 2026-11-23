//! [`LocalState`]: today's single-node behaviour behind [`SharedState`].
//!
//! Every operation delegates to the code that did the work before the
//! trait existed: `machine.toml` read from the config directory, the
//! node's ledger, the claim store's files, the registry's pointer files.
//! What a single node never needed (seats shared with other nodes, a spend
//! slice, a publish sink) is granted, unbounded or a no-op, so nothing a
//! single-node user did before is refused now.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::types::{
    LeaseToken, Mode, Principal, PublishReceipt, PublishRow, Reservation, ResourceId, Settlement,
    Slice,
};
use super::{Selection, SharedState, SharedStateError, TicketClaim, TicketGrant};
use crate::ids::RunId;
use crate::learn::registry::{EvaluatedArtifact, Registry};
use crate::ledger::{Claim, DecisionAnswer, Ledger, NewIntent, SuccessorIntent, SuccessorProcess};
use crate::money::MicroUsd;
use crate::policy::MachineSettings;
use crate::procs::LockFile;
use crate::programme::claim::{Store, Taken, LOCK_WAIT};

/// Each resource's current epoch and holder (`None` once released).
type EpochTable = HashMap<ResourceId, (u64, Option<Principal>)>;

/// The node name a single node's seats are kept under.
const THIS_NODE: &str = "local";

/// How long a registry swap waits for another swap's lock.
const REGISTRY_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Today's behaviour: paths resolve from the environment at each call, as
/// they always did. The only state is the epoch of each lease this
/// instance handed out, so its fences hold within the instance; fencing
/// across processes and nodes is an external implementation's job.
#[derive(Debug, Clone, Default)]
pub struct LocalState {
    epochs: Arc<Mutex<EpochTable>>,
}

impl LocalState {
    pub fn new() -> Self {
        Self::default()
    }

    fn epochs(&self) -> std::sync::MutexGuard<'_, EpochTable> {
        self.epochs.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Bump `resource`'s epoch and record `holder` as its holder.
    fn grant(&self, resource: ResourceId, holder: &Principal) -> LeaseToken {
        let mut table = self.epochs();
        let entry = table.entry(resource.clone()).or_insert((0, None));
        entry.0 += 1;
        entry.1 = Some(holder.clone());
        LeaseToken {
            resource,
            holder: holder.clone(),
            epoch: entry.0,
            expires_at: None,
        }
    }

    /// Whether `lease` is still the current hold of its resource.
    fn fence(&self, lease: &LeaseToken) -> Result<(), SharedStateError> {
        let table = self.epochs();
        match table.get(&lease.resource) {
            Some((epoch, Some(holder))) if *epoch == lease.epoch && *holder == lease.holder => {
                Ok(())
            }
            _ => Err(SharedStateError::LeaseLost {
                resource: lease.resource.clone(),
                epoch: lease.epoch,
            }),
        }
    }
}

impl SharedState for LocalState {
    fn selection(&self) -> Selection {
        Selection::Local
    }

    fn machine_settings(&self) -> Result<MachineSettings, SharedStateError> {
        let path = crate::paths::machine_settings_path().map_err(SharedStateError::Home)?;
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            // Absent is empty: no ceilings, no grants. A fresh machine then
            // reaches the trust check and is told `missing_trust_grant`,
            // which names the way out, instead of failing on a read before
            // any run exists (SPEC §5).
            Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => {
                format!(
                    "schema_version = {}\n",
                    crate::policy::MACHINE_SCHEMA_VERSION
                )
            }
            // Unreadable is not absent: ceilings and grants the file states
            // would silently not apply.
            Err(cause) => return Err(SharedStateError::Read { path, cause }),
        };
        MachineSettings::from_toml_str(&text).map_err(|cause| SharedStateError::Invalid {
            path,
            cause: Box::new(cause),
        })
    }

    fn daily_spent(&self, ledger: &Ledger, day_start: &str) -> Result<MicroUsd, SharedStateError> {
        Ok(ledger.spend_since(day_start)?)
    }

    fn slice(&self) -> Result<Slice, SharedStateError> {
        Ok(Slice::Unbounded)
    }

    fn reserve_spend(
        &self,
        seat: &LeaseToken,
        amount: MicroUsd,
    ) -> Result<Reservation, SharedStateError> {
        self.fence(seat)?;
        let id = match &seat.resource {
            ResourceId::Seat { dispatch, .. } => format!("{dispatch}#{}", seat.epoch),
            other => format!("{other}#{}", seat.epoch),
        };
        Ok(Reservation {
            id,
            seat: seat.clone(),
            amount,
        })
    }

    fn settle_spend(
        &self,
        reservation: &Reservation,
        _actual: MicroUsd,
    ) -> Result<Settlement, SharedStateError> {
        // The ledger books what a dispatch cost where it always did; this
        // only says whether the seat was still current.
        Ok(match self.fence(&reservation.seat) {
            Ok(()) => Settlement::Booked,
            Err(_) => Settlement::TrueUp,
        })
    }

    fn acquire_seat(
        &self,
        holder: &Principal,
        dispatch: &str,
    ) -> Result<LeaseToken, SharedStateError> {
        // A single node's seats are the coordinator's own; there is no one
        // else to share them with, so the shared half always grants.
        Ok(self.grant(
            ResourceId::Seat {
                node: THIS_NODE.into(),
                dispatch: dispatch.into(),
            },
            holder,
        ))
    }

    fn renew_seat(&self, lease: &LeaseToken) -> Result<LeaseToken, SharedStateError> {
        self.fence(lease)?;
        Ok(lease.clone())
    }

    fn release_seat(&self, lease: &LeaseToken) -> Result<(), SharedStateError> {
        self.fence(lease)?;
        let mut table = self.epochs();
        if let Some(entry) = table.get_mut(&lease.resource) {
            entry.1 = None;
        }
        Ok(())
    }

    fn claim_ticket(
        &self,
        store: &Store,
        claim: &TicketClaim<'_>,
    ) -> Result<TicketGrant, SharedStateError> {
        let lock = store.lock(claim.key, LOCK_WAIT, claim.now)?;
        lock.sweep(claim.closed, claim.now)?;
        let taken = lock.claim_work(
            claim.id,
            &claim.holder.id,
            claim.repo_display,
            claim.contest,
            claim.now,
        )?;
        let lease = match &taken {
            Taken::Claimed { .. } => Some(self.grant(
                ResourceId::Ticket {
                    repo_key: claim.key.repo_key.clone(),
                    slug: claim.key.slug.clone(),
                    id: claim.id.to_string(),
                },
                claim.holder,
            )),
            Taken::Refused { .. } => None,
        };
        Ok(TicketGrant { taken, lease })
    }

    fn claim_intent(
        &self,
        ledger: &Ledger,
        intent_id: &str,
        run: &RunId,
        alive: &dyn Fn(u32) -> SuccessorProcess,
    ) -> Result<Claim, SharedStateError> {
        Ok(ledger.claim_intent(intent_id, run, alive)?)
    }

    fn release_claim(
        &self,
        ledger: &Ledger,
        intent_id: &str,
        run: &RunId,
        refusal: &str,
    ) -> Result<bool, SharedStateError> {
        Ok(ledger.release_claim(intent_id, run, refusal)?)
    }

    fn resolve_decision_with_intent(
        &self,
        ledger: &Ledger,
        run: &RunId,
        answer: &DecisionAnswer<'_>,
        intent: &NewIntent,
    ) -> Result<Option<SuccessorIntent>, SharedStateError> {
        Ok(ledger.resolve_decision_with_intent(run, answer, intent)?)
    }

    fn registry_pointer(&self, registry: &Registry) -> Result<Option<String>, SharedStateError> {
        Ok(registry.active_id()?)
    }

    fn swap_registry_pointer(
        &self,
        registry: &Registry,
        expected: Option<&str>,
        evaluated: &EvaluatedArtifact,
    ) -> Result<(), SharedStateError> {
        // `Registry::promote` takes no lock; this one makes compare and
        // swap one step against every other swap through the trait.
        // holds-until: the learning cycle's automatic promote and rollback
        // (`learn/cycle.rs`) go through `SharedState`; until then they
        // write the pointer without this lock and can race a swap. It is
        // an flock, released by the kernel when the process dies, and the
        // file is never deleted, so a killed promote leaves nothing stale.
        // Beside the pointer files, outside `artifacts/`, which `list` reads.
        let artifacts = registry.artifacts_dir();
        let path = artifacts
            .parent()
            .unwrap_or(artifacts)
            .join(".pointer.lock");
        let started = std::time::Instant::now();
        let _lock = loop {
            match LockFile::try_acquire(&path) {
                Ok(Some(lock)) => break lock,
                Ok(None) if started.elapsed() < REGISTRY_LOCK_WAIT => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Ok(None) => {
                    return Err(SharedStateError::Refused {
                        reason: format!(
                            "another promotion held {} for {}s",
                            path.display(),
                            REGISTRY_LOCK_WAIT.as_secs()
                        ),
                    })
                }
                Err(cause) => return Err(SharedStateError::Lock { path, cause }),
            }
        };
        let found = registry.active_id()?;
        if found.as_deref() != expected {
            return Err(SharedStateError::CasConflict {
                expected: expected.map(str::to_string),
                found,
            });
        }
        Ok(registry.promote(evaluated)?)
    }

    fn announce_schema(&self, _node: &str, _version: u64) -> Result<(), SharedStateError> {
        // One node: the ledger's own migration check is the whole story.
        Ok(())
    }

    fn publish(&self, rows: &[PublishRow]) -> Result<PublishReceipt, SharedStateError> {
        // One node: the rows already live in its ledger, and nobody else
        // reads them. Nothing is kept, so a replay is no different.
        Ok(PublishReceipt {
            accepted: rows.len() as u64,
            duplicates: 0,
            queued: 0,
            through_seq: rows.iter().map(|row| row.seq).max(),
        })
    }

    fn queued(&self) -> Result<u64, SharedStateError> {
        Ok(0)
    }

    fn mode(&self) -> Mode {
        Mode::Online
    }
}
