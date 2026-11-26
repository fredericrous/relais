//! The seam between relais and the state several nodes must agree on
//! (programme agents-many-nodes, an-t06 and an-t14).
//!
//! The nodes keep one ledger each (an-t01). What they share is a short
//! list of single-owner operations: the machine's trust and spending
//! config, coordinator seats and their leases, the daily spend, the
//! compare-and-set claims (programme tickets, successor intents, decision
//! answers), the learned registry's pointer, the schema version, and a
//! publish sink for each node's append-only rows. [`SharedState`] names
//! them; [`LocalState`] is today's single-node behaviour behind it, and
//! the default.
//!
//! `machine.toml` selects the implementation with a top-level key:
//! `shared_state = "local"` (or no key) or `"external"`. External means a
//! private build of relais that links a closed client and hands its
//! factory to [`current_with`] at the CLI's composition root; a build
//! without one refuses the routed operations with
//! [`SharedStateError::ExternalAbsent`] rather than fall back, since a
//! silent fallback would split the daily cap and the claims.
//!
//! The trait is public and unstable: it changes with a minor release until
//! the closed build (an-t13) consumes it. The conformance suite
//! (`conformance`, behind the `conformance` feature) is the contract both
//! sides run: `run_all`, and `run_settings_writes`, which is handed a
//! scratch settings path so a check never writes the real one.

mod error;
mod local;
pub mod node;
mod types;

#[cfg(any(test, feature = "conformance"))]
pub mod conformance;
#[cfg(any(test, feature = "conformance"))]
pub mod fake;

use std::path::Path;
use std::sync::Arc;

pub use crate::policy::SharedStateSelection as Selection;
pub use error::SharedStateError;
pub use local::LocalState;
pub use node::{TicketClaim, TicketGrant};
pub use types::{
    DecisionGrant, IntentAnswer, LeaseToken, Mode, Pointer, Principal, PrincipalKind,
    PublishReceipt, PublishRow, Reservation, ResourceId, SettingsEdit, SettingsWhen, SettingsWrite,
    Settlement, Slice, Swap, TicketAnswer, TicketRequest, DECISION_GRANT_TTL_SECS,
};

use crate::ids::RunId;
use crate::money::MicroUsd;
use crate::policy::MachineSettings;

/// What a line that would send the person to the shared primary ends with:
/// no public relais command reads the primary's state yet (an-t24 kept that
/// deliberate), so the line names where the rest is written down instead of
/// a check the person cannot run (an-t31).
pub(crate) const PRIMARY_UNREAD: &str =
    "no relais command reads the shared primary yet; see docs/shared-state.md, Where to look";

/// The single-owner operations nodes share (an-t01's list), shaped by
/// an-t10 (slice, degraded mode, publish queue), an-t11 (an epoch in every
/// [`LeaseToken`], [`SharedStateError::LeaseLost`] on fenced calls) and
/// an-t12 (a [`Principal`] holder).
///
/// The trait is the shared half only, in plain values: no handle (the
/// ledger, the claim store, the registry), no callback and no borrowed
/// request crosses it, so a client can carry every call to a primary on
/// another node. The node half (the local write on those handles) is
/// relais's own, in [`node`]: shared first, then local, then one release
/// when the local step refuses after a shared grant. An implementation with
/// no shared half for an operation answers "the node decides"
/// ([`TicketAnswer::NodeDecides`], [`Pointer::NodeDecides`],
/// [`Swap::NodeDecides`], `None` from [`SharedState::daily_spent`]) or
/// grants.
///
/// A fence is only as wide as its implementation: `LocalState` fences
/// within one instance, an external client across nodes. A caller never
/// reads `lease_lost` from `LocalState` as proof that another node lost.
pub trait SharedState: Send + Sync {
    /// Which implementation this is.
    fn selection(&self) -> Selection;

    /// The machine's settings: trust grants, `[spending]` ceilings and the
    /// rest of `machine.toml`.
    fn machine_settings(&self) -> Result<MachineSettings, SharedStateError>;

    /// Write the machine settings (an-t20), the counterpart of
    /// [`SharedState::machine_settings`]: every reader goes through the
    /// trait, so every writer must, or a node would write a file nobody
    /// reads.
    ///
    /// Only an operator principal (`PrincipalKind::Operator`) may write them (an-t12: a pod
    /// never changes trust or the settings' authority, and a node's
    /// certificate does not make it the person). Any other kind is
    /// [`SharedStateError::Refused`] with nothing changed, on every
    /// implementation: the check is part of the contract
    /// (`conformance::only_an_operator_writes_settings`). The write is one
    /// atomic edit that keeps what it does not name; a write read back
    /// through `machine_settings` shows it
    /// (`conformance::a_settings_write_reads_back`).
    fn write_settings(
        &self,
        writer: &Principal,
        edit: &SettingsEdit,
    ) -> Result<SettingsWrite, SharedStateError>;

    /// What has been spent since `day_start` (RFC 3339, the start of the
    /// UTC day) by the global counter; `None` when there is none, and the
    /// node's own ledger is the count ([`node::daily_spent`]).
    fn daily_spent(&self, day_start: &str) -> Result<Option<MicroUsd>, SharedStateError>;

