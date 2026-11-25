//! [`LocalState`]: today's single-node behaviour behind [`SharedState`].
//!
//! A single node has no shared half: `machine.toml` is read from the
//! config directory as before, and for every operation the node's own
//! ledger, claim files and pointer files decide ([`super::node`] runs them).
//! So this implementation grants, says the node decides, or does nothing;
//! what a single node never needed (seats shared with other nodes, a spend
//! slice, a publish sink) is granted, unbounded or a no-op, so nothing a
//! single-node user did before is refused now.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::types::{
    DecisionGrant, IntentAnswer, LeaseToken, Mode, Pointer, Principal, PrincipalKind,
    PublishReceipt, PublishRow, Reservation, ResourceId, SettingsEdit, SettingsWhen, SettingsWrite,
    Settlement, Slice, Swap, TicketAnswer, TicketRequest,
};
use super::{Selection, SharedState, SharedStateError};
use crate::ids::RunId;
use crate::money::MicroUsd;
use crate::policy::MachineSettings;
use crate::trust::Granted;

/// The settings write's principal check, one sentence for every
/// implementation (an-t20; an-t12: operators only).
pub(super) fn require_operator(writer: &Principal) -> Result<(), SharedStateError> {
    if writer.kind == PrincipalKind::Operator {
        return Ok(());
    }
    Err(SharedStateError::Refused {
        reason: format!(
            "only an operator writes the machine settings; the {} {} may not; run it as the \
             person at a terminal",
            match writer.kind {
                PrincipalKind::Operator => "operator",
                PrincipalKind::Node => "node",
                PrincipalKind::Task => "task",
            },
            writer.id
        ),
    })
}

/// Each resource's current epoch and holder (`None` once released).
type EpochTable = HashMap<ResourceId, (u64, Option<Principal>)>;

/// The node name a single node's seats are kept under.
const THIS_NODE: &str = "local";

/// Today's behaviour: paths resolve from the environment at each call, as
/// they always did. The only state is the epoch of each lease this
/// instance handed out, so its fences hold within the instance; fencing
/// across processes and nodes is an external implementation's job.
#[derive(Debug, Clone, Default)]
pub struct LocalState {
    epochs: Arc<Mutex<EpochTable>>,
    /// A settings file other than the config directory's (conformance runs
    /// and tests); `None` resolves the path at each call.
    settings_path: Option<PathBuf>,
}

impl LocalState {
    pub fn new() -> Self {
        Self::default()
    }

    /// A node whose settings file is `path`, not the config directory's.
    pub fn at(path: PathBuf) -> Self {
        Self {
            settings_path: Some(path),
            ..Self::default()
        }
    }

