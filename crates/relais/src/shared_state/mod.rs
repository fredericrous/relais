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
//! sides run.

mod error;
mod local;
mod types;

#[cfg(any(test, feature = "conformance"))]
pub mod conformance;
#[cfg(any(test, feature = "conformance"))]
pub mod fake;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

pub use crate::policy::SharedStateSelection as Selection;
pub use error::SharedStateError;
pub use local::LocalState;
pub use types::{
    LeaseToken, Mode, Principal, PrincipalKind, PublishReceipt, PublishRow, Reservation,
    ResourceId, Settlement, Slice,
};

use crate::ids::RunId;
use crate::learn::registry::{EvaluatedArtifact, Registry};
use crate::ledger::{Claim, DecisionAnswer, Ledger, NewIntent, SuccessorIntent, SuccessorProcess};
use crate::money::MicroUsd;
use crate::policy::MachineSettings;
use crate::programme::claim::{ClaimKey, Contest, Store, Taken};

/// One request to take a programme ticket as `work`.
#[derive(Debug, Clone)]
pub struct TicketClaim<'a> {
    pub key: &'a ClaimKey,
    /// The programme's closed tickets: their stale claims are swept first.
    pub closed: &'a BTreeSet<String>,
    pub id: &'a str,
    /// The session taking it; its id is the claim file's `session`.
    pub holder: &'a Principal,
    /// How the repository is shown in the claim file.
    pub repo_display: &'a str,
    pub contest: Contest,
    /// Unix seconds.
    pub now: i64,
}

/// A ticket claim's answer, and the fence of the hold when it was taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketGrant {
    pub taken: Taken,
    /// `Some` when [`Taken::Claimed`]; `None` when refused.
    pub lease: Option<LeaseToken>,
}

/// The single-owner operations nodes share (an-t01's list), shaped by
/// an-t10 (slice, degraded mode, publish queue), an-t11 (an epoch in every
/// [`LeaseToken`], [`SharedStateError::LeaseLost`] on fenced calls) and
/// an-t12 (a [`Principal`] holder).
///
/// Node-local handles (the ledger, the claim store, the registry) are
/// parameters, not state: they stay on the node, and an external
/// implementation adds the shared half around them. They tie an
/// implementation to relais's own types, which is why the trait is
/// unstable until a closed client consumes it.
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

    /// What has been spent since `day_start` (RFC 3339, the start of the
    /// UTC day): this node's ledger alone for a single node, the global
    /// counter for an external primary.
    fn daily_spent(&self, ledger: &Ledger, day_start: &str) -> Result<MicroUsd, SharedStateError>;

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

    /// Take a programme ticket as `work` (the rules of
    /// [`crate::programme::claim::SlugLock::claim_work`]).
    fn claim_ticket(
        &self,
        store: &Store,
        claim: &TicketClaim<'_>,
    ) -> Result<TicketGrant, SharedStateError>;

    /// Claim a successor intent for the run minted for it.
    fn claim_intent(
        &self,
        ledger: &Ledger,
        intent_id: &str,
        run: &RunId,
        alive: &dyn Fn(u32) -> SuccessorProcess,
    ) -> Result<Claim, SharedStateError>;

    /// Give an intent claim back when its run never started.
    fn release_claim(
        &self,
        ledger: &Ledger,
        intent_id: &str,
        run: &RunId,
        refusal: &str,
    ) -> Result<bool, SharedStateError>;

    /// Answer a decision and write its successor intent, as one operation:
    /// the answer and the intent never exist one without the other.
    fn resolve_decision_with_intent(
        &self,
        ledger: &Ledger,
        run: &RunId,
        answer: &DecisionAnswer<'_>,
        intent: &NewIntent,
    ) -> Result<Option<SuccessorIntent>, SharedStateError>;

    /// The id the active pointer names.
    fn registry_pointer(&self, registry: &Registry) -> Result<Option<String>, SharedStateError>;

    /// Point the registry at `evaluated` when the active pointer still
    /// names `expected`; else [`SharedStateError::CasConflict`].
    fn swap_registry_pointer(
        &self,
        registry: &Registry,
        expected: Option<&str>,
        evaluated: &EvaluatedArtifact,
    ) -> Result<(), SharedStateError>;

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