    /// The part of the daily cap this node may spend without asking.
    fn slice(&self) -> Result<Slice, SharedStateError>;

    /// Set `amount` aside under `seat`. Fenced: a stale seat is
    /// [`SharedStateError::LeaseLost`]; a slice that cannot cover it is
    /// [`SharedStateError::SliceExhausted`].
    fn reserve_spend(
        &self,
        seat: &LeaseToken,
        amount: MicroUsd,
    ) -> Result<Reservation, SharedStateError>;

    /// Book what a reservation actually cost. Never refused for a stale
    /// seat: the money is spent, so it is a true-up (an-t11).
    fn settle_spend(
        &self,
        reservation: &Reservation,
        actual: MicroUsd,
    ) -> Result<Settlement, SharedStateError>;

    /// Take the seat for `dispatch`; every acquire bumps the epoch.
    fn acquire_seat(
        &self,
        holder: &Principal,
        dispatch: &str,
    ) -> Result<LeaseToken, SharedStateError>;

    /// Extend a held seat. Fenced.
    fn renew_seat(&self, lease: &LeaseToken) -> Result<LeaseToken, SharedStateError>;

    /// Give a seat back. Fenced.
    fn release_seat(&self, lease: &LeaseToken) -> Result<(), SharedStateError>;

    /// The shared half of taking a programme ticket as `work`; the claim
    /// file's rules ([`crate::programme::claim::SlugLock::claim_work`]) run
    /// on the node in [`node::claim_ticket`].
    fn claim_ticket(&self, request: &TicketRequest) -> Result<TicketAnswer, SharedStateError>;

    /// Fence a claim the node's claim file granted under
    /// [`TicketAnswer::NodeDecides`]; every grant bumps the epoch.
    fn grant_ticket(&self, request: &TicketRequest) -> Result<LeaseToken, SharedStateError>;

    /// Give a ticket hold back. Fenced.
    fn release_ticket(&self, lease: &LeaseToken) -> Result<(), SharedStateError>;

    /// The shared half of a `handoff` reservation (`relais programme next
    /// --claim`): the ticket is held for the next session until
    /// [`crate::programme::claim::HANDOFF_HOLD_SECS`] after it was made,
    /// and a later `work` claim of any session takes it over. The claim
    /// file's rules run on the node in [`node::reserve_handoff`].
    ///
    /// A retry by the reservation's own holder (a `next --claim` whose
    /// claim file was never written) is never `Held`: a primary grants it
    /// at a higher epoch and moves the hold's end to `HANDOFF_HOLD_SECS`
    /// after the retry. Another holder is
    /// refused while the reservation lasts: a primary answers `Held`,
    /// naming the reserver; a client with no shared half answers
    /// `NodeDecides` and the claim file refuses.
    /// Checked by `conformance::a_handoff_retried_by_its_holder_is_reserved_again`
    /// and, for a primary, `conformance::a_primary_refuses_another_holders_reservation`
    /// and `conformance::a_primary_ends_a_reservation_hold_from_its_last_grant`.
    /// A node whose claim file already holds the holder's `handoff` keeps the
    /// shared hold when the holder asks again
    /// (`conformance::a_handoff_whose_claim_file_exists_keeps_its_shared_hold`).
    fn reserve_handoff(&self, request: &TicketRequest) -> Result<TicketAnswer, SharedStateError>;

    /// Extend every live `work` hold `holder` has: the session is still
    /// working (a resumed session, a claim command). A hold already lapsed
    /// stays lapsed; nothing is refused.
    fn renew_tickets(&self, holder: &Principal) -> Result<(), SharedStateError>;

    /// Give back every `work` hold `holder` has (its session ended); its
    /// `handoff` holds stay, as the claim files keep them.
    fn release_tickets(&self, holder: &Principal) -> Result<(), SharedStateError>;

    /// The shared half of claiming a successor intent for `run`.
    /// `replacing` names the run this claim takes over, as a compare-and-set:
    /// granted only when that run is still the holder.
    fn claim_intent(
        &self,
        intent_id: &str,
        run: &RunId,
        replacing: Option<&RunId>,
    ) -> Result<IntentAnswer, SharedStateError>;

    /// Give an intent claim back (its run never started, or the node's
    /// ledger refused it).
    fn release_intent(&self, intent_id: &str, run: &RunId) -> Result<(), SharedStateError>;

    /// The shared half of answering `run`'s decision: one answer per
    /// decision, compare-and-set. The answer and its intent are written on
    /// the node, in one ledger transaction ([`node::resolve_decision_with_intent`]).
    ///
    /// An implementation that holds the grant answers
    /// [`DecisionGrant::HeldElsewhere`] to a second ask, and lets the grant
    /// lapse [`DECISION_GRANT_TTL_SECS`] after it was given unless
    /// [`SharedState::release_decision`] gave it back, so an answerer that
    /// died before its ledger write does not hold the decision for good.
    /// `LocalState` holds none: the node's ledger decides. The lapse is
    /// checked by `conformance::a_decision_grant_lapses`. The node's ledger
    /// fences a late answer only within that one ledger.
    fn answer_decision(&self, run: &RunId) -> Result<DecisionGrant, SharedStateError>;