    fn settings_path(&self) -> Result<PathBuf, SharedStateError> {
        match &self.settings_path {
            Some(path) => Ok(path.clone()),
            None => crate::paths::machine_settings_path().map_err(SharedStateError::Home),
        }
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
        let path = self.settings_path()?;
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

    fn write_settings(
        &self,
        writer: &Principal,
        edit: &SettingsEdit,
    ) -> Result<SettingsWrite, SharedStateError> {
        require_operator(writer)?;
        let path = self.settings_path()?;
        let failed =
            |e: crate::trust::GrantError| SharedStateError::SettingsWrite { cause: Box::new(e) };
        let written = |written: bool| SettingsWrite {
            written,
            removed: false,
        };
        match edit {
            SettingsEdit::GrantTrust { key, record } => {
                crate::trust::grant(&path, key, record).map(|g| written(g == Granted::Written))
            }
            SettingsEdit::GrantRepository { repo_key, record } => {
                crate::trust::grant_repository(&path, repo_key, record)
                    .map(|g| written(g == Granted::Written))
            }
            SettingsEdit::SetAutoPromote {
                grant,
                when: SettingsWhen::IfAbsent,
            } => crate::learn::envelope::write_auto_promote_if_absent(&path, grant).map(written),
            SettingsEdit::SetAutoPromote {
                grant,
                when: SettingsWhen::Always,
            } => crate::learn::envelope::write_auto_promote(&path, grant).map(|()| written(true)),
            SettingsEdit::AutoPromoteOff(off) => {
                crate::learn::envelope::write_off(&path, off).map(|removed| SettingsWrite {
                    written: true,
                    removed,
                })
            }
            SettingsEdit::SetRoutingEnvelope {
                envelope,
                when: SettingsWhen::IfAbsent,
            } => crate::router::write_envelope_if_absent(&path, envelope).map(written),
            SettingsEdit::SetRoutingEnvelope {
                envelope,
                when: SettingsWhen::Always,
            } => crate::router::write_envelope(&path, envelope).map(|()| written(true)),
            SettingsEdit::RoutingOff(off) => {
                crate::router::write_off(&path, off).map(|removed| SettingsWrite {
                    written: true,
                    removed,
                })
            }
        }
        .map_err(failed)
    }

    fn daily_spent(&self, _day_start: &str) -> Result<Option<MicroUsd>, SharedStateError> {
        // No global counter: the node's ledger is the count.
        Ok(None)
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

    fn claim_ticket(&self, _request: &TicketRequest) -> Result<TicketAnswer, SharedStateError> {
        // The claim file decides; `grant_ticket` fences what it wrote.
        Ok(TicketAnswer::NodeDecides)
    }

    fn grant_ticket(&self, request: &TicketRequest) -> Result<LeaseToken, SharedStateError> {
        Ok(self.grant(request.resource(), &request.holder))
    }

    fn release_ticket(&self, lease: &LeaseToken) -> Result<(), SharedStateError> {
        self.fence(lease)?;
        let mut table = self.epochs();
        if let Some(entry) = table.get_mut(&lease.resource) {
            entry.1 = None;
        }
        Ok(())
    }

    fn reserve_handoff(&self, _request: &TicketRequest) -> Result<TicketAnswer, SharedStateError> {
        // The claim file decides; a handoff claim is never fenced.
        Ok(TicketAnswer::NodeDecides)
    }

    fn renew_tickets(&self, _holder: &Principal) -> Result<(), SharedStateError> {
        // Nothing expires here: the claim files keep their own times.
        Ok(())
    }

    fn release_tickets(&self, holder: &Principal) -> Result<(), SharedStateError> {
        // The fences this instance handed the holder for its tickets end.
        for (resource, entry) in self.epochs().iter_mut() {
            if matches!(resource, ResourceId::Ticket { .. }) && entry.1.as_ref() == Some(holder) {
                entry.1 = None;
            }
        }
        Ok(())
    }

    fn claim_intent(
        &self,
        _intent_id: &str,
        _run: &RunId,
        _replacing: Option<&RunId>,
    ) -> Result<IntentAnswer, SharedStateError> {
        // The ledger's own compare-and-set is the whole claim.
        Ok(IntentAnswer::Granted)
    }

    fn release_intent(&self, _intent_id: &str, _run: &RunId) -> Result<(), SharedStateError> {
        Ok(())
    }

    fn answer_decision(&self, _run: &RunId) -> Result<DecisionGrant, SharedStateError> {
        // The ledger transaction finds whether the decision is still open.
        Ok(DecisionGrant::Granted)
    }

    fn release_decision(&self, _run: &RunId) -> Result<(), SharedStateError> {
        Ok(())
    }

    fn registry_pointer(&self) -> Result<Pointer, SharedStateError> {
        Ok(Pointer::NodeDecides)
    }

    fn swap_registry_pointer(
        &self,
        _expected: Option<&str>,
        _to: Option<&str>,
    ) -> Result<Swap, SharedStateError> {
        // The node compares and swaps its pointer files under their flock.
        Ok(Swap::NodeDecides)
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