    /// Give a decision grant back when the node had no open decision.
    fn release_decision(&self, run: &RunId) -> Result<(), SharedStateError>;

    /// The id the shared active pointer names.
    fn registry_pointer(&self) -> Result<Pointer, SharedStateError>;

    /// Point the shared pointer at `to` when it still names `expected`;
    /// else [`SharedStateError::CasConflict`].
    fn swap_registry_pointer(
        &self,
        expected: Option<&str>,
        to: Option<&str>,
    ) -> Result<Swap, SharedStateError>;

    /// Say which ledger schema version this node runs.
    fn announce_schema(&self, node: &str, version: u64) -> Result<(), SharedStateError>;

    /// Hand append-only rows to the sink; idempotent by node and sequence.
    fn publish(&self, rows: &[PublishRow]) -> Result<PublishReceipt, SharedStateError>;

    /// Rows waiting for the primary to come back.
    fn queued(&self) -> Result<u64, SharedStateError>;

    /// Whether the node talks to its primary.
    fn mode(&self) -> Mode;
}

/// Builds the closed client. A private build passes one to
/// [`current_with`] from the CLI's composition root (an-t13).
pub type ExternalFactory = fn() -> Result<Arc<dyn SharedState>, SharedStateError>;

/// The `shared_state` value `machine.toml` text gives: `Ok(None)` when the
/// key is absent, `Err` with a sentence when it is not one of the two
/// values or the text is not TOML.
pub fn selection_in(text: &str) -> Result<Option<Selection>, String> {
    let table: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    match table.get("shared_state") {
        None => Ok(None),
        Some(toml::Value::String(value)) => Selection::parse(value).map(Some),
        Some(other) => Err(format!(
            "shared_state = {other}: expected \"local\" or \"external\""
        )),
    }
}

/// The selection `machine.toml` at `path` states. Anything that keeps the
/// key from being read (no file, an unreadable or invalid one, an unknown
/// value) selects [`Selection::Local`]: the strict read that follows
/// reports the error exactly as it did before the key existed.
pub fn selection_at(path: &Path) -> Selection {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| selection_in(&text).ok().flatten())
        .unwrap_or_default()
}

/// The implementation the machine selects, given the closed client this
/// build links (`None` in public relais). Built per call: `LocalState`
/// resolves its paths at each call, so a test that points `HOME` elsewhere
/// gets the new paths.
pub fn current_with(
    external: Option<ExternalFactory>,
) -> Result<Arc<dyn SharedState>, SharedStateError> {
    let path = crate::paths::machine_settings_path().map_err(SharedStateError::Home);
    let selection = match &path {
        Ok(path) => selection_at(path),
        // No home: no machine.toml to select from. The local read says so.
        Err(_) => Selection::Local,
    };
    match selection {
        Selection::Local => Ok(Arc::new(LocalState::new())),
        Selection::External => match external {
            Some(factory) => factory(),
            None => Err(SharedStateError::ExternalAbsent {
                machine_toml: path?,
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_reads_as_its_two_values_and_absent_is_none() {
        assert_eq!(selection_in("schema_version = 1\n"), Ok(None));
        assert_eq!(
            selection_in("shared_state = \"local\"\n"),
            Ok(Some(Selection::Local))
        );
        assert_eq!(
            selection_in("shared_state = \"external\"\n"),
            Ok(Some(Selection::External))
        );
        assert!(selection_in("shared_state = \"bogus\"\n").is_err());
        assert!(selection_in("shared_state = 3\n").is_err());
    }

    #[test]
    fn the_strict_settings_parse_accepts_the_key_and_refuses_a_bad_value() {
        let base = format!(
            "schema_version = {}\n",
            crate::policy::MACHINE_SCHEMA_VERSION
        );
        let plain = MachineSettings::from_toml_str(&base).expect("no key");
        for value in ["local", "external"] {
            let with_key = format!("shared_state = \"{value}\"\n{base}");
            assert_eq!(
                MachineSettings::from_toml_str(&with_key).expect("a valid key"),
                plain,
                "the key selects; it is not a setting"
            );
        }
        let bogus = format!("shared_state = \"bogus\"\n{base}");
        let error = MachineSettings::from_toml_str(&bogus).expect_err("an unknown value");
        assert!(error.to_string().contains("shared_state"), "{error}");
    }

    #[test]
    fn an_unreadable_or_unknown_selection_falls_back_to_local() {
        let dir = crate::test_support::temp_dir("shared-state-selection");
        let path = dir.join("machine.toml");
        assert_eq!(selection_at(&path), Selection::Local, "absent file");
        std::fs::write(&path, "shared_state = \"bogus\"\n").expect("write");
        assert_eq!(selection_at(&path), Selection::Local, "unknown value");
        std::fs::write(&path, "not toml [").expect("write");
        assert_eq!(selection_at(&path), Selection::Local, "invalid file");
        std::fs::write(&path, "shared_state = \"external\"\n").expect("write");
        assert_eq!(selection_at(&path), Selection::External);
    }
}
